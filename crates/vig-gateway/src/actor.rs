//! Der Scheduler-Actor (Spec 9.4).
//!
//! Genau ein Task besitzt den Schedulerzustand. Netzwerkworker senden
//! Ereignisse ueber einen **bounded** Kanal, Backendaufrufe laufen als eigene
//! Tasks und melden ihr Ergebnis als Ereignis zurueck.
//!
//! Der Zuschnitt ist bewusst: keine Locks im Entscheidungspfad, deterministische
//! Zustandsuebergaenge, klare Backpressure und ein Trace, der offline im
//! Simulator nachgespielt werden kann (Spec 30.2).
//!
//! ## Warum der Kanal begrenzt ist
//!
//! Ein unbegrenzter Kanal waere eine Warteschlange, die der Scheduler nicht
//! sieht und nicht steuert — genau das Problem, das ADR-0002 fuer das Backend
//! beschreibt, nur auf der Eingangsseite. Ist der Kanal voll, wird der
//! Aufrufer gebremst, statt Speicher wachsen zu lassen (Spec L-003, 26.4).

use crate::clock::MonotonicClock;
use crate::cooperative::GenerativeJob;
use crate::executor::{Executor, Ticket, TritonExecutor};
use crate::outcome::{mark_obsolete, status_for};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tonic::Status;
use vig_backend_triton::{BackendError, TritonClient};
use vig_config::schema::Resolved;
use vig_core::generative::{Plan, Progress, Verdict};
use vig_core::model::Cooperative;
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::predictor::{ClockClass, StateClass, ThrottleClass};
use vig_core::scheduler::{Action, Event, Scheduler, SchedulerError};
use vig_core::{Instant, Metrics, RequestDescriptor, RequestId, RequestState, SlotIdx};
use vig_protocol_oip::inference::{ModelInferRequest, ModelInferResponse};

/// Kapazitaet des Ereigniskanals.
///
/// Grosszuegig genug, um Ankunftsspitzen aufzunehmen, klein genug, damit
/// Ueberlast als Backpressure beim Aufrufer ankommt statt als Speicherwachstum.
/// Sperrfrist zwischen zwei Warnungen desselben Modells.
const WARN_COOLDOWN: vig_core::time::Duration =
    vig_core::time::Duration::from_nanos_unbounded(60_000_000_000);

const CHANNEL_CAPACITY: usize = 1_024;

/// Abstand zwischen zwei Abgleichversuchen mit der Backendstatistik.
///
/// Der Abgleich laeuft nur, solange ein Ausfuehrungsende unbelegt ist — also
/// im Ausnahmefall. Haeufiger zu fragen beschleunigt die Erholung, belastet
/// aber ein Backend, das ohnehin gerade Probleme hat.
const RECONCILE_INTERVAL_MS: u64 = 250;

/// Prueft jedes Backend regelmaessig auf Erreichbarkeit (Review R11).
///
/// Aktiv, unabhaengig vom Verkehr und **je Endpunkt**. Vorher hing die
/// Bereitschaftsaussage an einem globalen Zaehler, den nur eine erfolgreiche
/// Inferenz zuruecksetzte. Das hatte drei Folgen, und alle drei sind falsch:
///
/// * Nahm ein Loadbalancer den Verkehr weg, weil die Bereitschaft rot war,
///   gab es keinen Ausloeser mehr, der sie wieder gruen macht.
/// * Ein Erfolg an Backend B setzte den Ausfall von Backend A zurueck.
/// * Vor der ersten Inferenz stand der Zaehler auf null — und das las sich
///   wie „bereit", obwohl nichts geprueft war.
///
/// Jede Probe hat eine Frist. Der Verbindungsaufbau hat eine, die **Antwort**
/// eines aufgebauten Kanals nicht; ein Backend, das annimmt und dann
/// schweigt, liesse den Waechter sonst haengen — und ein haengender Waechter
/// meldet nie etwas.
fn spawn_reachability_probe(tx: &mpsc::Sender<Msg>, backends: &HashMap<String, Arc<dyn Executor>>) {
    for (endpoint, backend) in backends {
        let tx = tx.clone();
        let endpoint = endpoint.clone();
        let backend = Arc::clone(backend);
        tokio::spawn(async move {
            let interval = std::time::Duration::from_millis(REACHABILITY_PROBE_INTERVAL_MS);
            let limit = std::time::Duration::from_millis(REACHABILITY_PROBE_TIMEOUT_MS);
            loop {
                let reachable = matches!(
                    tokio::time::timeout(limit, backend.reachable()).await,
                    Ok(Ok(()))
                );
                if tx
                    .send(Msg::Reachability {
                        endpoint: endpoint.clone(),
                        reachable,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(interval).await;
            }
        });
    }
}

/// Wie oft jedes Backend aktiv auf Erreichbarkeit geprueft wird.
///
/// Aktiv und nicht aus dem Verkehr abgeleitet: nimmt ein Loadbalancer den
/// Verkehr weg, weil die Bereitschaft rot ist, gaebe es sonst keinen
/// Ausloeser mehr, der sie wieder gruen macht (Review R11).
const REACHABILITY_PROBE_INTERVAL_MS: u64 = 1_000;

/// Wie lange eine Erreichbarkeitsprobe hoechstens dauern darf.
///
/// Der Verbindungsaufbau hat eine Frist, die **Antwort** eines aufgebauten
/// Kanals hat keine. Ein Backend, das die Verbindung annimmt und dann
/// schweigt, liesse die Probe sonst haengen — und ein haengender Waechter
/// meldet nie etwas, also sieht alles gut aus.
const REACHABILITY_PROBE_TIMEOUT_MS: u64 = 2_000;

/// Das Ergebnis, das ein wartender Client bekommt.
pub type Reply = Result<ModelInferResponse, Status>;

/// Eine Nachricht an den Actor.
#[derive(Debug)]
pub(crate) enum Msg {
    /// Ein neuer Request ist eingetroffen.
    Arrival {
        /// Die Scheduling-Metadaten.
        descriptor: Box<RequestDescriptor>,
        /// Der unveraenderte OIP-Request zur Weitergabe.
        request: Box<ModelInferRequest>,
        /// Wohin die Antwort geht.
        reply: oneshot::Sender<Reply>,
        /// Die Reservierung des Nutzlastbudgets.
        ///
        /// Sie reist mit und wird erst freigegeben, wenn die Ausfuehrung
        /// nachweislich vorbei ist. Beim Client zu enden waere die falsche
        /// Lebensdauer: nach einem Timeout rechnet das Backend weiter und
        /// haelt die Nutzlast (Review R04).
        permit: crate::budget::PayloadPermit,
    },
    /// Ein Backendaufruf ist beendet.
    BackendDone {
        /// Der betroffene Request.
        request: RequestId,
        /// Der belegte Slot.
        slot: SlotIdx,
        /// Das Ergebnis.
        result: Box<Result<ModelInferResponse, BackendError>>,
    },
    /// Der Abgleich meldet den Zaehlerstand eines Backendmodells.
    ///
    /// **Nicht** „dieser Request ist fertig": das kann ein aggregierter
    /// Zaehler nicht sagen. Er kann nur sagen, wie viele Inferenzen dieses
    /// Modell insgesamt abgeschlossen hat. Ob daraus ein Nachweis folgt,
    /// entscheidet der Actor — er allein kennt die Zahl der Auslieferungen,
    /// die noch offen sein koennen.
    CompletionEvidence {
        /// Das Backendmodell.
        model: String,
        /// Der Statistikzaehler des Modells.
        completed: u64,
        /// Ob der Zaehler zurueckgesprungen ist.
        ///
        /// Ein Zaehler faellt nur, wenn das Modell neu geladen oder der
        /// Server neu gestartet wurde — und dann ist alles, was dort lief,
        /// ohnehin verloren.
        restarted: bool,
    },
    /// Ein Anwendungshinweis (NV-18, ADR-0029).
    ///
    /// Er wird **nicht** beantwortet: eine Ablehnung ist eine Betriebsmeldung
    /// und kein Fehler des Requests, der ihn mitgebracht hat. Ein Hinweis,
    /// der die Zulassung eines Requests scheitern liesse, waere ein Hebel,
    /// den er nicht haben soll.
    Hint {
        /// Der Hinweis in Kernform.
        hint: Box<vig_core::hints::Hint>,
    },
    /// Das Ergebnis einer aktiven Erreichbarkeitsprobe (Review R11).
    Reachability {
        /// Der gepruefte Endpunkt.
        endpoint: String,
        /// Ob er geantwortet hat.
        reachable: bool,
    },
    /// Die Abgleichs-Basislinie eines Backendmodells ist eingetroffen.
    ///
    /// Einmal beim Start. Bis sie da ist, laufen Auslieferungen dieses Modells
    /// ohne zaehlerbasierten Nachweis — vorsichtig und nicht falsch.
    ReconcileBaseline {
        /// Das Backendmodell.
        model: String,
        /// Der Statistikzaehler zum Startzeitpunkt.
        completed: u64,
    },
    /// Der beobachtete Hardwarezustand hat sich gemeldet (NV-04, NV-06).
    ///
    /// Der Kern misst nichts; er bekommt den Zustand gesagt, wie er auch die
    /// Zeit gesagt bekommt. Bleibt die Meldung aus, bleibt der Zustand
    /// „unbekannt" — und die zustandsabhaengige Prognose gibt dann gar keine
    /// Aussage statt der guenstigsten.
    HardwareState {
        /// Die beobachtete Klasse, ohne Belegungsgrad.
        state: StateClass,
    },
    /// Ein Backendaufruf antwortet seit dem Timeout nicht.
    ///
    /// Traegt **keinen** Slot: der Kredit wird bewusst nicht zurueckgegeben.
    BackendTimeout {
        /// Der betroffene Request.
        request: RequestId,
    },
    /// Der Client wartet nicht mehr auf diesen Request.
    Cancel {
        /// Der zurueckgezogene Request.
        request: RequestId,
    },
    /// Beendet den Actor, sobald keine Arbeit mehr offen ist.
    ///
    /// Traegt einen Kanal, ueber den der Actor seinen Abschluss meldet: ohne
    /// ihn wuesste der Aufrufer nicht, wann er den Prozess verlassen darf.
    Shutdown(oneshot::Sender<()>),
    /// Ein Weckruf.
    Tick,
    /// Momentaufnahme der Zaehler (fuer Metrikabfragen und Tests).
    Snapshot(oneshot::Sender<Metrics>),
}

/// Ein Anspruch auf einen Slotkredit, der **nur durch Nachweis** endet.
///
/// Der Kredit gehoert zur Recheneinheit, nicht zum Client. Er wird deshalb
/// erst zurueckgegeben, wenn belegt ist, dass die Einheit wieder frei ist:
/// durch eine Antwort des Backends oder durch einen Abgleich mit dessen
/// Statistik. Eine abgelaufene Frist ist **kein** Beleg — ein Timeout sagt,
/// dass wir nicht laenger warten wollen, nicht dass die GPU aufgehoert hat.
#[derive(Debug, Clone)]
struct Lease {
    /// Der Slot, dessen Kredit gehalten wird.
    slot: SlotIdx,
    /// Das Backendmodell, gegen dessen Statistik abgeglichen wird.
    backend_model: String,
    /// Der Endpunkt, an den dieser Aufruf ging.
    ///
    /// Ein Transportfehler sagt etwas ueber **diesen** Endpunkt und ueber
    /// keinen anderen. Ohne ihn liesse sich der Befund nur global buchen, und
    /// ein Erfolg an Backend B deckte den Ausfall von Backend A zu
    /// (Review R11).
    endpoint: String,
    /// Wie der Anspruch derzeit steht.
    state: LeaseState,
}

/// Der Wissensstand ueber eine ausgelieferte Inferenz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseState {
    /// Der Aufruf laeuft; seine Antwort wird den Kredit freigeben.
    Running,
    /// Das Timeout ist abgelaufen, der Aufruf laeuft weiter.
    ///
    /// Der Client ist beantwortet. Der Kredit bleibt, bis der Aufruf
    /// zurueckkehrt — das ist der Nachweis.
    TimedOut,
    /// Der Aufruf brach ab; ob die Einheit noch rechnet, ist unbekannt.
    ///
    /// Es wird nie mehr eine Antwort kommen. Der Nachweis muss deshalb beim
    /// Backend geholt werden.
    Reconciling,
    /// Der Aufruf brach ab, und der Abgleich konnte noch nicht anlaufen.
    ///
    /// Es fehlt die Basislinie des Statistikzaehlers; sie wird beim Start
    /// geholt und ist gewoehnlich binnen Millisekunden da. Ein Aufruf, der
    /// genau davor abbricht, haelt seinen Kredit — und der naechste Tick
    /// versucht den Abgleich erneut. Ein Ziel ohne Basislinie waere im Zweifel
    /// zu klein und gaebe den Kredit frei, der gehalten gehoert.
    AwaitingBaseline,
}

/// Meldet dem Actor, dass niemand mehr auf einen Request wartet.
///
/// Beim regulaeren Abschluss wird der Waechter mit [`Self::disarm`]
/// entschaerft — der Kanal ist dann bereits bedient, und `Msg::Cancel` waere
/// nur noch Rauschen.
///
/// Bewusst ueber ein Feld und nicht ueber `core::mem::forget`: `forget` liesse
/// den enthaltenen `Sender` liegen. Dessen Zaehler faellt dann nie, der
/// Eingangskanal gilt auf ewig als offen, und je erfolgreichem Request bliebe
/// etwas Speicher zurueck — bei einem Dauerlauf ueber Stunden genau das Leck,
/// das dieser Review an anderer Stelle bemaengelt hat.
///
/// `try_send` statt `send`, weil `Drop` nicht warten kann: geht die Nachricht
/// bei vollem Kanal verloren, verhaelt sich das System wie bisher — der
/// Request laeuft dann eben durch. Ein blockierender Drop waere schlimmer.
struct CancelOnDrop {
    tx: mpsc::Sender<Msg>,
    /// `None`, sobald der Request regulaer beantwortet wurde.
    request: Option<RequestId>,
}

impl CancelOnDrop {
    /// Nimmt den Waechter aus der Schaltung.
    fn disarm(&mut self) {
        self.request = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(request) = self.request {
            let _ = self.tx.try_send(Msg::Cancel { request });
        }
    }
}

/// Der Griff, ueber den das Gateway den Actor erreicht.
#[derive(Debug, Clone)]
pub struct Handle {
    tx: mpsc::Sender<Msg>,
}

impl Handle {
    /// Reicht einen Request ein und wartet auf sein Ergebnis.
    ///
    /// # Errors
    ///
    /// `ResourceExhausted`, wenn der Ereigniskanal voll ist — das ist die
    /// Backpressure des Gateways, nicht ein Fehler. `Internal`, wenn der Actor
    /// beendet wurde.
    pub async fn submit(
        &self,
        descriptor: RequestDescriptor,
        request: ModelInferRequest,
        permit: crate::budget::PayloadPermit,
    ) -> Reply {
        let id = descriptor.id;
        let (reply, wait) = oneshot::channel();
        let msg = Msg::Arrival {
            descriptor: Box::new(descriptor),
            request: Box::new(request),
            reply,
            permit,
        };
        self.tx.try_send(msg).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                Status::resource_exhausted("Vigilant nimmt derzeit keine weiteren Requests an")
            }
            mpsc::error::TrySendError::Closed(_) => {
                Status::internal("der Scheduler ist nicht mehr aktiv")
            }
        })?;

        // Bricht der Client ab, verwirft tonic diese Future. Ohne den Waechter
        // bliebe der Request in der Queue, wuerde spaeter weitergereicht und
        // verbraeuchte Backendzeit fuer einen Empfaenger, den es nicht mehr
        // gibt — und zwar genau die Kapazitaet, um die noch wartende Stroeme
        // konkurrieren.
        let mut cancel = CancelOnDrop {
            tx: self.tx.clone(),
            request: Some(id),
        };
        let outcome = wait
            .await
            .map_err(|_| Status::internal("der Scheduler hat den Request verworfen"))?;
        cancel.disarm();
        outcome
    }

    /// Faehrt den Actor herunter und wartet auf seinen Abschluss.
    ///
    /// Wartende Arbeit wird noch beantwortet, laufende Backendaufrufe laufen zu
    /// Ende — ein Abbruch mitten in einer Inferenz liesse den Client ohne
    /// Antwort und die GPU trotzdem rechnen. Kommt der Actor innerhalb von
    /// `deadline` nicht zum Abschluss, kehrt diese Funktion trotzdem zurueck
    /// und meldet `false`: eine Drain-Frist, die nicht endet, ist keine.
    ///
    /// # Errors
    ///
    /// Wenn der Actor bereits beendet ist.
    pub async fn drain(&self, deadline: std::time::Duration) -> Result<bool, Status> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Msg::Shutdown(tx))
            .await
            .map_err(|_| Status::internal("der Scheduler ist nicht mehr aktiv"))?;
        Ok(tokio::time::timeout(deadline, rx).await.is_ok())
    }

    /// Liest die aktuellen Zaehler.
    ///
    /// # Errors
    ///
    /// Wenn der Actor beendet wurde.
    /// Reicht einen Anwendungshinweis an den Kern durch (NV-18).
    ///
    /// Ohne Antwort und ohne Fehlerweg zum Aufrufer: was der Kern mit dem
    /// Hinweis macht, entscheidet die Policy des Betreibers, und eine
    /// Ablehnung ist eine Betriebsmeldung. Ein Hinweis darf die Zulassung des
    /// Requests, der ihn mitgebracht hat, nicht scheitern lassen.
    pub fn offer_hint(&self, hint: vig_core::hints::Hint) {
        // `try_send`: ein voller Kanal heisst, dass gerade Wichtigeres
        // ansteht. Ein Hinweis, der dafuer Platz verdraengt, waere die
        // falsche Reihenfolge.
        let _ = self.tx.try_send(Msg::Hint {
            hint: Box::new(hint),
        });
    }

    /// Der aktuelle Metrikabzug.
    ///
    /// # Errors
    ///
    /// Wenn der Actor beendet wurde.
    pub async fn metrics(&self) -> Result<Metrics, Status> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Msg::Snapshot(tx))
            .await
            .map_err(|_| Status::internal("der Scheduler ist nicht mehr aktiv"))?;
        rx.await
            .map_err(|_| Status::internal("der Scheduler hat nicht geantwortet"))
    }
}

