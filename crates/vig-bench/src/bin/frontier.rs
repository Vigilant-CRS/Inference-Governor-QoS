//! `frontier` — was bringt die Variantenwahl, und was kostet sie? (Spec 19.7)
//!
//! Kernvergleich C: dieselbe logische Funktion in zwei Groessen, die Last wird
//! stufenweise erhoeht. Gemessen werden Qualitaetsmix, Deadline-Erfolg, Zahl
//! der Variantenwechsel und das Hystereseverhalten — und die Abdeckung, denn
//! eine Variantenwahl, die Qualitaet haelt und dafuer Perioden verliert, hat
//! nichts gewonnen.
//!
//! ## Was hier verglichen wird
//!
//! Vier Governor-Konfigurationen, **alle** ueber den Governor, mit sonst
//! gleichem Vertrag. Der einzige Unterschied ist die Variantenpolicy:
//!
//! * `gross` — nur die grosse Variante. Was feste Qualitaet kostet.
//! * `klein` — nur die kleine. Was feste Geschwindigkeit kostet.
//! * `auto` — beide, automatische Wahl mit der Voreinstellung von
//!   `variant_dwell_ms` (100 ms).
//! * `auto ohne Dwell` — dasselbe mit Verweildauer null. Der Unterschied zu
//!   `auto` ist die Hysterese aus Spec 12.4, und nur sie.
//!
//! Triton ist hier bewusst **nicht** die Baseline. Die Frage ist nicht, ob der
//! Governor Triton schlaegt — das beantwortet `gate-m3` —, sondern ob die
//! automatische Wahl besser ist als jede feste Wahl, die ein Betreiber auch
//! ohne sie treffen koennte.
//!
//! ## Was das Modellpaar ist, und was nicht
//!
//! `detector_large` und `detector_small` sind ein **Laufzeitpaar**: ResNet-50
//! und ResNet-18 auf identischer Eingabe (8×3×224×224). Sie erzeugen eine
//! reproduzierbare Laufzeitspreizung, und dafuer sind sie da. Sie sind kein
//! Detektorpaar: die Ausgabetensoren heissen verschieden, und auf
//! Protokollebene sind die beiden nicht austauschbar. Die Qualitaet ist
//! **erklaert** (`user_declared`), nicht gemessen — die kleine traegt 0,8,
//! weil irgendeine Zahl unter 1,0 stehen muss. Der Qualitaetsmix dieser
//! Messung sagt deshalb, wie oft der Governor die grosse Variante halten
//! konnte; ueber Erkennungsgenauigkeit sagt er nichts.
//!
//! ## Kalibrierung
//!
//! Die Perioden werden gegen die **gemessenen** Mediane gesetzt, nicht gegen
//! `p99 × Marge` — der Fehler, der die erste Fassung von `load-ramp` wertlos
//! gemacht hat. Vor der Rampe misst das Werkzeug beide Varianten direkt gegen
//! Triton, ueber dieselbe Shared-Memory-Region, und schreibt die gemessenen
//! Quantile als Profile in die Konfiguration. 100 % Angebotslast heisst: die
//! **grosse** Variante allein lastet den Slot mit ihrem Median genau aus.
//!
//! ## Aufraeumen
//!
//! Jeder Lauf bekommt ein frisches Gateway, damit die Zaehler bei null
//! beginnen. Jedes davon wird nach dem Lauf beendet — der gRPC-Server und der
//! Actor. Das ist keine Hoeflichkeit: jeder Actor beobachtet die Karte mit
//! einem eigenen `nvidia-smi`-Thread, und 84 liegengebliebene Beobachter
//! waeren genau der Fehler, den `dd83086` behoben hat.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines
)]

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vig_backend_triton::TritonClient;
use vig_bench::shm::Region;
use vig_bench::workload::{InputSpec, StreamDef, StreamReport, drive};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{
    InferParameter, ModelInferRequest, SystemSharedMemoryRegisterRequest,
    SystemSharedMemoryUnregisterRequest, infer_parameter,
};

