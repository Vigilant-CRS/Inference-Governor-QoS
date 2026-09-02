//! `diy-baseline` — was kann der Governor, das Clientcode nicht auch kann?
//!
//! Der haerteste Einwand gegen dieses Produkt lautet: *"Wozu ein Governor? Ich
//! verwerfe veraltete Frames einfach im Client."* Der Einwand ist gut, und er
//! ist zur Haelfte richtig. Supersession — nur der neueste Frame zaehlt, es
//! ist immer nur einer unterwegs — sind vielleicht fuenfzig Zeilen und
//! verhindert, dass ein Strom sich selbst zustaut.
//!
//! Dieser Benchmark misst genau diesen Eigenbau als dritten Arm neben dem
//! naiven Client und dem Governor. Die Hypothese, die er pruefen soll:
//!
//! - **Selbststau** kann der Client selbst loesen. Dafuer braucht es uns nicht.
//! - **Vorrang zwischen Stroemen** kann er nicht. Drei Pumpen nebeneinander
//!   wissen nichts voneinander; am Server entscheidet weiter die
//!   Ankunftsreihenfolge, und der geschuetzte Strom wartet hinter Arbeit, die
//!   niemand braucht.
//!
//! Faellt die Hypothese, ist das eine ernste Nachricht ueber den Wert des
//! Produkts — und sie gehoert dann genauso ins Repository wie ein Erfolg.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines
)]

use onetimer_bench::shm::Region;
use onetimer_bench::workload::{InputSpec, StreamDef, StreamReport, drive};
use onetimer_config::Config;
use onetimer_gateway::{GatewayService, MonotonicClock, actor};
use onetimer_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use onetimer_protocol_oip::inference::{
    ModelMetadataRequest, SystemSharedMemoryRegisterRequest, SystemSharedMemoryUnregisterRequest,
};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

const SECONDS: u64 = 12;
const REPEATS: usize = 3;
/// Der interessante Bereich: ab dem Knick aus `load-ramp`.
const LOADS: [u64; 3] = [100, 125, 150];

/// Wie in `load-ramp` gegen die gemessenen Mediane kalibriert.
const BASE: [(&str, &str, u64, u64); 3] = [
    ("detector", "rfdetr", 23, 46),
    ("pose", "pose_main", 23, 46),
    ("depth", "depth_main", 46, 92),
];

/// Die drei Betriebsarten.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    /// Client schickt jeden Frame, Puffer 8. Der Strohmann.
    Naiv,
    /// Client haelt nur den neuesten Frame, einer unterwegs. Der Eigenbau.
    Eigenbau,
    /// Governor davor, Client wie im naiven Fall.
    Governor,
}

impl Arm {
    fn label(self) -> &'static str {
        match self {
            Self::Naiv => "Triton, naiver Client",
            Self::Eigenbau => "Triton + Supersession im Client",
            Self::Governor => "OneTimer",
        }
    }
}

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

fn streams(load: u64, arm: Arm, specs: &HashMap<String, InputSpec>) -> Vec<StreamDef> {
    BASE.iter()
        .map(|(name, physical, base_period, base_age)| StreamDef {
            name,
            model: if arm == Arm::Governor { name } else { physical },
            period: Duration::from_millis(scaled_period(*base_period, load)),
            max_age: Duration::from_millis(scaled_period(*base_age, load)),
            // Die Pumpe haelt selbst nur einen Request offen; fuer die
            // anderen Arme der Puffer aus `load-ramp`.
            in_flight_cap: if arm == Arm::Eigenbau { 1 } else { 8 },
            input: specs.get(*name).cloned(),
            pump: arm == Arm::Eigenbau,
        })
        .collect()
}

/// Der Median einer Messreihe.
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