/// Der Actor.
struct Actor {
    scheduler: Scheduler,
    config: Arc<Resolved>,
    /// Ein Client je Backend-Endpunkt.
    ///
    /// Vision- und Sprachmodelle laufen in getrennten Servern, weil ihre
    /// Backends unvereinbare Bibliotheksstaende brauchen. Die Kapazitaets-
    /// rechnung bleibt davon unberuehrt: die Slots modellieren die GPU, nicht
    /// den Prozess.
    backends: HashMap<String, Arc<dyn Executor>>,
    clock: MonotonicClock,
    tx: mpsc::Sender<Msg>,
    /// Wartende Clients je Request.
    waiting: HashMap<RequestId, oneshot::Sender<Reply>>,
    /// Noch nicht weitergereichte OIP-Requests.
    ///
    /// Sie liegen hier, weil ein Request zwischen Ankunft und Dispatch noch
    /// verdraengt werden kann. Erst beim Dispatch wandert die Payload weiter.
    inbox: HashMap<RequestId, Box<ModelInferRequest>>,
    /// Bereits eingetroffene Backendantworten, die auf ihre Bewertung warten.
    responses: HashMap<RequestId, Result<ModelInferResponse, BackendError>>,
    /// Laufende zerlegte Auftraege (ADR-0014).
    jobs: HashMap<RequestId, GenerativeJob>,
    /// Die Beschreibung eines laufenden Auftrags, fuer seine Fortsetzung.
    descriptors: HashMap<RequestId, RequestDescriptor>,
    /// Ausgelieferte Inferenzen, deren Ende noch nicht belegt ist.
    ///
    /// Solange ein Eintrag hier steht, ist der Slotkredit gehalten. Er ist
    /// die einzige Wahrheit darueber, ob die Recheneinheit belegt sein
    /// koennte — der Scheduler kennt nur, was er selbst gestartet hat.
    leases: HashMap<RequestId, Lease>,
    /// Naechste Lease-Generation.
    /// Wie oft ein Backendaufruf das Timeout ueberschritten hat.
    backend_timeouts: u64,
    /// Transportfehler seit dem letzten erfolgreichen Backendaufruf.
    consecutive_transport_failures: u64,
    /// Wie oft ein Ausfuehrungsende durch Abgleich belegt wurde.
    reconciled: u64,
    /// Wie viele Inferenzen dieser Governor je Backendmodell ausgeliefert hat.
    dispatched_per_model: HashMap<String, u64>,
    /// Der Statistikzaehler je Backendmodell zum Startzeitpunkt.
    ///
    /// Einmal beim Start geholt, nicht beim Abgleich: eine Basislinie, die
    /// erst der Abgleich abfragt, kaeme zu spaet — das Backend kann inzwischen
    /// fertig geworden sein. Fehlt ein Eintrag, gibt es fuer dieses Modell
    /// keinen zaehlerbasierten Nachweis.
    reconcile_baseline: HashMap<String, u64>,
    /// Requests, die wegen vollstaendiger Quarantaene abgewiesen wurden.
    metrics_rejected_quarantined: u64,
    /// Die Fortschrittsbuchhaltung ueber alle zerlegten Auftraege (NV-16).
    ///
    /// Getrennt nach Prefill und Dekodierung: ein Re-Prefill erzeugt kein
    /// Token, und ihn als Fortschritt zu buchen hiesse, dieselbe Arbeit
    /// zweimal zu verkaufen.
    generative: GenerativeAccounting,
    /// Das jeweils letzte Ergebnis der Erreichbarkeitsprobe je Endpunkt
    /// (Review R11).
    ///
    /// Ein Eintrag entsteht erst mit der ersten Antwort. Solange er fehlt,
    /// gilt der Endpunkt als **nicht** belegt erreichbar — vor der ersten
    /// Probe ist die Lieferfaehigkeit ungeprueft, und ein Zaehler, der dann
    /// auf null steht, ist keine Zusage.
    reachability: HashMap<String, bool>,
    /// Die Nutzlastreservierungen laufender Auftraege (Review R04).
    ///
    /// Sie enden mit der **Ausfuehrung**, nicht mit dem Client. Nach einem
    /// Client-Timeout rechnet das Backend weiter und haelt die Nutzlast; das
    /// Budget dort freizugeben liesse eine zweite Nutzlast derselben Groesse
    /// zu, waehrend die erste noch steht.
    permits: HashMap<RequestId, crate::budget::PayloadPermit>,
    /// Backendaufrufe, die noch offen sind.
    outstanding: u64,
    /// Gesetzt, sobald ein geordnetes Ende angefordert wurde.
    shutdown: Option<oneshot::Sender<()>>,
    /// Die Revision der Profilidentitaet dieser Konfiguration (NV-03, NV-06).
    ///
    /// Wechselt nur beim Neuladen, nicht mit dem Hardwarezustand. Beides
    /// zusammen bildet die Epoche, unter der Beobachtungen gelten.
    profile_revision: u32,
    /// Die aktuelle Kennung eines zerlegten Auftrags, unter seiner
    /// **urspruenglichen** Kennung.
    ///
    /// Eine Fortsetzung tritt als neue Ankunft mit neuer Kennung an. Der
    /// Client kennt nur seine erste: bricht er ab, traegt sein Cancel die
    /// urspruengliche Kennung, und ohne diese Abbildung liefe der Auftrag
    /// weiter, bis sein Tokenbudget erschoepft ist. Bei einem generativen
    /// Modell ist das die teuerste Arbeit im System — fuer einen Empfaenger,
    /// den es nicht mehr gibt.
    continuation_of: HashMap<RequestId, RequestId>,
    /// Kennungen fuer Fortsetzungsauftraege.
    next_id: u64,
    /// Naechster geplanter Weckruf.
    next_wake: Option<Instant>,
    /// Wann zuletzt vor einer vertragswidrigen Last gewarnt wurde.
    ///
    /// Ohne Sperrfrist stuende die Warnung bei jedem Tick im Protokoll und
    /// waere nach einer Minute unlesbar — eine Meldung, die zu oft kommt,
    /// wird weggefiltert und schuetzt dann nichts mehr.
    warned_arrival: [Option<Instant>; vig_core::ids::MAX_MODELS],
}