const SECONDS: u64 = 15;
const REPEATS: usize = 3;
/// Angebotslast in Prozent der Kapazitaet der **grossen** Variante.
const LOADS: [u64; 7] = [50, 75, 90, 100, 110, 125, 150];
/// Client-Puffertiefe.
///
/// Acht und nicht eins: bei einem einzigen offenen Request verwirft der
/// Client selbst, und die Entscheidung, um die es geht — welche Variante den
/// naechsten Frame rechnet —, faellt dann gar nicht erst im Governor. Alle
/// vier Konfigurationen laufen mit derselben Tiefe.
const CAP: usize = 8;
/// Kalibrierlaeufe je Variante, nach [`WARMUP`] ungezaehlten.
const SAMPLES: usize = 120;
const WARMUP: usize = 20;
/// Die Voreinstellung von `variant_dwell_ms` in `vig-config`.
const DEFAULT_DWELL_MS: u64 = 100;

const LARGE: &str = "detector_large";
const SMALL: &str = "detector_small";
/// Erklaerte, nicht gemessene Qualitaet der kleinen Variante.
const SMALL_QUALITY: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    LargeOnly,
    SmallOnly,
    Auto,
    AutoNoDwell,
}

impl Policy {
    const ALL: [Self; 4] = [
        Self::LargeOnly,
        Self::SmallOnly,
        Self::Auto,
        Self::AutoNoDwell,
    ];

    const fn index(self) -> usize {
        match self {
            Self::LargeOnly => 0,
            Self::SmallOnly => 1,
            Self::Auto => 2,
            Self::AutoNoDwell => 3,
        }
    }

    const fn dwell_ms(self) -> u64 {
        match self {
            Self::AutoNoDwell => 0,
            _ => DEFAULT_DWELL_MS,
        }
    }
}

/// Gemessene Laufzeitquantile einer Variante, alle in Mikrosekunden.
#[derive(Debug, Clone, Copy)]
struct Profile {
    p50: u64,
    p95: u64,
    p99: u64,
}

/// Was ein Lauf ueber die Entscheidungen des Governors sagt.
#[derive(Debug, Clone, Copy, Default)]
struct Decisions {
    /// Anteil der Dispatches auf der grossen Variante, in Promille.
    large_permille: u64,
    /// Aufwertungen je Minute.
    upgrades_per_min: u64,
    /// Abwertungen je Minute.
    downgrades_per_min: u64,
    /// Anteil der Dispatches ohne verletzte Deadline, in Promille.
    deadline_success_permille: u64,
}

fn shm_parameters(name: &str, bytes: u64) -> std::collections::HashMap<String, InferParameter> {
    let mut map = std::collections::HashMap::new();
    map.insert(
        "shared_memory_region".to_owned(),
        InferParameter {
            parameter_choice: Some(infer_parameter::ParameterChoice::StringParam(
                name.to_owned(),
            )),
        },
    );
    map.insert(
        "shared_memory_offset".to_owned(),
        InferParameter {
            parameter_choice: Some(infer_parameter::ParameterChoice::Int64Param(0)),
        },
    );
    map.insert(
        "shared_memory_byte_size".to_owned(),
        InferParameter {
            parameter_choice: Some(infer_parameter::ParameterChoice::Int64Param(
                i64::try_from(bytes).unwrap_or(0),
            )),
        },
    );
    map
}

