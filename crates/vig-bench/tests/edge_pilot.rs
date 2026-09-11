//! Funktionstest des Edge-Piloten ohne GPU.
//!
//! Die Metriken sind einzeln getestet (`pilot::tests`). Hier laeuft die ganze
//! Kette: Kameras im Echtzeittakt, Governor-Gateway, ein Backend, das eine
//! feste Alarmdetektion zurueckgibt, und die Auswertung gegen eine
//! synthetische Annotation. Wenn eine dieser Nahtstellen die Detektion
//! verliert — Ausgabetensoren nicht durchgereicht, falsch dekodiert, dem
//! falschen Frame zugeordnet —, faellt es hier auf und nicht erst in einer
//! Nachtmessung.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use vig_bench::backend::{self, Backend};
use vig_bench::pilot::{
    self, ArmConfig, ArmReport, ArrivalRule, BoxN, CameraDef, GtObject, Sequence,
};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::model_infer_response::InferOutputTensor;
use vig_sim::workload::RuntimeDistribution;

const SIDE: usize = 8;
const FRAMES: usize = 100;

fn tensor(name: &str, shape: Vec<i64>) -> InferOutputTensor {
    InferOutputTensor {
        name: name.to_owned(),
        datatype: "FP32".to_owned(),
        shape,
        parameters: HashMap::new(),
        contents: None,
    }
}

/// Ein Alarmobjekt (Klasse 14) mitten im Bild, in jeder Antwort.
fn alarm_output() -> backend::FixedOutput {
    let dets: Vec<u8> = [0.5_f32, 0.5, 0.2, 0.2]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let mut logits = [-6.0_f32; 24];
    logits[14] = 6.0;
    let labels: Vec<u8> = logits.iter().flat_map(|v| v.to_le_bytes()).collect();
    (
        vec![
            tensor("labels", vec![1, 1, 24]),
            tensor("dets", vec![1, 1, 4]),
        ],
        vec![labels, dets],
    )
}

/// 25 fps, 100 Frames: die erste Haelfte ist ein Clip ohne Alarmobjekt, ab Frame
/// 50 steht das Objekt genau dort, wo das Backend sie meldet.
fn sequence() -> Arc<Sequence> {
    let object = BoxN {
        x1: 0.4,
        y1: 0.4,
        x2: 0.6,
        y2: 0.6,
    };
    let mut gt = vec![Vec::new(); FRAMES];
    for frame in gt.iter_mut().skip(50) {
        frame.push(GtObject {
            id: 1,
            bbox: object,
            visibility: 1.0,
        });
    }
    Arc::new(
        Sequence::from_frames_with("synth", 25, gt, ArrivalRule::FirstAppearance)
            .with_negative(&[(0, 50)]),
    )
}

fn camera(model: &str) -> CameraDef {
    CameraDef {
        name: "cam0".to_owned(),
        model: model.to_owned(),
        sequence: sequence(),
        frames: Arc::new(vec![128_u8; SIDE * SIDE * 3 * FRAMES]),
        size: SIDE,
        rate_hz: 25.0,
        regions: Vec::new(),
        input: (
            "input".to_owned(),
            "FP32".to_owned(),
            vec![
                1,
                3,
                i64::try_from(SIDE).unwrap(),
                i64::try_from(SIDE).unwrap(),
            ],
        ),
        target_classes: vec![13, 14, 15],
        iou_threshold: 0.3,
        classes: 24,
        ideal: None,
    }
}

async fn start_backend() -> String {
    let mut runtimes = HashMap::new();
    runtimes.insert(
        "rfdetr".to_owned(),
        RuntimeDistribution::constant(vig_core::Duration::from_millis(2).unwrap()),
    );
    let backend = Arc::new(Backend::new(1, runtimes, 7).with_fixed_output(alarm_output()));
    backend::start(backend).await.to_string()
}

async fn start_gateway(backend: &str) -> String {
    let yaml = format!(
        "version: 1\nbackend:\n  type: triton\n  grpc_endpoint: \"{backend}\"\n  slots: 1\n  \
         pipelining_depth: 0\nmodels:\n  cam0:\n    class: protected\n    \
         queue: {{ policy: latest, capacity: 1 }}\n    \
         contract: {{ period_ms: 40, deadline_ms: 40, max_age_ms: 80 }}\n    \
         variants:\n      - id: v4\n        backend_model: rfdetr\n        \
         quality: {{ value: 1.0, source: user_declared }}\n        \
         profile: {{ p50_us: 2000, p95_us: 2500, p99_us: 3000, samples: 100 }}\n"
    );
    let resolved = Arc::new(Config::from_yaml(&yaml).unwrap().resolve().unwrap());
    let clock = MonotonicClock::start();
    let triton = Arc::new(vig_backend_triton::TritonClient::new(backend.to_owned()));
    let handle = actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, triton, handle, clock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let incoming = vig_bench::incoming(listener);
        let _ = tonic::transport::Server::builder()
            .add_service(GrpcInferenceServiceServer::new(service))
            .serve_with_incoming(incoming)
            .await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    address
}

fn check(label: &str, report: &ArmReport) {
    let cam = &report.cameras[0];
    assert!(
        cam.delivered > 50,
        "{label}: zu wenige Lieferungen: {cam:?}"
    );
    assert_eq!(cam.rejected, 0, "{label}: {cam:?}");
    // Das Objekt kommt bei 2,0 s an; der Lauf dauert 4 s, die Frist 2 s.
    assert_eq!(cam.alarm.events, 1, "{label}: {:?}", cam.alarm);
    assert_eq!(cam.alarm.missed, 0, "{label}: {:?}", cam.alarm);
    assert!(
        cam.alarm.latencies_ms[0] < 200,
        "{label}: Alarm nach {} ms",
        cam.alarm.latencies_ms[0]
    );
    // Das Backend meldet immer ein Objekt: auf dem Clip ohne Objekt ist jede
    // Lieferung ein Fehlalarm.
    assert!(cam.negative_deliveries > 0, "{label}: {cam:?}");
    assert_eq!(
        cam.false_alarms, cam.negative_deliveries,
        "{label}: {cam:?}"
    );
    assert!(
        cam.recall.hits > 0 && cam.recall.hits <= cam.recall.total,
        "{label}: {:?}",
        cam.recall
    );
    assert!(
        cam.coverage.covered_permille() > 900,
        "{label}: {:?}",
        cam.coverage
    );
    // Kopierpfad: kein Puffer, also nichts erschoepft und nichts gesperrt.
    assert_eq!(
        (cam.buffers_exhausted, cam.buffers_quarantined),
        (0, 0),
        "{label}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pilot_pipeline_runs_end_to_end_without_a_gpu() {
    let backend = start_backend().await;
    let run = |endpoint: String, model: &str, via_governor: bool| ArmConfig {
        endpoint,
        via_governor,
        cameras: vec![camera(model)],
        in_flight_cap: 2,
        duration: Duration::from_secs(4),
        llm: None,
    };

    let direct = Box::pin(pilot::run_arm(run(backend.clone(), "rfdetr", false)))
        .await
        .unwrap();
    check("direkt", &direct);

    let gateway = start_gateway(&backend).await;
    let governed = Box::pin(pilot::run_arm(run(gateway, "cam0", true)))
        .await
        .unwrap();
    check("ueber den Governor", &governed);
}