/// Startet den Actor und gibt seinen Griff zurueck.
///
/// # Errors
///
/// [`SchedulerError`], wenn die aufgeloeste Konfiguration keinen gueltigen
/// Scheduler ergibt.
pub fn spawn(
    config: Arc<Resolved>,
    backend: &Arc<TritonClient>,
    clock: MonotonicClock,
    unverified: &[vig_core::ModelIdx],
) -> Result<Handle, SchedulerError> {
    // Fuer jeden in der Konfiguration genannten Endpunkt ein Client. Der
    // uebergebene deckt den Standardendpunkt ab.
    let mut backends: HashMap<String, Arc<dyn Executor>> = HashMap::new();
    for endpoint in config.endpoints() {
        let client = if endpoint == config.backend_endpoint {
            Arc::clone(backend)
        } else {
            Arc::new(TritonClient::new(&endpoint))
        };
        backends.insert(endpoint, Arc::new(TritonExecutor::new(client)));
    }
    spawn_with(config, backends, clock, unverified)
}

/// Startet den Actor mit selbst gewaehlten Executoren (NV-07).
///
/// Die Naht, an der ein Test ohne GPU ansetzt: derselbe Actor, dieselbe
/// Ablaufsteuerung, ein Backend, das auf Kommando abbricht. Genau die
/// Fehlerpfade, in denen sich entscheidet, ob ein Slotkredit zu frueh
/// zurueckkommt, lassen sich auf echter Hardware kaum herbeifuehren.
///
/// # Errors
///
/// Wenn die Konfiguration keinen gueltigen Scheduler ergibt.
pub fn spawn_with<S: std::hash::BuildHasher>(
    config: Arc<Resolved>,
    backends: HashMap<String, Arc<dyn Executor>, S>,
    clock: MonotonicClock,
    unverified: &[vig_core::ModelIdx],
) -> Result<Handle, SchedulerError> {
    // Der Actor fuehrt seine eigene Tabelle; der Hasher des Aufrufers geht
    // ihn nichts an.
    let backends: HashMap<String, Arc<dyn Executor>> = backends.into_iter().collect();
    spawn_owned(config, backends, clock, unverified)
}

fn spawn_owned(
    config: Arc<Resolved>,
    backends: HashMap<String, Arc<dyn Executor>>,
    clock: MonotonicClock,
    unverified: &[vig_core::ModelIdx],
) -> Result<Handle, SchedulerError> {
    let overload = OverloadController::new(OverloadConfig::default(), clock.now())
        .map_err(|_| SchedulerError::NoModels)?;
    let mut scheduler = Scheduler::new(
        config.contracts.clone(),
        config.slots.clone(),
        overload,
        config.margin,
    )?;
    // NV-24: der Missbudget-Regler wird hier eingeschaltet, wenn der Betreiber
    // ihn eingeschaltet hat. Vorher war er gebaut, getestet und durch keine
    // Konfiguration erreichbar (Review R09).
    scheduler.set_miss_aware_policy(config.miss_aware_policy);
    // NV-18: dasselbe fuer die Hinweispolicy. Ohne `hints:` in der
    // Konfiguration ist sie geschlossen und nimmt nichts an.
    scheduler.set_hint_policy(config.hint_policy);

    // G-010: Profile, deren Umgebung sich geaendert hat, werden vorsichtiger
    // geplant, bis der Estimator eigene Messungen hat (ADR-0016).
    for model in unverified {
        scheduler.mark_profile_unverified(*model);
    }

    // Was der Vertrag fordert und dieser Dienst nicht einloest, wird beim
    // Start genannt — einmal, laut, je Modell (Review R05). Ein gueltiges
    // YAML-Dokument ist kein angenommener Betriebsvertrag, und der Betreiber
    // soll es hier erfahren und nicht beim ersten Vorfall.
    for (index, contract) in config.contracts.iter().enumerate() {
        let Some(extension) = contract.extension.as_ref() else {
            continue;
        };
        let model = config.model_names.get(index).map_or("?", String::as_str);
        for item in extension.unenforced().iter() {
            tracing::warn!(
                model,
                field = item.field,
                reason = item.reason,
                "im Vertrag gefordert, von dieser Version nicht durchgesetzt"
            );
        }
    }

    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    spawn_hardware_probe(&tx);
    spawn_baseline_probe(&tx, &config, &backends);
    spawn_reachability_probe(&tx, &backends);
    // Die Revision der Profilidentitaet: solange sie nicht aus dem Manifest
    // kommt, ist sie 1 fuer eine geladene Konfiguration. Wichtig ist nicht
    // ihr Wert, sondern dass sie sich aendert, wenn die Profile es tun.
    let profile_revision = 1;
    let actor = Actor {
        scheduler,
        profile_revision,
        config,
        backends,
        clock,
        tx: tx.clone(),
        waiting: HashMap::new(),
        inbox: HashMap::new(),
        responses: HashMap::new(),
        jobs: HashMap::new(),
        descriptors: HashMap::new(),
        continuation_of: HashMap::new(),
        leases: HashMap::new(),
        backend_timeouts: 0,
        consecutive_transport_failures: 0,
        reconciled: 0,
        dispatched_per_model: HashMap::new(),
        reconcile_baseline: HashMap::new(),
        metrics_rejected_quarantined: 0,
        generative: GenerativeAccounting::default(),
        permits: HashMap::new(),
        reachability: HashMap::new(),
        outstanding: 0,
        shutdown: None,
        // Fortsetzungen bekommen Kennungen aus einem eigenen Bereich, damit
        // sie sich nicht mit denen des Gateways ueberschneiden.
        next_id: u64::MAX.wrapping_div(2),
        next_wake: None,
        warned_arrival: [None; vig_core::ids::MAX_MODELS],
    };
    tokio::spawn(actor.run(rx));
    Ok(Handle { tx })
}