/// Misst eine Variante direkt gegen Triton, ohne Governor.
///
/// Serialisiert, ein Request nach dem anderen: gemessen wird die Laufzeit
/// allein auf der Karte, die Groesse, gegen die die Perioden gesetzt werden.
async fn calibrate(client: &TritonClient, model: &str, input: &InputSpec) -> Profile {
    let region = input.region.clone().expect("Shared-Memory-Region");
    let request = ModelInferRequest {
        model_name: model.to_owned(),
        inputs: vec![InferInputTensor {
            name: input.name.clone(),
            datatype: input.datatype.clone(),
            shape: input.shape.clone(),
            parameters: shm_parameters(&region, input.byte_size),
            contents: None,
        }],
        ..Default::default()
    };
    for _ in 0..WARMUP {
        let _ = client.infer(request.clone()).await;
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        if client.infer(request.clone()).await.is_err() {
            continue;
        }
        samples.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
    assert!(
        !samples.is_empty(),
        "{model}: kein einziger Kalibrierlauf gelang"
    );
    samples.sort_unstable();
    let pick = |q: usize| -> u64 {
        let i = (samples.len() * q / 100).min(samples.len() - 1);
        samples.get(i).copied().unwrap_or(0)
    };
    Profile {
        p50: pick(50),
        p95: pick(95),
        p99: pick(99),
    }
}

/// Die Periode bei `load` Prozent, in ganzen Millisekunden.
///
/// Ganze Millisekunden, weil der Vertrag sie so nimmt. Die Rundung verschiebt
/// die tatsaechliche Last bei kurzen Perioden merklich; der Bericht nennt
/// deshalb die **effektive** Last aus der gerundeten Periode.
fn period_ms(large: Profile, load: u64) -> u64 {
    let micros = large.p50.saturating_mul(100) / load.max(1);
    // Kaufmaennisch runden, nie unter eine Millisekunde.
    micros
        .saturating_add(500)
        .checked_div(1_000)
        .unwrap_or(1)
        .max(1)
}

fn variant_yaml(id: &str, model: &str, quality: f64, profile: Profile) -> String {
    format!(
        "      - id: {id}\n        backend_model: {model}\n        \
         quality: {{ value: {quality:.2}, source: user_declared }}\n        \
         profile: {{ p50_us: {}, p95_us: {}, p99_us: {}, samples: {SAMPLES} }}\n",
        profile.p50, profile.p95, profile.p99,
    )
}

fn config_yaml(
    policy: Policy,
    period: u64,
    endpoint: &str,
    large: Profile,
    small: Profile,
) -> String {
    let mut variants = String::new();
    if policy != Policy::SmallOnly {
        variants.push_str(&variant_yaml("large", LARGE, 1.0, large));
    }
    if policy != Policy::LargeOnly {
        variants.push_str(&variant_yaml("small", SMALL, SMALL_QUALITY, small));
    }
    let mut out = String::new();
    let _ = write!(
        out,
        "version: 1\nbackend:\n  type: triton\n  grpc_endpoint: {endpoint}\n  \
         slots: 1\n  pipelining_depth: 0\n  safety_margin_percent: 110\n\
         models:\n  detector:\n    class: protected\n    \
         queue: {{ policy: latest, capacity: 1 }}\n    \
         contract: {{ period_ms: {period}, deadline_ms: {}, max_age_ms: {}, \
         variant_dwell_ms: {} }}\n    variants:\n{variants}",
        period.saturating_mul(3).div_ceil(2),
        period.saturating_mul(2),
        policy.dwell_ms(),
    );
    out
}

fn stream(period: u64, input: &InputSpec) -> StreamDef {
    StreamDef {
        name: "detector",
        model: "detector",
        period: Duration::from_millis(period),
        max_age: Duration::from_millis(period.saturating_mul(2)),
        in_flight_cap: CAP,
        input: Some(input.clone()),
        text: None,
        pump: false,
        burst: None,
    }
}

fn median(mut values: Vec<u64>) -> u64 {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

fn spread(values: &[u64]) -> (u64, u64) {
    (
        values.iter().copied().min().unwrap_or(0),
        values.iter().copied().max().unwrap_or(0),
    )
}

fn load_average() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(ToOwned::to_owned))
        .unwrap_or_else(|| "?".to_owned())
}

fn uncovered(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .find(|r| r.name == "detector")
        .map_or(1_000, |r| r.coverage.uncovered_permille())
}

/// Die Verbrauchersicht: Abtastzeitpunkte ohne brauchbares Ergebnis.
///
/// Neben der Fenstersicht, weil die hier kippt: liegt die Laufzeit der
/// grossen Variante nahe an der Periode, faellt eine Lieferung mal knapp vor,
/// mal knapp hinter eine Fenstergrenze, und die Fenstersicht zaehlt leere
/// Fenster, in denen dem Verbraucher nichts fehlte. Unter Saettigung zaehlt
/// sie umgekehrt Fenster als abgedeckt, deren Ergebnis beim Abtasten schon zu
/// alt ist (docs/analysis/bursts-and-frontier.md).
fn consumer_uncovered(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .find(|r| r.name == "detector")
        .map_or(1_000, |r| r.coverage.consumer_uncovered_permille())
}

