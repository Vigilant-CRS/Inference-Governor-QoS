//! `vig-fit` — lohnt sich der Governor auf dieser Maschine, mit diesen Modellen?
//!
//! Die Frage, die jeder Interessent zuerst stellt, und die bisher nur
//! beantworten konnte, wer sich in unsere Benchmarks einarbeitet. Das Werkzeug
//! faehrt **seine** Modelle, **seine** Vertraege und **seine** Hardware, erst
//! direkt gegen das Backend und dann ueber den Governor, an mehreren
//! Lastpunkten — und sagt in einem Satz, was dabei herauskommt.
//!
//! ## Warum das kein Benchmark ist
//!
//! Ein Benchmark will eine Zahl verteidigen. Dieses Werkzeug will eine
//! Entscheidung ermoeglichen, und die haeufigste richtige Entscheidung ist
//! „brauchst du nicht": unterhalb der Saettigung kostet der Governor nur
//! seinen Aufwand. Es ist deshalb ausdruecklich so gebaut, dass ein negatives
//! Ergebnis genauso klar herauskommt wie ein positives.
//!
//! ## Was es nicht beantwortet
//!
//! * **Nicht, ob eure Erkennung gut genug ist.** Es misst Versorgung, nicht
//!   Qualitaet.
//! * **Nicht, was auf anderer Hardware passiert.** Eine Messung gilt fuer die
//!   Maschine, auf der sie lief.
//! * **Nicht, was ueber Stunden passiert.** Dafuer gibt es den Dauerlauf.
//!
//! Gemessen wird ueber den Kopierpfad, nicht ueber Shared Memory: beide Seiten
//! zahlen denselben Transport, und das Werkzeug laeuft damit auch dort, wo es
//! kein `/dev/shm` gibt. Der Vergleich bleibt einer des Schedulings.
//!
//! ## Nur der Governor-Arm
//!
//! `VIG_FIT_ARMS=governed` laesst den direkten Arm weg. Das ist **kein**
//! Urteil ueber den Governor, sondern die Bewertung einer Einstellung:
//! `vig autotune` vergleicht damit mehrere Einstellungen desselben Governors
//! gegeneinander (ADR-0045) und braucht dafuer den direkten Weg nicht jedes
//! Mal. Das JSON sagt es in `arms`, die direkten Felder stehen auf `null`,
//! und der Satz nennt sich Abstimmungslauf — damit niemand eine Bewertung
//! ohne Vergleich als „lohnt sich" liest.
//!
//! ## Gueltigkeit
//!
//! Ein Strom meldet auch dann einen Bericht, wenn nichts ankam. Seine
//! Abdeckung ist dann 1000 ‰ und liest sich wie ein Urteil gegen den
//! Governor (Review vom 15.09., R04). Deshalb wird jede Zelle
//! (Lastpunkt × Strom × Arm) geprueft, bevor sie zaehlt:
//!
//! * **Ungueltig** ist ein Arm ohne Verbindung, mit auch nur einem Transport-,
//!   Protokoll- oder Modellfehler, ohne jede Antwort im Messfenster, und —
//!   nur direkt — ohne jede Lieferung. Das ist ein Integrationsfehler.
//! * **Gueltig** bleibt ein Governor-Arm, der unter Last absichtlich abweist
//!   (`superseded`, `stale`, `infeasible`, Backpressure), auch wenn er dabei
//!   gar nichts liefert. Das ist ein negativer Befund
//!   (`vig_bench::workload::is_governor_refusal`).
//!
//! Ein Urteil gibt es nur fuer eine vollstaendig gueltige Matrix. Sonst
//! beginnt der Satz mit „Kein Ergebnis", nennt die erste ungueltige Zelle
//! mit Grund, und das JSON steht auf `conclusive: false`.
//!
//! ## Exitcodes
//!
//! * `0` — ein Ergebnis: jede Zelle gueltig.
//! * `1` — kein Ergebnis: keine Zelle, mindestens eine ungueltige Zelle, oder
//!   das JSON liess sich nicht schreiben. Das JSON wird vorher geschrieben,
//!   damit der Grund nachlesbar bleibt; die letzte Zeile auf stderr nennt ihn.
//! * `2` — Aufruf oder Konfiguration abgelehnt.
//! * `101` — Abbruch, wenn das Backend schon vor der Messung nicht
//!   erreichbar ist oder seine Modellmetadaten fehlen.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;
use vig_bench::workload::{InputSpec, Integrity, StreamDef, StreamReport, connect, drive};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::ModelMetadataRequest;
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;

/// Die Lastpunkte, an denen gemessen wird.
///
/// Unterhalb, an und oberhalb der Saettigung. Der Knick liegt nach unseren
/// Messungen zwischen 100 und 110 % — deshalb liegen dort zwei Punkte.
const POINTS: [u64; 4] = [90, 100, 110, 125];

/// Ab hier gilt ein Strom als schlecht versorgt.
///
/// Fuenf Prozent verfehlter Takte sind fuer eine Regelung schon viel; als
/// Schwelle fuer „hier faengt es an wehzutun" ist das eine bewusst
/// konservative Wahl und keine gemessene Grenze.
const HURTS_PERMILLE: u64 = 50;

/// Welche Arme gefahren werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arms {
    /// Direkt und ueber den Governor — die Frage „lohnt es sich?".
    Both,
    /// Nur ueber den Governor — die Bewertung einer Einstellung, ohne
    /// Vergleich.
    Governed,
}

impl Arms {
    /// Aus `VIG_FIT_ARMS`; `None` bei einem unbekannten Wert.
    fn from_env() -> Option<Self> {
        match std::env::var("VIG_FIT_ARMS").ok().as_deref() {
            None | Some("both") => Some(Self::Both),
            Some("governed") => Some(Self::Governed),
            Some(_) => None,
        }
    }

    /// Das Wort im JSON.
    const fn label(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Governed => "governed",
        }
    }
}

/// Eine Zeile des Ergebnisses.
#[derive(Debug, Clone)]
struct Row {
    load: u64,
    stream: String,
    protected: bool,
    /// Unabgedeckte Abtastungen aus Verbrauchersicht, direkt am Backend;
    /// `None`, wenn der direkte Arm nicht gefahren wurde.
    direct: Option<u64>,
    /// Dasselbe ueber den Governor.
    governed: u64,
    direct_gap_ms: Option<u64>,
    governed_gap_ms: u64,
    /// Abtastungen (Takte) hinter dem Promillewert; `None` ohne direkten Arm.
    ///
    /// Ohne sie ist 37 ‰ nicht von 37 ‰ zu unterscheiden: einer von 27
    /// Takten oder 7 von 200. `vig autotune` bemisst daran seine Rauschschwelle.
    direct_samples: Option<u64>,
    governed_samples: u64,
    /// Was die Requests des direkten Arms erlebt haben; `None` ohne ihn.
    direct_tally: Option<Tally>,
    /// Dasselbe ueber den Governor.
    governed_tally: Tally,
}

