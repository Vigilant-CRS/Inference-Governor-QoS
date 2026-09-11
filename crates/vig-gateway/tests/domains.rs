//! Ressourcendomaenen im Zusammenspiel, ohne GPU (NV-22, ADR-0037).
//!
//! Zwei Fake-Executoren stehen fuer zwei GPUs. Geprueft wird, was die
//! Abnahme verlangt: jeder Auftrag erreicht den Besitzer seiner GPU; eine
//! belegte GPU haelt die andere nicht auf; ein Ausfall auf einer GPU nimmt
//! der anderen keinen Kredit; die Kennzahlen stehen im globalen Modellindex
//! und je Domaene. Und ohne Domaenen bleibt es ein einziger Actor.

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
use vig_core::{
    Criticality, Instant, ModelIdx, PayloadRef, QueuePolicy, RequestDescriptor, RequestId,
    SupersessionKey,
};
use vig_gateway::budget::PayloadPermit;
use vig_gateway::executor::Executor;
use vig_gateway::testing::{FakeExecutor, request_for};
use vig_gateway::{MonotonicClock, actor, exporter};

const GPU0: &str = "127.0.0.1:59990";
const GPU1: &str = "127.0.0.1:59991";

/// GPU 0 rechnet den Detektor, GPU 1 das Sprachmodell.
const TWO_GPUS: &str = r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:59990"
  slots: 1
  pipelining_depth: 0
  inference_timeout_ms: 5000
  domains:
    gpu1:
      gpu_index: 1
      grpc_endpoint: "127.0.0.1:59991"
      slots: 1
      pipelining_depth: 0
models:
  detector:
    class: protected
    queue: { policy: fifo, capacity: 64 }
    contract: { period_ms: 33, deadline_ms: 33 }
    variants:
      - id: main
        backend_model: detector_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }
  vlm:
    class: best_effort
    domain: gpu1
    queue: { policy: fifo, capacity: 4 }
    contract: { deadline_ms: 10000 }
    variants:
      - id: main
        backend_model: vlm_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 90000, p95_us: 90000, p99_us: 90000, samples: 1000 }
"#;

/// Dieselben zwei Modelle auf **einer** GPU mit einem Slot — die Gegenprobe.
const ONE_GPU: &str = r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:59990"
  slots: 1
  pipelining_depth: 0
  inference_timeout_ms: 5000
models:
  detector:
    class: normal
    queue: { policy: fifo, capacity: 64 }
    contract: { deadline_ms: 10000 }
    variants:
      - id: main
        backend_model: detector_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }
  vlm:
    class: normal
    queue: { policy: fifo, capacity: 4 }
    contract: { deadline_ms: 10000 }
    variants:
      - id: main
        backend_model: vlm_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 90000, p95_us: 90000, p99_us: 90000, samples: 1000 }
"#;

/// Globale Indizes: die Modelle stehen in Namensreihenfolge.
const DETECTOR: ModelIdx = ModelIdx(0);
const VLM: ModelIdx = ModelIdx(1);

