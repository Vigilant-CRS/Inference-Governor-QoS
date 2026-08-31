//! End-to-End-Verhalten des Gateways gegen ein echtes gRPC-Backend (Spec WP9).
//!
//! Geprueft wird die Zusage, die den Produktnutzen traegt: ein Standardclient
//! aendert nur den Zielendpunkt, und die Frische-Semantik wirkt auf dem Draht —
//! nicht nur im Simulator.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::manual_div_ceil,
    clippy::too_many_lines
)]

mod mock_backend;

use onetimer_config::Config;
use onetimer_gateway::{GatewayService, MonotonicClock, actor};
use onetimer_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient;
use onetimer_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use onetimer_protocol_oip::inference::infer_parameter::ParameterChoice;
use onetimer_protocol_oip::inference::model_infer_request::InferInputTensor;
use onetimer_protocol_oip::inference::{InferParameter, ModelInferRequest, ModelMetadataRequest};
use onetimer_protocol_oip::params::{P_CLASS, P_DEADLINE_US, P_MAX_AGE_US};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tonic::transport::Channel;

use mock_backend::MockBackend;

/// Baut eine Konfiguration mit einem LATEST-Modell und dem angegebenen Profil.
fn config_yaml(endpoint: &str, p50_us: u64, p99_us: u64, max_age_ms: u64) -> String {
    format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 1
  pipelining_depth: 0
models:
  detector:
    class: protected
    queue:
      policy: latest
      capacity: 1
    contract:
      period_ms: 20
      deadline_ms: 200
      max_age_ms: {max_age_ms}
    variants:
      - id: large
        backend_model: detector_large
        quality:
          value: 1.0
          source: measured
        profile: {{ p50_us: {p50_us}, p95_us: {p99_us}, p99_us: {p99_us}, samples: 1000 }}
"
    )
}

/// Startet Backend und Gateway und gibt Client und Backend zurueck.
async fn start(
    compute: Duration,
    p50_us: u64,
    p99_us: u64,
    max_age_ms: u64,
) -> (
    GrpcInferenceServiceClient<Channel>,
    Arc<MockBackend>,
    SocketAddr,
) {
    let backend = Arc::new(MockBackend::new(compute));
    let backend_address = mock_backend::start(Arc::clone(&backend)).await;

    let yaml = config_yaml(&backend_address.to_string(), p50_us, p99_us, max_age_ms);
    let config = Config::from_yaml(&yaml).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
    let resolved = Arc::new(config.resolve().unwrap());

    let clock = MonotonicClock::start();
    let triton = Arc::new(onetimer_backend_triton::TritonClient::new(
        backend_address.to_string(),
    ));
    let handle = actor::spawn(Arc::clone(&resolved), Arc::clone(&triton), clock).unwrap();
    let service = GatewayService::new(resolved, triton, handle, clock);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_address = listener.local_addr().unwrap();
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
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = tuned_client(&gateway_address.to_string()).await;
    (client, backend, backend_address)
}

/// Ein Client mit denselben Transportgrenzen wie der Governor.
///
/// Spec 19.1 verlangt, dass die Vergleichsseite nicht schlechter konfiguriert
/// ist als die eigene. Ein Direktclient mit tonic-Voreinstellungen wuerde bei
/// Tensornutzlasten kuenstlich langsam wirken.
async fn tuned_client(address: &str) -> GrpcInferenceServiceClient<Channel> {
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))
        .unwrap()
        .initial_stream_window_size(onetimer_backend_triton::STREAM_WINDOW_BYTES)
        .initial_connection_window_size(onetimer_backend_triton::CONNECTION_WINDOW_BYTES)
        .tcp_nodelay(true)
        .connect()
        .await
        .unwrap();
    GrpcInferenceServiceClient::new(channel)
        .max_decoding_message_size(onetimer_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
        .max_encoding_message_size(onetimer_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
}

/// Ein Request mit einer Nutzlast der angegebenen Groesse.
///
/// Bildet den gRPC-Copy-Pfad ab: die Tensordaten reisen im Request selbst.
fn request_with_payload(model: &str, id: u64, bytes: usize) -> ModelInferRequest {
    let mut r = request(model, id);
    r.raw_input_contents = vec![vec![0_u8; bytes]];
    r
}

/// Ein Request, der seine Nutzlast per Shared-Memory-Referenz uebergibt.
///
/// Der eigentliche Produktpfad (ADR-0003): im Request steht nur, **wo** die
/// Daten liegen — Regionsname, Offset, Groesse. OneTimer reicht diese Angaben
/// weiter und beruehrt die Tensordaten nie. Der Aufwand des Governors wird
/// damit unabhaengig von der Tensorgroesse.
fn request_with_shm_reference(
    model: &str,
    id: u64,
    region: &str,
    bytes: usize,
) -> ModelInferRequest {
    let mut r = request(model, id);
    let mut parameters = HashMap::new();
    parameters.insert(
        "shared_memory_region".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::StringParam(region.to_owned())),
        },
    );
    parameters.insert(
        "shared_memory_byte_size".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(
                i64::try_from(bytes).unwrap_or(i64::MAX),
            )),
        },
    );
    parameters.insert(
        "shared_memory_offset".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(0)),
        },
    );
    r.inputs = vec![InferInputTensor {
        name: "input".to_owned(),
        datatype: "UINT8".to_owned(),
        shape: vec![1, 1080, 1920, 3],
        parameters,
        contents: None,
    }];
    // Entscheidend: keine Rohdaten im Request.
    r.raw_input_contents = Vec::new();
    r
}