impl Actor {
    async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        loop {
            let sleep = self.sleep_until_next_wake();
            let msg = tokio::select! {
                received = rx.recv() => match received {
                    Some(msg) => msg,
                    None => break,
                },
                () = sleep => {
                    // Der geplante Weckruf ist damit verbraucht. Ohne das
                    // Zuruecksetzen bliebe `next_wake` in der Vergangenheit
                    // stehen, und `sleep_until_next_wake()` faende dauerhaft
                    // die 1-ms-Notbremse — der Actor liefe dann auch im
                    // vollstaendig leeren System im Millisekundentakt weiter.
                    // Auf Edgehardware ist das Leerlaufenergie fuer nichts.
                    // Braucht der Scheduler einen neuen Weckruf, meldet er ihn
                    // beim naechsten Durchlauf ohnehin wieder an.
                    self.next_wake = None;
                    Msg::Tick
                }
            };
            self.handle(msg);

            // Nach jeder Nachricht pruefen, ob das angeforderte Ende jetzt
            // erreichbar ist: kein wartender Client mehr, kein Backendaufruf
            // mehr offen.
            // Auch `outstanding`: nach einem Timeout ist der Client
            // beantwortet, die Recheneinheit aber womoeglich weiterhin belegt.
            // Wer nur die Clients zaehlt, meldet ein sauberes Ende, waehrend
            // die GPU noch rechnet — und der naechste Prozess startet in eine
            // Belegung, von der er nichts weiss.
            // Auch die offenen Anspruecke: nach einem Timeout oder einem
            // Abbruch ist der Client beantwortet und der Aufruf womoeglich
            // zurueck, die Recheneinheit aber weiterhin belegt. Wer nur
            // Clients und laufende Aufrufe zaehlt, meldet ein sauberes Ende,
            // waehrend die GPU noch rechnet.
            if self.shutdown.is_some()
                && self.waiting.is_empty()
                && self.responses.is_empty()
                && self.outstanding == 0
                && self.leases.is_empty()
            {
                if let Some(done) = self.shutdown.take() {
                    let _ = done.send(());
                }
                break;
            }
        }
        tracing::info!("Scheduler-Actor beendet");
    }

    /// Wartet bis zum naechsten geplanten Weckruf.
    ///
    /// Ohne diesen Weckruf bliebe eine non-work-conserving Entscheidung
    /// haengen: der Scheduler hat absichtlich nichts gestartet und wuerde ohne
    /// aeusseres Ereignis nie wieder nachsehen (Spec 10.7).
    fn sleep_until_next_wake(&self) -> tokio::time::Sleep {
        let now = self.clock.now();
        let delay = match self.next_wake {
            Some(wake) if wake > now => {
                std::time::Duration::from_nanos(wake.saturating_since(now).as_nanos())
            }
            // Kein geplanter Weckruf: lange schlafen, bis ein Ereignis kommt.
            Some(_) => std::time::Duration::from_millis(1),
            None => std::time::Duration::from_secs(3_600),
        };
        tokio::time::sleep(delay)
    }

    /// Nimmt einen Request an — oder weist ihn sofort ab.
    ///
    /// Gibt `false` zurueck, wenn die Bearbeitung hier endet.
    fn accept<S: FnMut(Action)>(
        &mut self,
        now: Instant,
        mut descriptor: RequestDescriptor,
        mut request: Box<ModelInferRequest>,
        reply: oneshot::Sender<Reply>,
        permit: crate::budget::PayloadPermit,
        sink: &mut S,
    ) -> bool {
        let id = descriptor.id;
        // Ab hier gehoert die Reservierung dem Actor. Wird der Request unten
        // abgewiesen, faellt sie mit dieser Funktion — das ist richtig, denn
        // dann hat nichts gerechnet.
        self.permits.insert(id, permit);

        // Steht jeder Slotkredit in Quarantaene, kann nichts starten — und
        // zwar nicht "gerade nicht", sondern bis das Backend antwortet. Diesen
        // Request einzureihen hiesse, den Client bis in sein eigenes Timeout
        // warten zu lassen und dabei Speicher fuer Arbeit zu halten, die nie
        // beginnt. Ehrlicher ist eine sofortige Absage.
        let slots = self.config.slots.len() as u64;
        if slots > 0 && self.held_credits() >= slots {
            self.metrics_rejected_quarantined = self.metrics_rejected_quarantined.saturating_add(1);
            let _ = reply.send(Err(Status::unavailable(format!(
                "alle {slots} Slotkredite stehen wegen eines Backendtimeouts in \
                 Quarantaene; es kann derzeit nichts gestartet werden"
            ))));
            return false;
        }

        // Ein zerlegbarer Auftrag bekommt seinen Zustand **hier**, bei der
        // ersten Ankunft. Wird er erst in der Fortsetzung angelegt, gibt es nie
        // eine erste Fortsetzung: `forward()` faende keinen Job und reichte den
        // vollstaendigen Request weiter — die Zerlegung waere dann eine
        // Konfigurationsoption ohne Wirkung.
        if let Some(cooperative) = self
            .config
            .contracts
            .get(descriptor.logical_model.get())
            .and_then(|c| c.cooperative)
            && let Some(mut job) =
                GenerativeJob::from_request(&request, cooperative.max_total_tokens)
        {
            if worth_decomposing(&cooperative, &job) {
                descriptor = decomposed_descriptor(descriptor, id, &job);
                self.jobs.insert(id, job);
                self.descriptors.insert(id, descriptor);
                self.continuation_of.insert(id, id);
            } else {
                report_refusal(
                    &cooperative,
                    &job,
                    self.config
                        .model_names
                        .get(descriptor.logical_model.get())
                        .map_or("?", String::as_str),
                );
                // Ungeteilt heisst **nicht** unbegrenzt. `max_total_tokens`
                // ist die Zusage des Betreibers, dass aus fremd kontrollierter
                // Eingabe keine unbeschraenkte Arbeit entsteht (Spec 8.3), und
                // durchgesetzt wird sie ausschliesslich beim Bau des Quantums.
                // Faellt der Job weg, faellt sonst auch die Grenze weg: ein
                // Client ohne eigenes `max_tokens` bekaeme freie Fahrt, und
                // der Kern haette die Profillaufzeit der Variante eingeplant.
                //
                // Deshalb wird hier **ein** Quantum ueber das volle zulaessige
                // Budget gebaut und genau das weitergereicht. Ein Lauf am
                // Stueck, aber innerhalb des Vertrags.
                *request = job.build_quantum(&request, job.max_total_tokens);
                self.generative.refused = self.generative.refused.saturating_add(1);
            }
        }

        self.waiting.insert(id, reply);
        self.inbox.insert(id, request);
        self.scheduler
            .on_event(now, Event::Arrival(descriptor), sink);
        true
    }

    /// Verarbeitet die Rueckmeldung eines Backendaufrufs.
    ///
    /// Gibt `false` zurueck, wenn die Bearbeitung hier endet — der Slotkredit
    /// bleibt dann bewusst gehalten.
    fn on_backend_done<S: FnMut(Action)>(
        &mut self,
        now: Instant,
        request: RequestId,
        slot: SlotIdx,
        result: Box<Result<ModelInferResponse, BackendError>>,
        sink: &mut S,
    ) -> bool {
        // Ein Fehler ist keine Fertigstellung. Wuerde er als
        // `Completion` gemeldet, zaehlte der Scheduler ihn als
        // gueltiges Ergebnis, `backend_failures` bliebe im echten
        // Gateway dauerhaft null — und der Margen-Regler bekaeme die
        // Fast-Null-Laufzeit eines Verbindungsfehlers als Beleg
        // dafuer, dass die Prognose zu konservativ war.
        // Eine Antwort nach dem Timeout ist keine Fertigstellung:
        // ihr Client ist laengst beantwortet, und ihre Laufzeit ist
        // die des Timeouts, nicht die des Modells. Als `Completion`
        // gezaehlt wuerde sie den Margen-Regler mit einer Zahl
        // fuettern, die nichts ueber die Prognose aussagt. Der Slot
        // wird hier aber sehr wohl frei — jetzt ist belegt, dass das
        // Backend fertig ist.
        self.outstanding = self.outstanding.saturating_sub(1);
        // Nur Transportfehler sagen etwas ueber die Erreichbarkeit.
        // Ein Modellfehler betrifft diesen Request, nicht das Backend.
        match result.as_ref() {
            Err(e) if e.is_transport_failure() => {
                self.consecutive_transport_failures =
                    self.consecutive_transport_failures.saturating_add(1);
                // Und der Befund gehoert zu **diesem** Endpunkt. Ein
                // beobachteter Transportfehler ist echte Evidenz und wirkt
                // sofort; die aktive Probe ist das, was ihn wieder aufhebt
                // (Review R11). Andersherum — Erholung an den Verkehr zu
                // haengen — war der Fehler.
                if let Some(lease) = self.leases.get(&request) {
                    let endpoint = lease.endpoint.clone();
                    self.reachability.insert(endpoint, false);
                }
            }
            _ => {
                self.consecutive_transport_failures = 0;
                // Eine Antwort — auch eine ablehnende — ist der staerkste
                // Erreichbarkeitsnachweis, den es gibt: staerker als jede
                // Probe. Ein Modellfehler heisst „das Backend hat geantwortet
                // und diesen Request abgelehnt", nicht „das Backend ist weg".
                if let Some(lease) = self.leases.get(&request) {
                    let endpoint = lease.endpoint.clone();
                    self.reachability.insert(endpoint, true);
                }
            }
        }

        // Ein **abgebrochener** Aufruf beweist nicht, dass die
        // Recheneinheit aufgehoert hat. Den Kredit hier
        // zurueckzugeben waere derselbe Fehler wie beim Timeout, nur
        // schwerer zu sehen: der Aufruf ist zurueckgekehrt, also
        // *sieht* alles beendet aus.
        //
        // Anders beim Verbindungsaufbau: kommt schon der Kanal nicht
        // zustande, hat der Request das Backend nie erreicht, und der
        // Kredit gehoert sofort zurueck.
        let unknown_execution = match result.as_ref() {
            Err(e) => e.execution_state() == vig_backend_triton::ExecutionState::Unknown,
            Ok(_) => false,
        };
        // Ein Aufruf, der schon am Kanalaufbau scheiterte, hat das Backend nie
        // erreicht. Er wird nie eine Fertigstellung erzeugen, und ihn in der
        // Auslieferungssumme zu fuehren machte das Abgleichsziel unerreichbar:
        // ein spaeter tatsaechlich abgeschlossener Auftrag bliebe dann
        // dauerhaft in Quarantaene (Review R01).
        if matches!(result.as_ref(),
            Err(e) if e.execution_state() == vig_backend_triton::ExecutionState::NotStarted)
            && let Some(lease) = self.leases.get(&request)
            && let Some(count) = self.dispatched_per_model.get_mut(&lease.backend_model)
        {
            *count = count.saturating_sub(1);
        }
        // Fencing: eine Meldung ohne passenden Anspruch gehoert zu einer
        // aelteren Generation. Sie darf keinen Kredit freigeben — sonst
        // erfindet eine doppelte Fertigstellung Kapazitaet.
        let Some(lease) = self.leases.get(&request).cloned() else {
            tracing::debug!(%request, "Meldung ohne offenen Anspruch; verworfen");
            return false;
        };
        let timed_out = lease.state == LeaseState::TimedOut;

        if unknown_execution {
            // Es kommt nie mehr eine Antwort. Der Nachweis muss vom Backend
            // geholt werden — eine Frist waere hier eine Behauptung, keine
            // Feststellung. Bis der Nachweis vorliegt, bleibt der Kredit.
            // Der Kredit wird in jedem Fall gehalten. Konnte der Abgleich
            // nicht anlaufen — die Basislinie fehlt noch —, merkt sich der
            // Anspruch das und der naechste Tick versucht es erneut.
            let started = self.start_reconciliation(&lease);
            if let Some(entry) = self.leases.get_mut(&request) {
                entry.state = if started {
                    LeaseState::Reconciling
                } else {
                    LeaseState::AwaitingBaseline
                };
            }
            tracing::warn!(
                %request,
                "Backendaufruf abgebrochen; Ausfuehrungsende unbekannt. Der \
                 Slotkredit bleibt gehalten, bis das Backend das Ende belegt."
            );
            if !timed_out {
                self.responses.insert(request, *result);
                self.finish(request, RequestState::ExecutionUnknown);
            }
            return false;
        }

        // Antwort oder belegtes Ende: der Anspruch endet, genau einmal.
        self.leases.remove(&request);
        // Und mit ihm die Nutzlastreservierung — jetzt ist belegt, dass das
        // Backend diese Bytes nicht mehr haelt.
        self.release_permit(request);
        let failed = timed_out || result.is_err();
        self.responses.insert(request, *result);
        let event = if failed {
            Event::BackendFailure { request, slot }
        } else {
            Event::Completion { request, slot }
        };
        self.scheduler.on_event(now, event, sink);
        true
    }

    fn handle(&mut self, msg: Msg) {
        let now = self.clock.now();
        let mut actions = Vec::new();
        let mut sink = |action: Action| actions.push(action);

        match msg {
            Msg::Arrival {
                descriptor,
                request,
                reply,
                permit,
            } => {
                if !self.accept(now, *descriptor, request, reply, permit, &mut sink) {
                    return;
                }
            }
            Msg::BackendDone {
                request,
                slot,
                result,
            } => {
                if !self.on_backend_done(now, request, slot, result, &mut sink) {
                    return;
                }
            }
            Msg::CompletionEvidence {
                model,
                completed,
                restarted,
            } => {
                self.on_completion_evidence(now, &model, completed, restarted, &mut sink);
            }
            Msg::BackendTimeout { request } => {
                // **Nur der Client wird freigegeben, nicht der Slot.** Der
                // Kredit gehoert zur Recheneinheit, und die ist womoeglich
                // noch belegt. Ihn hier zurueckzugeben hiesse, eine zweite
                // Ausfuehrung auf dieselbe GPU zu legen und anschliessend mit
                // einer Belegung zu planen, die es nicht gibt.
                //
                // Der Slot bleibt in Quarantaene, bis das Backend antwortet.
                // Tut es das nie, sagt die Bereitschaftspruefung es.
                if let Some(lease) = self.leases.get_mut(&request)
                    && lease.state == LeaseState::Running
                {
                    // Der Aufruf laeuft weiter; seine Rueckkehr ist der
                    // Nachweis. Nur der Client wird jetzt freigegeben.
                    lease.state = LeaseState::TimedOut;
                    self.backend_timeouts = self.backend_timeouts.saturating_add(1);
                }
                self.finish(request, RequestState::BackendTimeout);
                return;
            }
            Msg::Cancel { request } => {
                // Der Client nennt die Kennung, die er kennt. Laeuft dahinter
                // ein zerlegter Auftrag, wartet inzwischen dessen Fortsetzung
                // unter einer anderen.
                let target = self
                    .continuation_of
                    .get(&request)
                    .copied()
                    .unwrap_or(request);
                self.scheduler
                    .on_event(now, Event::Cancel { request: target }, &mut sink);
            }
            Msg::Shutdown(done) => {
                // Der Ausgang wird gemerkt, nicht sofort bedient: es kann noch
                // Arbeit offen sein, und ein Client ohne Antwort ist genau das,
                // was ein geordnetes Herunterfahren vermeiden soll.
                self.shutdown = Some(done);
            }
            Msg::Hint { hint } => {
                let model = hint.model;
                match self.scheduler.offer_hint(*hint, now) {
                    Ok(effect) => tracing::debug!(
                        model = model.get(),
                        ?effect,
                        "Anwendungshinweis angenommen"
                    ),
                    Err(rejection) => tracing::info!(
                        model = model.get(),
                        ?rejection,
                        "Anwendungshinweis abgelehnt"
                    ),
                }
            }
            Msg::Reachability {
                endpoint,
                reachable,
            } => {
                self.reachability.insert(endpoint, reachable);
            }
            Msg::ReconcileBaseline { model, completed } => {
                self.on_baseline(model, completed);
                // Und sofort die Anspruechen nachziehen, die auf sie gewartet
                // haben. Nicht erst beim naechsten Tick: in einem leeren
                // System gibt es keinen — der Actor schlaeft dann, bis wieder
                // Arbeit kommt, und ein gehaltener Kredit waere bis dahin
                // unversorgt.
                self.retry_pending_reconciliation();
            }
            Msg::HardwareState { state } => self.on_hardware_state(state),
            Msg::Tick => {
                self.scheduler.on_event(now, Event::Tick, &mut sink);
                self.report_contract_mismatch(now);
                self.retry_pending_reconciliation();
            }
            Msg::Snapshot(tx) => self.on_snapshot(tx),
        }

        for action in actions {
            self.apply(now, action);
        }
    }

    /// Meldet Stroeme, die dauerhaft schneller liefern als vereinbart.
    ///
    /// Der Dauerlauf hat gezeigt, dass der Governor in diesem Fall still
    /// degradiert: er verwirft mehr Frames, die Abdeckung faellt, und nichts
    /// sagt warum. Eine Last, die den Vertrag sprengt, ist ein Befund — der
    /// Governor kann den Vertrag einhalten oder die Last bedienen, nicht
    /// beides (siehe `docs/benchmark/soak.md`).
    fn report_contract_mismatch(&mut self, now: Instant) {
        for index in 0..self.config.model_names.len() {
            let Ok(raw) = u16::try_from(index) else {
                continue;
            };
            let model = vig_core::ModelIdx(raw);
            if self.scheduler.arrival_exceeds_contract(model) != Some(true) {
                continue;
            }
            let due = self
                .warned_arrival
                .get(index)
                .copied()
                .flatten()
                .is_none_or(|last| now.saturating_since(last) >= WARN_COOLDOWN);
            if !due {
                continue;
            }
            if let Some(slot) = self.warned_arrival.get_mut(index) {
                *slot = Some(now);
            }
            let metrics = self.scheduler.metrics();
            let observed = metrics.arrival_period_us.get(index).copied().unwrap_or(0);
            let contracted = metrics.contract_period_us.get(index).copied().unwrap_or(0);
            tracing::warn!(
                model = %self.config.model_names.get(index).map_or("?", String::as_str),
                observed_period_us = observed,
                contract_period_us = contracted,
                "Die Last liegt dauerhaft ueber der vereinbarten Periode. Der \
                 Governor haelt den Vertrag und verwirft den Ueberschuss; die \
                 Abdeckung faellt entsprechend. Entweder die Periode anpassen \
                 oder die Quelle drosseln."
            );
        }
    }

    fn apply(&mut self, now: Instant, action: Action) {
        match action {
            Action::Dispatch {
                request,
                model,
                variant,
                slot,
                quantum,
                ..
            } => {
                self.forward(request, model, variant, slot, quantum);
            }
            Action::Terminate { request, state } => self.finish(request, state),
            Action::WakeAt(at) => {
                self.next_wake = Some(match self.next_wake {
                    Some(existing) if existing <= at && existing > now => existing,
                    _ => at,
                });
            }
            // Die Rueckkopplung fuer den Online Runtime Estimator (WP11).
            // Noch verarbeitet sie niemand; sie wird bewusst emittiert, damit
            // die Schnittstelle steht, bevor der Schaetzer gebaut wird.
            Action::ObservedRuntime { .. } => {}
        }
    }

    /// Reicht einen Request an das Backend weiter.
    fn forward(
        &mut self,
        request: RequestId,
        model: vig_core::ModelIdx,
        variant: vig_core::VariantIdx,
        slot: SlotIdx,
        quantum: Option<u32>,
    ) {
        // Bei einem zerlegten Auftrag bleibt das urspruengliche Template
        // erhalten: `continue_job()` baut daraus das naechste Quantum. Wird es
        // hier entnommen, endet jeder Auftrag nach seinem ersten Quantum, ohne
        // dass es auffiele — der Client bekaeme einfach eine kurze Antwort.
        let template = if self.jobs.contains_key(&request) {
            self.inbox.get(&request).cloned()
        } else {
            self.inbox.remove(&request)
        };
        let Some(mut oip) = template else {
            tracing::warn!(%request, "Dispatch ohne zugehoerigen Request");
            return;
        };
        let Some(backend_model) = self.config.backend_model(model, variant.get()) else {
            tracing::error!(%request, "keine Backendvariante fuer die Auswahl");
            self.finish(request, RequestState::Failed);
            return;
        };

        // Der einzige Eingriff in die Nutzlast: das logische Modell wird durch
        // die gewaehlte physische Variante ersetzt (Spec 12.1). Alles andere
        // bleibt unangetastet, damit unbekannte OIP-Felder ueberleben
        // (Spec 6.1).
        backend_model.clone_into(&mut oip.model_name);

        // Bei einem zerlegten Auftrag wird nicht der urspruengliche Request
        // weitergereicht, sondern das naechste Quantum: Prompt plus bisher
        // Erzeugtes, begrenzt auf die vom Scheduler bestimmte Tokenzahl.
        if let (Some(tokens), Some(job)) = (quantum, self.jobs.get_mut(&request)) {
            let quantum_request = job.build_quantum(&oip, tokens);
            *oip = quantum_request;
            backend_model.clone_into(&mut oip.model_name);
        }

        // Eigenschaft des Modells, nicht der Zerlegung: ein decoupled Modell
        // braucht den Stream-Aufruf auch ungeteilt. Nach aussen bleibt der
        // Request unaer.
        let decoupled = self.config.is_decoupled(model);
        let Some(backend) = self.backend_for(model) else {
            tracing::error!(%request, "kein Backend fuer dieses Modell");
            self.finish(request, RequestState::Failed);
            return;
        };
        let tx = self.tx.clone();
        let timeout = std::time::Duration::from_nanos(self.config.inference_timeout.as_nanos());
        let model_name = oip.model_name.clone();
        self.outstanding = self.outstanding.saturating_add(1);

        // Der Anspruch auf den Slotkredit entsteht **hier**, mit dem Dispatch,
        // und endet erst mit einem Nachweis. Die Wanduhrzeit dient
        // ausschliesslich dem spaeteren Abgleich gegen Tritons `last_inference`.
        // Die laufende Summe der Auslieferungen an dieses Modell. Sie ist die
        // Groesse, gegen die der Abgleich prueft — und sie sinkt wieder, wenn
        // sich herausstellt, dass ein Aufruf das Backend nie erreicht hat.
        self.dispatched_per_model
            .entry(oip.model_name.clone())
            .and_modify(|n| *n = n.saturating_add(1))
            .or_insert(1);
        self.leases.insert(
            request,
            Lease {
                slot,
                backend_model: oip.model_name.clone(),
                endpoint: self.config.endpoint_of(model).to_owned(),
                state: LeaseState::Running,
            },
        );
        tokio::spawn(async move {
            let call = backend.execute(Ticket {
                model: model_name,
                decoupled,
                request: oip,
            });
            tokio::pin!(call);

            // Zwei Stufen, und das ist der Punkt: beim Timeout wird der Client
            // beantwortet, aber **weiter auf das Backend gewartet**. Nur die
            // echte Antwort belegt, dass die Recheneinheit wieder frei ist.
            // Ein Abbruch des Aufrufs wuerde das Gegenteil vortaeuschen.
            let result = if let Ok(finished) = tokio::time::timeout(timeout, &mut call).await {
                finished
            } else {
                let _ = tx.send(Msg::BackendTimeout { request }).await;
                call.await
            };
            let _ = tx
                .send(Msg::BackendDone {
                    request,
                    slot,
                    result: Box::new(result),
                })
                .await;
        });
    }

    /// Reiht das naechste Quantum eines Auftrags als neue Ankunft ein.
    ///
    /// Bewusst als **vollwertige Ankunft**: das Quantum muss die Zulassung
    /// erneut durchlaufen. Waere es privilegiert, koennte ein einmal
    /// gestarteter generativer Auftrag geschuetzte Arbeit dauerhaft
    /// verdraengen — genau das, was die Zerlegung verhindern soll.
    ///
    /// Die Generation Time bleibt die des urspruenglichen Auftrags. Ein
    /// Auftrag, der insgesamt zu lange braucht, altert damit korrekt und wird
    /// verworfen, statt unbegrenzt weiterzulaufen.
    fn continue_job(&mut self, request: RequestId, job: GenerativeJob) {
        let Some(descriptor) = self.descriptors.remove(&request) else {
            return;
        };
        let Some(reply) = self.waiting.remove(&request) else {
            return;
        };
        let Some(oip) = self.inbox.remove(&request) else {
            return;
        };
        self.responses.remove(&request);

        self.next_id = self.next_id.saturating_add(1);
        let continuation = RequestId(self.next_id);
        let next = decomposed_descriptor(descriptor, continuation, &job);

        // Die Abbildung wandert mit: Schluessel bleibt die Kennung, die der
        // Client kennt, Wert wird die neue.
        let origin = self
            .continuation_of
            .iter()
            .find(|(_, current)| **current == request)
            .map_or(request, |(origin, _)| *origin);
        self.continuation_of.insert(origin, continuation);

        if let Some(permit) = self.permits.remove(&request) {
            self.permits.insert(continuation, permit);
        }
        self.jobs.insert(continuation, job);
        self.descriptors.insert(continuation, next);
        self.waiting.insert(continuation, reply);
        self.inbox.insert(continuation, oip);

        let now = self.clock.now();
        let mut actions = Vec::new();
        self.scheduler
            .on_event(now, Event::Arrival(next), &mut |action: Action| {
                actions.push(action);
            });
        for action in actions {
            self.apply(now, action);
        }
    }

    /// Wie viele Slotkredite derzeit gehalten werden, weil ihr Ende unbelegt ist.
    fn held_credits(&self) -> u64 {
        self.leases
            .values()
            .filter(|l| l.state != LeaseState::Running)
            .count() as u64
    }

    /// Startet den Abgleich mit der Backendstatistik.
    ///
    /// Der einzige Weg, einen Anspruch ohne Antwort des Backends zu beenden.
    /// Die Aufgabe **liest** — sie greift nicht ein: einen fremden
    /// Serverprozess zurueckzusetzen, um die eigene Buchhaltung zu bereinigen,
    /// waere eine Befugnis, die dieser Governor nicht hat und nicht haben
    /// soll.
    ///
    /// Belegt ist das Ende auf zwei Wegen:
    ///
    /// * Das Modell meldet mindestens so viele abgeschlossene Inferenzen, wie
    ///   dieser Governor ihm ausgeliefert hat. Dann ist von unserer Arbeit
    ///   nichts mehr offen.
    /// * Der Zaehler ist **gefallen**. Ein Zaehler faellt nur, wenn das Modell
    ///   neu geladen oder der Server neu gestartet wurde — und dann ist alles,
    ///   was dort lief, ohnehin verloren.
    ///
    /// Beides setzt voraus, dass dieser Governor der einzige Aufrufer des
    /// Modells ist. Genau das ist der dokumentierte Aufbau, und `trust:
    /// strict` setzt es auf unserer Seite durch. Teilt sich ein fremder Client
    /// dasselbe Modell, zaehlt Triton dessen Arbeit mit, und der Nachweis wird
    /// zum Indiz. Das steht so in `docs/how-it-works.md`.
    fn start_reconciliation(&self, lease: &Lease) -> bool {
        let Some(backend) = self.backend_for_model_name(&lease.backend_model) else {
            tracing::warn!(
                model = %lease.backend_model,
                "kein Backendclient fuer den Abgleich; der Slotkredit bleibt gehalten"
            );
            return false;
        };
        // Basislinie plus eigene Auslieferungen — in der Zaehldomaene des
        // Backends. Ist die Basislinie noch nicht da (sie wird beim Start
        // geholt), wird der Abgleich nicht gestartet und beim naechsten Tick
        // erneut versucht. Ein Ziel ohne Basislinie waere im Zweifel zu klein
        // und gaebe einen Kredit frei, der gehalten gehoert.
        if !self.reconcile_baseline.contains_key(&lease.backend_model) {
            tracing::debug!(
                model = %lease.backend_model,
                "Abgleich wartet auf die Basislinie; der Slotkredit bleibt gehalten"
            );
            return false;
        }
        let tx = self.tx.clone();
        let model = lease.backend_model.clone();
        let interval = std::time::Duration::from_millis(RECONCILE_INTERVAL_MS);

        tokio::spawn(async move {
            let mut highest = 0_u64;
            loop {
                match backend.completion_evidence(&model).await {
                    Ok(evidence) => {
                        let restarted = evidence.completed < highest;
                        highest = highest.max(evidence.completed);
                        // Gemeldet, nicht entschieden: was der Zaehlerstand
                        // belegt, weiss nur der Actor.
                        if tx
                            .send(Msg::CompletionEvidence {
                                model: model.clone(),
                                completed: evidence.completed,
                                restarted,
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(error) => {
                        // Kein Nachweis heisst: der Kredit bleibt gehalten.
                        // Weiter fragen, bis der Actor endet.
                        tracing::debug!(%model, %error, "Abgleich noch ohne Nachweis");
                    }
                }
                tokio::time::sleep(interval).await;
            }
        });
        true
    }

    /// Wertet einen gemeldeten Zaehlerstand aus (NV-20, Review R01).
    ///
    /// Ein aggregierter Zaehler ist **kein** auftragsspezifischer
    /// Ausfuehrungsnachweis. Er sagt, wie viele Inferenzen dieses Modell
    /// insgesamt abgeschlossen hat, und nicht, **welche**. Frueher stand als
    /// Ziel „Basislinie plus die eigene Auslieferungsnummer" — und das ist
    /// falsch in beide Richtungen:
    ///
    /// * Laeuft Auftrag A noch und wird B danach fertig, steigt der Zaehler
    ///   auf das Ziel von A. A galt damit als beendet, waehrend seine
    ///   Recheneinheit womoeglich noch rechnete. Die Kapazitaetsrechnung war
    ///   ab da falsch.
    /// * Ein Auftrag, der das Backend **nie erreicht** hat, hob das Ziel
    ///   trotzdem an. Ein spaeter tatsaechlich abgeschlossener Auftrag konnte
    ///   dadurch dauerhaft in Quarantaene bleiben.
    ///
    /// Was ein aggregierter Zaehler tragen kann, ist eine **Ruhe-Aussage**:
    /// hat das Modell mindestens so viele Inferenzen abgeschlossen, wie
    /// diesem Governor je zugestellt wurden, ist von dessen Arbeit nichts
    /// mehr offen — und zwar von keiner einzelnen. Dann enden alle gehaltenen
    /// Anspruechen dieses Modells gemeinsam. Das ist die schwaechere Aussage,
    /// und sie ist die einzige, die stimmt.
    ///
    /// Ein zurueckgesprungener Zaehler traegt dieselbe Aussage aus einem
    /// anderen Grund: der Prozess, der die Ausfuehrung hielt, gibt es nicht
    /// mehr.
    ///
    /// Das Generation-Fencing von frueher ist damit hinfaellig und
    /// entfernt. Es schuetzte davor, dass eine verspaetete Meldung zu einem
    /// abgeloesten Anspruch einen Kredit freigibt. Der Nachweis wird jetzt
    /// **hier** gefuehrt, gegen den Live-Zustand: ein alter Zaehlerstand kann
    /// nur zu wenig sein, nie zu viel — die Auslieferungssumme waechst
    /// zwischenzeitlich, das Ziel wird also nur schwerer. Eine verspaetete
    /// Meldung ist damit im schlimmsten Fall wirkungslos.
    fn on_completion_evidence<S: FnMut(Action)>(
        &mut self,
        now: Instant,
        model: &str,
        completed: u64,
        restarted: bool,
        sink: &mut S,
    ) {
        let Some(baseline) = self.reconcile_baseline.get(model).copied() else {
            return;
        };
        // Gezaehlt wird, was das Backend **erreicht** hat. Ein Aufruf, der
        // schon am Kanalaufbau scheiterte, wird nie eine Fertigstellung
        // erzeugen; ihn im Ziel zu fuehren machte das Ziel unerreichbar.
        let dispatched = self.dispatched_per_model.get(model).copied().unwrap_or(0);
        let target = baseline.saturating_add(dispatched);
        if !restarted && completed < target {
            return;
        }

        let proven: Vec<RequestId> = self
            .leases
            .iter()
            .filter(|(_, lease)| lease.backend_model == model && lease.state != LeaseState::Running)
            .map(|(request, _)| *request)
            .collect();
        for request in proven {
            let Some(lease) = self.leases.remove(&request) else {
                continue;
            };
            self.reconciled = self.reconciled.saturating_add(1);
            self.release_permit(request);
            tracing::info!(
                %request,
                model,
                completed,
                target,
                restarted,
                "Backend belegt, dass von unserer Arbeit nichts mehr laeuft; \
                 Slotkredit wird zurueckgegeben"
            );
            self.scheduler.on_event(
                now,
                Event::BackendFailure {
                    request,
                    slot: lease.slot,
                },
                sink,
            );
        }
    }

    /// Beantwortet eine Metrikabfrage.
    ///
    /// Ausgelagert, weil der Abzug mit jedem Paket waechst und `handle` ein
    /// Verteiler bleiben soll.
    fn on_snapshot(&self, tx: oneshot::Sender<Metrics>) {
        // Die Margen liegen nicht im Zaehlerblock, sondern in den
        // Reglern. Sie gehoeren trotzdem in den Snapshot: ueber Stunden
        // gelesen zeigen sie, ob das System zur Ruhe kommt.
        let mut metrics = *self.scheduler.metrics();
        metrics.models = self.config.model_names.len();
        // Timeout und Quarantaene kennt nur das Gateway: der Kern hat
        // keine Uhr und keinen Backendaufruf.
        metrics.slots = self.config.slots.len() as u64;
        metrics.backend_timeouts = self.backend_timeouts;
        metrics.quarantined = self.held_credits();
        metrics.consecutive_transport_failures = self.consecutive_transport_failures;
        metrics.backends = self.backends.len() as u64;
        metrics.backends_reachable = self
            .reachability
            .iter()
            .filter(|(endpoint, ok)| **ok && self.backends.contains_key(*endpoint))
            .count() as u64;
        metrics.rejected_quarantined = self.metrics_rejected_quarantined;
        metrics.generative_prefill_us = self.generative.prefill_work.as_micros();
        metrics.generative_decode_us = self.generative.decode_work.as_micros();
        metrics.generative_fixed_us = self.generative.fixed_work.as_micros();
        metrics.generative_context_tokens = self.generative.longest_context;
        metrics.decomposition_refused = self.generative.refused;
        metrics.reconciled = self.reconciled;
        metrics.reconcile_baseline_missing = self.baselines_missing();
        metrics.outstanding_backend_calls = self.outstanding;
        for (index, slot) in metrics.margin_percent.iter_mut().enumerate() {
            if let Ok(model) = u16::try_from(index) {
                *slot = self
                    .scheduler
                    .margin_of(vig_core::ModelIdx(model))
                    .as_percent();
            }
        }
        let _ = tx.send(metrics);
    }

    /// Uebernimmt die Abgleichs-Basislinie eines Backendmodells (NV-20).
    ///
    /// Angenommen wird sie nur, **solange dem Modell noch nichts ausgeliefert
    /// wurde**. Danach koennte der Zaehler eigene, schon abgeschlossene
    /// Inferenzen enthalten; die Basislinie waere dann zu hoch, das Ziel
    /// unerreichbar und der Kredit dauerhaft gehalten. Eine zu hohe Basislinie
    /// ist nicht die sichere Seite, sondern eine andere Art, kaputt zu sein.
    fn on_baseline(&mut self, model: String, completed: u64) {
        if self.reconcile_baseline.contains_key(&model) {
            // Eine zweite Meldung ist eine spaetere Momentaufnahme und als
            // Basislinie falsch.
            return;
        }
        if self
            .dispatched_per_model
            .get(&model)
            .is_some_and(|n| *n > 0)
        {
            tracing::warn!(
                %model,
                "Basislinie kaeme nach der ersten Auslieferung und waere \
                 womoeglich zu hoch; fuer dieses Modell gibt es keinen \
                 zaehlerbasierten Endnachweis. Governor bei erreichbarem \
                 Backend neu starten."
            );
            return;
        }
        self.reconcile_baseline.insert(model, completed);
        // Und sofort die Anspruechen nachziehen, die auf sie gewartet haben.
        // Nicht erst beim naechsten Tick: in einem leeren System gibt es
        // keinen — der Actor schlaeft dann, bis wieder Arbeit kommt, und ein
        // gehaltener Kredit waere bis dahin unversorgt.
        self.retry_pending_reconciliation();
    }

    /// Wie viele Backendmodelle keine Abgleichs-Basislinie haben.
    fn baselines_missing(&self) -> u64 {
        let total: usize = self.config.backend_models.iter().map(Vec::len).sum();
        u64::try_from(total.saturating_sub(self.reconcile_baseline.len())).unwrap_or(u64::MAX)
    }

    /// Versucht ausstehende Abgleiche erneut zu starten.
    ///
    /// Ein Abgleich kommt nicht zustande, solange die Basislinie fehlt. Sie
    /// wird beim Start geholt und ist gewoehnlich binnen Millisekunden da —
    /// ein Aufruf, der genau davor abbricht, soll deshalb nicht dauerhaft
    /// unversorgt bleiben.
    fn retry_pending_reconciliation(&mut self) {
        let waiting: Vec<(RequestId, Lease)> = self
            .leases
            .iter()
            .filter(|(_, lease)| lease.state == LeaseState::AwaitingBaseline)
            .map(|(id, lease)| (*id, lease.clone()))
            .collect();
        for (request, lease) in waiting {
            if self.start_reconciliation(&lease)
                && let Some(entry) = self.leases.get_mut(&request)
            {
                entry.state = LeaseState::Reconciling;
            }
        }
    }

    /// Uebernimmt den beobachteten Hardwarezustand (NV-04, NV-06).
    ///
    /// Die Profilrevision bleibt, was sie ist: sie wechselt nur beim Neuladen
    /// der Konfiguration, nicht mit dem Taktzustand. Beides zusammen bildet
    /// die Epoche, unter der gelernte Zellen gelten.
    fn on_hardware_state(&mut self, state: StateClass) {
        self.scheduler
            .observe_hardware(state, self.profile_revision);
    }

    /// Der Client fuer ein Backendmodell, sofern eindeutig bestimmbar.
    fn backend_for_model_name(&self, backend_model: &str) -> Option<Arc<dyn Executor>> {
        let index = self
            .config
            .backend_models
            .iter()
            .position(|variants| variants.iter().any(|v| v == backend_model))?;
        let model = vig_core::ModelIdx(u16::try_from(index).ok()?);
        self.backends
            .get(self.config.endpoint_of(model))
            .map(Arc::clone)
    }

    /// Vergisst allen Zustand, den ein Request hinterlassen haben kann.
    ///
    /// An einer Stelle gebuendelt, weil jeder vergessene Eintrag hier ein Leck
    /// waere, das erst nach Stunden auffaellt — und dann als „langsam
    /// wachsender Speicherverbrauch" und nicht als Fehler.
    fn forget_job(&mut self, request: RequestId) {
        self.inbox.remove(&request);
        self.descriptors.remove(&request);
        self.jobs.remove(&request);
        self.continuation_of
            .retain(|origin, current| *origin != request && *current != request);
        self.release_permit(request);
    }

    /// Gibt die Nutzlastreservierung frei — aber nur, wenn nichts mehr
    /// rechnen kann (Review R04).
    ///
    /// Ein gehaltener Slotkredit heisst: das Backend koennte diesen Request
    /// noch bearbeiten und haelt seine Nutzlast. Das Budget dann freizugeben
    /// waere dieselbe erfundene Kapazitaet wie ein zu frueh
    /// zurueckgegebener Slotkredit, nur in einer anderen Waehrung.
    fn release_permit(&mut self, request: RequestId) {
        if self.leases.contains_key(&request) {
            return;
        }
        self.permits.remove(&request);
    }

    /// Der Client fuer das Backend eines Modells.
    fn backend_for(&self, model: vig_core::ModelIdx) -> Option<Arc<dyn Executor>> {
        self.backends
            .get(self.config.endpoint_of(model))
            .map(Arc::clone)
    }

    /// Beantwortet einen wartenden Client — oder setzt einen zerlegten
    /// Auftrag fort.
    fn finish(&mut self, request: RequestId, state: RequestState) {
        // Ein Quantum, das erfolgreich war und den Auftrag noch nicht beendet
        // hat, ist keine Antwort an den Client, sondern der Anlass fuer das
        // naechste Quantum.
        if state == RequestState::CompletedValid
            && let Some(mut job) = self.jobs.remove(&request)
            && let Some(Ok(response)) = self.responses.get(&request)
        {
            // Fortgesetzt wird nur, wenn ueberhaupt noch jemand zuhoert.
            // `is_closed()` fragt den Antwortkanal selbst — das ist der
            // verlaessliche Punkt: ein Abbruch waehrend eines laufenden
            // Quantums findet den Auftrag in keiner Queue, weil er gerade im
            // Backend ist. Ohne diese Pruefung liefe er bis zum Ende seines
            // Tokenbudgets weiter, und genau ein generativer Auftrag ist die
            // teuerste Arbeit im System.
            let listening = self
                .waiting
                .get(&request)
                .is_some_and(|reply| !reply.is_closed());
            let finished = job.absorb(response);
            // Gebucht wird hier und nicht in `continue_job`: dort laeuft das
            // **letzte** Quantum nie durch, und seine Dekodierarbeit fiele aus
            // der Statistik. Bei n Quanten waeren n-1 gezaehlt, und das
            // Verhaeltnis Prefill zu Dekodierung — die Groesse, an der sich
            // entscheidet, ob die Zerlegung noch traegt — waere systematisch
            // zugunsten des Prefills verzerrt.
            if let Some(cooperative) = self
                .config
                .contracts
                .get(descriptor_model(self.descriptors.get(&request)))
                .and_then(|c| c.cooperative)
            {
                self.generative.record(&cooperative, &job);
            }
            if !finished && listening {
                self.continue_job(request, job);
                return;
            }
            // Fertig: die gesammelte Antwort an den Client.
            if let Some(Ok(response)) = self.responses.get_mut(&request) {
                *response = job.build_response(response);
            }
        }

        let Some(reply) = self.waiting.remove(&request) else {
            // Auch ohne wartenden Client darf kein Auftragszustand
            // zurueckbleiben: sonst waechst der Actor mit jedem Abbruch.
            self.forget_job(request);
            self.responses.remove(&request);
            return;
        };
        self.forget_job(request);

        let payload = self.responses.remove(&request);
        // Zustaende, in denen wir den Slotkredit halten, tragen mehr
        // Information als der rohe Backendfehler: sie sagen, was wir **nicht**
        // wissen. Deshalb gewinnt hier die Abbildung des Zustands.
        if matches!(
            state,
            RequestState::ExecutionUnknown | RequestState::BackendTimeout
        ) && let Some(status) = status_for(state)
        {
            let _ = reply.send(Err(status));
            return;
        }

        let outcome = match (state, payload) {
            (RequestState::CompletedValid, Some(Ok(response))) => Ok(response),
            (RequestState::CompletedObsolete, Some(Ok(mut response))) => {
                mark_obsolete(&mut response);
                Ok(response)
            }
            (_, Some(Err(error))) => Err(Status::unavailable(error.to_string())),
            (state, _) => Err(status_for(state)
                .unwrap_or_else(|| Status::internal("unerwarteter Requestzustand"))),
        };
        let _ = reply.send(outcome);
    }
}

/// Holt einmal beim Start den Statistikzaehler je Backendmodell.
///
/// Die Basislinie des Abgleichs. Tritons Zaehler laeuft ueber die Lebensdauer
/// des Triton-Prozesses, und der ueberlebt den Governor gewoehnlich: nach
/// einem Governor-Neustart steht er schon bei tausenden Abschluessen. Ohne
/// Basislinie waere „das Backend meldet mindestens so viele Abschluesse wie
/// wir ausgeliefert haben" beim ersten Request sofort wahr — und der Abgleich
/// gaebe einen Slotkredit frei, waehrend die Recheneinheit noch rechnet.
///
/// Einmal beim Start und nicht beim Abgleich: eine Basislinie, die erst der
/// Abgleich abfragt, kaeme zu spaet.
fn spawn_baseline_probe(
    tx: &mpsc::Sender<Msg>,
    config: &Arc<Resolved>,
    backends: &HashMap<String, Arc<dyn Executor>>,
) {
    // Je Backendmodell den Client seines Endpunkts.
    let mut targets: Vec<(String, Arc<dyn Executor>)> = Vec::new();
    for (index, variants) in config.backend_models.iter().enumerate() {
        let Ok(model_index) = u16::try_from(index) else {
            continue;
        };
        let endpoint = config.endpoint_of(vig_core::ModelIdx(model_index));
        let Some(backend) = backends.get(endpoint) else {
            continue;
        };
        for name in variants {
            targets.push((name.clone(), Arc::clone(backend)));
        }
    }

    let tx = tx.clone();
    let interval = std::time::Duration::from_millis(RECONCILE_INTERVAL_MS);
    tokio::spawn(async move {
        // Je Modell wiederholen, bis eine Antwort kommt. Ein Backend, das beim
        // Start gerade hochfaehrt, soll nicht dauerhaft ohne Basislinie
        // bleiben — nach der ersten Auslieferung wird sie aber nicht mehr
        // angenommen, und das ist der Grund fuer die Eile.
        let mut open = targets;
        while !open.is_empty() {
            let mut still_open = Vec::new();
            for (model, backend) in open {
                match backend.completion_evidence(&model).await {
                    Ok(evidence) => {
                        tracing::debug!(
                            %model,
                            completed = evidence.completed,
                            "Abgleichs-Basislinie geholt"
                        );
                        if tx
                            .send(Msg::ReconcileBaseline {
                                model,
                                completed: evidence.completed,
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            %model, %error,
                            "noch keine Abgleichs-Basislinie vom Backend; \
                             bis dahin haelt ein abgebrochener Aufruf seinen \
                             Slotkredit"
                        );
                        still_open.push((model, backend));
                    }
                }
            }
            if still_open.is_empty() {
                return;
            }
            open = still_open;
            tokio::time::sleep(interval).await;
        }
    });
}

/// Wie oft der Hardwarezustand gelesen wird, in Millisekunden.
///
/// Zwei Sekunden: ein Taktwechsel oder ein thermisches Limit soll erkannt
/// werden, bevor sich die Laufzeiten ueber viele Auftraege hinweg
/// verschieben. Haeufiger zu lesen kostet einen Prozessstart je Messung fuer
/// eine Groesse, die sich in Sekunden und nicht in Millisekunden aendert.
const HARDWARE_PROBE_INTERVAL_MS: u64 = 2_000;

/// Liest den Hardwarezustand im Hintergrund und meldet ihn dem Actor.
///
/// Bleibt die Meldung aus — kein `nvidia-smi`, keine Rechte, kein Geraet —
/// bleibt der Zustand im Kern „unbekannt". Das ist der sichere Fall: die
/// zustandsabhaengige Prognose gibt dann gar keine Aussage statt der
/// guenstigsten.
fn spawn_hardware_probe(tx: &mpsc::Sender<Msg>) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let mut collector = vig_platform::NvidiaSmi::default();
        let mut health = vig_platform::CollectorHealth::default();
        let interval = std::time::Duration::from_millis(HARDWARE_PROBE_INTERVAL_MS);
        loop {
            // Ein Unterprozess gehoert nicht auf den Actor-Thread.
            let probe = tokio::task::spawn_blocking({
                let mut collector = collector.clone();
                move || {
                    use vig_platform::Collector as _;
                    collector.snapshot()
                }
            })
            .await;

            let state = match probe {
                Ok(Ok(snapshot)) => {
                    health.record_success(snapshot.taken_at_ms);
                    snapshot.gpu(0).map_or_else(StateClass::default, |gpu| {
                        StateClass {
                            // Der Belegungsgrad kommt vom Kern, nicht von hier.
                            occupancy: 0,
                            throttle: if gpu.limiting_reasons().is_empty() {
                                ThrottleClass::Nominal
                            } else {
                                ThrottleClass::Limited
                            },
                            clock: ClockClass::from_mhz(
                                gpu.clock_sm_mhz.value().copied(),
                                gpu.clock_sm_max_mhz.value().copied(),
                            ),
                        }
                    })
                }
                Ok(Err(reason)) => {
                    health.record_failure(&reason);
                    // Erst nach der Schwelle wird der Zustand auf unbekannt
                    // gesetzt: ein einzelner Timeout unter Last darf die
                    // Betriebsart nicht umschalten.
                    if health.fallback() == vig_platform::Fallback::StateAware {
                        tokio::time::sleep(interval).await;
                        continue;
                    }
                    tracing::warn!(
                        %reason,
                        failures = health.consecutive_failures(),
                        "Hardwarezustand nicht lesbar; es wird ohne Geraetezustand geplant"
                    );
                    StateClass::default()
                }
                Err(_) => return,
            };

            if tx.send(Msg::HardwareState { state }).await.is_err() {
                return;
            }
            let _ = &mut collector;
            tokio::time::sleep(interval).await;
        }
    });
}