fn spawn(yaml: &str, backends: &[(&str, &Arc<FakeExecutor>)]) -> vig_gateway::Handle {
    let resolved = Arc::new(
        vig_config::Config::from_yaml(yaml)
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let mut map: HashMap<String, Arc<dyn Executor>> = HashMap::new();
    for (endpoint, fake) in backends {
        map.insert(
            (*endpoint).to_owned(),
            Arc::clone(fake) as Arc<dyn Executor>,
        );
    }
    actor::spawn_with(resolved, map, MonotonicClock::start(), &[]).unwrap()
}

fn descriptor(
    id: u64,
    model: ModelIdx,
    criticality: Criticality,
    now: Instant,
) -> RequestDescriptor {
    RequestDescriptor {
        id: RequestId(id),
        logical_model: model,
        supersession_key: SupersessionKey(id),
        generation_time: now,
        arrival_time: now,
        absolute_deadline: None,
        max_age: None,
        criticality,
        queue_policy: QueuePolicy::Fifo,
        stateful: false,
        variant: None,
        payload: PayloadRef::default(),
        context_tokens: 0,
        decomposable: false,
    }
}

/// Wartet, bis die Bedingung gilt, hoechstens zwei Sekunden.
async fn eventually(mut condition: impl FnMut() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("Bedingung nach zwei Sekunden nicht erfuellt");
}

/// Jeder Auftrag erreicht das Backend seiner GPU, und der Scheduler dort
/// kennt das Modell unter seinem eigenen Index.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_request_reaches_the_backend_of_its_gpu() {
    let (gpu0, gpu1) = (
        Arc::new(FakeExecutor::default()),
        Arc::new(FakeExecutor::default()),
    );
    gpu0.expect_ok("detector_main");
    gpu1.expect_ok("vlm_main");
    let handle = spawn(TWO_GPUS, &[(GPU0, &gpu0), (GPU1, &gpu1)]);
    assert_eq!(handle.domain_count(), 2);
    let clock = MonotonicClock::start();

    handle
        .submit(
            descriptor(1, DETECTOR, Criticality::Protected, clock.now()),
            request_for("detector"),
            PayloadPermit::untracked(),
        )
        .await
        .unwrap();
    handle
        .submit(
            descriptor(2, VLM, Criticality::BestEffort, clock.now()),
            request_for("vlm"),
            PayloadPermit::untracked(),
        )
        .await
        .unwrap();

    assert_eq!(gpu0.executed_models(), ["detector_main"]);
    assert_eq!(gpu1.executed_models(), ["vlm_main"]);

    // Je Domaene, in ihrem eigenen Index ...
    let parts = handle.domain_metrics().await.unwrap();
    let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["default", "gpu1"]);
    assert_eq!(parts[1].gpu_index, 1);
    assert_eq!(
        (parts[0].metrics.received, parts[1].metrics.received),
        (1, 1)
    );
    assert_eq!((parts[0].metrics.slots, parts[1].metrics.slots), (1, 1));

    // ... und als Gesamtsicht im globalen: dieselben Reihen wie ohne Domaenen.
    let total = handle.metrics().await.unwrap();
    assert_eq!(total.received, 2);
    assert_eq!(total.completed_valid, 2);
    assert_eq!(total.models, 2);
    assert_eq!(total.slots, 2);
    assert_eq!(total.margin_percent[..2], [110, 110]);

    let text = exporter::render_domains(&parts);
    assert!(
        text.contains("vig_domain_info{domain=\"gpu1\",gpu=\"1\"} 1"),
        "{text}"
    );
    assert!(text.contains("vig_domain_requests_completed_valid_total{domain=\"default\"} 1"));

    assert!(handle.drain(Duration::from_secs(2)).await.unwrap());
}

/// Eine belegte GPU haelt die andere nicht auf.
///
/// Das Sprachmodell rechnet eine Sekunde auf GPU 1. Waehrenddessen laufen auf
/// GPU 0 drei Detektorframes durch — geschuetzt, mit 33-ms-Periode, und der
/// Look-ahead auf GPU 1 hat davon nichts zurueckgehalten. Die Gegenprobe auf
/// einer GPU mit einem Slot zeigt, dass es ohne Domaenen anders waere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_gpu_does_not_hold_up_the_other() {
    let (gpu0, gpu1) = (
        Arc::new(FakeExecutor::default()),
        Arc::new(FakeExecutor::default()),
    );
    for _ in 0..3 {
        gpu0.expect_ok("detector_main");
    }
    gpu1.expect_slow("vlm_main", Duration::from_secs(1));
    let handle = spawn(TWO_GPUS, &[(GPU0, &gpu0), (GPU1, &gpu1)]);
    let clock = MonotonicClock::start();

    let background = handle.clone();
    let vlm = tokio::spawn(async move {
        background
            .submit(
                descriptor(1, VLM, Criticality::BestEffort, clock.now()),
                request_for("vlm"),
                PayloadPermit::untracked(),
            )
            .await
    });
    eventually(|| gpu1.executed() == 1).await;

    for id in 10..13 {
        handle
            .submit(
                descriptor(id, DETECTOR, Criticality::Protected, clock.now()),
                request_for("detector"),
                PayloadPermit::untracked(),
            )
            .await
            .unwrap();
    }
    assert!(
        !vlm.is_finished(),
        "die Detektorframes waren fertig, bevor das Sprachmodell es war"
    );
    let parts = handle.domain_metrics().await.unwrap();
    assert_eq!(parts[1].metrics.deferred_for_protected, 0);
    vlm.await.unwrap().unwrap();

    // Gegenprobe: eine GPU, ein Slot. Der Detektor wartet auf das
    // Sprachmodell, weil beide dieselbe Recheneinheit brauchen.
    let shared = Arc::new(FakeExecutor::default());
    shared.expect_slow("vlm_main", Duration::from_secs(1));
    shared.expect_ok("detector_main");
    let single = spawn(ONE_GPU, &[(GPU0, &shared)]);
    assert_eq!(single.domain_count(), 1);
    let background = single.clone();
    let vlm = tokio::spawn(async move {
        background
            .submit(
                descriptor(1, VLM, Criticality::Normal, clock.now()),
                request_for("vlm"),
                PayloadPermit::untracked(),
            )
            .await
    });
    eventually(|| shared.executed() == 1).await;
    let started = std::time::Instant::now();
    single
        .submit(
            descriptor(2, DETECTOR, Criticality::Normal, clock.now()),
            request_for("detector"),
            PayloadPermit::untracked(),
        )
        .await
        .unwrap();
    // Eine Untergrenze, keine Zeitmessung: der Slot wird erst frei, wenn das
    // Sprachmodell nach seiner Sekunde antwortet.
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "der Detektor lief, obwohl der einzige Slot belegt war"
    );
    eventually(|| vlm.is_finished()).await;
    assert_eq!(shared.executed_models(), ["vlm_main", "detector_main"]);
}