async fn start_gateway(yaml: &str) -> String {
    let config = Config::from_yaml(yaml).expect("Konfiguration gueltig");
    let findings = config.diagnose();
    assert!(
        findings.is_empty(),
        "Konfiguration hat Befunde: {findings:?}"
    );
    let resolved = Arc::new(config.resolve().expect("aufloesbar"));
    let clock = MonotonicClock::start();
    let triton = Arc::new(onetimer_backend_triton::TritonClient::new(
        &resolved.backend_endpoint,
    ));
    let handle = actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler");
    let service = GatewayService::new(resolved, triton, handle, clock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Port");
    let address = listener.local_addr().expect("Adresse").to_string();
    tokio::spawn(async move {
        let stream = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let _ = tonic::transport::Server::builder()
            .initial_stream_window_size(onetimer_backend_triton::STREAM_WINDOW_BYTES)
            .initial_connection_window_size(onetimer_backend_triton::CONNECTION_WINDOW_BYTES)
            .add_service(
                GrpcInferenceServiceServer::new(service)
                    .max_decoding_message_size(onetimer_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(onetimer_backend_triton::DEFAULT_MAX_MESSAGE_BYTES),
            )
            .serve_with_incoming(stream)
            .await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    address
}

/// Die unabgedeckten Perioden des **geschuetzten** Stroms.
///
/// Fuer die Produktaussage zaehlt dieser. Dass `high`-Stroeme hinter
/// `protected` zuruecktreten, ist die konfigurierte Absicht und kein Defekt;
/// sie mit in ein Maximum zu werfen wuerde beides vermengen.
fn protected_uncovered(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .find(|r| r.name == "detector")
        .map_or(0, |r| r.coverage.uncovered_permille())
}

/// Die schlechteste Abdeckung über alle Ströme, in Promille unabgedeckt.
fn worst_uncovered(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .map(|r| r.coverage.uncovered_permille())
        .max()
        .unwrap_or(0)
}

fn worst_aoi_ms(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .map(|r| r.coverage.aoi_p95_ns / 1_000_000)
        .max()
        .unwrap_or(0)
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
    let triton_endpoint = "127.0.0.1:8001";
    let triton = onetimer_backend_triton::TritonClient::new(triton_endpoint);

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
        let region =
            Region::create(&format!("onetimer_diy_{logical}"), byte_size).expect("Shm-Region");
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

    println!("diy-baseline: was kann der Governor, das Clientcode nicht auch kann?");
    println!(
        "Triton {triton_endpoint} · RF-DETR (protected), Pose, Tiefe (high) · \
         ein Slot · Shared Memory"
    );
    println!("{SECONDS} s je Lauf, {REPEATS} Wiederholungen, Median berichtet\n");
    println!("  Last | Betriebsart                     | Detektor | alle Stroeme | AoI p95");
    println!("  -----|---------------------------------|----------|--------------|--------");

    let duration = Duration::from_secs(SECONDS);
    for load in LOADS {
        let yaml = config_yaml(load, triton_endpoint);
        let gateway = start_gateway(&yaml).await;

        for arm in [Arm::Naiv, Arm::Eigenbau, Arm::Governor] {
            let mut prot = Vec::new();
            let mut all = Vec::new();
            let mut aoi = Vec::new();
            for _ in 0..REPEATS {
                let endpoint = if arm == Arm::Governor {
                    gateway.as_str()
                } else {
                    triton_endpoint
                };
                let reports = drive(
                    endpoint,
                    &streams(load, arm, &specs),
                    duration,
                    arm == Arm::Governor,
                )
                .await;
                prot.push(protected_uncovered(&reports));
                all.push(worst_uncovered(&reports));
                aoi.push(worst_aoi_ms(&reports));
            }
            let (pmin, pmax) = spread(&prot);
            println!(
                "  {load:>3} % | {:<31} | {:>4} ‰ [{pmin}-{pmax}] | {:>8} ‰ | {:>4} ms",
                arm.label(),
                median(prot.clone()),
                median(all),
                median(aoi),
            );
        }
        println!("  -----|---------------------------------|----------|--------------|--------");
    }

    println!("\nUnabgedeckte Perioden nach ADR-0005. `Detektor` ist der geschuetzte");
    println!(
        "Strom, `alle Stroeme` der schlechteste. Systemlast: {}",
        load_average()
    );
    drop(regions);
}