/// Was die Requests eines Arms in einer Zelle erlebt haben.
///
/// Die Abdeckung allein unterscheidet nicht zwischen „der Governor hat
/// abgewiesen" und „es kam nie eine Verbindung zustande". Beides sind 1000 ‰.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tally {
    sent: u64,
    delivered: u64,
    /// Absichtlich abgewiesen: Aushungern unter Last, ein gueltiger Befund.
    refused: u64,
    /// Transport-, Protokoll- oder Modellfehler: ein Integrationsfehler.
    errors: u64,
    integrity: Integrity,
}

impl Tally {
    /// `may_starve` wie bei [`StreamReport::integrity`]: nur der Governor-Arm
    /// darf ohne Lieferung gueltig sein.
    const fn of(report: &StreamReport, may_starve: bool) -> Self {
        Self {
            sent: report.sent,
            delivered: report.delivered,
            refused: report.refused,
            errors: report.errors,
            integrity: report.integrity(may_starve),
        }
    }
}

/// Welcher Arm einer Zelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Direct,
    Governed,
}

impl Side {
    const fn de(self) -> &'static str {
        match self {
            Self::Direct => "direkt",
            Self::Governed => "Governor",
        }
    }

    const fn en(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Governed => "governor",
        }
    }
}

impl Row {
    /// Die Arme dieser Zelle, die gefahren wurden.
    fn tallies(&self) -> impl Iterator<Item = (Side, Tally)> {
        self.direct_tally
            .map(|tally| (Side::Direct, tally))
            .into_iter()
            .chain([(Side::Governed, self.governed_tally)])
    }

    /// Der erste ungueltige Arm; `None`, wenn die Zelle eine Messung ist.
    fn invalid_arm(&self) -> Option<(Side, Integrity)> {
        self.tallies()
            .find(|(_, tally)| !tally.integrity.is_valid())
            .map(|(side, tally)| (side, tally.integrity))
    }

    fn is_valid(&self) -> bool {
        self.invalid_arm().is_none()
    }
}

/// Die Zeile einer Zelle aus den Berichten beider Arme.
///
/// `direct` ist `None`, wenn der direkte Arm nicht gefahren wurde.
fn row_of(
    load: u64,
    stream: &str,
    protected: bool,
    direct: Option<&StreamReport>,
    governed: &StreamReport,
) -> Row {
    Row {
        load,
        stream: stream.to_owned(),
        protected,
        direct: direct.map(|a| a.coverage.consumer_uncovered_permille()),
        governed: governed.coverage.consumer_uncovered_permille(),
        direct_gap_ms: direct.map(|a| a.coverage.longest_gap_ns / 1_000_000),
        governed_gap_ms: governed.coverage.longest_gap_ns / 1_000_000,
        direct_samples: direct.map(|a| a.coverage.total),
        governed_samples: governed.coverage.total,
        direct_tally: direct.map(|a| Tally::of(a, false)),
        governed_tally: Tally::of(governed, true),
    }
}

/// Der Bericht eines Stroms, oder `absent`, wenn es keinen gibt.
fn report_of<'a>(
    reports: &'a [StreamReport],
    stream: &str,
    absent: &'a StreamReport,
) -> &'a StreamReport {
    reports.iter().find(|r| r.name == stream).unwrap_or(absent)
}

/// Ein Governor im Prozess, fuer genau einen Lastpunkt.
struct RunningGateway {
    address: String,
    /// Haelt den Scheduler am Leben, bis der Server beendet ist.
    _handle: vig_gateway::Handle,
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
}

impl RunningGateway {
    /// Startet Scheduler und gRPC-Server auf einem freien Port.
    async fn start(resolved: Arc<vig_config::schema::Resolved>) -> Self {
        let triton = Arc::new(vig_backend_triton::TritonClient::new(
            &resolved.backend_endpoint,
        ));
        let clock = MonotonicClock::start();
        let handle =
            actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
        let service = GatewayService::new(resolved, triton, handle.clone(), clock);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Port");
        let address = listener.local_addr().expect("Adresse").to_string();
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let stream = vig_bench::incoming(listener);
            let _ = tonic::transport::Server::builder()
                .initial_stream_window_size(vig_backend_triton::STREAM_WINDOW_BYTES)
                .initial_connection_window_size(vig_backend_triton::CONNECTION_WINDOW_BYTES)
                .add_service(
                    GrpcInferenceServiceServer::new(service)
                        .max_decoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
                        .max_encoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES),
                )
                .serve_with_incoming_shutdown(stream, async {
                    let _ = stopped.await;
                })
                .await;
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        Self {
            address,
            _handle: handle,
            shutdown,
            server,
        }
    }

    /// Beendet den Server und wartet darauf.
    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = self.server.await;
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("Tokio-Runtime");
    runtime.block_on(run());
}

