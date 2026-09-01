//! `wire-bench` — bringt der Governor auf dem Draht etwas?
//!
//! Faehrt denselben Workload zweimal durch denselben Stack: einmal direkt zum
//! Backend, einmal ueber OneTimer. Gleiche Frames, gleiche Laufzeiten, gleiche
//! Kapazitaet, gleicher Client.
//!
//! Drei Szenarien, weil eine einzelne Zahl hier in die Irre fuehren wuerde:
//!
//! * **A** — geschuetzte Vertraege uebersteigen die Kapazitaet. `doctor` lehnt
//!   das ab. Zeigt, was passiert, wenn man es trotzdem faehrt.
//! * **B** — geschuetzte Vertraege passen, aber der Best-Effort-Job ist laenger
//!   als die geschuetzte Periode. Zeigt die Grenze nicht-praeemptiver
//!   Ausfuehrung (Spec 10.10, 15).
//! * **C** — dieselbe Last auf zwei Slots ohne Co-Run-Verbot.
//!
//! **Kein Vergleich gegen Triton.** Das Backend ist ein Modell mit begrenzter
//! Ausfuehrungskapazitaet, kein Inferenzserver. Gate M3 gegen einen getunten
//! Triton auf echter Hardware steht weiterhin aus.

// Ein Benchmark, der eine kaputte Umgebung stillschweigend umgeht, misst
// etwas anderes als beabsichtigt: ein ungueltiges Szenario soll den Lauf
// abbrechen, nicht ein Ergebnis liefern, das zu keiner dokumentierten
// Konfiguration gehoert.
#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::format_push_string,
    clippy::integer_division,
    clippy::too_many_lines
)]

use onetimer_bench::backend::{self, Backend};
use onetimer_bench::workload::{StreamDef, StreamReport, drive};
use onetimer_config::Config;
use onetimer_gateway::{GatewayService, MonotonicClock, actor};
use onetimer_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use onetimer_sim::workload::RuntimeDistribution;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const RUN_SECONDS: u64 = 15;
const SEED: u64 = 0x5EED_BEEF;

/// Puffertiefen, mit denen **beide Seiten** gefahren werden.
///
/// Eine flache Tiefe drosselt den Client selbst, eine tiefe erzeugt Rueckstand.
/// Je Strom zaehlt das bessere Ergebnis — fuer die Baseline **und** fuer den
/// Governor. Nur eine Seite ihre beste Tiefe waehlen zu lassen waere ein
/// verstecktes Handicap: bei geringer Last ist eine Tiefe von 1 eine
/// Selbstdrosselung, die von sich aus optimal ist (Spec 19.1).
const CAPS: [usize; 3] = [1, 4, 16];

fn ms(v: u64) -> onetimer_core::Duration {
    onetimer_core::Duration::from_nanos_unbounded(v.saturating_mul(1_000_000))
}

fn runtimes() -> HashMap<String, RuntimeDistribution> {
    let mut map = HashMap::new();
    for (name, p50, p99) in [
        ("detector_large", 10_u64, 16_u64),
        ("detector_small", 6, 10),
        ("pose_main", 8, 14),
        ("depth_main", 12, 20),
        ("vlm_main", 200, 320),
    ] {
        if let Some(d) = RuntimeDistribution::from_percentiles(ms(p50), ms(p99)) {
            map.insert(name.to_owned(), d);
        }
    }
    map
}

/// Ein Vergleichsszenario.
struct Scenario {
    name: &'static str,
    note: &'static str,
    slots: usize,
    no_corun: bool,
    /// `(Name, physisches Modell, Periode ms, max_age ms)`
    streams: Vec<(&'static str, &'static str, u64, u64)>,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "A - geschuetzte Vertraege ueberzeichnet",
            note: "doctor meldet PROTECTED_WORKLOAD_UNSCHEDULABLE",
            slots: 1,
            no_corun: true,
            streams: vec![
                ("detector", "detector_large", 33, 66),
                ("pose", "pose_main", 33, 66),
                ("depth", "depth_main", 66, 132),
                ("vlm", "vlm_main", 500, 1_500),
            ],
        },
        Scenario {
            name: "B - Vertraege tragfaehig, ein Slot",
            note: "der Best-Effort-Job ist laenger als die geschuetzte Periode",
            slots: 1,
            no_corun: true,
            streams: vec![
                ("detector", "detector_large", 50, 100),
                ("pose", "pose_main", 100, 200),
                ("depth", "depth_main", 200, 400),
                ("vlm", "vlm_main", 2_000, 4_000),
            ],
        },
        Scenario {
            name: "C - dieselbe Last, zwei Slots, kein Co-Run-Verbot",
            note: "Best-Effort bekommt eigene Kapazitaet",
            slots: 2,
            no_corun: false,
            streams: vec![
                ("detector", "detector_large", 50, 100),
                ("pose", "pose_main", 100, 200),
                ("depth", "depth_main", 200, 400),
                ("vlm", "vlm_main", 2_000, 4_000),
            ],
        },
    ]
}

