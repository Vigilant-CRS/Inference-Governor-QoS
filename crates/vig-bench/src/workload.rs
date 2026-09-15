//! Der Lasttreiber: Kameras, die unabhaengig vom Systemzustand weiter liefern.
//!
//! Der Punkt der Uebung ist, dass ein Sensor **nicht** langsamer wird, wenn das
//! System ueberlastet ist. Ein Treiber, der auf die Antwort wartet, bevor er
//! den naechsten Frame schickt, wuerde das Problem wegdefinieren, um das es
//! geht — und beiden Seiten einen Vorteil verschaffen, den es real nicht gibt.
//!
//! Beide Vergleichslaeufe bekommen denselben Treiber, dieselben Perioden,
//! dieselbe Obergrenze offener Requests und dieselben Frame-Nummern. Der
//! einzige Unterschied ist, wohin die Requests gehen.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tonic::transport::Channel;
use vig_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient;
use vig_protocol_oip::inference::infer_parameter::ParameterChoice;
use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{InferParameter, ModelInferRequest};
use vig_protocol_oip::params::P_AGE_US;
use vig_sim::coverage::{Coverage, CoverageTracker};

/// Die Eingabe, die ein Strom mitschickt.
///
/// Als **Shared-Memory-Referenz**: der Request nennt nur Region, Offset und
/// Groesse. Ein Gate-M3-Vergleich ueber den Copy-Pfad wuerde den Transport
/// messen statt das Scheduling, und der Governor traegt diese Kosten doppelt
/// (ADR-0003).
#[derive(Debug, Clone)]
pub struct InputSpec {
    /// Name des Eingabetensors laut Modellmetadaten.
    pub name: String,
    /// Datentyp laut Modellmetadaten.
    pub datatype: String,
    /// Vollstaendige Form einschliesslich Batchdimension.
    pub shape: Vec<i64>,
    /// Die beim Backend registrierte Region.
    ///
    /// `None`, wenn der Server kein Shared Memory kann. Dann reist die
    /// Nutzlast im Request — langsamer, aber ueberall verfuegbar. Welcher
    /// Weg moeglich ist, sagt der Server selbst
    /// (`vig_backend_triton::Capabilities`).
    pub region: Option<String>,
    /// Groesse des Tensors in Bytes.
    pub byte_size: u64,
    /// Echte Eingaben fuer den Kopierpfad, reihum nach Frame-Nummer.
    ///
    /// `None` schickt Nullbytes wie bisher. Fuer ein Faltungsnetz ist das
    /// gleichgueltig, fuer eine Nachbearbeitung im Graphen nicht: auf Nullen
    /// findet ein Detektor nichts, und seine NMS sortiert nichts.
    pub payload: Option<Arc<Vec<Vec<u8>>>>,
}

/// Ein generativer Auftrag: Prompt und Samplingparameter.
///
/// Absichtlich schlicht gehalten. Der Lasttreiber soll ein Sprachmodell
/// **beschaeftigen**, nicht seine Ausgabe bewerten — die Frage des Dauerlaufs
/// ist, ob der Governor den getakteten Strom neben einem langen,
/// nicht unterbrechbaren Auftrag noch durchbringt.
#[derive(Debug, Clone)]
pub struct TextSpec {
    /// Der Prompt. Reihum nach Frame-Nummer, wenn mehrere angegeben sind.
    pub prompts: Vec<String>,
    /// Wie viele Token je Auftrag erzeugt werden.
    ///
    /// Die Groesse, die die Auftragsdauer bestimmt — und damit, wie lange der
    /// geschuetzte Strom im schlechtesten Fall warten muss.
    pub max_tokens: u32,
}