fn request(model: &str, id: u64) -> ModelInferRequest {
    ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: id.to_string(),
        parameters: HashMap::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        raw_input_contents: Vec::new(),
    }
}

/// Diagnose: wie gross sind die Futures, die auf dem Stack des Aufrufers landen?
#[tokio::test(flavor = "multi_thread")]
async fn report_future_sizes() {
    let backend = Arc::new(MockBackend::new(Duration::from_millis(1)));
    println!(
        "start()            = {} Bytes",
        std::mem::size_of_val(&start(Duration::from_millis(1), 1, 2, 3))
    );
    println!(
        "mock start()       = {} Bytes",
        std::mem::size_of_val(&mock_backend::start(backend))
    );
    let (mut client, _, _) = start(Duration::from_millis(1), 2_000, 3_000, 200).await;
    println!(
        "client.model_infer = {} Bytes",
        std::mem::size_of_val(&client.model_infer(request("detector", 1)))
    );
}

/// Spec L-001/L-002: ein Standardclient inferiert ohne kundenspezifisches SDK,
/// und ein unkonfiguriertes Modell wird unveraendert durchgereicht.
#[tokio::test(flavor = "multi_thread")]
async fn an_unconfigured_model_passes_through_unchanged() {
    let (mut client, backend, _) = start(Duration::from_millis(1), 2_000, 3_000, 200).await;

    let response = client
        .model_infer(request("irgendein_modell", 1))
        .await
        .unwrap();
    assert_eq!(response.into_inner().model_name, "irgendein_modell");
    assert_eq!(backend.served.load(Ordering::Relaxed), 1);
    assert_eq!(backend.seen_models.lock().unwrap()[0], "irgendein_modell");
}

/// Spec 12.1: der Client fragt das logische Modell, das Backend sieht die
/// gewaehlte physische Variante.
#[tokio::test(flavor = "multi_thread")]
async fn a_configured_model_is_mapped_to_its_physical_variant() {
    let (mut client, backend, _) = start(Duration::from_millis(1), 2_000, 3_000, 200).await;

    let response = client
        .model_infer(request("detector", 1))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.id, "1");
    assert_eq!(
        backend.seen_models.lock().unwrap()[0],
        "detector_large",
        "das Backend sieht die physische Variante"
    );

    // Und die Metadaten kommen unter dem logischen Namen zurueck.
    let meta = client
        .model_metadata(ModelMetadataRequest {
            name: "detector".to_owned(),
            version: String::new(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        meta.name, "detector",
        "der Client sieht seinen eigenen Namen"
    );
}

/// Der Kernnutzen auf dem Draht: bei einer Anfrageflut ueberlebt der juengste
/// wartende Request, die aelteren werden mit einer klaren Begruendung
/// abgewiesen (Spec 11.1, G-001).
#[tokio::test(flavor = "multi_thread")]
async fn a_burst_is_superseded_down_to_the_newest_request() {
    // Backend rechnet 60 ms, es kommen 12 Requests fast gleichzeitig.
    let (client, backend, _) = start(Duration::from_millis(60), 60_000, 70_000, 5_000).await;

    let mut tasks = Vec::new();
    for id in 1..=12_u64 {
        let mut client = client.clone();
        tasks.push(tokio::spawn(async move {
            client.model_infer(request("detector", id)).await
        }));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let mut served = 0_u32;
    let mut superseded = 0_u32;
    let mut other = Vec::new();
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => served += 1,
            Err(status) => {
                let reason = status
                    .metadata()
                    .get("onetimer-reason")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("?")
                    .to_owned();
                if reason == "superseded" {
                    superseded += 1;
                } else {
                    other.push(reason);
                }
            }
        }
    }

    println!("bedient={served} superseded={superseded} sonstige={other:?}");
    assert_eq!(
        served + superseded + u32::try_from(other.len()).unwrap_or(u32::MAX),
        12,
        "jeder Request endet terminal"
    );
    assert!(
        superseded > 0,
        "veraltete Requests muessen verdraengt werden"
    );
    assert!(served > 0, "es muss weiter gerechnet werden");

    // Der entscheidende Punkt: das Backend hat deutlich weniger Arbeit
    // ausgefuehrt, als Requests eingegangen sind.
    let executed = backend.served.load(Ordering::Relaxed);
    assert!(
        executed < 12,
        "das Backend darf nicht jeden Frame rechnen, waren {executed}"
    );
}