impl Scenario {
    fn direct_streams(&self, cap: usize) -> Vec<StreamDef> {
        self.streams
            .iter()
            .map(|(name, model, period, max_age)| StreamDef {
                name,
                model,
                period: Duration::from_millis(*period),
                max_age: Duration::from_millis(*max_age),
                in_flight_cap: cap,
                input: None,
                pump: false,
            })
            .collect()
    }

    fn governed_streams(&self, cap: usize) -> Vec<StreamDef> {
        self.streams
            .iter()
            .map(|(name, _, period, max_age)| StreamDef {
                name,
                model: name,
                period: Duration::from_millis(*period),
                max_age: Duration::from_millis(*max_age),
                in_flight_cap: cap,
                input: None,
                pump: false,
            })
            .collect()
    }

    fn config_yaml(&self, backend_endpoint: &str) -> String {
        let slots = self.slots;
        let corun = if self.no_corun {
            "\n  no_corun:\n    - [detector, vlm]"
        } else {
            ""
        };
        let mut models = String::new();
        for (name, model, period, max_age) in &self.streams {
            let best_effort = *name == "vlm";
            let (class, queue) = if best_effort {
                (
                    "best_effort",
                    "{ policy: fifo, capacity: 4, overflow: backpressure_client }",
                )
            } else if *name == "detector" {
                ("protected", "{ policy: latest, capacity: 1 }")
            } else {
                ("high", "{ policy: latest, capacity: 1 }")
            };
            let (p50, p95, p99) = match *model {
                "detector_large" => (10_000, 13_000, 16_000),
                "pose_main" => (8_000, 11_000, 14_000),
                "depth_main" => (12_000, 16_000, 20_000),
                _ => (200_000, 270_000, 320_000),
            };
            let deadline = period;
            let extra = if *name == "detector" {
                "\n      - id: small\n        backend_model: detector_small\n        \
                 quality: { value: 0.93, source: measured }\n        \
                 profile: { p50_us: 6000, p95_us: 8000, p99_us: 10000, samples: 2000 }"
            } else {
                ""
            };
            models.push_str(&format!(
                "\n  {name}:\n    class: {class}\n    queue: {queue}\n    \
                 contract: {{ period_ms: {period}, deadline_ms: {deadline}, \
                 max_age_ms: {max_age} }}\n    variants:\n      - id: main\n        \
                 backend_model: {model}\n        quality: {{ value: 1.0, source: measured }}\n        \
                 profile: {{ p50_us: {p50}, p95_us: {p95}, p99_us: {p99}, samples: 2000 }}{extra}"
            ));
        }
        format!(
            "version: 1\nbackend:\n  type: triton\n  grpc_endpoint: {backend_endpoint}\n  \
             slots: {slots}\n  pipelining_depth: 0{corun}\nmodels:{models}\n"
        )
    }
}

async fn fresh_backend(slots: usize) -> (String, Arc<Backend>) {
    let backend = Arc::new(Backend::new(slots, runtimes(), SEED));
    let address = backend::start(Arc::clone(&backend)).await;
    (address.to_string(), backend)
}