/// Eine Tabellenzeile: je Policy Median und Spannweite, dazu die Systemlast.
fn table_row(load: u64, effective: u64, columns: &[Vec<u64>; 4], loads: &[String]) -> String {
    let mut row = format!("  {load:>3} % ({effective:>3} %) |");
    for column in columns {
        let (lo, hi) = spread(column);
        let _ = write!(row, " {:>4} ‰ [{lo}-{hi}]", median(column.clone()));
        let width = row.len();
        // Spalten ausrichten, auch wenn die Spannweite unterschiedlich lang ist.
        let pad = 19_usize.saturating_sub(width.saturating_sub(row.rfind('|').unwrap_or(0)));
        row.push_str(&" ".repeat(pad));
        row.push('|');
    }
    let _ = write!(row, " {}", loads.join(" "));
    row
}

fn print_table(title: &str, rows: &[String]) {
    println!("{title}\n");
    println!(
        "  Last (eff.) | gross            | klein            | auto             | auto ohne Dwell  | Last-Ø"
    );
    println!(
        "  ------------|------------------|------------------|------------------|------------------|-------"
    );
    for row in rows {
        println!("{row}");
    }
    println!();
}

/// Ein laufendes Gateway, das sich wieder beenden laesst.
struct Gateway {
    address: String,
    handle: vig_gateway::Handle,
    stop: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
}

impl Gateway {
    async fn start(yaml: &str) -> Self {
        let config = Config::from_yaml(yaml).expect("Konfiguration gueltig");
        let findings = config.diagnose();
        assert!(
            findings.is_empty(),
            "Konfiguration hat Befunde: {findings:?}"
        );
        let resolved = Arc::new(config.resolve().expect("aufloesbar"));
        let clock = MonotonicClock::start();
        let triton = Arc::new(TritonClient::new(&resolved.backend_endpoint));
        let handle =
            actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
        // Wie `vig serve` und `gate-m3`: der Governor beobachtet die Karte.
        handle.observe_hardware();
        let service = GatewayService::new(resolved, triton, handle.clone(), clock);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Port");
        let address = listener.local_addr().expect("Adresse").to_string();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let incoming = vig_bench::incoming(listener);
            let _ = tonic::transport::Server::builder()
                .initial_stream_window_size(vig_backend_triton::STREAM_WINDOW_BYTES)
                .initial_connection_window_size(vig_backend_triton::CONNECTION_WINDOW_BYTES)
                .add_service(
                    GrpcInferenceServiceServer::new(service)
                        .max_decoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
                        .max_encoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES),
                )
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = stopped.await;
                })
                .await;
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        Self {
            address,
            handle,
            stop,
            server,
        }
    }

    /// Liest die Zaehler und beendet Server und Actor.
    ///
    /// Erst lesen, dann beenden: ein beendeter Actor antwortet nicht mehr.
    async fn finish(self) -> vig_core::metrics::Metrics {
        let metrics = self.handle.metrics().await.expect("Metriken");
        let _ = self.stop.send(());
        let _ = self.server.await;
        // Der Actor endet mit dem Drain; mit ihm schliesst der Kanal, und der
        // Hardwarebeobachter merkt es vor seiner naechsten Abfrage.
        let _ = self.handle.drain(Duration::from_secs(5)).await;
        metrics
    }
}