/// Ein Sensorstrom im Lastmodell.
#[derive(Debug, Clone)]
pub struct StreamDef {
    /// Name im Report.
    pub name: &'static str,
    /// Modellname, den der Client anfragt.
    ///
    /// Ueber den Governor ist das der logische Name, direkt der physische.
    pub model: &'static str,
    /// Periode zwischen zwei Frames.
    pub period: Duration,
    /// Fachliches Hoechstalter fuer die Coverage-Bewertung.
    pub max_age: Duration,
    /// Obergrenze gleichzeitig offener Requests dieses Stroms.
    ///
    /// Bildet einen Client mit endlichem Puffer ab. Ohne Grenze wuerde der
    /// Treiber selbst unbegrenzt Speicher belegen und damit etwas anderes
    /// messen als das System.
    pub in_flight_cap: usize,
    /// Die Eingabe, falls das Backend eine braucht.
    ///
    /// `None` fuer das Mock-Backend, das keine Tensoren auswertet.
    pub input: Option<InputSpec>,
    /// Ein generativer Auftrag statt eines Tensors.
    ///
    /// Gesetzt, wenn dieser Strom ein Sprachmodell fuettert. Dann reisen zwei
    /// BYTES-Tensoren (`text_input`, `sampling_parameters`) laengenpraefigiert
    /// im Request, und `input` bleibt ungenutzt.
    ///
    /// **Die Abdeckung dieses Stroms bedeutet nichts.** Der Zaehler bewertet
    /// periodische Abtastung; ein Auftrag ueber mehrere Sekunden ist keine.
    /// Was hier zaehlt, sind abgeschlossene Generierungen — `delivered` —,
    /// so wie `wp26` es auswertet. Wer die Abdeckungsspalte dieses Stroms
    /// liest, liest eine Zahl ohne Gegenstand.
    pub text: Option<TextSpec>,
    /// Verwirft der Client selbst veraltete Frames?
    ///
    /// Das ist der naheliegende Eigenbau: nur der neueste Frame zaehlt, es ist
    /// immer nur einer unterwegs, und trifft ein neuer ein, waehrend der alte
    /// noch laeuft, wird der alte fallengelassen. Genau die LATEST-Semantik
    /// aus Spec 9.3 — aber im Client statt im Governor.
    ///
    /// Der Vergleich dieser Betriebsart gegen den Governor beantwortet den
    /// haertesten Einwand gegen das ganze Produkt: was kann der Governor, das
    /// eine Stunde Clientcode nicht auch kann?
    pub pump: bool,
    /// Lastspitzen, falls der Strom welche hat (Spec 19.4).
    ///
    /// `None` laesst den Strom im festen Takt laufen, genau wie vor diesem
    /// Feld — alle frueheren Messungen bleiben damit vergleichbar.
    pub burst: Option<Burst>,
}

/// Wiederkehrende Lastspitzen eines Stroms (Spec 19.4).
///
/// Alle `every` beginnt eine Spitze, die `length` dauert; waehrenddessen
/// liefert der Sensor mit `period` statt mit der Grundperiode. Die
/// Abdeckung wird weiter im Grundtakt bewertet: der Verbraucher tastet nicht
/// schneller ab, nur weil die Kamera es tut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Burst {
    /// Abstand zwischen zwei Spitzenbeginnen.
    pub every: Duration,
    /// Dauer einer Spitze.
    pub length: Duration,
    /// Periode waehrend der Spitze.
    pub period: Duration,
}

impl Burst {
    /// Die Periode zum Zeitpunkt `elapsed` seit Laufbeginn.
    #[must_use]
    pub fn period_at(&self, base: Duration, elapsed: Duration) -> Duration {
        let phase = elapsed
            .as_nanos()
            .checked_rem(self.every.as_nanos())
            .unwrap_or(0);
        if phase < self.length.as_nanos() {
            self.period
        } else {
            base
        }
    }
}

/// Das Ergebnis eines Stroms.
#[derive(Debug, Clone)]
pub struct StreamReport {
    /// Name des Stroms.
    pub name: &'static str,
    /// Vom Sensor erzeugte Frames.
    pub emitted: u64,
    /// Tatsaechlich gesendete Requests.
    pub sent: u64,
    /// Frames, die der Client wegen seiner eigenen Grenze nicht senden konnte.
    pub client_dropped: u64,
    /// Beantwortete Requests.
    pub delivered: u64,
    /// Alle nicht beantworteten Requests: `refused` plus `errors`.
    ///
    /// Die Summe wie vor der Aufteilung, damit bestehende Auswertungen
    /// (Dauerlauf, `oip-check`) dieselbe Spalte behalten.
    pub rejected: u64,
    /// Davon absichtlich abgewiesen, siehe [`is_governor_refusal`].
    ///
    /// Unter Last ein legitimes Ergebnis: der Governor hat entschieden,
    /// diesen Frame nicht mehr zu rechnen.
    pub refused: u64,
    /// Davon Transport-, Protokoll- oder Modellfehler, und Requests, die
    /// sich gar nicht bauen liessen.
    ///
    /// Kein Ergebnis ueber das Scheduling, sondern ein Integrationsfehler.
    pub errors: u64,
    /// Ob der Strom eine Verbindung zum Ziel bekam und gefahren wurde.
    ///
    /// `false` heisst: kein einziger Request ging hinaus, und die Abdeckung
    /// dieses Berichts ist die eines Stroms, der nie lief.
    pub connected: bool,
    /// Abdeckung und Age of Information.
    pub coverage: Coverage,
}

