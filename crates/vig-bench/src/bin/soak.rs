//! `soak` — was passiert nach der ersten Minute? (Spec 19.6, Phase 5)
//!
//! Alle bisherigen Messungen dieses Projekts sind zwoelf bis dreissig Sekunden
//! lang. Sie beantworten, ob der Governor funktioniert, und schweigen zu der
//! Frage, die fuer ein Geraet auf einem Roboter zaehlt: **funktioniert er noch
//! nach acht Stunden?**
//!
//! Was ein kurzer Lauf grundsaetzlich nicht sehen kann:
//!
//! - **Speicher, der nicht zurueckkommt.** Ein Leck von wenigen Kilobyte je
//!   Sekunde faellt in fuenfzehn Sekunden unter jede Nachweisgrenze und legt
//!   ein Geraet nach zwei Tagen lahm.
//! - **Margendrift.** Der Estimator zieht die Marge nach einer Unterprognose
//!   schnell hoch und nur langsam wieder herunter (ADR-0013). Ob sie sich
//!   einpendelt oder ueber Stunden monoton steigt, ist in einer Minute nicht
//!   zu unterscheiden.
//! - **Lastspitzen.** Die Rampe faehrt stationaere Punkte. Der realistische
//!   Fall ist Grundlast mit Spitzen — und genau der wurde nie gemessen.
//!
//! ## Aufbau
//!
//! Fenster von je einer Minute, im Zyklus fuenf Fenster Grundlast und ein
//! Fenster Spitze. Nach jedem Fenster werden die Stroeme, der komplette
//! Metrikabzug, der Speicherverbrauch des Prozesses und die Systemlast
//! fortgeschrieben. Ausgewertet wird die erste gegen die letzte Stunde.
//!
//! Der Lauf ueberlebt einen Ausfall des Backends: ein Fenster ohne Lieferung
//! wird protokolliert, nicht abgebrochen.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;
use vig_bench::shm::Region;
use vig_bench::workload::{InputSpec, StreamDef, drive};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::{
    ModelMetadataRequest, SystemSharedMemoryRegisterRequest, SystemSharedMemoryUnregisterRequest,
};

/// Ein Messfenster.
const WINDOW_SECONDS: u64 = 60;
/// Fenster je Zyklus; das letzte ist die Spitze.
const CYCLE: u64 = 6;
/// Grundlast in Prozent — knapp unter der Kante aus `load-ramp`.
const BASE_LOAD: u64 = 90;
/// Spitzenlast in Prozent — deutlich darueber.
const BURST_LOAD: u64 = 150;
/// Voreingestellte Laufzeit; ueber `SOAK_HOURS` aenderbar.
const DEFAULT_HOURS: u64 = 8;

/// Wie in `load-ramp` gegen die gemessenen Mediane kalibriert.
const BASE: [(&str, &str, u64, u64); 3] = [
    ("detector", "rfdetr", 23, 46),
    ("pose", "pose_main", 23, 46),
    ("depth", "depth_main", 46, 92),
];

fn scaled_period(base_ms: u64, load_percent: u64) -> u64 {
    // Höhere Last bedeutet kürzere Periode.
    base_ms
        .saturating_mul(100)
        .checked_div(load_percent)
        .unwrap_or(base_ms)
        .max(5)
}

fn config_yaml(load: u64, endpoint: &str) -> String {
    let mut models = String::new();
    for (logical, physical, base_period, base_age) in BASE {
        let period = scaled_period(base_period, load);
        let max_age = scaled_period(base_age, load);
        let class = if logical == "detector" {
            "protected"
        } else {
            "high"
        };
        let (p50, p95, p99) = match physical {
            "rfdetr" => (14916, 17081, 17470),
            "pose_main" => (3987, 4630, 5506),
            _ => (7908, 9367, 9534),
        };
        let _ = write!(
            models,
            "\n  {logical}:\n    class: {class}\n    \
             queue: {{ policy: latest, capacity: 1 }}\n    \
             contract: {{ period_ms: {period}, deadline_ms: {}, max_age_ms: {max_age} }}\n    \
             variants:\n      - id: main\n        backend_model: {physical}\n        \
             quality: {{ value: 1.0, source: user_declared }}\n        \
             profile: {{ p50_us: {p50}, p95_us: {p95}, p99_us: {p99}, samples: 120 }}",
            period.saturating_mul(3).checked_div(2).unwrap_or(period),
        );
    }
    format!(
        "version: 1\nbackend:\n  type: triton\n  grpc_endpoint: {endpoint}\n  \
         slots: 1\n  pipelining_depth: 0\n  safety_margin_percent: 110\nmodels:{models}\n"
    )
}

