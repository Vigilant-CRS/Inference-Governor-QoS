//! `gate-m3` — der Produktvergleich gegen echten Triton auf echter GPU.
//!
//! Derselbe Workload zweimal: einmal direkt zu Triton, einmal ueber OneTimer.
//! Gleiche Modelle, gleiche Hardware, gleiche Frames, gleicher Client,
//! gleicher Transport.
//!
//! ## Warum Shared Memory
//!
//! Die Tensordaten reisen als Referenz, nicht im Request. Auf dem Copy-Pfad
//! kostet ein 3-MB-Frame den Governor rund das Doppelte der Uebertragungszeit
//! (ADR-0003, `docs/benchmark/data-plane.md`) — der Vergleich wuerde dann den
//! Transport messen und nicht das Scheduling. Shared Memory ist zugleich die
//! Konfiguration, die ein reales Deployment ohnehin verwendet.
//!
//! ## Was hier die Baseline ist
//!
//! Triton selbst, unveraendert, mit denselben Modellen und derselben
//! Instance-Group-Konfiguration. Kein Strohmann: dynamisches Batching ist auf
//! beiden Seiten aus, weil die Warteschlange vor den Governor gehoert und
//! nicht dahinter (ADR-0002) — und weil ein Batcher die Baseline bei
//! periodischer Einzelbildlast nicht schneller, sondern nur traeger machen
//! wuerde.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines
)]