async fn start_gateway(
    yaml: &str,
    backend_endpoint: &str,
) -> (String, u64, onetimer_gateway::Handle) {
    let config = Config::from_yaml(yaml).expect("Benchmark-Konfiguration ist gueltig");
    let findings = config.diagnose();
    assert!(
        findings.is_empty(),
        "Konfiguration hat Befunde: {findings:?}"
    );
    let resolved = Arc::new(config.resolve().expect("aufloesbar"));
    let utilization = resolved.protected_utilization_permille();

    let clock = MonotonicClock::start();
    let triton = Arc::new(onetimer_backend_triton::TritonClient::new(backend_endpoint));
    let handle = actor::spawn(Arc::clone(&resolved), &triton, clock).expect("Scheduler");
    let service = GatewayService::new(resolved, triton, handle.clone(), clock);

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
    tokio::time::sleep(Duration::from_millis(120)).await;
    (address, utilization, handle)
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
    let duration = Duration::from_secs(RUN_SECONDS);
    let only = std::env::args().nth(1);
    println!(
        "wire-bench: derselbe Workload, einmal direkt zum Backend und einmal ueber OneTimer.\n\
         Messdauer {RUN_SECONDS} s je Lauf.\nBeide Seiten werden mit den \
         Puffertiefen {CAPS:?} gefahren; je Strom zaehlt das jeweils beste\n\
         Ergebnis. Nur eine Seite ihre beste Tiefe waehlen zu lassen waere ein \
         verstecktes Handicap.\n"
    );

    for scenario in scenarios() {
        if let Some(filter) = only.as_deref()
            && !scenario.name.starts_with(filter)
        {
            continue;
        }
        println!("\n{}", "=".repeat(78));
        println!("{}\n  {}", scenario.name, scenario.note);

        // --- Baseline: beste Puffertiefe je Strom -------------------------
        let mut best: HashMap<&'static str, StreamReport> = HashMap::new();
        let mut baseline_executed = 0_u64;
        for cap in CAPS {
            let (endpoint, backend) = fresh_backend(scenario.slots).await;
            let reports = drive(&endpoint, &scenario.direct_streams(cap), duration, false).await;
            baseline_executed = baseline_executed.max(backend.executed.load(Ordering::Relaxed));
            for report in reports {
                let entry = best.entry(report.name).or_insert_with(|| report.clone());
                if report.coverage.covered_permille() > entry.coverage.covered_permille() {
                    *entry = report;
                }
            }
        }

        // --- Mit Governor, dieselbe Tiefensuche ----------------------------
        let mut governed_best: HashMap<&'static str, StreamReport> = HashMap::new();
        let mut governed_executed = 0_u64;
        let mut utilization = 0_u64;
        let mut last_metrics = None;
        for cap in CAPS {
            let (backend_endpoint, backend) = fresh_backend(scenario.slots).await;
            let yaml = scenario.config_yaml(&backend_endpoint);
            let (gateway_endpoint, u, handle) = start_gateway(&yaml, &backend_endpoint).await;
            utilization = u;
            let reports = drive(
                &gateway_endpoint,
                &scenario.governed_streams(cap),
                duration,
                true,
            )
            .await;
            governed_executed = governed_executed.max(backend.executed.load(Ordering::Relaxed));
            last_metrics = handle.metrics().await.ok();
            for report in reports {
                let entry = governed_best
                    .entry(report.name)
                    .or_insert_with(|| report.clone());
                if report.coverage.covered_permille() > entry.coverage.covered_permille() {
                    *entry = report;
                }
            }
        }

        println!(
            "\n  Geschuetzte serialisierte Auslastung: {} % {}",
            utilization / 10,
            if utilization > 1_000 {
                "-> doctor: NOT_READY"
            } else {
                ""
            }
        );
        println!("\n  Strom     | Abdeckung ohne | mit    | AoI p95 ohne | mit     | Faktor");
        println!("  ----------|----------------|--------|--------------|---------|-------");
        for (name, _, _, _) in &scenario.streams {
            let (Some(a), Some(b)) = (best.get(name), governed_best.get(name)) else {
                continue;
            };
            let without = a.coverage.uncovered_permille();
            let with = b.coverage.uncovered_permille();
            let factor = if with == 0 {
                if without == 0 {
                    "—".to_owned()
                } else {
                    "besser".to_owned()
                }
            } else if without >= with {
                format!("{:.1}x", without as f64 / with as f64)
            } else {
                format!("-{:.1}x", with as f64 / without.max(1) as f64)
            };
            println!(
                "  {name:<9} | {:>12} % | {:>4} % | {:>9} ms | {:>4} ms | {factor:>6}",
                a.coverage.covered_permille() / 10,
                b.coverage.covered_permille() / 10,
                a.coverage.aoi_p95_ns / 1_000_000,
                b.coverage.aoi_p95_ns / 1_000_000,
            );
        }
        println!(
            "  Backend: {baseline_executed} Inferenzen ohne, {governed_executed} mit Governor"
        );
        if let Some(m) = last_metrics {
            println!(
                "  Governor: angenommen {} weitergereicht {} supersediert {} \
                 stale {} unmachbar {} verspaetet {} zurueckgestellt {} \
                 best-effort ausgehungert {}",
                m.received,
                m.forwarded,
                m.superseded,
                m.stale,
                m.rejected_infeasible,
                m.dispatched_late,
                m.deferred_for_protected,
                m.best_effort_starved,
            );
        }
    }

    println!(
        "\n\nAbdeckung nach ADR-0005: Anteil der Perioden, in denen ein Ergebnis geliefert\n\
         wurde, dessen Alter unter max_age lag. Eine hohe Abdeckung bei wenigen\n\
         Inferenzen ist besser als eine niedrige bei vielen."
    );
}