async fn run() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!(
            "Aufruf: vig-fit <vig.yaml>\n\n\
             Faehrt die Modelle dieser Konfiguration erst direkt gegen das\n\
             Backend und dann ueber den Governor, an mehreren Lastpunkten.\n\n\
             Umgebung:\n  \
             VIG_FIT_SECONDS  Messdauer je Arm und Punkt (Vorgabe: {} Takte des\n                   \
             langsamsten geschuetzten Stroms, {} bis {} s)\n  \
             VIG_FIT_POINTS   Lastpunkte in Prozent (Vorgabe {POINTS:?})\n  \
             VIG_FIT_ARMS     both (Vorgabe) oder governed: nur der Governor-Arm,\n                   \
             eine Bewertung ohne Vergleich (vig autotune, Schritt tune)\n  \
             VIG_FIT_JSON     Ergebnis zusaetzlich als JSON in diese Datei",
            vig_config::window::CYCLES,
            vig_config::window::FLOOR_SECONDS,
            vig_config::window::CAP_SECONDS
        );
        std::process::exit(2);
    };
    let Some(arms) = Arms::from_env() else {
        eprintln!("VIG_FIT_ARMS kennt nur `both` und `governed`.");
        std::process::exit(2);
    };

    let text = std::fs::read_to_string(&path).expect("Konfiguration lesbar");
    let config = Config::from_yaml(&text).expect("Konfiguration gueltig");
    let findings = config.diagnose();
    if !findings.is_empty() {
        eprintln!("Die Konfiguration hat offene Befunde — erst `vig doctor` gruen bekommen:");
        for finding in &findings {
            eprintln!("  {finding}");
        }
        std::process::exit(2);
    }
    let base = Arc::new(config.resolve().expect("aufloesbar"));

    let points = std::env::var("VIG_FIT_POINTS").ok().map_or_else(
        || POINTS.to_vec(),
        |raw| {
            raw.split(',')
                .filter_map(|p| p.trim().parse::<u64>().ok())
                .collect()
        },
    );
    // Das Fenster in Takten, nicht in Sekunden (`vig_config::window`): zehn
    // Sekunden sind bei einer 370-ms-Periode 27 Takte, und ein verfehlter Takt
    // ist dann schon 37 ‰.
    let lowest = points.iter().copied().min().unwrap_or(100);
    let seconds = env_u64("VIG_FIT_SECONDS")
        .unwrap_or_else(|| vig_config::window::seconds(&config, lowest, false));
    let slowest = vig_config::window::slowest_protected_period_ms(&config);
    let duration = Duration::from_secs(seconds);

    match arms {
        Arms::Both => println!("vig-fit: lohnt sich der Governor hier?"),
        Arms::Governed => println!(
            "vig-fit: Bewertung einer Governor-Einstellung (nur Governor-Arm, kein Vergleich)"
        ),
    }
    println!(
        "Backend {} · {} Modelle · geschuetzte serialisierte Auslastung {} % bei den \
         Vertragsperioden",
        base.backend_endpoint,
        base.model_names.len(),
        base.protected_utilization_permille() / 10
    );
    let cycles = slowest.map_or_else(String::new, |period| {
        format!(
            " ({} Takte des langsamsten geschuetzten Stroms bei {lowest} %)",
            vig_config::window::cycles_in(seconds, period * 100 / lowest.max(1))
        )
    });
    println!(
        "Lastpunkte {points:?} % · {seconds} s je Arm und Punkt{cycles} · Datenpfad Kopie im \
         Request\n"
    );

    // Fremde Rechenzeit statt `loadavg`: Ein Pixel 2 steht im Leerlauf bei
    // 3,5 und verbraucht dabei 0,04 Kerne, und der alte Hinweis nannte jede
    // Messung auf einem Telefon unruhig (15.09.2026). Gemessen wird vor dem
    // Lauf, solange das Backend ruht.
    let load_before = foreign_load().await;
    if load_before.is_none_or(|centi| centi >= FOREIGN_CORES_CENTI) {
        println!(
            "  HINWEIS Fremde Rechenzeit vor dem Lauf: {}. Auf einer unruhigen \
             Maschine\n          misst dieses Werkzeug teilweise die andere Arbeit. \
             Fuer eine belastbare\n          Zahl erst die Maschine frei machen.\n",
            cores(load_before)
        );
    }

    // --- Eingaben vorbereiten: eine Nullnutzlast je Modell, aus den Metadaten
    let client = vig_backend_triton::TritonClient::new(&base.backend_endpoint);
    let health = client.health().await.expect("Backend erreichbar");
    assert!(health.ready, "das Backend meldet sich als nicht bereit");

    let mut inputs: HashMap<String, InputSpec> = HashMap::new();
    let mut physical: HashMap<String, String> = HashMap::new();
    for (index, logical) in base.model_names.iter().enumerate() {
        let model = vig_core::ModelIdx(u16::try_from(index).unwrap_or(0));
        let name = base
            .backend_model(model, 0)
            .expect("Variante vorhanden")
            .to_owned();
        let endpoint = base.endpoint_of(model).to_owned();
        let per_model = vig_backend_triton::TritonClient::new(endpoint.as_str());
        let metadata = per_model
            .raw()
            .await
            .expect("Backend erreichbar")
            .model_metadata(ModelMetadataRequest {
                name: name.clone(),
                version: String::new(),
            })
            .await
            .expect("Modellmetadaten")
            .into_inner();
        let input = metadata.inputs.first().expect("Modell hat eine Eingabe");
        let shape: Vec<i64> = input
            .shape
            .iter()
            .enumerate()
            .map(|(position, d)| if *d < 0 && position == 0 { 1 } else { *d })
            .collect();
        let elements: i64 = shape.iter().copied().product();
        let byte_size = u64::try_from(elements).unwrap_or(0) * element_size(&input.datatype);
        inputs.insert(
            logical.clone(),
            InputSpec {
                name: input.name.clone(),
                datatype: input.datatype.clone(),
                shape,
                region: None,
                byte_size,
                payload: None,
            },
        );
        physical.insert(logical.clone(), name);
    }

    // --- Messen ------------------------------------------------------------
    let mut rows: Vec<Row> = Vec::new();
    for load in &points {
        let scaled = scale(&config, *load);
        let resolved = Arc::new(
            scaled
                .resolve()
                .expect("skalierte Konfiguration aufloesbar"),
        );
        let defs = |use_logical: bool| -> Vec<StreamDef> {
            resolved
                .model_names
                .iter()
                .enumerate()
                .filter_map(|(index, logical)| {
                    let contract = resolved.contracts.get(index)?;
                    Some(StreamDef {
                        text: None,
                        name: Box::leak(logical.clone().into_boxed_str()),
                        model: Box::leak(
                            if use_logical {
                                logical.clone()
                            } else {
                                physical.get(logical).cloned()?
                            }
                            .into_boxed_str(),
                        ),
                        period: contract.period.map_or(Duration::from_millis(100), to_std),
                        max_age: contract.max_age.map_or(Duration::from_secs(1), to_std),
                        in_flight_cap: 4,
                        input: inputs.get(logical).cloned(),
                        pump: false,
                        burst: None,
                    })
                })
                .collect()
        };

        print!("  {load:>3} % Last ");
        let direct = match arms {
            Arms::Both => {
                print!(" direkt …");
                Some(drive(&base.backend_endpoint, &defs(false), duration, false).await)
            }
            Arms::Governed => None,
        };

        // Der Governor bekommt fuer jeden Punkt seine eigene Instanz: die
        // Vertraege unterscheiden sich, und ein Scheduler, der mit den
        // Perioden des vorigen Punktes plant, misst etwas anderes als das,
        // was hier steht.
        let gateway = RunningGateway::start(Arc::clone(&resolved)).await;
        let _warm = connect(&gateway.address).await;

        print!(" Governor …");
        let governed = drive(&gateway.address, &defs(true), duration, true).await;
        gateway.stop().await;
        println!(" fertig");

        // Jede Zelle bekommt eine Zeile, auch ohne Bericht oder ohne
        // Lieferung. Vorher entschied das Vorhandensein des Berichts, und
        // `drive` liefert ihn immer — ein geschlossener Port wurde so zu
        // 1000 ‰ gegen den Governor (R04). Ob die Zeile zaehlt, entscheidet
        // jetzt `Row::is_valid`; eine fehlende Zelle ist ungueltig, nicht
        // stillschweigend weg.
        let absent = StreamReport::not_run("");
        for logical in &resolved.model_names {
            let protected = config
                .models
                .get(logical)
                .is_some_and(|m| m.class == "protected");
            rows.push(row_of(
                *load,
                logical,
                protected,
                direct
                    .as_deref()
                    .map(|reports| report_of(reports, logical, &absent)),
                report_of(&governed, logical, &absent),
            ));
        }
    }

    // --- Bericht -----------------------------------------------------------
    println!("\n  Unabgedeckte Abtastungen aus Verbrauchersicht, je Promille.\n");
    println!(
        "  Last | Strom            | Klasse      | direkt | Governor | laengste Luecke d/G \
         | G geliefert/abgewiesen/Fehler"
    );
    println!(
        "  -----|------------------|-------------|--------|----------|---------------------\
         |------------------------------"
    );
    let dash = || "—".to_owned();
    for row in &rows {
        println!(
            "  {:>3} % | {:<16} | {:<11} | {:>5} ‰ | {:>6} ‰ | {:>6} / {:<6} ms | {}/{}/{}",
            row.load,
            row.stream,
            if row.protected {
                "geschuetzt"
            } else {
                "nachrangig"
            },
            row.direct.map_or_else(dash, |d| d.to_string()),
            row.governed,
            row.direct_gap_ms.map_or_else(dash, |d| d.to_string()),
            row.governed_gap_ms,
            row.governed_tally.delivered,
            row.governed_tally.refused,
            row.governed_tally.errors,
        );
    }
    // Ungueltige Arme einzeln, mit ihren Zaehlern: wer das liest, soll den
    // Integrationsfehler finden, nicht die Promille deuten.
    let mut first_invalid = true;
    for row in &rows {
        for (side, tally) in row.tallies().filter(|(_, t)| !t.integrity.is_valid()) {
            if first_invalid {
                println!();
                first_invalid = false;
            }
            println!(
                "  UNGUELTIG {} % · {} · {}: {} (gesendet {}, geliefert {}, abgewiesen {}, \
                 Fehler {})",
                row.load,
                row.stream,
                side.de(),
                reason_de(tally.integrity),
                tally.sent,
                tally.delivered,
                tally.refused,
                tally.errors
            );
        }
    }

    let load_after = foreign_load().await;
    println!("\n{}", verdict(&rows, &points, arms));
    println!(
        "\n  Fremde Rechenzeit {} vor, {} nach dem Lauf. Gemessen wurde die\n  \
         Versorgung, nicht die Erkennungsqualitaet; die Zahlen gelten fuer diese\n  \
         Maschine und diese Vertraege.",
        cores(load_before),
        cores(load_after)
    );

    if let Ok(path) = std::env::var("VIG_FIT_JSON") {
        let json = as_json(&rows, &points, seconds, load_before, load_after, arms);
        match std::fs::write(&path, json) {
            Ok(()) => println!("  JSON: {path}"),
            Err(error) => {
                eprintln!("  JSON nicht schreibbar ({error})");
                std::process::exit(1);
            }
        }
    }
    // Ohne gueltige Matrix gibt es kein Urteil, und ein Aufrufer darf das
    // nicht an einem Exitcode 0 vorbeilesen. Die letzte Zeile auf stderr nennt
    // den Grund; `vig autotune` gibt genau diese Zeile weiter.
    if let Some(reason) = inconclusive(&rows) {
        match reason {
            Inconclusive::NoCells => {
                eprintln!("vig-fit: kein Ergebnis, kein Strom hat geliefert");
            }
            Inconclusive::InvalidCells { invalid, total, .. } => eprintln!(
                "vig-fit: kein Ergebnis, {invalid} von {total} Messzellen ungueltig \
                 (Integrationsfehler, siehe UNGUELTIG)"
            ),
        }
        std::process::exit(1);
    }
}