use onetimer_bench::shm::Region;
use onetimer_bench::workload::{InputSpec, StreamDef, StreamReport, connect, drive};
use onetimer_config::Config;
use onetimer_gateway::{GatewayService, MonotonicClock, actor};
use onetimer_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use onetimer_protocol_oip::inference::{
    ModelMetadataRequest, SystemSharedMemoryRegisterRequest, SystemSharedMemoryUnregisterRequest,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const RUN_SECONDS: u64 = 30;
const CAPS: [usize; 2] = [1, 8];

/// Was ein Strom fuer den Lauf braucht.
struct Prepared {
    name: String,
    physical: String,
    period: Duration,
    max_age: Duration,
    input: InputSpec,
    _region: Region,
}

fn to_std(d: onetimer_core::Duration) -> Duration {
    Duration::from_nanos(d.as_nanos())
}

fn element_size(datatype: &str) -> usize {
    match datatype {
        "BOOL" | "INT8" | "UINT8" => 1,
        "INT16" | "UINT16" | "FP16" | "BF16" => 2,
        "INT64" | "UINT64" | "FP64" => 8,
        _ => 4,
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
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "examples/gate_m3/onetimer.yaml".to_owned());
    let text = std::fs::read_to_string(&path).expect("Konfiguration lesbar");
    let config = Config::from_yaml(&text).expect("Konfiguration gueltig");
    let findings = config.diagnose();
    assert!(
        findings.is_empty(),
        "Konfiguration hat Befunde: {findings:?}"
    );
    let resolved = Arc::new(config.resolve().expect("aufloesbar"));

    let triton = Arc::new(onetimer_backend_triton::TritonClient::new(
        &resolved.backend_endpoint,
    ));

    println!("gate-m3: OneTimer gegen Triton, gleiche Modelle, gleiche GPU");
    println!(
        "Backend {} · geschuetzte serialisierte Auslastung {} %",
        resolved.backend_endpoint,
        resolved.protected_utilization_permille() / 10
    );
    println!("Messdauer {RUN_SECONDS} s je Lauf und Puffertiefe {CAPS:?}\n");

    // --- Vorbereiten: Metadaten holen, Shm-Regionen anlegen und registrieren
    let mut prepared = Vec::new();
    for (index, logical) in resolved.model_names.iter().enumerate() {
        let physical = resolved
            .backend_model(
                onetimer_core::ModelIdx(u16::try_from(index).unwrap_or(0)),
                0,
            )
            .expect("Variante vorhanden")
            .to_owned();
        let contract = resolved.contracts.get(index).expect("Vertrag vorhanden");

        let metadata = triton
            .raw()
            .await
            .expect("Backend erreichbar")
            .model_metadata(ModelMetadataRequest {
                name: physical.clone(),
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
        let byte_size = u64::try_from(elements).unwrap_or(0) * element_size(&input.datatype) as u64;

        let region_name = format!("onetimer_{logical}");
        let region = Region::create(&region_name, byte_size).expect("Shm-Region anlegen");

        // Aufraeumen, falls ein frueherer Lauf abgebrochen ist.
        let _ = triton
            .raw()
            .await
            .expect("Backend erreichbar")
            .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
                name: region.name.clone(),
            })
            .await;
        triton
            .raw()
            .await
            .expect("Backend erreichbar")
            .system_shared_memory_register(SystemSharedMemoryRegisterRequest {
                name: region.name.clone(),
                key: region.key.clone(),
                offset: 0,
                byte_size,
            })
            .await
            .expect("Shm-Region registrieren");

        println!(
            "  {logical:<9} -> {physical:<15} {:>6} KB  Periode {:>4} ms",
            byte_size / 1024,
            to_std(contract.deadline).as_millis()
        );

        prepared.push(Prepared {
            name: logical.clone(),
            physical,
            period: contract.period.map_or(Duration::from_millis(500), to_std),
            max_age: contract.max_age.map_or(Duration::from_secs(1), to_std),
            input: InputSpec {
                name: input.name.clone(),
                datatype: input.datatype.clone(),
                shape,
                region: Some(region.name.clone()),
                byte_size,
            },
            _region: region,
        });
    }

    let streams = |use_logical: bool, cap: usize| -> Vec<StreamDef> {
        prepared
            .iter()
            .map(|p| StreamDef {
                name: Box::leak(p.name.clone().into_boxed_str()),
                model: Box::leak(
                    if use_logical {
                        p.name.clone()
                    } else {
                        p.physical.clone()
                    }
                    .into_boxed_str(),
                ),
                period: p.period,
                max_age: p.max_age,
                in_flight_cap: cap,
                input: Some(p.input.clone()),
                pump: false,
            })
            .collect()
    };

    let duration = Duration::from_secs(RUN_SECONDS);

    // --- Baseline: direkt zu Triton --------------------------------------
    let mut baseline: HashMap<String, StreamReport> = HashMap::new();
    for cap in CAPS {
        let reports = drive(
            &resolved.backend_endpoint,
            &streams(false, cap),
            duration,
            false,
        )
        .await;
        for report in reports {
            let entry = baseline
                .entry(report.name.to_owned())
                .or_insert_with(|| report.clone());
            if report.coverage.covered_permille() > entry.coverage.covered_permille() {
                *entry = report;
            }
        }
    }

    // --- Mit OneTimer -----------------------------------------------------
    let clock = MonotonicClock::start();
    let handle =
        actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
    let service = GatewayService::new(Arc::clone(&resolved), triton, handle.clone(), clock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Port");
    let gateway = listener.local_addr().expect("Adresse").to_string();
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
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _warm = connect(&gateway).await;

    let mut governed: HashMap<String, StreamReport> = HashMap::new();
    for cap in CAPS {
        let reports = drive(&gateway, &streams(true, cap), duration, true).await;
        for report in reports {
            let entry = governed
                .entry(report.name.to_owned())
                .or_insert_with(|| report.clone());
            if report.coverage.covered_permille() > entry.coverage.covered_permille() {
                *entry = report;
            }
        }
    }

    // --- Bericht -----------------------------------------------------------
    println!("\n  Strom     | Abdeckung Triton | OneTimer | AoI p95 Triton | OneTimer | Faktor");
    println!("  ----------|------------------|----------|----------------|----------|-------");
    for p in &prepared {
        let (Some(a), Some(b)) = (baseline.get(&p.name), governed.get(&p.name)) else {
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
            "  {:<9} | {:>14} % | {:>6} % | {:>11} ms | {:>5} ms | {factor:>6}",
            p.name,
            a.coverage.covered_permille() / 10,
            b.coverage.covered_permille() / 10,
            a.coverage.aoi_p95_ns / 1_000_000,
            b.coverage.aoi_p95_ns / 1_000_000,
        );
    }

    if let Ok(m) = handle.metrics().await {
        println!(
            "\n  Governor: angenommen {} weitergereicht {} supersediert {} stale {} \
             unmachbar {} verspaetet {} zurueckgestellt {} best-effort ausgehungert {}",
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
    println!(
        "\nAbdeckung nach ADR-0005: Anteil der Perioden mit einem gelieferten Ergebnis,\n\
         dessen Alter unter max_age lag."
    );
}