/// Die Fortschrittsbuchhaltung eines Governors ueber alle zerlegten Auftraege
/// (NV-16).
///
/// [`vig_core::generative::Progress`] fuehrt sie je Auftrag; hier werden die
/// Summen gehalten, die in die Metrik gehen. Der Unterschied zwischen Prefill
/// und Dekodierung ist der Punkt: nur das eine ist Fortschritt.
#[derive(Debug, Default)]
struct GenerativeAccounting {
    /// Summe der Prefill-Arbeit ueber alle Fortsetzungen.
    prefill_work: vig_core::Duration,
    /// Summe der Dekodierarbeit ueber alle Fortsetzungen.
    decode_work: vig_core::Duration,
    /// Summe der festen Kosten je Quantum: Round-Trip und Scheduling.
    ///
    /// Auf der Messmaschine der **groesste** Einzelterm der Zerlegung. Ihn
    /// wegzulassen liesse ausgerechnet den dominierenden Kostenanteil aus dem
    /// exportierten Verhaeltnis heraus.
    fixed_work: vig_core::Duration,
    /// Der laengste Kontext, den je eine Fortsetzung getragen hat.
    longest_context: u32,
    /// Auftraege, die ungeteilt liefen, weil die Zerlegung zu teuer war.
    refused: u64,
}

