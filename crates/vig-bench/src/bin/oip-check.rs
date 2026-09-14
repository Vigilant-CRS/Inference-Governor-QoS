//! `oip-check` — laeuft der Governor auch vor einem anderen OIP-Server?
//!
//! Alle Messungen dieses Projekts liefen bis hierher gegen NVIDIA Triton. Die
//! Behauptung „protokollkompatibel" stand damit im README, ohne je gegen
//! einen zweiten Server geprueft worden zu sein — und eine unbelegte
//! Behauptung ist in einer Wettbewerbsmatrix nichts wert.
//!
//! Dieses Werkzeug nimmt eine beliebige Konfiguration, startet den Governor
//! davor und schickt echten Verkehr durch. Es misst nicht die Leistung des
//! Servers — die haengt an dessen Hardware — sondern beantwortet die
//! Vorfrage: kommen Requests an, kommen Antworten zurueck, und regelt der
//! Governor dabei?

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::integer_division
)]

use std::sync::Arc;
use std::time::Duration;
use vig_bench::workload::{InputSpec, StreamDef, drive};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("Tokio-Runtime");
    runtime.block_on(run());
}

/// Baut je Modell einen Strom mit Eingaben aus den Modellmetadaten.
///
/// Die Nutzlast reist im Request: dieser Test soll auch gegen Server laufen,
/// die kein Shared Memory koennen — das ist ja gerade der Punkt.
async fn build_streams(resolved: &vig_config::schema::Resolved) -> Vec<StreamDef> {
    let probe = vig_backend_triton::TritonClient::new(&resolved.backend_endpoint);
    let mut streams = Vec::new();
    for (i, contract) in resolved.contracts.iter().enumerate() {
        let (Some(logical), Some(period), Some(max_age)) = (
            resolved.model_names.get(i),
            contract.period,
            contract.max_age,
        ) else {
            continue;
        };
        let Some(physical) = resolved
            .backend_models
            .get(i)
            .and_then(|v| v.first())
            .cloned()
        else {
            continue;
        };
        let Ok(metadata) = probe.model_metadata(&physical).await else {
            eprintln!("WARNUNG {physical}: Metadaten nicht abrufbar, Strom uebersprungen");
            continue;
        };
        let Some(tensor) = metadata.inputs.first() else {
            continue;
        };
        let shape: Vec<i64> = tensor
            .shape
            .iter()
            .enumerate()
            .map(|(pos, d)| if *d < 0 && pos == 0 { 1 } else { *d })
            .collect();
        let elements: i64 = shape.iter().copied().product();
        let byte_size = u64::try_from(elements).unwrap_or(0).saturating_mul(4);

        let name: &'static str = Box::leak(logical.clone().into_boxed_str());
        streams.push(StreamDef {
            text: None,
            name,
            model: name,
            period: Duration::from_nanos(period.as_nanos()),
            max_age: Duration::from_nanos(max_age.as_nanos()),
            in_flight_cap: 4,
            input: Some(InputSpec {
                name: tensor.name.clone(),
                datatype: tensor.datatype.clone(),
                shape,
                region: None,
                byte_size,
                payload: None,
            }),
            pump: false,
            burst: None,
        });
    }

    streams
}

async fn run() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Aufruf: oip-check <konfiguration.yaml> [sekunden]");
        std::process::exit(2);
    });
    let seconds: u64 = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    let text = std::fs::read_to_string(&path).expect("Konfiguration lesbar");
    let config = Config::from_yaml(&text).expect("Konfiguration gueltig");
    let findings = config.diagnose();
    assert!(
        findings.is_empty(),
        "Konfiguration hat Befunde: {findings:?}"
    );
    let resolved = Arc::new(config.resolve().expect("aufloesbar"));

    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(
        &resolved.backend_endpoint,
    ));

    // Was der Server ueber sich meldet, gehoert in den Bericht: ohne Shared
    // Memory laeuft ein anderer Datenpfad, und das erklaert spaeter die Zahlen.
    if let Ok(mut raw) = backend.raw().await
        && let Ok(response) = raw
            .server_metadata(vig_protocol_oip::inference::ServerMetadataRequest {})
            .await
    {
        let meta = response.into_inner();
        let caps = vig_backend_triton::Capabilities::from_metadata(&meta);
        println!("Server:        {} {}", meta.name, meta.version);
        println!(
            "Shared Memory: {}",
            if caps.can_pass_references() {
                "ja — Referenzpfad"
            } else {
                "nein — Kopierpfad"
            }
        );
    }

    let handle = actor::spawn(Arc::clone(&resolved), &backend, clock, &[]).expect("Scheduler");
    let service = GatewayService::new(Arc::clone(&resolved), backend, handle, clock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Port");
    let gateway = listener.local_addr().expect("Adresse").to_string();
    tokio::spawn(async move {
        let stream = vig_bench::incoming(listener);
        let _ = tonic::transport::Server::builder()
            .add_service(GrpcInferenceServiceServer::new(service))
            .serve_with_incoming(stream)
            .await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let streams = build_streams(&resolved).await;

    println!("Governor:      {gateway}");
    println!("Stroeme:       {}\n", streams.len());

    let reports = drive(&gateway, &streams, Duration::from_secs(seconds), true).await;

    println!("  Strom      | erzeugt | gesendet | geliefert | abgewiesen | unabgedeckt");
    println!("  -----------|---------|----------|-----------|------------|------------");
    let mut delivered_total = 0_u64;
    for r in &reports {
        delivered_total += r.delivered;
        println!(
            "  {:<10} | {:>7} | {:>8} | {:>9} | {:>10} | {:>8} ‰",
            r.name,
            r.emitted,
            r.sent,
            r.delivered,
            r.rejected,
            r.coverage.uncovered_permille()
        );
    }

    println!();
    if delivered_total == 0 {
        println!("ERGEBNIS keine einzige Antwort — der Governor regelt diesen Server nicht.");
        std::process::exit(1);
    }
    println!("ERGEBNIS {delivered_total} Antworten ueber den Governor.");
}