/// Ob ein Strombericht eine Messung ist oder ein Integrationsfehler.
///
/// Ein Bericht entsteht fuer jeden Strom, auch wenn nichts ankam. Seine
/// Abdeckung ist dann 1000 ‰ — eine Zahl, die wie ein vernichtendes Ergebnis
/// aussieht und keines ist (Review vom 15.09., R04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrity {
    /// Eine Messung: verbunden, ohne Fehler, mit Lieferungen oder mit
    /// legitimen Ablehnungen.
    Valid,
    /// Keine Verbindung zum Ziel, oder der Strom wurde gar nicht gefahren.
    NotRun,
    /// Transport-, Protokoll- oder Modellfehler; die Zahl steht dabei.
    Errors(u64),
    /// Weder eine Lieferung noch eine Ablehnung im Messfenster.
    NoOutcome,
    /// Keine Lieferung, nur Ablehnungen, auf einem Arm, der liefern muss.
    NothingDelivered,
}

impl Integrity {
    /// Das Wort fuer maschinenlesbare Berichte.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::NotRun => "not_run",
            Self::Errors(_) => "errors",
            Self::NoOutcome => "no_outcome",
            Self::NothingDelivered => "nothing_delivered",
        }
    }

    /// Ob der Bericht als Messung zaehlt.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }
}

impl StreamReport {
    /// Ein Bericht fuer einen Strom, der nicht gefahren wurde.
    #[must_use]
    pub fn not_run(name: &'static str) -> Self {
        Self {
            name,
            emitted: 0,
            sent: 0,
            client_dropped: 0,
            delivered: 0,
            rejected: 0,
            refused: 0,
            errors: 0,
            connected: false,
            coverage: Coverage::default(),
        }
    }

    /// Ob dieser Bericht eine Messung ist.
    ///
    /// `may_starve` sagt, ob ein Strom ohne jede Lieferung ein gueltiges
    /// Ergebnis sein kann: ueber den Governor ja, wenn er jeden Frame
    /// absichtlich abgewiesen hat — das ist Aushungern unter Last, ein
    /// negativer Befund. Direkt am Backend nein: ein Vergleichsarm ohne
    /// Lieferung vergleicht nichts.
    ///
    /// Schon ein einziger Fehler macht den Bericht ungueltig. Ein Fehler
    /// kostet Takte, die dann dem Scheduling angelastet wuerden.
    #[must_use]
    pub const fn integrity(&self, may_starve: bool) -> Integrity {
        if !self.connected {
            Integrity::NotRun
        } else if self.errors > 0 {
            Integrity::Errors(self.errors)
        } else if self.delivered > 0 {
            Integrity::Valid
        } else if self.refused == 0 {
            Integrity::NoOutcome
        } else if may_starve {
            Integrity::Valid
        } else {
            Integrity::NothingDelivered
        }
    }
}

/// Ob ein Status eine absichtliche Ablehnung des Governors ist.
///
/// Der Governor nennt den Grund im Metadatum
/// [`vig_gateway::outcome::REASON_HEADER`]. Scheduling-Entscheidungen sind
/// `superseded`, `stale` und `infeasible` (`outcome::status_for`) sowie ein
/// voller Abhaengigkeitsgraph `graph_full`/`graph_quota`
/// (`outcome::graph_rejection`). Ohne Grund zaehlt nur `ResourceExhausted`:
/// so antwortet der Governor auf eine volle Actor-Warteschlange und ein
/// erschoepftes Nutzlastbudget — Backpressure, also ebenfalls Last.
///
/// Alles andere ist ein Fehler: `Unavailable` (Verbindung, Backend,
/// `backend_failed`, Slotkredite in Quarantaene), `DeadlineExceeded`
/// (`backend_timeout`), `NotFound` (unbekanntes Modell), `InvalidArgument`,
/// `PermissionDenied`, `Internal`, und jeder Grund, den dieser Client nicht
/// kennt.
#[must_use]
pub fn is_governor_refusal(status: &tonic::Status) -> bool {
    match status
        .metadata()
        .get(vig_gateway::outcome::REASON_HEADER)
        .map(|value| value.to_str())
    {
        Some(Ok(reason)) => matches!(
            reason,
            "superseded" | "stale" | "infeasible" | "graph_full" | "graph_quota"
        ),
        Some(Err(_)) => false,
        None => status.code() == tonic::Code::ResourceExhausted,
    }
}

struct StreamState {
    tracker: Mutex<CoverageTracker>,
    emitted: AtomicU64,
    sent: AtomicU64,
    client_dropped: AtomicU64,
    delivered: AtomicU64,
    rejected: AtomicU64,
    refused: AtomicU64,
    errors: AtomicU64,
    permits: Arc<Semaphore>,
}