impl GenerativeAccounting {
    /// Bucht das Quantum, das dieser Auftrag gerade abgeschlossen hat.
    ///
    /// Gebucht wird der **Zuwachs** an Token, nicht der Stand: `job.tokens`
    /// zaehlt kumulativ und erschiene sonst bei jeder Fortsetzung erneut
    /// vollstaendig als Fortschritt.
    ///
    /// Der Prefill zaehlt erst ab dem **zweiten** Quantum. Der erste faellt
    /// auch beim ungeteilten Lauf an und ist kein Preis der Zerlegung; was sie
    /// kostet, sind die Wiederholungen — und nur die stehen in
    /// `generative_prefill_us`.
    fn record(&mut self, cooperative: &Cooperative, job: &GenerativeJob) {
        let cost = cooperative.cost_model();
        // Der Kontext, mit dem **dieses** Quantum ins Backend gegangen ist:
        // der Stand davor, nicht der danach.
        let context_before = job.context_tokens().saturating_sub(job.last_quantum_tokens);
        let mut progress = Progress::new(context_before);
        progress.record(job.last_quantum_tokens, cost);

        if job.quanta > 1 {
            self.prefill_work = add(self.prefill_work, progress.prefill_work);
        }
        self.decode_work = add(self.decode_work, progress.decode_work);
        self.fixed_work = add(self.fixed_work, progress.fixed_work);
        self.longest_context = self.longest_context.max(job.context_tokens());
    }
}