/// Das Urteil in einem Satz — auch, wenn es negativ ausfaellt.
fn verdict(rows: &[Row], points: &[u64], arms: Arms) -> String {
    match inconclusive(rows) {
        None => {}
        Some(Inconclusive::NoCells) => {
            return "  Kein Ergebnis: kein Strom hat geliefert. Laeuft das Backend, und \
                    passen die Modellnamen?"
                .to_owned();
        }
        Some(Inconclusive::InvalidCells {
            invalid,
            total,
            load,
            stream,
            side,
            integrity,
        }) => {
            return format!(
                "  Kein Ergebnis: Der Lauf ist nicht auswertbar. {invalid} von {total} \
                 Messzellen\n  sind ungueltig, zuerst bei {load} % Last, Strom {stream}, \
                 Arm {}: {}.\n  Das ist ein Integrationsfehler und kein Urteil ueber den \
                 Governor. Erst Backend,\n  Governor und Modellnamen pruefen, dann neu \
                 messen.",
                side.de(),
                reason_de(integrity)
            );
        }
    }
    if arms == Arms::Governed {
        let tuning = governed_only(rows);
        return format!(
            "  BEWERTUNG Abstimmungslauf ohne direkten Vergleich: Unter dem Governor\n  \
             verfehlen die geschuetzten Stroeme hoechstens {} ‰ (bei {} % Last), die\n  \
             nachrangigen hoechstens {} ‰. Ob sich der Governor hier lohnt, sagt nur\n  \
             ein Lauf mit beiden Armen.",
            tuning.protected_worst, tuning.protected_load, tuning.background_worst
        );
    }
    // Der erste Punkt, an dem der direkte Weg den geschuetzten Strom verliert.
    let Some(finding) = finding(rows, points) else {
        return "  Kein Ergebnis: kein Strom hat geliefert. Laeuft das Backend, und \
                passen die Modellnamen?"
            .to_owned();
    };
    let Finding::Breaks {
        load,
        direct,
        governed,
        price_direct,
        price_governed,
    } = finding
    else {
        let highest = points.iter().copied().max().unwrap_or(0);
        return format!(
            "  URTEIL Bis {highest} % Angebotslast verliert auch der direkte Weg nichts.\n  \
             Auf dieser Maschine, mit diesen Modellen und Vertraegen lohnt sich der\n  \
             Governor nicht — er kostet dann nur seinen eigenen Aufwand. Interessant\n  \
             wird es erst, wenn die Last ueber die Saettigung geht oder ein langer,\n  \
             nicht unterbrechbarer Auftrag dazwischenkommt."
        );
    };

    let mut out = if governed >= direct {
        format!(
            "  URTEIL Ab {load} % Last verliert der direkte Weg {direct} ‰ der Takte des\n  \
             geschuetzten Stroms — der Governor {governed} ‰, also **nicht weniger**.\n  \
             Das ist ein Ergebnis gegen uns: auf dieser Last bringt er hier nichts."
        )
    } else {
        format!(
            "  URTEIL Ab {load} % Last verfehlt der direkte Weg {direct} ‰ der Takte des\n  \
             geschuetzten Stroms, der Governor {governed} ‰."
        )
    };
    if price_direct > 0 || price_governed > 0 {
        let _ = write!(
            out,
            "\n  Der Preis steht daneben: die nachrangigen Stroeme verlieren direkt\n  \
             {price_direct} ‰, unter dem Governor {price_governed} ‰. Wer sie braucht, muss das\n  \
             gegeneinander abwaegen."
        );
    }
    out
}