/// ADR-0011 und Spec 16.2: Clientparameter ueberschreiben den Vertrag, und ein
/// fehlerhafter Parameter fuehrt zur Ablehnung statt zu einem stillen Default.
#[tokio::test(flavor = "multi_thread")]
async fn client_parameters_are_honoured_and_validated() {
    let (mut client, _, _) = start(Duration::from_millis(1), 2_000, 3_000, 200).await;

    let mut request = request("detector", 1);
    request.parameters.insert(
        P_CLASS.to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::StringParam("high".to_owned())),
        },
    );
    request.parameters.insert(
        P_DEADLINE_US.to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(50_000)),
        },
    );
    assert!(client.model_infer(request).await.is_ok());

    // Ein unbekannter onetimer_-Parameter ist ein Fehler, kein Hinweis.
    let mut bad = self::request("detector", 2);
    bad.parameters.insert(
        "onetimer_deadline_ms".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(30)),
        },
    );
    let status = client.model_infer(bad).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status:?}");

    // Ein absurdes max_age ebenso.
    let mut absurd = self::request("detector", 3);
    absurd.parameters.insert(
        P_MAX_AGE_US.to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(i64::MAX)),
        },
    );
    let status = client.model_infer(absurd).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

/// Misst den Zusatzaufwand des Governors auf dem gRPC-Copy-Pfad.
///
/// ADR-0003 haelt fest, dass dieser Pfad nur der Bootstrap ist und der
/// Shm-Referenz-Passthrough der eigentliche Produktpfad. Die Zahl wird deshalb
/// **berichtet**, nicht als Gate behauptet: sie misst den Transport, nicht das
/// Scheduling, und Spec 4.4 verlangt ausdruecklich, beides getrennt
/// auszuweisen.
///
/// Gemessen wird bei mehreren Backend-Geschwindigkeiten, weil der relative
/// Anteil sonst allein davon abhaengt, wie schnell das Backend gerade ist —
/// eine Prozentzahl ohne Bezugsgroesse waere hier bedeutungslos.
#[tokio::test(flavor = "multi_thread")]
async fn report_proxy_overhead_on_the_grpc_copy_path() {
    println!("\n  Backend | direkt      | ueber OneTimer | Zusatz     | relativ");
    println!("  --------|-------------|----------------|------------|--------");

    for compute_ms in [1_u64, 5, 20] {
        let (mut via_gateway, _, backend_address) = start(
            Duration::from_millis(compute_ms),
            compute_ms * 1_000,
            compute_ms * 1_500,
            60_000,
        )
        .await;
        let mut direct = GrpcInferenceServiceClient::connect(format!("http://{backend_address}"))
            .await
            .unwrap();

        let rounds = if compute_ms >= 20 { 60_u32 } else { 200 };

        // Aufwaermen: Verbindungsaufbau und Codepfad-Erstbenutzung gehoeren
        // nicht in die Messung.
        for _ in 0..30 {
            let _ = direct.model_infer(request("detector_large", 0)).await;
            let _ = via_gateway.model_infer(request("detector", 0)).await;
        }

        let t0 = Instant::now();
        for id in 0..rounds {
            direct
                .model_infer(request("detector_large", u64::from(id)))
                .await
                .unwrap();
        }
        let direct_us = t0.elapsed().as_micros() / u128::from(rounds);

        let t1 = Instant::now();
        for id in 0..rounds {
            via_gateway
                .model_infer(request("detector", u64::from(id)))
                .await
                .unwrap();
        }
        let proxied_us = t1.elapsed().as_micros() / u128::from(rounds);

        let overhead = proxied_us.saturating_sub(direct_us);
        let relative = overhead
            .saturating_mul(100)
            .checked_div(direct_us)
            .unwrap_or(0);
        println!(
            "  {compute_ms:>4} ms | {direct_us:>6} us/R | {proxied_us:>9} us/R | \
{overhead:>5} us/R | {relative:>4} %"
        );

        // Sehr lockere Schranke: der Test soll eine Groessenordnungsregression
        // fangen, nicht die Schwankung einer Testmaschine bestrafen.
        assert!(
            proxied_us < direct_us.saturating_mul(3).max(4_000),
            "Zusatzaufwand ausser Verhaeltnis bei {compute_ms} ms: \
             direkt {direct_us} us, proxied {proxied_us} us"
        );
    }
    println!();
}