/// Ein Ausfall auf GPU 1 nimmt GPU 0 keinen Kredit.
///
/// Das Backend auf GPU 1 antwortet nicht mehr: nach dem Timeout steht sein
/// einziger Slot in Quarantaene, und jeder weitere Auftrag dort wird sofort
/// abgewiesen. GPU 0 liefert weiter, ihr Slot ist frei, und die
/// Bereitschaftspruefung nennt GPU 1 und nur sie.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_gpu_takes_no_credit_from_the_other() {
    let (gpu0, gpu1) = (
        Arc::new(FakeExecutor::default()),
        Arc::new(FakeExecutor::default()),
    );
    gpu0.expect_ok("detector_main");
    gpu0.expect_ok("detector_main");
    gpu1.expect_slow("vlm_main", Duration::from_secs(30));
    // Ein kurzes Timeout: nach 200 ms gilt das Backend auf GPU 1 als haengend.
    let yaml = TWO_GPUS.replace("inference_timeout_ms: 5000", "inference_timeout_ms: 200");
    let handle = spawn(&yaml, &[(GPU0, &gpu0), (GPU1, &gpu1)]);
    let clock = MonotonicClock::start();

    let hung = handle
        .submit(
            descriptor(1, VLM, Criticality::BestEffort, clock.now()),
            request_for("vlm"),
            PayloadPermit::untracked(),
        )
        .await;
    assert!(
        hung.is_err(),
        "der Client bekommt nach dem Timeout eine Antwort"
    );
    let refused = handle
        .submit(
            descriptor(2, VLM, Criticality::BestEffort, clock.now()),
            request_for("vlm"),
            PayloadPermit::untracked(),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::Unavailable, "{refused:?}");

    for id in 10..12 {
        handle
            .submit(
                descriptor(id, DETECTOR, Criticality::Protected, clock.now()),
                request_for("detector"),
                PayloadPermit::untracked(),
            )
            .await
            .unwrap();
    }

    let parts = handle.domain_metrics().await.unwrap();
    assert_eq!(
        parts[0].metrics.quarantined, 0,
        "GPU 0 haelt keinen fremden Kredit"
    );
    assert_eq!(parts[1].metrics.quarantined, 1);
    assert_eq!(parts[1].metrics.rejected_quarantined, 1);
    assert_eq!(parts[0].metrics.completed_valid, 2);

    // Die Erreichbarkeitsprobe braucht einen Durchlauf, bevor GPU 0 als
    // geprueft gilt; danach nennt die Bereitschaft nur GPU 1.
    for _ in 0..200 {
        let parts = handle.domain_metrics().await.unwrap();
        if exporter::readiness(&parts[0].metrics).is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let reason = exporter::ready(&handle).await.unwrap_err();
    assert!(reason.contains("domain gpu1"), "{reason}");
    assert!(!reason.contains("domain default"), "{reason}");
}

/// Ohne Domaenen ist es ein Actor, und der Abzug heisst `default`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_domains_there_is_one_actor() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_ok("detector_main");
    let handle = spawn(ONE_GPU, &[(GPU0, &fake)]);
    assert_eq!(handle.domain_count(), 1);
    let clock = MonotonicClock::start();
    handle
        .submit(
            descriptor(1, DETECTOR, Criticality::Normal, clock.now()),
            request_for("detector"),
            PayloadPermit::untracked(),
        )
        .await
        .unwrap();
    let parts = handle.domain_metrics().await.unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].name, "default");
    assert_eq!(handle.metrics().await.unwrap(), parts[0].metrics);
    assert!(exporter::render_domains(&parts).is_empty());
}