/// Summiert zwei Dauern saettigend.
fn add(a: vig_core::Duration, b: vig_core::Duration) -> vig_core::Duration {
    vig_core::Duration::from_nanos_unbounded(a.as_nanos().saturating_add(b.as_nanos()))
}

/// Der Modellindex eines Deskriptors, oder ein Index ausserhalb jedes
/// Vertrags, wenn er fehlt.
///
/// `usize::MAX` trifft garantiert kein Modell; `contracts.get` liefert dann
/// `None`, und es wird nichts gebucht. Das ist die richtige Antwort: ohne
/// Deskriptor ist auch nicht bekannt, nach welchem Kostenmodell zu buchen
/// waere.
fn descriptor_model(descriptor: Option<&RequestDescriptor>) -> usize {
    descriptor.map_or(usize::MAX, |d| d.logical_model.get())
}

/// Ob dieser Auftrag zerlegt werden soll — oder ungeteilt laufen (NV-16).
///
/// Die Zerlegung ist nicht kostenlos: jedes Quantum traegt den gewachsenen
/// Prompt erneut ins Backend, und ohne wirksames Prefix-Caching wird er jedes
/// Mal neu berechnet. Bei genug Quanten kostet die Zerlegung mehr Arbeit, als
/// sie an Blockadezeit spart. ADR-0014 nennt den ungeteilten Lauf als
/// Rueckfall; hier wird er genommen.
///
/// Ohne `max_overhead_permille` im Vertrag bleibt es beim bisherigen
/// Verhalten: es wird zerlegt, was zerlegbar ist. Das ist Absicht — eine
/// Grenze, die niemand gesetzt hat, darf keine bestehende Konfiguration
/// stillschweigend umstellen. Der `doctor` nennt den Preis in jedem Fall.
///
/// Gerechnet wird mit Quanten in Mindestgroesse, also dem unguenstigsten
/// Fall. Die tatsaechliche Groesse haengt an der Luecke zur naechsten
/// geschuetzten Ankunft und steht hier noch nicht fest; sie kann nur groesser
/// ausfallen, und dann ist der Aufschlag kleiner als gerechnet.
fn worth_decomposing(cooperative: &Cooperative, job: &GenerativeJob) -> bool {
    let Some(limit) = cooperative.max_overhead_permille else {
        return true;
    };
    let plan = Plan::project(
        cooperative.cost_model(),
        job.prompt_tokens,
        job.max_total_tokens,
        cooperative.min_tokens,
    );
    match plan.verdict(limit) {
        // `Unmeasured` kann aus einem **aufgeloesten** Vertrag nicht kommen:
        // `is_measured` haengt an der Dekodierrate, und eine Rate von null
        // lehnt `ModelContract::validate` ab. Der Zweig steht hier trotzdem,
        // weil `ContextCost` ein oeffentlicher Typ ist und ein Plan auch
        // ausserhalb dieses Pfades gebaut werden kann. Er ist eine
        // Vollstaendigkeit, kein Schutz — und soll auch nicht als einer
        // gelesen werden.
        Verdict::Decompose | Verdict::Unmeasured => true,
        Verdict::TooExpensive { .. } => false,
    }
}