/// Skaliert die Perioden auf einen Lastpunkt.
///
/// Nur die Periode, nicht die Frist: Hoehere Last heisst, dass die Quelle
/// haeufiger liefert. Was ein einzelnes Ergebnis wert ist und wann es zu spaet
/// kommt, aendert sich dadurch nicht.
fn scale(config: &Config, load_percent: u64) -> Config {
    let mut scaled = config.clone();
    for model in scaled.models.values_mut() {
        if let Some(period) = model.contract.period_ms {
            model.contract.period_ms = Some((period * 100 / load_percent.max(1)).max(1));
        }
    }
    scaled
}

/// Das Ergebnis als JSON.
///
/// Neu seit dem Review vom 15.09. (R04), alle bisherigen Felder bleiben:
///
/// * je Zelle `valid` — `true` nur, wenn jeder gefahrene Arm eine Messung
///   ist. Eine ungueltige Zelle ist kein Ergebnis; ihre Promille bedeuten
///   nichts.
/// * je Zelle `direct_state` / `governed_state` — `valid`, `not_run` (keine
///   Verbindung, kein Bericht), `errors` (Transport-, Protokoll- oder
///   Modellfehler), `no_outcome` (weder Lieferung noch Ablehnung),
///   `nothing_delivered` (direkt ohne jede Lieferung). `direct_state` ist
///   `null` ohne direkten Arm.
/// * je Zelle `{direct,governed}_{sent,delivered,refused,errors}` —
///   gesendete Requests, Lieferungen, absichtliche Ablehnungen des Governors
///   und Fehler; die direkten `null` ohne direkten Arm.
/// * `invalid_cells` — Zahl der ungueltigen Zellen.
/// * `inconclusive_reason` — `null` bei einem Ergebnis, sonst `no_cells`
///   oder `invalid_cells`.
/// * `conclusive` — `true` nur bei mindestens einer Zelle und keiner
///   ungueltigen. Vorher genuegte eine Zeile.
fn as_json(
    rows: &[Row],
    points: &[u64],
    seconds: u64,
    before: Option<u64>,
    after: Option<u64>,
    arms: Arms,
) -> String {
    let cells: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let mut cell = serde_json::json!({
                "load_percent": r.load,
                "stream": r.stream,
                "protected": r.protected,
                "direct_uncovered_permille": r.direct,
                "governed_uncovered_permille": r.governed,
                "direct_longest_gap_ms": r.direct_gap_ms,
                "governed_longest_gap_ms": r.governed_gap_ms,
                // Takte hinter dem Promillewert; `vig autotune` rechnet damit
                // seine Rauschschwelle.
                "direct_samples": r.direct_samples,
                "governed_samples": r.governed_samples,
            });
            // Einzeln eingefuegt statt im Makro: `json!` stoesst bei vielen
            // Feldern an die Rekursionsgrenze.
            let direct = r.direct_tally;
            let governed = r.governed_tally;
            if let Some(map) = cell.as_object_mut() {
                let fields = [
                    ("valid", serde_json::json!(r.is_valid())),
                    (
                        "direct_state",
                        serde_json::json!(direct.map(|t| t.integrity.label())),
                    ),
                    (
                        "governed_state",
                        serde_json::json!(governed.integrity.label()),
                    ),
                    ("direct_sent", serde_json::json!(direct.map(|t| t.sent))),
                    (
                        "direct_delivered",
                        serde_json::json!(direct.map(|t| t.delivered)),
                    ),
                    (
                        "direct_refused",
                        serde_json::json!(direct.map(|t| t.refused)),
                    ),
                    ("direct_errors", serde_json::json!(direct.map(|t| t.errors))),
                    ("governed_sent", serde_json::json!(governed.sent)),
                    ("governed_delivered", serde_json::json!(governed.delivered)),
                    ("governed_refused", serde_json::json!(governed.refused)),
                    ("governed_errors", serde_json::json!(governed.errors)),
                ];
                for (key, value) in fields {
                    map.insert(key.to_owned(), value);
                }
            }
            cell
        })
        .collect();
    let inconclusive = inconclusive(rows);
    serde_json::json!({
        "tool": "vig-fit",
        // `governed`: nur der Governor-Arm lief. Dann sind die direkten
        // Felder `null` — nicht gemessen, nicht null Promille.
        "arms": arms.label(),
        "seconds_per_arm": seconds,
        "load_points_percent": points,
        // Hundertstel Kerne fremder Rechenzeit; `null`, wo nicht beobachtbar.
        "foreign_cores_centi_before": before,
        "foreign_cores_centi_after": after,
        "view": "consumer",
        "cells": cells,
        "verdict": verdict(rows, points, arms).trim().to_owned(),
        // Dieselbe Aussage fuer den englischen Qualifikationsbericht von
        // `vig autotune`. Vorher stand dort der deutsche Satz mitten in einem
        // englischen Dokument — ein Urteil, das der Leser nicht lesen kann,
        // ist keines (validierung-autotune.md, Befund 5).
        "verdict_en": verdict_en(rows, points, arms),
        // Ohne gueltige Matrix gibt es kein Urteil. Das Feld sagt es
        // maschinenlesbar, damit niemand „kein Strom hat geliefert" oder eine
        // Zelle voller Verbindungsfehler als Ergebnis uebernimmt.
        "conclusive": inconclusive.is_none(),
        "inconclusive_reason": inconclusive.as_ref().map(Inconclusive::label),
        "invalid_cells": rows.iter().filter(|r| !r.is_valid()).count(),
    })
    .to_string()
}