impl StreamState {
    /// Zaehlt einen nicht beantworteten Request, getrennt nach Ursache.
    fn record_failure(&self, refusal: bool) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
        if refusal {
            self.refused.fetch_add(1, Ordering::Relaxed);
        } else {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Faehrt den Lauf und gibt je Strom einen Bericht zurueck.
///
/// `via_governor` steuert nur, ob der Client die Governor-Altersangabe
/// mitschickt; das Ziel bestimmt der Aufrufer ueber `endpoint`.
///
/// Kommt keine Verbindung zustande, bricht der Lauf nicht ab: der Strom
/// meldet `connected: false` und null Lieferungen. Ob ein Bericht eine
/// Messung ist, sagt [`StreamReport::integrity`].
pub async fn drive(
    endpoint: &str,
    streams: &[StreamDef],
    duration: Duration,
    via_governor: bool,
) -> Vec<StreamReport> {
    let origin = Instant::now();
    let core_duration = vig_core::Duration::from_nanos_unbounded(
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
    );

    let mut states = Vec::new();
    for stream in streams {
        states.push(Arc::new(StreamState {
            tracker: Mutex::new(CoverageTracker::new(
                to_core(stream.period),
                to_core(stream.max_age),
                vig_core::Instant::ZERO,
                core_duration,
            )),
            emitted: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            client_dropped: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            permits: Arc::new(Semaphore::new(stream.in_flight_cap)),
        }));
    }

    let mut tasks = Vec::new();
    let mut connected = vec![false; streams.len()];
    for (index, stream) in streams.iter().enumerate() {
        let Some(state) = states.get(index).cloned() else {
            continue;
        };
        let stream = stream.clone();
        // Ist das Ziel gerade weg, faellt dieser Strom fuer dieses Fenster aus
        // und meldet null Lieferungen — der Lauf geht weiter. Der Bericht sagt
        // es in `connected`, damit niemand die Nullen als Messung liest.
        let Ok(client) = try_connect(endpoint).await else {
            continue;
        };
        if let Some(flag) = connected.get_mut(index) {
            *flag = true;
        }
        tasks.push(tokio::spawn(async move {
            if stream.pump {
                run_stream_pump(client, stream, state, origin, duration, via_governor).await;
            } else {
                run_stream(client, stream, state, origin, duration, via_governor).await;
            }
        }));
    }
    for task in tasks {
        let _ = task.await;
    }

    streams
        .iter()
        .zip(states.iter())
        .zip(connected)
        .map(|((stream, state), connected)| StreamReport {
            name: stream.name,
            emitted: state.emitted.load(Ordering::Relaxed),
            sent: state.sent.load(Ordering::Relaxed),
            client_dropped: state.client_dropped.load(Ordering::Relaxed),
            delivered: state.delivered.load(Ordering::Relaxed),
            rejected: state.rejected.load(Ordering::Relaxed),
            refused: state.refused.load(Ordering::Relaxed),
            errors: state.errors.load(Ordering::Relaxed),
            connected,
            coverage: state
                .tracker
                .lock()
                .map_or(Coverage::default(), |tracker| tracker.finish()),
        })
        .collect()
}

async fn run_stream(
    client: GrpcInferenceServiceClient<Channel>,
    stream: StreamDef,
    state: Arc<StreamState>,
    origin: Instant,
    duration: Duration,
    via_governor: bool,
) {
    let mut ticker = tokio::time::interval(stream.period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut next = tokio::time::Instant::now();
    let mut frame = 0_u64;
    // Jeder Request ist eine eigene Aufgabe. Frueher wurde keine davon
    // abgewartet: `drive` kehrte zurueck, waehrend Requests noch liefen, las die
    // Zaehler vor ihnen, und noch laufende Requests des direkten Arms belegten
    // das Backend, waehrend schon der Governor-Arm oder der naechste Lastpunkt
    // mass (Befund beim Review-Fix R04, 15.09.; dieselbe Klasse wie R03 vom
    // 14.09. in `pilot::run_arm`).
    let mut inflight = tokio::task::JoinSet::new();

    loop {
        while inflight.try_join_next().is_some() {}
        // Ohne Spitzen der feste Takt wie immer. Mit Spitzen richtet sich der
        // naechste Takt danach, ob gerade eine laeuft; verpasste Takte werden
        // wie bei `Skip` nicht nachgeholt.
        if stream.burst.is_some() {
            tokio::time::sleep_until(next).await;
        } else {
            ticker.tick().await;
        }
        let capture = Instant::now();
        if capture.duration_since(origin) >= duration {
            break;
        }
        if let Some(burst) = stream.burst {
            let period = burst.period_at(stream.period, capture.duration_since(origin));
            next = next.checked_add(period).unwrap_or(next);
            let now = tokio::time::Instant::now();
            if next < now {
                next = now;
            }
        }
        frame = frame.saturating_add(1);
        state.emitted.fetch_add(1, Ordering::Relaxed);

        // Der Client hat einen endlichen Puffer. Ist er voll, geht der Frame
        // verloren - in beiden Laeufen gleichermassen.
        let Ok(permit) = Arc::clone(&state.permits).try_acquire_owned() else {
            state.client_dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        };

        let mut client = client.clone();
        let state = Arc::clone(&state);
        let id = format!("{}:{frame}", stream.name);
        let model = stream.model;
        let input = stream.input.clone();
        let text = stream.text.clone();
        inflight.spawn(async move {
            let _permit = permit;
            let age = capture.elapsed();
            let Some(request) = build_request(
                model,
                &id,
                frame,
                via_governor,
                age,
                input.as_ref(),
                text.as_ref(),
            ) else {
                // Kein gueltiger `BYTES`-Rahmen (Prompt ab 4 GiB): nicht
                // gesendet, aber gezaehlt — als Fehler, nicht als Ablehnung.
                state.record_failure(false);
                return;
            };
            state.sent.fetch_add(1, Ordering::Relaxed);

            match client.model_infer(request).await {
                Ok(_) => {
                    state.delivered.fetch_add(1, Ordering::Relaxed);
                    let completion = Instant::now().duration_since(origin);
                    let generation = capture.duration_since(origin);
                    if let Ok(mut tracker) = state.tracker.lock() {
                        tracker.record_delivery(
                            vig_core::Instant::from_nanos(
                                u64::try_from(completion.as_nanos()).unwrap_or(u64::MAX),
                            ),
                            vig_core::Instant::from_nanos(
                                u64::try_from(generation.as_nanos()).unwrap_or(u64::MAX),
                            ),
                        );
                    }
                }
                Err(status) => state.record_failure(is_governor_refusal(&status)),
            }
        });
    }
    // Die offenen Requests zu Ende kommen lassen, begrenzt: Ein haengendes
    // Backend darf den Lauf nicht unbegrenzt aufhalten. Was danach noch laeuft,
    // wird abgebrochen und fehlt im Bericht — genau wie vorher, aber nicht
    // mehr still im naechsten Messfenster.
    let drained = tokio::time::timeout(DRAIN_AFTER_WINDOW, async {
        while inflight.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        inflight.abort_all();
    }
}

/// Wie lange `run_stream` nach dem Messfenster auf offene Requests wartet.
const DRAIN_AFTER_WINDOW: Duration = Duration::from_secs(30);

/// Der Eigenbau: LATEST-Semantik im Client.
///
/// Ein Erzeuger legt jeden Frame in ein Fach, das nur den neuesten haelt; ein
/// Sender nimmt ihn heraus, sobald die vorige Antwort da ist. Ueberschriebene
/// Frames sind verworfen — sie waren beim Absenden schon nicht mehr die
/// aktuelle Lage.
///
/// Was diese Schleife nicht kann, ist der Kern des Vergleichs: sie sieht nur
/// ihren eigenen Strom. Zwei solche Pumpen nebeneinander wissen nichts
/// voneinander, und am Server entscheidet weiter die Ankunftsreihenfolge.
async fn run_stream_pump(
    client: GrpcInferenceServiceClient<Channel>,
    stream: StreamDef,
    state: Arc<StreamState>,
    origin: Instant,
    duration: Duration,
    via_governor: bool,
) {
    let slot: Arc<Mutex<Option<(u64, Instant)>>> = Arc::new(Mutex::new(None));
    let ready = Arc::new(tokio::sync::Notify::new());

    let producer = {
        let slot = Arc::clone(&slot);
        let ready = Arc::clone(&ready);
        let state = Arc::clone(&state);
        let period = stream.period;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut frame = 0_u64;
            loop {
                ticker.tick().await;
                let capture = Instant::now();
                if capture.duration_since(origin) >= duration {
                    break;
                }
                frame = frame.saturating_add(1);
                state.emitted.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut guard) = slot.lock()
                    && guard.replace((frame, capture)).is_some()
                {
                    // Ein noch nicht gesendeter Frame wurde ueberholt.
                    state.client_dropped.fetch_add(1, Ordering::Relaxed);
                }
                ready.notify_one();
            }
        })
    };

    let mut client = client;
    loop {
        if Instant::now().duration_since(origin) >= duration {
            break;
        }
        let next = slot.lock().ok().and_then(|mut guard| guard.take());
        let Some((frame, capture)) = next else {
            // Nichts zu tun; auf den naechsten Frame warten, aber nicht
            // ueber das Ende des Laufs hinaus.
            let _ = tokio::time::timeout(Duration::from_millis(5), ready.notified()).await;
            continue;
        };

        let id = format!("{}:{frame}", stream.name);
        let age = capture.elapsed();
        let Some(request) = build_request(
            stream.model,
            &id,
            frame,
            via_governor,
            age,
            stream.input.as_ref(),
            stream.text.as_ref(),
        ) else {
            // Kein gueltiger `BYTES`-Rahmen (Prompt ab 4 GiB): nicht gesendet,
            // aber gezaehlt — als Fehler, nicht als Ablehnung.
            state.record_failure(false);
            continue;
        };
        state.sent.fetch_add(1, Ordering::Relaxed);

        match client.model_infer(request).await {
            Ok(_) => {
                state.delivered.fetch_add(1, Ordering::Relaxed);
                let completion = Instant::now().duration_since(origin);
                let generation = capture.duration_since(origin);
                if let Ok(mut tracker) = state.tracker.lock() {
                    tracker.record_delivery(
                        vig_core::Instant::from_nanos(
                            u64::try_from(completion.as_nanos()).unwrap_or(u64::MAX),
                        ),
                        vig_core::Instant::from_nanos(
                            u64::try_from(generation.as_nanos()).unwrap_or(u64::MAX),
                        ),
                    );
                }
            }
            Err(status) => state.record_failure(is_governor_refusal(&status)),
        }
    }
    producer.abort();
}

fn build_request(
    model: &str,
    id: &str,
    frame: u64,
    via_governor: bool,
    age: Duration,
    input: Option<&InputSpec>,
    text: Option<&TextSpec>,
) -> Option<ModelInferRequest> {
    if let Some(spec) = text {
        return build_text_request(model, id, frame, spec);
    }
    let mut parameters = HashMap::new();
    if via_governor {
        // ADR-0011: der hosttopologieunabhaengige Weg. Der Client sagt, wie alt
        // das Sensordatum beim Senden war.
        parameters.insert(
            P_AGE_US.to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::Int64Param(
                    i64::try_from(age.as_micros()).unwrap_or(i64::MAX),
                )),
            },
        );
    }
    let mut raw_contents = Vec::new();
    let inputs = input.map_or_else(Vec::new, |spec| {
        let Some(region) = spec.region.clone() else {
            // Kopierpfad: die Nutzlast reist im Request mit — die echten
            // Eingaben reihum, wenn es welche gibt, sonst Nullbytes.
            let chosen = spec.payload.as_ref().and_then(|payload| {
                let count = u64::try_from(payload.len()).ok()?;
                let index = usize::try_from(frame.checked_rem(count)?).ok()?;
                payload.get(index).cloned()
            });
            raw_contents.push(
                chosen.unwrap_or_else(|| vec![0_u8; usize::try_from(spec.byte_size).unwrap_or(0)]),
            );
            return vec![InferInputTensor {
                name: spec.name.clone(),
                datatype: spec.datatype.clone(),
                shape: spec.shape.clone(),
                parameters: HashMap::new(),
                contents: None,
            }];
        };
        let mut tensor_params = HashMap::new();
        tensor_params.insert(
            "shared_memory_region".to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::StringParam(region)),
            },
        );
        tensor_params.insert(
            "shared_memory_byte_size".to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::Int64Param(
                    i64::try_from(spec.byte_size).unwrap_or(i64::MAX),
                )),
            },
        );
        tensor_params.insert(
            "shared_memory_offset".to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::Int64Param(0)),
            },
        );
        vec![InferInputTensor {
            name: spec.name.clone(),
            datatype: spec.datatype.clone(),
            shape: spec.shape.clone(),
            parameters: tensor_params,
            contents: None,
        }]
    });

    Some(ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: id.to_owned(),
        parameters,
        inputs,
        outputs: Vec::new(),
        // Leer, wenn die Nutzlast als Referenz reist; sonst die Rohbytes.
        raw_input_contents: raw_contents,
    })
}

