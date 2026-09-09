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
use crate::outcome::{mark_obsolete, status_for};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tonic::Status;
use vig_backend_triton::{BackendError, TritonClient};
use vig_config::schema::Resolved;
use vig_core::overload::{OverloadConfig, OverloadController};
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

/// Das Ergebnis, das ein wartender Client bekommt.
pub type Reply = Result<ModelInferResponse, Status>;

/// Eine Nachricht an den Actor.
#[derive(Debug)]
pub enum Msg {
    /// Ein neuer Request ist eingetroffen.
    Arrival {
        /// Die Scheduling-Metadaten.
        descriptor: Box<RequestDescriptor>,
        /// Der unveraenderte OIP-Request zur Weitergabe.
        request: Box<ModelInferRequest>,
        /// Wohin die Antwort geht.
        reply: oneshot::Sender<Reply>,
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
    /// Der Abgleich hat belegt, dass eine Ausfuehrung beendet ist.
    ///
    /// Traegt die Generation mit: eine Meldung zu einem laengst abgeloesten
    /// Anspruch darf keinen Kredit freigeben.
    ExecutionProven {
        /// Der betroffene Request.
        request: RequestId,
        /// Die Generation, fuer die der Nachweis gilt.
        generation: u64,
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
    /// Fortlaufende Kennung dieses Anspruchs.
    ///
    /// Fencing: eine verspaetete Meldung zu einer aelteren Generation darf
    /// weder diesen noch einen spaeteren Kredit freigeben. Ohne sie koennte
    /// eine doppelte Fertigstellungsmeldung Kapazitaet erfinden.
    generation: u64,
    /// Der Slot, dessen Kredit gehalten wird.
    slot: SlotIdx,
    /// Das Backendmodell, gegen dessen Statistik abgeglichen wird.
    backend_model: String,
    /// Wie viele Inferenzen dieser Governor diesem Modell bis hier
    /// ausgeliefert hat, diese eingeschlossen.
    ///
    /// Meldet das Backend mindestens so viele abgeschlossene Inferenzen, ist
    /// von unserer Arbeit nichts mehr offen. Das ist der Nachweis, und er
    /// braucht keine vorher abgefragte Basislinie — die waere ein Rennen
    /// gegen ein Backend, das inzwischen fertig geworden sein kann.
    dispatched_total: u64,
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
    pub async fn submit(&self, descriptor: RequestDescriptor, request: ModelInferRequest) -> Reply {
        let id = descriptor.id;
        let (reply, wait) = oneshot::channel();
        let msg = Msg::Arrival {
            descriptor: Box::new(descriptor),
            request: Box::new(request),
            reply,
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
    backends: HashMap<String, Arc<TritonClient>>,
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
    next_generation: u64,
    /// Wie oft ein Backendaufruf das Timeout ueberschritten hat.
    backend_timeouts: u64,
    /// Transportfehler seit dem letzten erfolgreichen Backendaufruf.
    consecutive_transport_failures: u64,
    /// Wie oft ein Ausfuehrungsende durch Abgleich belegt wurde.
    reconciled: u64,
    /// Wie viele Inferenzen dieser Governor je Backendmodell ausgeliefert hat.
    ///
    /// Die Bezugsgroesse des Abgleichs. Sie ist der Grund, warum kein
    /// Basiswert vom Backend geholt werden muss: was wir gestartet haben,
    /// wissen wir selbst — und eine Basislinie, die erst der Abgleich abfragt,
    /// kaeme zu spaet, wenn das Backend inzwischen fertig geworden ist.
    dispatched_per_model: HashMap<String, u64>,
    /// Requests, die wegen vollstaendiger Quarantaene abgewiesen wurden.
    metrics_rejected_quarantined: u64,
    /// Backendaufrufe, die noch offen sind.
    outstanding: u64,
    /// Gesetzt, sobald ein geordnetes Ende angefordert wurde.
    shutdown: Option<oneshot::Sender<()>>,
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
    let mut backends: HashMap<String, Arc<TritonClient>> = HashMap::new();
    for endpoint in config.endpoints() {
        let client = if endpoint == config.backend_endpoint {
            Arc::clone(backend)
        } else {
            Arc::new(TritonClient::new(&endpoint))
        };
        backends.insert(endpoint, client);
    }

    let overload = OverloadController::new(OverloadConfig::default(), clock.now())
        .map_err(|_| SchedulerError::NoModels)?;
    let mut scheduler = Scheduler::new(
        config.contracts.clone(),
        config.slots.clone(),
        overload,
        config.margin,
    )?;
    // G-010: Profile, deren Umgebung sich geaendert hat, werden vorsichtiger
    // geplant, bis der Estimator eigene Messungen hat (ADR-0016).
    for model in unverified {
        scheduler.mark_profile_unverified(*model);
    }

    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let actor = Actor {
        scheduler,
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
        next_generation: 0,
        backend_timeouts: 0,
        consecutive_transport_failures: 0,
        reconciled: 0,
        dispatched_per_model: HashMap::new(),
        metrics_rejected_quarantined: 0,
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
        descriptor: RequestDescriptor,
        request: Box<ModelInferRequest>,
        reply: oneshot::Sender<Reply>,
        sink: &mut S,
    ) -> bool {
        let id = descriptor.id;

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
            && let Some(job) = GenerativeJob::from_request(&request, cooperative.max_total_tokens)
        {
            self.jobs.insert(id, job);
            self.descriptors.insert(id, descriptor);
            self.continuation_of.insert(id, id);
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
            }
            _ => self.consecutive_transport_failures = 0,
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
            if let Some(entry) = self.leases.get_mut(&request) {
                entry.state = LeaseState::Reconciling;
            }
            self.start_reconciliation(&lease, request);
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
            } => {
                if !self.accept(now, *descriptor, request, reply, &mut sink) {
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
            Msg::ExecutionProven {
                request,
                generation,
            } => {
                // Nur wenn der Anspruch noch derselbe ist. Ein Nachweis fuer
                // eine alte Generation gehoert zu einem Kredit, der laengst
                // zurueckgegeben wurde.
                let matching = self
                    .leases
                    .get(&request)
                    .is_some_and(|l| l.generation == generation);
                if let Some(lease) = matching.then(|| self.leases.remove(&request)).flatten() {
                    self.reconciled = self.reconciled.saturating_add(1);
                    tracing::info!(
                        %request,
                        "Backend belegt das Ausfuehrungsende; Slotkredit wird zurueckgegeben"
                    );
                    self.scheduler.on_event(
                        now,
                        Event::BackendFailure {
                            request,
                            slot: lease.slot,
                        },
                        &mut sink,
                    );
                }
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
            Msg::Tick => {
                self.scheduler.on_event(now, Event::Tick, &mut sink);
                self.report_contract_mismatch(now);
            }
            Msg::Snapshot(tx) => {
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
                metrics.rejected_quarantined = self.metrics_rejected_quarantined;
                metrics.reconciled = self.reconciled;
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
                return;
            }
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
        if let (Some(tokens), Some(job)) = (quantum, self.jobs.get(&request)) {
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
        self.outstanding = self.outstanding.saturating_add(1);

        // Der Anspruch auf den Slotkredit entsteht **hier**, mit dem Dispatch,
        // und endet erst mit einem Nachweis. Die Wanduhrzeit dient
        // ausschliesslich dem spaeteren Abgleich gegen Tritons `last_inference`.
        self.next_generation = self.next_generation.saturating_add(1);
        let dispatched_total = self
            .dispatched_per_model
            .entry(oip.model_name.clone())
            .and_modify(|n| *n = n.saturating_add(1))
            .or_insert(1);
        let target = *dispatched_total;
        self.leases.insert(
            request,
            Lease {
                generation: self.next_generation,
                slot,
                backend_model: oip.model_name.clone(),
                dispatched_total: target,
                state: LeaseState::Running,
            },
        );
        tokio::spawn(async move {
            let call = async move {
                if decoupled {
                    backend.infer_decoupled(*oip).await
                } else {
                    backend.infer(*oip).await
                }
            };
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
        let mut next = descriptor;
        next.id = continuation;

        // Die Abbildung wandert mit: Schluessel bleibt die Kennung, die der
        // Client kennt, Wert wird die neue.
        let origin = self
            .continuation_of
            .iter()
            .find(|(_, current)| **current == request)
            .map_or(request, |(origin, _)| *origin);
        self.continuation_of.insert(origin, continuation);

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
    fn start_reconciliation(&self, lease: &Lease, request: RequestId) {
        let Some(backend) = self.backend_for_model_name(&lease.backend_model) else {
            tracing::warn!(
                model = %lease.backend_model,
                "kein Backendclient fuer den Abgleich; der Slotkredit bleibt gehalten"
            );
            return;
        };
        let tx = self.tx.clone();
        let model = lease.backend_model.clone();
        let generation = lease.generation;
        let target = lease.dispatched_total;
        let interval = std::time::Duration::from_millis(RECONCILE_INTERVAL_MS);

        tokio::spawn(async move {
            let mut highest = 0_u64;
            loop {
                match backend.completion_evidence(&model).await {
                    Ok(evidence) => {
                        let restarted = evidence.completed < highest;
                        highest = highest.max(evidence.completed);
                        if evidence.completed >= target || restarted {
                            let _ = tx
                                .send(Msg::ExecutionProven {
                                    request,
                                    generation,
                                })
                                .await;
                            return;
                        }
                    }
                    Err(error) => {
                        // Kein Nachweis heisst: der Kredit bleibt gehalten.
                        // Weiter fragen, bis der Actor endet.
                        tracing::debug!(%request, %error, "Abgleich noch ohne Nachweis");
                    }
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    /// Der Client fuer ein Backendmodell, sofern eindeutig bestimmbar.
    fn backend_for_model_name(&self, backend_model: &str) -> Option<Arc<TritonClient>> {
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
    }

    /// Der Client fuer das Backend eines Modells.
    fn backend_for(&self, model: vig_core::ModelIdx) -> Option<Arc<TritonClient>> {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{CancelOnDrop, Msg};
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
}