/// Nennt den Preis, wenn eine Zerlegung abgelehnt wird.
///
/// `Verdict::TooExpensive` traegt die Zahl, um die es geht. Sie nur zu zaehlen
/// und wegzuwerfen hiesse, dem Betreiber zu sagen „ich habe etwas
/// abgelehnt" — und ihm zu verschweigen, warum. Der Zaehler
/// `decomposition_refused` sagt, **wie oft**; diese Zeile sagt, **wie teuer**.
fn report_refusal(cooperative: &Cooperative, job: &GenerativeJob, model: &str) {
    let plan = Plan::project(
        cooperative.cost_model(),
        job.prompt_tokens,
        job.max_total_tokens,
        cooperative.min_tokens,
    );
    if let Verdict::TooExpensive {
        overhead_permille,
        limit_permille,
    } = plan.verdict(cooperative.max_overhead_permille.unwrap_or(u32::MAX))
    {
        tracing::info!(
            model,
            quanta = plan.quanta,
            overhead_permille,
            limit_permille,
            "Zerlegung abgelehnt: sie kostet mehr Arbeit, als der Vertrag zulaesst"
        );
    }
}

/// Der Deskriptor eines zerlegten Auftrags: derselbe Auftrag, sein aktueller
/// Kontext (NV-16).
///
/// Beide Pfade laufen hier durch — die erste Annahme in [`Actor::accept`] und
/// jede Fortsetzung in [`Actor::continue_job`]. Bewusst dieselbe Funktion und
/// keine zwei Zuweisungen in zwei langen Methoden: der Kontext ist die
/// einzige Groesse, die sich zwischen zwei Quanten aendert, und sie
/// entscheidet ueber die Zuschneidung des naechsten. Faellt sie an einer der
/// beiden Stellen weg, schneidet der Kern dort ein Quantum zu gross zu, und
/// es zieht ueber seine Luecke hinaus — ohne dass irgendwo ein Fehler sichtbar
/// wuerde.
///
/// Schon das **erste** Quantum traegt Kontext: den Prompt. Er ist in absoluten
/// Zahlen der groesste einzelne Prefill des ganzen Auftrags, und ihn dort auf
/// null zu lassen hiesse, ausgerechnet den teuersten Schritt gratis zu planen.
///
/// `decomposable` sagt dem Kern, dass dieser Auftrag wirklich in Quanten
/// laeuft. Ein Vertrag mit `cooperative` sagt nur, dass das **Modell**
/// zerlegbar ist.
///
/// Die Generation Time bleibt die des urspruenglichen Auftrags. Ein Auftrag,
/// der insgesamt zu lange braucht, altert damit korrekt und wird verworfen,
/// statt unbegrenzt weiterzulaufen.
fn decomposed_descriptor(
    previous: RequestDescriptor,
    id: RequestId,
    job: &GenerativeJob,
) -> RequestDescriptor {
    RequestDescriptor {
        id,
        context_tokens: job.context_tokens(),
        decomposable: true,
        ..previous
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{CancelOnDrop, GenerativeJob, Msg, RequestDescriptor, decomposed_descriptor};
    use tokio::sync::mpsc;
    use vig_core::RequestId;

    /// Ein fallengelassener Client erzeugt genau ein Cancel-Ereignis.
    #[tokio::test]
    async fn an_armed_guard_reports_the_cancellation() {
        let (tx, mut rx) = mpsc::channel(4);
        drop(CancelOnDrop {
            tx,
            request: Some(RequestId(7)),
        });
        match rx.recv().await {
            Some(Msg::Cancel { request }) => assert_eq!(request, RequestId(7)),
            other => panic!("erwartet wurde ein Cancel, nicht {other:?}"),
        }
    }

    /// Ein regulaer beantworteter Request erzeugt keines — und der Waechter
    /// gibt seinen Sender wieder frei.
    ///
    /// Die zweite Zusage ist die wichtigere: waere der Waechter mit
    /// `core::mem::forget` entschaerft, bliebe der Sender fuer immer am Kanal
    /// haengen. `rx.recv()` gaebe dann auch nach dem Fallenlassen aller Sender
    /// nicht `None` zurueck, und der Actor koennte nicht enden.
    #[tokio::test]
    async fn a_disarmed_guard_reports_nothing_and_releases_its_sender() {
        let (tx, mut rx) = mpsc::channel::<Msg>(4);
        let mut guard = CancelOnDrop {
            tx,
            request: Some(RequestId(7)),
        };
        guard.disarm();
        drop(guard);
        assert!(
            rx.recv().await.is_none(),
            "kein Ereignis, und der Kanal ist geschlossen"
        );
    }

    /// Jeder zerlegte Auftrag traegt seinen Kontext, vom ersten Quantum an.
    ///
    /// Der Kontext ist die einzige Groesse, die sich zwischen zwei Quanten
    /// aendert. Waere er nicht dabei, schnitte der Kern jedes Quantum gleich
    /// gross zu — der Fehler, den NV-16 behebt. Und schon das erste traegt
    /// welchen: den Prompt. Er ist in absoluten Zahlen der groesste einzelne
    /// Prefill des ganzen Auftrags.
    #[test]
    fn every_quantum_carries_its_context_including_the_first() {
        let mut job = job_with_prompt("Beschreibe das Bild: ");
        // Die erste Annahme: 21 Zeichen, rund 6 Token Prompt.
        let first = decomposed_descriptor(descriptor(RequestId(1)), RequestId(1), &job);
        assert_eq!(first.context_tokens, 6, "der Prompt ist Kontext");
        assert!(first.decomposable, "und dieser Auftrag wird zerlegt");

        // Nach dem ersten Quantum: Prompt plus das bisher Erzeugte.
        job.tokens = 16;
        let second = decomposed_descriptor(first, RequestId(2), &job);
        assert_eq!(second.id, RequestId(2));
        assert_eq!(second.context_tokens, job.context_tokens());
        assert!(
            second.context_tokens > first.context_tokens,
            "der Kontext waechst mit jedem Quantum"
        );

        // Und weiter: monoton, nie zurueck.
        job.tokens = 32;
        let third = decomposed_descriptor(second, RequestId(3), &job);
        assert!(third.context_tokens > second.context_tokens);
        assert!(third.decomposable);
    }

    /// Alles ausser Kennung und Kontext bleibt unveraendert.
    ///
    /// Insbesondere die Generation Time: an ihr altert der Auftrag. Setzte die
    /// Fortsetzung sie neu, liefe ein generativer Auftrag unbegrenzt weiter,
    /// statt irgendwann als veraltet zu enden.
    #[test]
    fn a_continuation_changes_nothing_but_its_id_and_context() {
        let job = job_with_prompt("x");
        let first = descriptor(RequestId(1));
        let next = decomposed_descriptor(first, RequestId(2), &job);

        assert_eq!(next.generation_time, first.generation_time);
        assert_eq!(next.arrival_time, first.arrival_time);
        assert_eq!(next.absolute_deadline, first.absolute_deadline);
        assert_eq!(next.criticality, first.criticality);
        assert_eq!(next.logical_model, first.logical_model);
        assert_eq!(next.supersession_key, first.supersession_key);
        assert_eq!(next.payload, first.payload);
    }

    fn job_with_prompt(prompt: &str) -> GenerativeJob {
        GenerativeJob {
            prompt: prompt.to_owned(),
            generated: String::new(),
            tokens: 0,
            prompt_tokens: u32::try_from(prompt.len().div_ceil(4)).unwrap_or(u32::MAX),
            max_total_tokens: 64,
            declared_sampling: None,
            extra_inputs: Vec::new(),
            quanta: 0,
            last_requested_tokens: None,
            last_quantum_tokens: 0,
        }
    }

    fn descriptor(id: RequestId) -> RequestDescriptor {
        RequestDescriptor {
            id,
            logical_model: vig_core::ModelIdx(0),
            supersession_key: vig_core::SupersessionKey(0),
            generation_time: vig_core::Instant::ZERO,
            arrival_time: vig_core::Instant::ZERO,
            absolute_deadline: None,
            max_age: None,
            criticality: vig_core::Criticality::BestEffort,
            queue_policy: vig_core::QueuePolicy::Fifo,
            stateful: false,
            variant: None,
            payload: vig_core::PayloadRef::default(),
            context_tokens: 0,
            decomposable: false,
        }
    }
}
