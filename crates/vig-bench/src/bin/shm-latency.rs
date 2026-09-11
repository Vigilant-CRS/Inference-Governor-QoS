//! `shm-latency` — was kostet der Transport, was das Backend, und haelt das
//! Datenpfadbudget?
//!
//! Die Messung, die NV-09 offen liess (siehe
//! `docs/spikes/nv09-tensorrt-direct.md`). Ein direkter TensorRT-Aufruf im
//! Prozess dauert fuer `pose_main` 1795 us; ueber gRPC ohne Shared Memory
//! misst der Client 5050 us. Die Frage ist, wie viel davon der **Transport**
//! ist — denn den hat dieses Projekt mit ADR-0003 laengst adressiert, und der
//! Rest waere der Gewinn eines eigenen Executors.
//!
//! Deshalb hier derselbe Aufruf ueber **System Shared Memory**: der Tensor
//! reist als Referenz, der Request traegt wenige hundert Byte.
//!
//! Bewusst ein eigenes Werkzeug und keine Option an `vig profile`: `profile`
//! schreibt Vertragsprofile, und ein Profil, das ueber Shared Memory gemessen
//! wurde, gilt nur fuer Clients, die Shared Memory benutzen. Das ist eine
//! Aussage ueber den Aufbau und gehoert nicht unbemerkt in eine
//! Konfiguration.
//!
//! ## Das Datenpfadbudget (NV-20)
//!
//! Derselbe Aufruf laeuft ein zweites Mal ueber einen Governor im selben
//! Prozess — gestartet wie in `gate-m3` —, **abwechselnd** Request um Request
//! mit dem direkten, damit eine Drift der Maschine nicht einer Seite allein
//! zufaellt. Das Urteil kommt aus derselben Tabelle wie die Mock-Pruefung
//! (`vig_gateway::datapath_budget`), und der Exitcode ist 1, wenn es nicht
//! PASS lautet: ein Freigabeskript soll daran scheitern koennen, ohne Text zu
//! lesen. Siehe `docs/datapath-budgets.md`.
//!
//! Der Hardwarewaechter laeuft dabei nicht. Gemessen wird der Datenpfad, nicht
//! die Beobachtung der Karte.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines
)]

use std::sync::Arc;
use std::time::{Duration, Instant};
use vig_backend_triton::TritonClient;
use vig_bench::shm::Region;
use vig_config::Config;
use vig_gateway::datapath_budget::{self, Latency};
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::model_infer_request::{
    InferInputTensor, InferRequestedOutputTensor,
};
use vig_protocol_oip::inference::{
    InferParameter, ModelInferRequest, SystemSharedMemoryRegisterRequest,
    SystemSharedMemoryUnregisterRequest, infer_parameter,
};