/// Ein generativer Auftrag an das vLLM-Backend.
///
/// Dieselbe Form wie im Kopierpfad oben: Die Tensoren tragen `contents: None`,
/// die Nutzbytes reisen in `raw_input_contents`. Neu ist nur, dass es **zwei**
/// sind und dass sie laengenpraefigiert werden.
///
/// **Ohne Altersangabe.** Der Governor bekommt hier bewusst kein `P_AGE_US`:
/// Ein Generierungsauftrag hat kein Aufnahmealter, das abliefe — er ist die
/// Hintergrundarbeit, gegen die der getaktete Strom verteidigt wird, und
/// nicht selbst ein Sensordatum. Ihn mit einem Alter zu versehen hiesse, ihn
/// verwerfbar zu machen, und genau das soll er nicht sein.
///
/// `None`, wenn der Prompt nicht in einen `BYTES`-Rahmen passt (ab 4 GiB).
fn build_text_request(
    model: &str,
    id: &str,
    frame: u64,
    spec: &TextSpec,
) -> Option<ModelInferRequest> {
    let prompt = if spec.prompts.is_empty() {
        ""
    } else {
        let index = usize::try_from(frame)
            .unwrap_or(0)
            .checked_rem(spec.prompts.len())
            .unwrap_or(0);
        spec.prompts.get(index).map_or("", String::as_str)
    };
    let sampling = format!(
        "{{\"max_tokens\": {}, \"temperature\": 0.0}}",
        spec.max_tokens
    );
    let tensor = |name: &str| InferInputTensor {
        name: name.to_owned(),
        datatype: "BYTES".to_owned(),
        shape: vec![1],
        parameters: HashMap::new(),
        contents: None,
    };
    Some(ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: id.to_owned(),
        parameters: HashMap::new(),
        inputs: vec![
            tensor(vig_gateway::cooperative::TEXT_INPUT),
            tensor(vig_gateway::cooperative::SAMPLING_PARAMETERS),
        ],
        outputs: Vec::new(),
        raw_input_contents: vec![
            vig_gateway::cooperative::write_length_prefixed(prompt)?,
            vig_gateway::cooperative::write_length_prefixed(&sampling)?,
        ],
    })
}

