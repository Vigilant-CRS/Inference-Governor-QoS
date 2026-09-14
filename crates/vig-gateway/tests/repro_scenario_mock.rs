//! Die Szenariodatei des Reproduktionspakets, gefahren gegen ein echtes
//! gRPC-Backend — ohne GPU.
//!
//! Lastfall (c) verspricht eine Variantenwahl mit echten Modellen. Dieses
//! Versprechen ist waehrend der Entwicklung schon einmal **still** ausgefallen:
//! eine unvollstaendige Semantikangabe genuegt, und der Governor faehrt
//! stattdessen eine feste Variante, waehrend die Messung weiterhin
//! „Variantenwahl" heisst. `vig doctor` meldet das als eine Warnung unter
//! vielen.
//!
//! Hier wird es deshalb am Draht festgenagelt: Kann die grosse Variante ihre
//! Frist rechnerisch nicht halten, muss beim Backend die **kleine** ankommen.
//!
//! Geprueft wird die ausgelieferte Datei selbst und keine im Test erfundene
//! Konfiguration — sonst pruefte der Test sich selbst und nicht das, was ein
//! Anwender kopiert.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod mock_backend;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::ModelInferRequest;
use vig_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient;
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;

use mock_backend::MockBackend;

/// Die Vorlage, die `tools/repro/run.sh` ausliefert.
const SZENARIO: &str = include_str!("../../../tools/repro/scenarios/c-zwei-groessen.yaml");

/// Setzt ein Profil ein, wie `vig calibrate` es nach einer geglueckten
/// Messreihe schreiben wuerde. Die Vorlage traegt absichtlich keines.
fn mit_profil(yaml: &str, backend_model: &str, p50: u64, p95: u64, p99: u64) -> String {
    let anker = format!("        backend_model: {backend_model}\n");
    let ersatz = format!(
        "{anker}        profile: {{ p50_us: {p50}, p95_us: {p95}, p99_us: {p99}, samples: 200 }}\n"
    );
    let ersetzt = yaml.replace(&anker, &ersatz);
    assert_ne!(
        ersetzt, yaml,
        "Ankerzeile fuer {backend_model} nicht gefunden"
    );
    ersetzt
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

/// Unter einer Frist, die nur die kleine Variante haelt, muss die kleine
/// Variante beim Backend ankommen.
///
/// Die Zahlen sind so gewaehlt, dass die Entscheidung eindeutig ist: `r50`
/// traegt ein p99 von 30 ms, mit der Marge von 110 % aus der Vorlage also
/// 33 ms konservativ — genau die Frist. `r18` liegt bei 11 ms.
#[tokio::test(flavor = "multi_thread")]
async fn die_ausgelieferte_variantenvorlage_waehlt_unter_last_die_kleine() {
    // Das Mock rechnet fuer jede Anfrage gleich lang; die Wahl faellt allein
    // ueber die deklarierten Profile, und genau das soll hier geprueft werden.
    let backend = Arc::new(MockBackend::new(Duration::from_millis(2)));
    let backend_address = mock_backend::start(Arc::clone(&backend)).await;

    let yaml = SZENARIO.replace("__ENDPUNKT__", &backend_address.to_string());
    let yaml = mit_profil(&yaml, "rtdetr_r50", 28_000, 29_000, 30_000);
    let yaml = mit_profil(&yaml, "rtdetr_r18", 9_000, 10_000, 11_000);

    let config = Config::from_yaml(&yaml).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
    let resolved = Arc::new(config.resolve().unwrap());

    let clock = MonotonicClock::start();
    let triton = Arc::new(vig_backend_triton::TritonClient::new(
        backend_address.to_string(),
    ));
    let handle = actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, triton, handle, clock);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let stream = tonic::transport::server::TcpIncoming::from(listener).with_nodelay(Some(true));
        let _ = tonic::transport::Server::builder()
            .add_service(GrpcInferenceServiceServer::new(service))
            .serve_with_incoming(stream)
            .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = GrpcInferenceServiceClient::connect(format!("http://{gateway_address}"))
        .await
        .unwrap();

    // Im Takt des Vertrages, nicht so schnell wie moeglich: die Wahl haengt am
    // Schlupf bis zur naechsten Ankunft, nicht an einer Flut.
    for id in 0..20_u64 {
        let _ = client.model_infer(request("detektor", id)).await;
        tokio::time::sleep(Duration::from_millis(33)).await;
    }

    let gesehen = backend.seen_models.lock().unwrap().clone();
    assert!(
        !gesehen.is_empty(),
        "das Backend hat keine einzige Anfrage gesehen — der Aufbau ist kaputt, \
         nicht die Variantenwahl"
    );
    assert!(
        gesehen.iter().any(|m| m == "rtdetr_r18"),
        "die kleine Variante kam nie beim Backend an; gesehen wurden: {gesehen:?}. \
         Entweder ist die automatische Variantenwahl aus (Semantikangabe \
         unvollstaendig?) oder die Vorlage plant anders als angenommen."
    );
}
