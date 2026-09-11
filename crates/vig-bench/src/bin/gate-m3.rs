//! `gate-m3` — der Produktvergleich gegen echten Triton auf echter GPU.
//!
//! Derselbe Workload zweimal: einmal direkt zu Triton, einmal ueber Vigilant.
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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use vig_bench::shm::Region;
use vig_bench::workload::{InputSpec, StreamDef, StreamReport, connect, drive};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::{
    ModelMetadataRequest, SystemSharedMemoryRegisterRequest, SystemSharedMemoryUnregisterRequest,
};

const RUN_SECONDS: u64 = 30;
const CAPS: [usize; 2] = [1, 8];

/// Was ein Strom fuer den Lauf braucht.
struct Prepared {
    name: String,
    physical: String,
    /// Der Triton-Prozess, der dieses Modell bedient.
    ///
    /// Meist fuer alle derselbe. Zwei Prozesse auf einer GPU — etwa Vision
    /// und Sprache getrennt, oder unter XSched mit verschiedenen Prioritaeten
    /// (NV-15) — sind trotzdem **ein** Lauf und werden gleichzeitig gefahren.
    endpoint: String,
    period: Duration,
    max_age: Duration,
    input: InputSpec,
    _region: Region,
}

fn to_std(d: vig_core::Duration) -> Duration {
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
        .unwrap_or_else(|| "examples/gate_m3/vig.yaml".to_owned());
    let text = std::fs::read_to_string(&path).expect("Konfiguration lesbar");
    let config = Config::from_yaml(&text).expect("Konfiguration gueltig");
    let findings = config.diagnose();
    assert!(
        findings.is_empty(),
        "Konfiguration hat Befunde: {findings:?}"
    );
    let resolved = Arc::new(config.resolve().expect("aufloesbar"));

    let triton = Arc::new(vig_backend_triton::TritonClient::new(
        &resolved.backend_endpoint,
    ));

    println!("gate-m3: Vigilant gegen Triton, gleiche Modelle, gleiche GPU");
    println!(
        "Backend {} · geschuetzte serialisierte Auslastung {} %",
        resolved.backend_endpoint,
        resolved.protected_utilization_permille() / 10
    );
    println!("Messdauer {RUN_SECONDS} s je Lauf und Puffertiefe {CAPS:?}\n");

    // --- Vorbereiten: Metadaten holen, Shm-Regionen anlegen und registrieren
    let mut prepared = Vec::new();
    let mut clients: HashMap<String, Arc<vig_backend_triton::TritonClient>> = HashMap::new();
    for (index, logical) in resolved.model_names.iter().enumerate() {
        let model = vig_core::ModelIdx(u16::try_from(index).unwrap_or(0));
        let physical = resolved
            .backend_model(model, 0)
            .expect("Variante vorhanden")
            .to_owned();
        let contract = resolved.contracts.get(index).expect("Vertrag vorhanden");
        // Die Region muss bei **dem** Prozess registriert sein, der das Modell
        // rechnet — sonst faellt der Vergleich still auf den Copy-Pfad zurueck.
        let endpoint = resolved.endpoint_of(model).to_owned();
        let client = Arc::clone(clients.entry(endpoint.clone()).or_insert_with(|| {
            Arc::new(vig_backend_triton::TritonClient::new(endpoint.as_str()))
        }));

        let metadata = client
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

        let region_name = format!("vig_{logical}");
        let region = Region::create(&region_name, byte_size).expect("Shm-Region anlegen");

        // Aufraeumen, falls ein frueherer Lauf abgebrochen ist.
        let _ = client
            .raw()
            .await
            .expect("Backend erreichbar")
            .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
                name: region.name.clone(),
            })
            .await;
        client
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
            "  {logical:<9} -> {physical:<15} {:>6} KB  Periode {:>4} ms{}",
            byte_size / 1024,
            to_std(contract.deadline).as_millis(),
            if endpoint == resolved.backend_endpoint {
                String::new()
            } else {
                format!("  @ {endpoint}")
            }
        );

        prepared.push(Prepared {
            name: logical.clone(),
            physical,
            endpoint,
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

    let streams = |use_logical: bool, cap: usize, only: Option<&str>| -> Vec<StreamDef> {
        prepared
            .iter()
            .filter(|p| only.is_none_or(|endpoint| p.endpoint == endpoint))
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
                burst: None,
            })
            .collect()
    };

    let duration = Duration::from_secs(RUN_SECONDS);

    // --- Baseline: direkt zu Triton --------------------------------------
    let mut endpoints: Vec<String> = prepared.iter().map(|p| p.endpoint.clone()).collect();
    endpoints.sort();
    endpoints.dedup();
    let mut baseline: HashMap<String, StreamReport> = HashMap::new();
    for cap in CAPS {
        // Je Prozess ein Treiber, alle gleichzeitig. Bei einem einzigen
        // Endpunkt ist das genau der eine Aufruf von frueher.
        let mut runs = Vec::new();
        for endpoint in &endpoints {
            let defs = streams(false, cap, Some(endpoint));
            let endpoint = endpoint.clone();
            runs.push(tokio::spawn(async move {
                drive(&endpoint, &defs, duration, false).await
            }));
        }
        let mut reports = Vec::new();
        for run in runs {
            reports.extend(run.await.expect("Treiber beendet"));
        }
        for report in reports {
            let entry = baseline
                .entry(report.name.to_owned())
                .or_insert_with(|| report.clone());
            if report.coverage.covered_permille() > entry.coverage.covered_permille() {
                *entry = report;
            }
        }
    }

    // --- Mit Vigilant -----------------------------------------------------
    let clock = MonotonicClock::start();
    let handle =
        actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
    // Wie `vig serve`: der Governor beobachtet die Karte. Ohne das plant er
    // ohne Geraetezustand, und die Prognose (NV-06) haette nie eine Zelle.
    handle.observe_hardware();
    let service = GatewayService::new(Arc::clone(&resolved), triton, handle.clone(), clock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Port");
    let gateway = listener.local_addr().expect("Adresse").to_string();
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
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _warm = connect(&gateway).await;

    let mut governed: HashMap<String, StreamReport> = HashMap::new();
    for cap in CAPS {
        let reports = drive(&gateway, &streams(true, cap, None), duration, true).await;
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
    println!("\n  Strom     | Abdeckung Triton | Vigilant | Antwortalter p95 T | O | Faktor");
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
            a.coverage.response_age_p95_ns / 1_000_000,
            b.coverage.response_age_p95_ns / 1_000_000,
        );
    }

    // Die Verbrauchersicht daneben: was der Regler zum Abtastzeitpunkt
    // tatsaechlich vorliegen hatte, und wie lange er am Stueck ohne
    // brauchbares Ergebnis war. Eine Abdeckungszahl allein unterscheidet
    // verstreute Ausfaelle nicht von einem Block — fuer eine Regelung ist das
    // der ganze Unterschied.
    println!(
        "\n  Verbrauchersicht  | Abdeckung T | O | laengste Luecke T | O | mittlere AoI T | O"
    );
    println!(
        "  ------------------|-------------|------|-------------------|------|----------------|------"
    );
    for p in &prepared {
        let (Some(a), Some(b)) = (baseline.get(&p.name), governed.get(&p.name)) else {
            continue;
        };
        let share = |c: &vig_sim::coverage::Coverage| {
            c.consumer_covered
                .saturating_mul(100)
                .checked_div(c.total)
                .unwrap_or(0)
        };
        println!(
            "  {:<17} | {:>9} % | {:>3} % | {:>14} ms | {:>3} ms | {:>11} ms | {:>3} ms",
            p.name,
            share(&a.coverage),
            share(&b.coverage),
            a.coverage.longest_gap_ns / 1_000_000,
            b.coverage.longest_gap_ns / 1_000_000,
            a.coverage.mean_aoi_ns / 1_000_000,
            b.coverage.mean_aoi_ns / 1_000_000,
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
        // NV-06: ob die Prognose mutiger oder nur vorsichtiger war. Eine
        // Policy, die nur mehr ablehnt, haelt jede Zusage ein und ist
        // trotzdem wertlos — deshalb beide Richtungen getrennt.
        println!(
            "  Prognose ({}): verglichen {} ohne Zelle {} vorsichtiger {} mutiger {}",
            if m.predictor_active == 1 {
                "scharf"
            } else {
                "Schatten"
            },
            m.predictor_comparisons,
            m.predictor_fallbacks,
            m.predictor_more_conservative,
            m.predictor_more_optimistic,
        );
    }
    println!(
        "\nAbdeckung nach ADR-0005: Anteil der Perioden mit einem gelieferten Ergebnis,\n\
         dessen Alter unter max_age lag."
    );
}