fn to_core(d: Duration) -> vig_core::Duration {
    vig_core::Duration::from_nanos_unbounded(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Baut eine Clientverbindung mit denselben Transportgrenzen wie der Governor.
///
/// # Panics
///
/// Wenn keine Verbindung aufgebaut werden kann.
pub async fn connect(endpoint: &str) -> GrpcInferenceServiceClient<Channel> {
    #[allow(clippy::expect_used)]
    try_connect(endpoint).await.expect("Verbindung zum Ziel")
}

/// Wie [`connect`], aber ohne Panik bei unerreichbarem Ziel.
///
/// Fuer kurze Benchmarks ist ein Abbruch die richtige Antwort — laeuft das
/// Backend nicht, ist die Messung sinnlos. Fuer einen Dauerlauf ueber Stunden
/// waere sie falsch: ein einzelner Aussetzer des Backends darf nicht die
/// gesamte Nacht kosten, sondern gehoert protokolliert und ueberstanden.
///
/// # Errors
///
/// Wenn der Endpunkt ungueltig ist oder die Verbindung nicht zustande kommt.
pub async fn try_connect(
    endpoint: &str,
) -> Result<GrpcInferenceServiceClient<Channel>, tonic::transport::Error> {
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{endpoint}"))?
        .initial_stream_window_size(vig_backend_triton::STREAM_WINDOW_BYTES)
        .initial_connection_window_size(vig_backend_triton::CONNECTION_WINDOW_BYTES)
        .tcp_nodelay(true)
        .connect()
        .await?;
    Ok(GrpcInferenceServiceClient::new(channel)
        .max_decoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
        .max_encoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES))
}

#[cfg(test)]
mod tests {
    use super::{Burst, InputSpec, build_request};
    use std::sync::Arc;
    use std::time::Duration;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn copy_spec(payload: Option<Vec<Vec<u8>>>) -> InputSpec {
        InputSpec {
            name: "in".to_owned(),
            datatype: "UINT8".to_owned(),
            shape: vec![1, 2],
            region: None,
            byte_size: 2,
            payload: payload.map(Arc::new),
        }
    }

    /// Auf dem Kopierpfad reisen die echten Eingaben reihum nach
    /// Frame-Nummer; ohne sie bleibt es bei Nullbytes wie bisher.
    #[test]
    fn the_copy_path_cycles_real_inputs_by_frame() {
        let spec = copy_spec(Some(vec![vec![1, 1], vec![2, 2], vec![3, 3]]));
        let sent: Vec<Option<Vec<u8>>> = (0..4)
            .map(|frame| {
                build_request("m", "s:0", frame, false, ms(0), Some(&spec), None)
                    .map(|request| request.raw_input_contents.concat())
            })
            .collect();
        assert_eq!(
            sent,
            vec![
                Some(vec![1, 1]),
                Some(vec![2, 2]),
                Some(vec![3, 3]),
                Some(vec![1, 1])
            ]
        );

        let zeros = build_request("m", "s:0", 7, false, ms(0), Some(&copy_spec(None)), None);
        assert_eq!(
            zeros.map(|request| request.raw_input_contents),
            Some(vec![vec![0, 0]])
        );
    }

    /// 200 ms Spitze alle 2 s: innerhalb der Spitze die kurze Periode,
    /// danach die Grundperiode, und in der naechsten Spitze wieder die kurze.
    #[test]
    fn a_burst_shortens_the_period_only_inside_its_window() {
        let burst = Burst {
            every: ms(2_000),
            length: ms(200),
            period: ms(15),
        };
        let base = ms(25);
        assert_eq!(burst.period_at(base, ms(0)), ms(15));
        assert_eq!(burst.period_at(base, ms(199)), ms(15));
        assert_eq!(burst.period_at(base, ms(200)), base);
        assert_eq!(burst.period_at(base, ms(1_999)), base);
        assert_eq!(burst.period_at(base, ms(2_100)), ms(15));
    }

    /// Ein Abstand von null ist eine Fehlkonfiguration, keine Panik: der
    /// Rest ist dann null, und der Strom laeuft im Spitzentakt.
    #[test]
    fn a_zero_interval_does_not_panic() {
        let burst = Burst {
            every: Duration::ZERO,
            length: ms(200),
            period: ms(15),
        };
        assert_eq!(burst.period_at(ms(25), ms(5_000)), ms(15));
    }

    /// Eine Ablehnung des Governors ist ein Ergebnis unter Last, ein Fehler
    /// ist keines. Geprueft an den Statusobjekten, die `vig-gateway` selbst
    /// baut, nicht an nachgebauten.
    #[test]
    fn governor_refusals_are_told_apart_from_integration_errors() {
        use super::is_governor_refusal;
        use tonic::{Code, Status};
        use vig_core::RequestState;
        use vig_gateway::outcome::{REASON_HEADER, graph_rejection, status_for};

        let refusal = |state| status_for(state).is_some_and(|s| is_governor_refusal(&s));
        assert!(refusal(RequestState::Superseded));
        assert!(refusal(RequestState::Stale));
        assert!(refusal(RequestState::RejectedInfeasible));
        assert!(is_governor_refusal(&graph_rejection("graph_full", "x")));
        assert!(is_governor_refusal(&graph_rejection("graph_quota", "x")));
        // Volle Actor-Warteschlange und Nutzlastbudget: ohne Grund, aber Last.
        assert!(is_governor_refusal(&Status::resource_exhausted("voll")));

        for state in [
            RequestState::Failed,
            RequestState::ExecutionUnknown,
            RequestState::BackendTimeout,
            RequestState::Cancelled,
        ] {
            assert!(!refusal(state), "{state:?}");
        }
        assert!(!is_governor_refusal(&graph_rejection(
            "capture_mismatch",
            "x"
        )));
        for code in [
            Code::Unavailable,
            Code::NotFound,
            Code::InvalidArgument,
            Code::PermissionDenied,
            Code::Internal,
            Code::Unknown,
            Code::Unimplemented,
        ] {
            assert!(!is_governor_refusal(&Status::new(code, "x")), "{code:?}");
        }
        // Ein unbekannter Grund ist kein Freibrief, auch nicht mit dem Code
        // einer Backpressure.
        let mut unknown = Status::resource_exhausted("x");
        if let Ok(value) = "neu_und_unbekannt".parse() {
            unknown.metadata_mut().insert(REASON_HEADER, value);
        }
        assert!(!is_governor_refusal(&unknown));
    }

    /// Nur ein verbundener Strom ohne Fehler ist eine Messung. Ohne jede
    /// Lieferung gilt er nur dort, wo Aushungern ein Befund ist, und nur,
    /// wenn er tatsaechlich abgewiesen wurde.
    #[test]
    fn integrity_separates_measurement_from_integration_failure() {
        use super::{Integrity, StreamReport};
        let report = |connected: bool, delivered: u64, refused: u64, errors: u64| StreamReport {
            connected,
            sent: delivered.saturating_add(refused).saturating_add(errors),
            delivered,
            refused,
            errors,
            rejected: refused.saturating_add(errors),
            ..StreamReport::not_run("s")
        };
        assert_eq!(report(false, 0, 0, 0).integrity(true), Integrity::NotRun);
        assert_eq!(report(true, 50, 3, 1).integrity(true), Integrity::Errors(1));
        assert_eq!(report(true, 50, 3, 0).integrity(false), Integrity::Valid);
        assert_eq!(report(true, 0, 0, 0).integrity(true), Integrity::NoOutcome);
        assert_eq!(report(true, 0, 40, 0).integrity(true), Integrity::Valid);
        assert_eq!(
            report(true, 0, 40, 0).integrity(false),
            Integrity::NothingDelivered
        );
        assert!(!Integrity::Errors(2).is_valid());
        assert_eq!(Integrity::NotRun.label(), "not_run");
    }
}