fn decisions(m: &vig_core::metrics::Metrics, seconds: u64) -> Decisions {
    // Der Detektor ist das einzige Modell: `variant_selected` ist hier
    // eindeutig (siehe dort), und Index 0 ist die grosse Variante.
    let large = m.variant_selected.first().copied().unwrap_or(0);
    let total: u64 = m.variant_selected.iter().sum();
    let per_min = |count: u32| u64::from(count) * 60 / seconds.max(1);
    Decisions {
        large_permille: (large * 1_000).checked_div(total).unwrap_or(0),
        upgrades_per_min: per_min(m.variant_upgrades.first().copied().unwrap_or(0)),
        downgrades_per_min: per_min(m.variant_downgrades.first().copied().unwrap_or(0)),
        deadline_success_permille: (m.forwarded.saturating_sub(m.protected_deadline_misses)
            * 1_000)
            .checked_div(m.forwarded)
            .unwrap_or(0),
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
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8001".to_owned());
    let triton = TritonClient::new(&endpoint);

    // Eine Region fuer beide Varianten: sie nehmen dieselbe Eingabe, und der
    // Governor reicht dem gewaehlten Backendmodell dieselbe Referenz weiter.
    let metadata = triton.model_metadata(LARGE).await.expect("Metadaten");
    let input = metadata.inputs.first().expect("Eingabe");
    let shape: Vec<i64> = input
        .shape
        .iter()
        .enumerate()
        .map(|(i, d)| if *d < 0 && i == 0 { 1 } else { *d })
        .collect();
    let elements: i64 = shape.iter().copied().product();
    let byte_size = u64::try_from(elements).unwrap_or(0) * 4;
    let region = Region::create("vig_frontier_detector", byte_size).expect("Shm-Region");
    let raw = triton.raw().await.expect("Backend erreichbar");
    let _ = raw
        .clone()
        .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
            name: region.name.clone(),
        })
        .await;
    raw.clone()
        .system_shared_memory_register(SystemSharedMemoryRegisterRequest {
            name: region.name.clone(),
            key: region.key.clone(),
            offset: 0,
            byte_size,
        })
        .await
        .expect("Shm registrieren");
    let spec = InputSpec {
        name: input.name.clone(),
        datatype: input.datatype.clone(),
        shape,
        region: Some(region.name.clone()),
        byte_size,
        payload: None,
    };

    let small_input = triton.model_metadata(SMALL).await.expect("Metadaten");
    assert_eq!(
        small_input.inputs.first().map(|i| (&i.name, &i.shape)),
        Some((&spec.name, &input.shape)),
        "beide Varianten muessen dieselbe Eingabe nehmen"
    );

    // Auf dem Heap: die Kalibrierfutures halten den Request ueber jedes
    // `await` und sind dafuer zu gross fuer den Stack der Runtime.
    let large = Box::pin(calibrate(&triton, LARGE, &spec)).await;
    let small = Box::pin(calibrate(&triton, SMALL, &spec)).await;

    println!("frontier: was bringt die Variantenwahl? (Spec 19.7)");
    println!(
        "Triton {endpoint} · ein Slot · Shared Memory · {SECONDS} s je Lauf, \
         {REPEATS} Wiederholungen, Puffertiefe {CAP}"
    );
    println!(
        "Kalibriert, direkt gegen Triton, {SAMPLES} Laeufe: \
         {LARGE} p50 {} / p95 {} / p99 {} us · {SMALL} p50 {} / p95 {} / p99 {} us",
        large.p50, large.p95, large.p99, small.p50, small.p95, small.p99
    );
    println!(
        "Qualitaet erklaert, nicht gemessen: gross 1,0, klein {SMALL_QUALITY:.1}. \
         Das Paar ist ein Laufzeitpaar, kein Detektorpaar.\n"
    );

    let duration = Duration::from_secs(SECONDS);
    let mut decision_rows = Vec::new();
    let mut window_rows = Vec::new();
    let mut consumer_rows = Vec::new();
    for load in LOADS {
        let period = period_ms(large, load);
        let effective = large.p50.saturating_mul(100) / period.saturating_mul(1_000).max(1);

        let mut unc: [Vec<u64>; 4] = Default::default();
        let mut cons: [Vec<u64>; 4] = Default::default();
        let mut auto: Vec<Decisions> = Vec::new();
        let mut no_dwell: Vec<Decisions> = Vec::new();
        let mut loads = Vec::new();

        for _ in 0..REPEATS {
            loads.push(load_average());
            for policy in Policy::ALL {
                let gateway =
                    Gateway::start(&config_yaml(policy, period, &endpoint, large, small)).await;
                let reports =
                    drive(&gateway.address, &[stream(period, &spec)], duration, true).await;
                let metrics = gateway.finish().await;
                if let Some(column) = unc.get_mut(policy.index()) {
                    column.push(uncovered(&reports));
                }
                if let Some(column) = cons.get_mut(policy.index()) {
                    column.push(consumer_uncovered(&reports));
                }
                match policy {
                    Policy::Auto => auto.push(decisions(&metrics, SECONDS)),
                    Policy::AutoNoDwell => no_dwell.push(decisions(&metrics, SECONDS)),
                    Policy::LargeOnly | Policy::SmallOnly => {}
                }
            }
        }

        let window_row = table_row(load, effective, &unc, &loads);
        // Fortschritt schon waehrend des Laufs; die Tabellen folgen am Ende.
        println!("{window_row}");
        window_rows.push(window_row);
        consumer_rows.push(table_row(load, effective, &cons, &loads));
        decision_rows.push((load, effective, auto, no_dwell));
    }

    println!();
    print_table(
        "Unabgedeckte Lieferfenster (Median [Spannweite]), je Variantenpolicy:",
        &window_rows,
    );
    print_table(
        "Verbrauchersicht: Abtastzeitpunkte ohne brauchbares Ergebnis (Median [Spannweite]):",
        &consumer_rows,
    );

    println!("Entscheidungen der automatischen Wahl (Median ueber {REPEATS} Wiederholungen):\n");
    println!(
        "  Last (eff.) | Anteil gross auto | ohne Dwell | Wechsel/min auto (auf/ab) | ohne Dwell  | Deadline-Erfolg auto | ohne Dwell"
    );
    println!(
        "  ------------|-------------------|------------|---------------------------|-------------|----------------------|-----------"
    );
    for (load, effective, auto, no_dwell) in decision_rows {
        let m = |rows: &[Decisions], f: fn(&Decisions) -> u64| median(rows.iter().map(f).collect());
        println!(
            "  {load:>3} % ({effective:>3} %) | {:>15} ‰ | {:>8} ‰ | {:>11} / {:<11} | {:>4} / {:<4} | {:>18} ‰ | {:>7} ‰",
            m(&auto, |d| d.large_permille),
            m(&no_dwell, |d| d.large_permille),
            m(&auto, |d| d.upgrades_per_min),
            m(&auto, |d| d.downgrades_per_min),
            m(&no_dwell, |d| d.upgrades_per_min),
            m(&no_dwell, |d| d.downgrades_per_min),
            m(&auto, |d| d.deadline_success_permille),
            m(&no_dwell, |d| d.deadline_success_permille),
        );
    }

    println!(
        "\nLieferfenster nach ADR-0005: Anteil der Perioden, in denen kein Ergebnis\n\
         unter max_age ankam. Verbrauchersicht (NV-01): Anteil der Periodenenden,\n\
         an denen kein Ergebnis unter max_age vorlag — die Groesse, die einem\n\
         Regler fehlt. Liegt die Laufzeit nahe an der Periode, weichen beide\n\
         stark voneinander ab (docs/analysis/bursts-and-frontier.md). Effektive Last: Median der grossen Variante durch die auf ganze\n\
         Millisekunden gerundete Periode. Anteil gross: Dispatches auf Index 0; der\n\
         Detektor laeuft allein, `vig_variant_selected_total` ist deshalb eindeutig.\n\
         Deadline-Erfolg: weitergereichte Auftraege ohne verletzte Deadline.\n\
         Die Qualitaet ist erklaert, nicht gemessen — der Anteil gross sagt, wie oft\n\
         die grosse Variante gehalten wurde, nicht wie gut erkannt wurde.\n\
         Last-Ø ist die Systemlast beim Start jeder Wiederholung. Nicht neben\n\
         einem Build fahren; auf reservierten Kernen: taskset -c 8-15."
    );
    drop(region);
}