fn streams(load: u64, specs: &HashMap<String, InputSpec>) -> Vec<StreamDef> {
    BASE.iter()
        .map(|(name, _physical, base_period, base_age)| StreamDef {
            name,
            model: name,
            period: Duration::from_millis(scaled_period(*base_period, load)),
            max_age: Duration::from_millis(scaled_period(*base_age, load)),
            in_flight_cap: 8,
            input: specs.get(*name).cloned(),
            pump: false,
        })
        .collect()
}

fn load_average() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(ToOwned::to_owned))
        .unwrap_or_else(|| "?".to_owned())
}

/// Der residente Speicher dieses Prozesses in Kilobyte.
///
/// Die eigentliche Leckpruefung: der Governor laeuft hier im selben Prozess
/// wie der Treiber, ein Wachstum ueber Stunden ist also direkt sichtbar.
fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1).map(ToOwned::to_owned))
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

async fn start_gateway(yaml: &str, metrics_port: u16) -> String {
    let config = Config::from_yaml(yaml).expect("Konfiguration gueltig");
    let findings = config.diagnose();
    assert!(
        findings.is_empty(),
        "Konfiguration hat Befunde: {findings:?}"
    );
    let resolved = Arc::new(config.resolve().expect("aufloesbar"));
    let clock = MonotonicClock::start();
    let triton = Arc::new(vig_backend_triton::TritonClient::new(
        &resolved.backend_endpoint,
    ));
    let handle = actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler");
    let service = GatewayService::new(resolved, triton, handle.clone(), clock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Port");
    let address = listener.local_addr().expect("Adresse").to_string();
    tokio::spawn(async move {
        let stream = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let _ = tonic::transport::Server::builder()
            .initial_stream_window_size(vig_backend_triton::STREAM_WINDOW_BYTES)
            .initial_connection_window_size(vig_backend_triton::CONNECTION_WINDOW_BYTES)
            .add_service(
                GrpcInferenceServiceServer::new(service)
                    .max_decoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES),
            )
            .serve_with_incoming(stream)
            .await;
    });
    // Der Metrikendpunkt laeuft mit: acht Stunden Dauerbetrieb sind zugleich
    // der erste ernsthafte Test des Exporters selbst.
    let metrics_handle = handle.clone();
    tokio::spawn(async move {
        let address = ([127, 0, 0, 1], metrics_port).into();
        let _ = vig_gateway::exporter::serve(metrics_handle, address).await;
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    address
}

/// Holt `/metrics` ueber eine rohe TCP-Verbindung.
///
/// Ein HTTP-Client als Abhaengigkeit waere fuer einen GET gegen localhost
/// unverhaeltnismaessig: er zoege einen ganzen Baum an Krypto- und
/// Laufzeitbibliotheken nach, die durch die Lizenzpruefung muessen und nie
/// etwas anderes tun als diese eine Zeile.
async fn scrape(port: u16) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else {
        return "# Metrikendpunkt nicht erreichbar\n".to_owned();
    };
    let request = "GET /metrics HTTP/1.0\r\nHost: localhost\r\n\r\n";
    if stream.write_all(request.as_bytes()).await.is_err() {
        return "# Metrikendpunkt antwortet nicht\n".to_owned();
    }
    let mut raw = Vec::new();
    if stream.read_to_end(&mut raw).await.is_err() {
        return "# Metrikabzug abgebrochen\n".to_owned();
    }
    let text = String::from_utf8_lossy(&raw);
    // Kopf abschneiden; nur der Rumpf ist die Messung.
    text.split_once("\r\n\r\n")
        .map_or_else(|| text.to_string(), |(_, body)| body.to_owned())
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
    // Ein Dauerlauf ohne Protokoll verschweigt genau die Befunde, wegen derer
    // er laeuft — etwa die Warnung vor einer Last, die den Vertrag sprengt.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let triton_endpoint = "127.0.0.1:8001";
    let hours: u64 = std::env::var("SOAK_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_HOURS);
    let out_dir = std::env::var("SOAK_OUT").unwrap_or_else(|_| {
        "/run/media/dd/USB_40281/Projekte/InferenceQoS-runtime/soak".to_owned()
    });
    std::fs::create_dir_all(&out_dir).expect("Ausgabeverzeichnis");
    let metrics_port: u16 = 9490;

    let triton = vig_backend_triton::TritonClient::new(triton_endpoint);

    let mut specs = HashMap::new();
    let mut regions = Vec::new();
    for (logical, physical, _, _) in BASE {
        let metadata = triton
            .raw()
            .await
            .expect("Backend erreichbar")
            .model_metadata(ModelMetadataRequest {
                name: physical.to_owned(),
                version: String::new(),
            })
            .await
            .expect("Metadaten")
            .into_inner();
        let input = metadata.inputs.first().expect("Eingabe");
        let shape: Vec<i64> = input
            .shape
            .iter()
            .enumerate()
            .map(|(i, d)| if *d < 0 && i == 0 { 1 } else { *d })
            .collect();
        let elements: i64 = shape.iter().copied().product();
        let byte_size = u64::try_from(elements).unwrap_or(0) * 4;
        let region = Region::create(&format!("vig_soak_{logical}"), byte_size).expect("Shm-Region");
        let _ = triton
            .raw()
            .await
            .expect("Backend")
            .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
                name: region.name.clone(),
            })
            .await;
        triton
            .raw()
            .await
            .expect("Backend")
            .system_shared_memory_register(SystemSharedMemoryRegisterRequest {
                name: region.name.clone(),
                key: region.key.clone(),
                offset: 0,
                byte_size,
            })
            .await
            .expect("Shm registrieren");
        specs.insert(
            logical.to_owned(),
            InputSpec {
                name: input.name.clone(),
                datatype: input.datatype.clone(),
                shape,
                region: Some(region.name.clone()),
                byte_size,
            },
        );
        regions.push(region);
    }

    // Fenstergroesse und Anzahl sind ueberschreibbar, damit der Lauf vor der
    // Nacht in wenigen Minuten probegefahren werden kann. Die Voreinstellung
    // ist der Ernstfall.
    let window_seconds: u64 = std::env::var("SOAK_WINDOW_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(WINDOW_SECONDS);
    let windows: u64 = std::env::var("SOAK_WINDOWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(hours * 3600 / window_seconds.max(1));
    let mut streams_csv =
        std::fs::File::create(format!("{out_dir}/streams.csv")).expect("streams.csv");
    let mut metrics_log =
        std::fs::File::create(format!("{out_dir}/metrics.log")).expect("metrics.log");
    let _ = writeln!(
        streams_csv,
        "window,elapsed_s,load,stream,uncovered_permille,response_age_p95_ms,\
         emitted,sent,client_dropped,delivered,rejected,rss_kb,loadavg"
    );

    println!("soak: was passiert nach der ersten Minute?");
    println!("Triton {triton_endpoint} · {windows} Fenster à {window_seconds} s");
    println!(
        "Zyklus: {} x {BASE_LOAD} % Grundlast, 1 x {BURST_LOAD} % Spitze",
        CYCLE - 1
    );
    println!("Ausgabe: {out_dir}/streams.csv und metrics.log\n");

    // Ein Gateway fuer den ganzen Lauf. Genau das ist der Punkt: es soll
    // Stunden ueberstehen, nicht je Fenster neu entstehen.
    let yaml = config_yaml(BASE_LOAD, triton_endpoint);
    let gateway = start_gateway(&yaml, metrics_port).await;

    let started = std::time::Instant::now();
    let duration = Duration::from_secs(window_seconds);

    for window in 0..windows {
        let burst = window % CYCLE == CYCLE - 1;
        let load = if burst { BURST_LOAD } else { BASE_LOAD };
        let elapsed = started.elapsed().as_secs();

        let reports = drive(&gateway, &streams(load, &specs), duration, true).await;
        let rss = rss_kb();
        let avg = load_average();

        let mut delivered_total = 0_u64;
        for r in &reports {
            delivered_total += r.delivered;
            let _ = writeln!(
                streams_csv,
                "{window},{elapsed},{load},{},{},{},{},{},{},{},{},{rss},{avg}",
                r.name,
                r.coverage.uncovered_permille(),
                r.coverage.response_age_p95_ns / 1_000_000,
                r.emitted,
                r.sent,
                r.client_dropped,
                r.delivered,
                r.rejected,
            );
        }
        let _ = streams_csv.flush();

        let body = scrape(metrics_port).await;
        let _ = writeln!(
            metrics_log,
            "# window={window} elapsed_s={elapsed} load={load} rss_kb={rss} loadavg={avg}"
        );
        let _ = metrics_log.write_all(body.as_bytes());
        let _ = metrics_log.flush();

        if delivered_total == 0 {
            println!(
                "  Fenster {window} ({elapsed} s): KEINE Lieferung — Backend weg? \
                 Lauf geht weiter."
            );
        } else if window % 30 == 0 {
            let worst = reports
                .iter()
                .map(|r| r.coverage.uncovered_permille())
                .max()
                .unwrap_or(0);
            println!(
                "  Fenster {window:>4} ({:>3} min) Last {load:>3} % \
                 schlechtester Strom {worst:>4} ‰  RSS {rss} kB  Last-Ø {avg}",
                elapsed / 60
            );
        }
    }

    println!("\nFertig. Auswertung: {out_dir}/streams.csv");
    drop(regions);
}