/// Das Urteil auf Englisch, mit denselben Zahlen wie [`verdict`].
fn verdict_en(rows: &[Row], points: &[u64], arms: Arms) -> String {
    match inconclusive(rows) {
        None => {}
        Some(Inconclusive::NoCells) => {
            return "No result: no stream delivered. Is the backend running, and do the model \
                    names match?"
                .to_owned();
        }
        Some(Inconclusive::InvalidCells {
            invalid,
            total,
            load,
            stream,
            side,
            integrity,
        }) => {
            return format!(
                "No result: the run is inconclusive. {invalid} of {total} cells are invalid, \
                 first at {load} % load, stream {stream}, {} arm: {}. That is an integration \
                 failure, not a verdict on the governor. Check backend, governor and model \
                 names first, then measure again.",
                side.en(),
                reason_en(integrity)
            );
        }
    }
    if arms == Arms::Governed {
        let tuning = governed_only(rows);
        return format!(
            "Tuning evaluation without a direct comparison: under the governor the protected \
             streams miss at worst {} ‰ (at {} % load), the lower-priority streams at worst \
             {} ‰. Whether the governor pays off here only a run with both arms can say.",
            tuning.protected_worst, tuning.protected_load, tuning.background_worst
        );
    }
    let Some(finding) = finding(rows, points) else {
        return "No result: no stream delivered. Is the backend running, and do the model \
                names match?"
            .to_owned();
    };
    match finding {
        Finding::NoGain { highest } => format!(
            "Up to {highest} % offered load the direct path loses nothing either. On this \
             machine, with these models and contracts, the governor is not worth it — it only \
             costs its own overhead. It becomes interesting once load goes past saturation or \
             a long, non-interruptible job gets in the way."
        ),
        Finding::Breaks {
            load,
            direct,
            governed,
            price_direct,
            price_governed,
        } => {
            let mut out = if governed >= direct {
                format!(
                    "From {load} % load the direct path misses {direct} ‰ of the protected \
                     stream's cycles — the governor {governed} ‰, so no fewer. That is a result \
                     against us: on this load it brings nothing here."
                )
            } else {
                format!(
                    "From {load} % load the direct path misses {direct} ‰ of the protected \
                     stream's cycles, the governor {governed} ‰."
                )
            };
            if price_direct > 0 || price_governed > 0 {
                let _ = write!(
                    out,
                    " The price is right beside it: the lower-priority streams miss \
                     {price_direct} ‰ directly and {price_governed} ‰ under the governor. \
                     Whoever needs them has to weigh one against the other."
                );
            }
            out
        }
    }
}

/// Was das Urteil feststellt, ohne Sprache.
enum Finding {
    /// Auch der direkte Weg verliert bis zum hoechsten Lastpunkt nichts.
    NoGain { highest: u64 },
    /// Ab `load` verliert der direkte Weg den geschuetzten Strom.
    Breaks {
        load: u64,
        direct: u64,
        governed: u64,
        price_direct: u64,
        price_governed: u64,
    },
}

/// Die Feststellung hinter beiden Fassungen des Urteils; `None` ohne Lieferung.
fn finding(rows: &[Row], points: &[u64]) -> Option<Finding> {
    if rows.is_empty() {
        return None;
    }
    let worst = |load: u64, protected: bool, governed: bool| -> u64 {
        rows.iter()
            .filter(|r| r.load == load && r.protected == protected)
            .filter_map(|r| if governed { Some(r.governed) } else { r.direct })
            .max()
            .unwrap_or(0)
    };
    let highest = points.iter().copied().max().unwrap_or(0);
    let Some(load) = points
        .iter()
        .copied()
        .find(|load| worst(*load, true, false) > HURTS_PERMILLE)
    else {
        return Some(Finding::NoGain { highest });
    };
    Some(Finding::Breaks {
        load,
        direct: worst(load, true, false),
        governed: worst(load, true, true),
        price_direct: worst(load, false, false),
        price_governed: worst(load, false, true),
    })
}

/// Was eine Bewertung ohne direkten Arm feststellt.
struct GovernedOnly {
    /// Der schlechteste geschuetzte Strom ueber alle Lastpunkte.
    protected_worst: u64,
    /// Der Lastpunkt, an dem er auftrat (der erste bei Gleichstand).
    protected_load: u64,
    /// Der schlechteste nachrangige Strom ueber alle Lastpunkte.
    background_worst: u64,
}

/// Die Feststellung hinter dem Satz eines Abstimmungslaufs.
///
/// Bewusst nur Maxima und kein Urteil: Ohne direkten Arm gibt es nichts, wogegen
/// ein „lohnt sich" stehen koennte. Die Zielgroesse, nach der `vig autotune`
/// Einstellungen vergleicht, rechnet `autotune` selbst aus den Zellen.
fn governed_only(rows: &[Row]) -> GovernedOnly {
    let mut out = GovernedOnly {
        protected_worst: 0,
        protected_load: rows.first().map_or(0, |r| r.load),
        background_worst: 0,
    };
    for row in rows {
        if row.protected {
            if row.governed > out.protected_worst {
                out.protected_worst = row.governed;
                out.protected_load = row.load;
            }
        } else {
            out.background_worst = out.background_worst.max(row.governed);
        }
    }
    out
}

/// Warum ein Lauf kein Ergebnis hat.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Inconclusive {
    /// Keine einzige Zelle.
    NoCells,
    /// So viele Zellen sind ungueltig; die erste mit Arm und Grund.
    InvalidCells {
        invalid: usize,
        total: usize,
        load: u64,
        stream: String,
        side: Side,
        integrity: Integrity,
    },
}

impl Inconclusive {
    /// Das Wort im JSON.
    const fn label(&self) -> &'static str {
        match self {
            Self::NoCells => "no_cells",
            Self::InvalidCells { .. } => "invalid_cells",
        }
    }
}

/// `None`, wenn die Matrix ein Ergebnis traegt: mindestens eine Zelle, und
/// jede gueltig.
///
/// Bewusst alles oder nichts. Ein Urteil aus den gueltigen Zellen allein
/// liesse eine fehlende Zelle genau dort, wo der direkte Weg bricht, als
/// „lohnt sich nicht" erscheinen.
fn inconclusive(rows: &[Row]) -> Option<Inconclusive> {
    if rows.is_empty() {
        return Some(Inconclusive::NoCells);
    }
    let (first, (side, integrity)) = rows
        .iter()
        .find_map(|row| row.invalid_arm().map(|arm| (row, arm)))?;
    Some(Inconclusive::InvalidCells {
        invalid: rows.iter().filter(|r| !r.is_valid()).count(),
        total: rows.len(),
        load: first.load,
        stream: first.stream.clone(),
        side,
        integrity,
    })
}

fn reason_de(integrity: Integrity) -> String {
    match integrity {
        Integrity::Valid => "gueltig".to_owned(),
        Integrity::NotRun => "keine Verbindung zum Ziel".to_owned(),
        Integrity::Errors(count) => format!("{count} Transport-, Protokoll- oder Modellfehler"),
        Integrity::NoOutcome => {
            "weder eine Lieferung noch eine Ablehnung im Messfenster".to_owned()
        }
        Integrity::NothingDelivered => "keine einzige Lieferung".to_owned(),
    }
}

fn reason_en(integrity: Integrity) -> String {
    match integrity {
        Integrity::Valid => "valid".to_owned(),
        Integrity::NotRun => "no connection to the target".to_owned(),
        Integrity::Errors(count) => format!("{count} transport, protocol or model errors"),
        Integrity::NoOutcome => "neither a delivery nor a refusal within the window".to_owned(),
        Integrity::NothingDelivered => "not a single delivery".to_owned(),
    }
}

fn element_size(datatype: &str) -> u64 {
    match datatype {
        "FP32" | "INT32" | "UINT32" => 4,
        "FP16" | "INT16" | "UINT16" => 2,
        "FP64" | "INT64" | "UINT64" => 8,
        _ => 1,
    }
}