/// Aufwaermlaeufe je Seite, die nicht gezaehlt werden.
const WARMUP: usize = 30;
/// Direkte Laeufe, aus denen das Profil des Governors entsteht.
///
/// Die Konfiguration verlangt mindestens 100 Messungen je Profil — und ein
/// Profil, das nicht gemessen ist, waere hier eine erfundene Zahl.
const CALIBRATION: usize = 100;
/// Der logische Name, unter dem der Governor das Modell fuehrt.
const LOGICAL: &str = "probe";

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8001".to_owned());
    let model = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "pose_main".to_owned());
    let runs: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);

    let client = TritonClient::new(&endpoint);
    let metadata = client
        .model_metadata(&model)
        .await
        .expect("Modellmetadaten");
    let input = metadata.inputs.first().expect("ein Eingang").clone();
    let shape: Vec<i64> = input.shape.iter().map(|d| (*d).max(1)).collect();
    let elements: i64 = shape.iter().product();
    let bytes = usize::try_from(elements).unwrap_or(0).saturating_mul(4);

    let region = Region::create(
        &format!("vig_shmlat_{model}"),
        u64::try_from(bytes).unwrap_or(0),
    )
    .expect("Shm-Region");
    let raw = client.raw().await.expect("Backend erreichbar");
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
            byte_size: u64::try_from(bytes).unwrap_or(0),
        })
        .await
        .expect("Shm-Region registrieren");

    let request = ModelInferRequest {
        model_name: model.clone(),
        inputs: vec![InferInputTensor {
            name: input.name.clone(),
            datatype: input.datatype.clone(),
            shape,
            parameters: shm_parameters(&region.name, bytes),
            contents: None,
        }],
        outputs: metadata
            .outputs
            .iter()
            .map(|o| InferRequestedOutputTensor {
                name: o.name.clone(),
                parameters: std::collections::HashMap::new(),
            })
            .collect(),
        ..Default::default()
    };

    for _ in 0..WARMUP {
        let _ = client.infer(request.clone()).await;
    }

    // Das Profil, mit dem der Governor plant: gemessen, nicht angenommen.
    let mut calibration = Vec::with_capacity(CALIBRATION);
    for _ in 0..CALIBRATION {
        if let Some(us) = once(&client, &request).await {
            calibration.push(us);
        }
    }
    let profile = Latency::from_samples(&mut calibration).expect("Kalibrierung ohne Messwerte");

    // Der Governor im selben Prozess, vor demselben Triton. Die Shm-Region
    // ist bei Triton registriert; der Governor reicht die Referenz durch.
    let gateway = start_governor(&endpoint, &model, profile).await;
    let governed_client = TritonClient::new(gateway);
    let mut governed_request = request.clone();
    LOGICAL.clone_into(&mut governed_request.model_name);
    for _ in 0..WARMUP {
        let _ = governed_client.infer(governed_request.clone()).await;
    }

    let mut direct = Vec::with_capacity(runs);
    let mut governed = Vec::with_capacity(runs);
    let mut errors = 0_usize;
    for _ in 0..runs {
        match once(&client, &request).await {
            Some(us) => direct.push(us),
            None => errors += 1,
        }
        match once(&governed_client, &governed_request).await {
            Some(us) => governed.push(us),
            None => errors += 1,
        }
    }

    println!("{model} ueber System Shared Memory, {runs} Laeufe je Seite, abwechselnd:");
    println!("  Nutzlast {} KB, als Referenz uebertragen", bytes / 1024);

    let passed = if let (Some(d), Some(g)) = (
        Latency::from_samples(&mut direct),
        Latency::from_samples(&mut governed),
    ) {
        println!(
            "  direkt          p50 {} us | p99 {} us",
            d.p50_us, d.p99_us
        );
        println!(
            "  ueber Vigilant  p50 {} us | p99 {} us",
            g.p50_us, g.p99_us
        );
        let a = datapath_budget::assess(datapath_budget::shared_memory(), d, g);
        let portion = a.relative_permille.map_or_else(
            || {
                format!(
                    "nicht bewertet, direkter Aufruf unter {} ms",
                    datapath_budget::REFERENCE_CALL_US / 1_000
                )
            },
            |p| format!("{},{} %", p / 10, p % 10),
        );
        println!(
            "  Zusatz          p50 +{} us | p99 +{} us | Anteil {portion}",
            a.overhead_p50_us, a.overhead_p99_us
        );
        if errors > 0 {
            // Ein Lauf mit fehlgeschlagenen Aufrufen ist kein gueltiger
            // Nachweis — die Fehler koennten genau die langsamen gewesen
            // sein.
            println!(
                "\nDatenpfadbudget Shm-Referenz: KEIN URTEIL — {errors} Aufrufe \
                 fehlgeschlagen und nicht gezaehlt"
            );
            false
        } else {
            println!("\nDatenpfadbudget Shm-Referenz: {a}");
            !a.outcome.is_fail()
        }
    } else {
        println!("\nDatenpfadbudget Shm-Referenz: KEIN URTEIL — keine Messwerte");
        false
    };

    let _ = raw
        .clone()
        .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
            name: region.name.clone(),
        })
        .await;

    if !passed {
        std::process::exit(1);
    }
}

/// Ein Aufruf, in Mikrosekunden; `None`, wenn er fehlschlug.
///
/// Der Request wird vor dem Zeitnehmen kopiert: das ist Aufwand des Clients,
/// nicht des Pfads.
async fn once(client: &TritonClient, request: &ModelInferRequest) -> Option<u64> {
    let request = request.clone();
    let started = Instant::now();
    client.infer(request).await.ok()?;
    u64::try_from(started.elapsed().as_micros()).ok()
}

/// Startet einen Governor vor `endpoint`, der `model` als [`LOGICAL`] fuehrt.
///
/// Ein Modell, ein Slot, keine Pipelinetiefe, eine grosszuegige Deadline: der
/// Governor soll hier nichts entscheiden muessen ausser durchzureichen, damit
/// die Messung den Pfad misst und nicht eine Wartezeit.
async fn start_governor(endpoint: &str, model: &str, profile: Latency) -> String {
    let yaml = format!(
        r#"version: 1
backend:
  type: triton
  grpc_endpoint: "{endpoint}"
  slots: 1
  pipelining_depth: 0
models:
  {LOGICAL}:
    class: protected
    queue: {{ policy: fifo, capacity: 4 }}
    contract: {{ deadline_ms: 10000 }}
    variants:
      - id: main
        backend_model: {model}
        quality: {{ value: 1.0, source: user_declared }}
        profile: {{ p50_us: {p50}, p95_us: {p99}, p99_us: {p99}, samples: {CALIBRATION} }}
"#,
        p50 = profile.p50_us,
        p99 = profile.p99_us,
    );
    let resolved = Arc::new(
        Config::from_yaml(&yaml)
            .expect("Konfiguration gueltig")
            .resolve()
            .expect("aufloesbar"),
    );
    let triton = Arc::new(TritonClient::new(endpoint.to_owned()));
    let clock = MonotonicClock::start();
    let handle =
        actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
    let service = GatewayService::new(resolved, triton, handle, clock);

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
    tokio::time::sleep(Duration::from_millis(200)).await;
    address
}

/// Die Parameter, mit denen ein Tensor als Shm-Referenz reist.
fn shm_parameters(name: &str, bytes: usize) -> std::collections::HashMap<String, InferParameter> {
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