/// Misst, was der Governor auf dem gRPC-Copy-Pfad bei **echten Tensorgroessen**
/// kostet.
///
/// Das ist die Zahl, die ADR-0003 zum Kill-Kriterium erklaert: ein
/// 1920x1080x3-uint8-Frame sind 6,2 MB, und auf dem Copy-Pfad durchlaeuft die
/// Nutzlast pro Hop eine Deserialisierung und eine Reserialisierung. Der
/// Aufwand ist damit eine Eigenschaft des Transports, nicht des Schedulings —
/// und genau deshalb wird er getrennt ausgewiesen (Spec 4.4).
#[tokio::test(flavor = "multi_thread")]
async fn report_data_plane_overhead_by_payload_size() {
    println!("\n  Nutzlast   | direkt      | ueber OneTimer | Zusatz       | relativ");
    println!("  -----------|-------------|----------------|--------------|--------");

    // 224x224x3 (Klassifikation), 640x640x3 (YOLO), 1920x1080x3 (Vollbild).
    for (label, bytes) in [
        ("150 KB", 224 * 224 * 3_usize),
        ("1,2 MB", 640 * 640 * 3),
        ("6,2 MB", 1920 * 1080 * 3),
    ] {
        let (mut via_gateway, _, backend_address) =
            start(Duration::from_millis(5), 5_000, 7_000, 60_000).await;
        let mut direct = tuned_client(&backend_address.to_string()).await;

        let rounds = 40_u32;
        for _ in 0..10 {
            let _ = direct
                .model_infer(request_with_payload("detector_large", 0, bytes))
                .await;
            let _ = via_gateway
                .model_infer(request_with_payload("detector", 0, bytes))
                .await;
        }

        let t0 = Instant::now();
        for id in 0..rounds {
            direct
                .model_infer(request_with_payload("detector_large", u64::from(id), bytes))
                .await
                .unwrap();
        }
        let direct_us = t0.elapsed().as_micros() / u128::from(rounds);

        let t1 = Instant::now();
        for id in 0..rounds {
            via_gateway
                .model_infer(request_with_payload("detector", u64::from(id), bytes))
                .await
                .unwrap();
        }
        let proxied_us = t1.elapsed().as_micros() / u128::from(rounds);

        let overhead = proxied_us.saturating_sub(direct_us);
        let relative = overhead
            .saturating_mul(100)
            .checked_div(direct_us)
            .unwrap_or(0);
        println!(
            "  {label:>10} | {direct_us:>6} us/R | {proxied_us:>9} us/R | \
{overhead:>7} us/R | {relative:>4} %"
        );
    }
    // Zum Vergleich derselbe nominale Tensor, aber als Shm-Referenz.
    let (mut via_gateway, backend, backend_address) =
        start(Duration::from_millis(5), 5_000, 7_000, 60_000).await;
    let mut direct = tuned_client(&backend_address.to_string()).await;
    let bytes = 1920 * 1080 * 3_usize;

    for _ in 0..10 {
        let _ = direct
            .model_infer(request_with_shm_reference(
                "detector_large",
                0,
                "frames",
                bytes,
            ))
            .await;
        let _ = via_gateway
            .model_infer(request_with_shm_reference("detector", 0, "frames", bytes))
            .await;
    }

    let rounds = 40_u32;
    let t0 = Instant::now();
    for id in 0..rounds {
        direct
            .model_infer(request_with_shm_reference(
                "detector_large",
                u64::from(id),
                "frames",
                bytes,
            ))
            .await
            .unwrap();
    }
    let direct_us = t0.elapsed().as_micros() / u128::from(rounds);

    let t1 = Instant::now();
    for id in 0..rounds {
        via_gateway
            .model_infer(request_with_shm_reference(
                "detector",
                u64::from(id),
                "frames",
                bytes,
            ))
            .await
            .unwrap();
    }
    let proxied_us = t1.elapsed().as_micros() / u128::from(rounds);
    let overhead = proxied_us.saturating_sub(direct_us);
    let relative = overhead
        .saturating_mul(100)
        .checked_div(direct_us)
        .unwrap_or(0);
    println!(
        "  6,2 MB shm | {direct_us:>6} us/R | {proxied_us:>9} us/R | \
{overhead:>7} us/R | {relative:>4} %"
    );

    // Der Nachweis, dass die Referenz wirklich durchgereicht wurde und nicht
    // etwa aufgeloest: das Backend hat Rohdaten nie gesehen.
    assert_eq!(
        backend.raw_bytes_seen.load(Ordering::Relaxed),
        0,
        "auf dem Shm-Pfad duerfen keine Rohdaten uebertragen werden"
    );

    println!(
        "\n  Auf dem Copy-Pfad waechst der Anteil mit der Nutzlast; auf dem\n  \
         Shm-Pfad bleibt er flach. Genau das ist die Aussage von ADR-0003."
    );
}