fn to_std(d: vig_core::Duration) -> Duration {
    Duration::from_nanos(d.as_nanos())
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

/// Ab so viel fremder Rechenzeit gilt die Maschine als unruhig, in Hundertstel
/// Kernen — dieselbe Grenze wie in `vig autotune`.
const FOREIGN_CORES_CENTI: u64 = 100;

/// Fremde Rechenzeit ueber drei Sekunden, in Hundertstel Kernen; `None`, wo
/// sie nicht beobachtbar ist.
async fn foreign_load() -> Option<u64> {
    let before = vig_platform::cpu::CpuSample::now()?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    before.foreign_cores_centi(vig_platform::cpu::CpuSample::now()?)
}

/// `1.23 Kerne` oder `nicht beobachtbar`.
fn cores(centi: Option<u64>) -> String {
    centi.map_or_else(
        || "nicht beobachtbar".to_owned(),
        |c| {
            format!(
                "{}.{:02} Kerne",
                c.checked_div(100).unwrap_or(0),
                c.checked_rem(100).unwrap_or(0)
            )
        },
    )
}

#[cfg(test)]
// `parsed["arms"]` ist hier die Zusicherung, dass das Feld existiert.
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::{
        Arms, Integrity, Row, RunningGateway, Tally, as_json, report_of, row_of, verdict,
        verdict_en,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use vig_bench::backend::{self, Backend};
    use vig_bench::workload::{StreamDef, StreamReport, drive};
    use vig_sim::workload::RuntimeDistribution;

    /// Ein Arm, der alles geliefert hat.
    const HEALTHY: Tally = Tally {
        sent: 200,
        delivered: 200,
        refused: 0,
        errors: 0,
        integrity: Integrity::Valid,
    };

    fn row(load: u64, protected: bool, direct: u64, governed: u64) -> Row {
        Row {
            load,
            stream: if protected { "det" } else { "bg" }.to_owned(),
            protected,
            direct: Some(direct),
            governed,
            direct_gap_ms: Some(0),
            governed_gap_ms: 0,
            direct_samples: Some(200),
            governed_samples: 200,
            direct_tally: Some(HEALTHY),
            governed_tally: HEALTHY,
        }
    }

    /// Unterhalb der Saettigung ist „brauchst du nicht" die richtige Antwort,
    /// und sie muss genauso deutlich dastehen wie ein Erfolg.
    #[test]
    fn below_saturation_the_verdict_says_no() {
        let rows = vec![row(90, true, 0, 0), row(100, true, 3, 2)];
        let text = verdict(&rows, &[90, 100], Arms::Both);
        assert!(text.contains("lohnt sich der\n  Governor nicht"), "{text}");
    }

    /// Ohne direkten Arm stehen die direkten Felder auf `null`, und der Satz
    /// nennt sich Bewertung — nie ein Urteil, das es ohne Vergleich nicht gibt.
    #[test]
    fn a_governed_only_run_is_a_tuning_evaluation_not_a_verdict() {
        let governed_only = |load: u64, protected: bool, governed: u64| Row {
            direct: None,
            direct_gap_ms: None,
            direct_samples: None,
            direct_tally: None,
            ..row(load, protected, 0, governed)
        };
        let rows = vec![
            governed_only(110, true, 12),
            governed_only(110, false, 40),
            governed_only(125, true, 30),
            governed_only(125, false, 90),
        ];
        let text = as_json(&rows, &[110, 125], 10, Some(3), None, Arms::Governed);
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["arms"], "governed");
        assert_eq!(parsed["conclusive"], true);
        let cells = parsed["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 4);
        for cell in cells {
            assert!(cell["direct_uncovered_permille"].is_null(), "{cell}");
            assert!(cell["direct_longest_gap_ms"].is_null(), "{cell}");
            assert!(cell["governed_uncovered_permille"].is_u64(), "{cell}");
            assert!(cell["direct_samples"].is_null(), "{cell}");
            assert_eq!(cell["governed_samples"], 200, "{cell}");
        }
        let english = parsed["verdict_en"].as_str().unwrap();
        assert!(english.starts_with("Tuning evaluation"), "{english}");
        assert!(english.contains("30 ‰ (at 125 % load)"), "{english}");
        assert!(english.contains("90 ‰"), "{english}");
        assert!(!english.contains("not worth it"), "{english}");
        assert!(
            parsed["verdict"]
                .as_str()
                .unwrap()
                .contains("ohne direkten Vergleich")
        );

        // Mit beiden Armen bleibt alles, wie es war.
        let both = as_json(
            &[row(110, true, 300, 12)],
            &[110],
            10,
            None,
            None,
            Arms::Both,
        );
        let parsed: serde_json::Value = serde_json::from_str(&both).unwrap();
        assert_eq!(parsed["arms"], "both");
        assert_eq!(parsed["cells"][0]["direct_uncovered_permille"], 300);
    }

    /// Ueber der Saettigung nennt das Urteil den Punkt und beide Zahlen.
    #[test]
    fn above_saturation_it_names_the_point_and_both_numbers() {
        let rows = vec![
            row(100, true, 10, 8),
            row(110, true, 340, 12),
            row(110, false, 20, 300),
        ];
        let text = verdict(&rows, &[100, 110], Arms::Both);
        assert!(text.contains("Ab 110 % Last"), "{text}");
        assert!(text.contains("340 ‰"), "{text}");
        assert!(text.contains("12 ‰"), "{text}");
        assert!(text.contains("300 ‰"), "Preis fehlt: {text}");
    }

    /// Ist der Governor nicht besser, sagt das Werkzeug genau das.
    #[test]
    fn a_result_against_us_is_reported_as_such() {
        let rows = vec![row(110, true, 200, 260)];
        let text = verdict(&rows, &[110], Arms::Both);
        assert!(text.contains("nicht weniger"), "{text}");
        assert!(text.contains("gegen uns"), "{text}");
    }

    /// Ohne Lieferung gibt es kein Urteil, sondern einen Hinweis.
    #[test]
    fn no_delivery_is_not_a_verdict() {
        assert!(verdict(&[], &[100], Arms::Both).contains("Kein Ergebnis"));
    }

    fn parse(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    /// Das Gegenbeispiel aus dem Review vom 15.09. (R04).
    ///
    /// Ein geschlossener Port liefert nichts. Trotzdem stand im JSON eine
    /// Zelle mit 1000 ‰ und `conclusive: true`, und der Satz nannte das ein
    /// Ergebnis gegen den Governor — aus einem Integrationsfehler. Die Zeile
    /// entsteht hier auf demselben Weg wie in `run()`.
    #[tokio::test]
    async fn a_run_without_any_delivery_is_not_conclusive() {
        let defs = [StreamDef {
            name: "det",
            model: "det",
            period: Duration::from_millis(10),
            max_age: Duration::from_millis(10),
            in_flight_cap: 1,
            input: None,
            text: None,
            pump: false,
            burst: None,
        }];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        drop(listener);
        let reports = drive(&endpoint, &defs, Duration::from_millis(100), false).await;
        assert!(reports.iter().all(|r| r.delivered == 0), "{reports:?}");

        let absent = StreamReport::not_run("");
        let report = report_of(&reports, "det", &absent);
        let rows = [row_of(100, "det", true, Some(report), report)];
        let parsed = parse(&as_json(&rows, &[100], 1, Some(0), Some(0), Arms::Both));
        assert_eq!(parsed["conclusive"], false, "{parsed}");
        assert_eq!(parsed["inconclusive_reason"], "invalid_cells", "{parsed}");
        assert_eq!(parsed["invalid_cells"], 1, "{parsed}");

        let cell = &parsed["cells"][0];
        assert_eq!(cell["valid"], false, "{cell}");
        assert_eq!(cell["direct_state"], "not_run", "{cell}");
        assert_eq!(cell["governed_state"], "not_run", "{cell}");
        assert_eq!(cell["governed_delivered"], 0, "{cell}");
        assert_eq!(cell["governed_errors"], 0, "{cell}");
        // Die bisherigen Felder bleiben fuer bestehende Leser.
        assert!(cell["governed_uncovered_permille"].is_u64(), "{cell}");
        assert!(cell["governed_samples"].is_u64(), "{cell}");

        let german = parsed["verdict"].as_str().unwrap();
        assert!(german.starts_with("Kein Ergebnis"), "{german}");
        assert!(german.contains("nicht auswertbar"), "{german}");
        assert!(german.contains("keine Verbindung zum Ziel"), "{german}");
        assert!(!german.contains("gegen uns"), "{german}");
        let english = parsed["verdict_en"].as_str().unwrap();
        assert!(english.starts_with("No result"), "{english}");
        assert!(english.contains("inconclusive"), "{english}");
        assert!(!english.contains("against us"), "{english}");
    }

    /// Eine einzige ungueltige Zelle haelt das ganze Urteil zurueck, auch
    /// wenn die uebrigen eines tragen wuerden — und auch im
    /// Abstimmungslauf. Aushungern unter dem Governor dagegen bleibt ein
    /// Befund.
    #[test]
    fn one_invalid_cell_withholds_the_whole_verdict() {
        let broken = Row {
            governed_tally: Tally {
                errors: 3,
                integrity: Integrity::Errors(3),
                ..HEALTHY
            },
            ..row(110, true, 300, 1000)
        };
        let rows = [row(100, true, 10, 8), broken.clone()];
        let text = verdict(&rows, &[100, 110], Arms::Both);
        assert!(text.contains("Kein Ergebnis"), "{text}");
        assert!(text.contains("1 von 2 Messzellen"), "{text}");
        assert!(text.contains("Arm Governor: 3 Transport-"), "{text}");
        assert!(!text.contains("gegen uns"), "{text}");
        assert!(verdict_en(&rows, &[100, 110], Arms::Both).contains("governor arm: 3 transport"));

        let tuning = Row {
            direct: None,
            direct_gap_ms: None,
            direct_samples: None,
            direct_tally: None,
            ..broken
        };
        let parsed = parse(&as_json(&[tuning], &[110], 10, None, None, Arms::Governed));
        assert_eq!(parsed["conclusive"], false, "{parsed}");
        assert!(parsed["cells"][0]["direct_state"].is_null(), "{parsed}");
        assert!(parsed["cells"][0]["direct_errors"].is_null(), "{parsed}");

        let starved = Row {
            governed_tally: Tally {
                delivered: 0,
                refused: 200,
                ..HEALTHY
            },
            ..row(125, true, 300, 1000)
        };
        let parsed = parse(&as_json(&[starved], &[125], 10, None, None, Arms::Both));
        assert_eq!(parsed["conclusive"], true, "{parsed}");
        assert_eq!(parsed["cells"][0]["valid"], true, "{parsed}");
        assert!(parsed["verdict"].as_str().unwrap().contains("gegen uns"));
    }

    /// Weist der Governor unter Last Frames ab, ist die Zelle gueltig: ein
    /// Ergebnis ueber das Scheduling, kein Integrationsfehler.
    ///
    /// Ein Slot, 30 ms je Inferenz, ein Takt von 20 ms: 150 % Last. Die
    /// Ablehnungen kommen vom echten Gateway ueber den Draht, mit dem Grund
    /// im Metadatum.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refusals_under_load_keep_the_cell_valid() {
        let runtime = vig_core::Duration::from_millis(30).unwrap();
        let backend = Arc::new(Backend::new(
            1,
            HashMap::from([(
                "det_main".to_owned(),
                RuntimeDistribution::constant(runtime),
            )]),
            7,
        ));
        let endpoint = backend::start(Arc::clone(&backend)).await;
        let yaml = format!(
            "version: 1\nbackend:\n  type: triton\n  grpc_endpoint: {endpoint}\n  slots: 1\n  \
             pipelining_depth: 0\nmodels:\n  det:\n    class: protected\n    queue: {{ policy: latest, \
             capacity: 1 }}\n    contract: {{ period_ms: 20, deadline_ms: 40, max_age_ms: 40 }}\n    variants:\n      \
             - id: main\n        backend_model: det_main\n        quality: {{ value: 1.0, \
             source: measured }}\n        profile: {{ p50_us: 30000, p95_us: 30000, p99_us: \
             30000, samples: 2000 }}\n"
        );
        let resolved = Arc::new(
            vig_config::Config::from_yaml(&yaml)
                .unwrap()
                .resolve()
                .unwrap(),
        );
        let gateway = RunningGateway::start(resolved).await;
        let defs = [StreamDef {
            name: "det",
            model: "det",
            period: Duration::from_millis(20),
            max_age: Duration::from_millis(40),
            in_flight_cap: 4,
            input: None,
            text: None,
            pump: false,
            burst: None,
        }];
        let reports = drive(&gateway.address, &defs, Duration::from_millis(1_500), true).await;
        gateway.stop().await;

        let absent = StreamReport::not_run("");
        let report = report_of(&reports, "det", &absent);
        assert!(report.connected, "{report:?}");
        assert!(report.refused > 0, "keine Ablehnung unter Last: {report:?}");
        assert_eq!(report.errors, 0, "{report:?}");
        assert_eq!(report.rejected, report.refused, "{report:?}");

        let rows = [row_of(150, "det", true, None, report)];
        let parsed = parse(&as_json(&rows, &[150], 1, None, None, Arms::Governed));
        assert_eq!(parsed["conclusive"], true, "{parsed}");
        let cell = &parsed["cells"][0];
        assert_eq!(cell["valid"], true, "{cell}");
        assert_eq!(cell["governed_state"], "valid", "{cell}");
        assert!(cell["governed_refused"].as_u64().unwrap() > 0, "{cell}");
        assert_eq!(cell["governed_errors"], 0, "{cell}");
    }
}
