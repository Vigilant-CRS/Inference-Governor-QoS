//! Fehlerpfade des Actors, gefahren ohne GPU (NV-07).
//!
//! Was hier geprueft wird, laesst sich auf echter Hardware kaum
//! herbeifuehren: ein Aufruf, der das Backend nie erreicht, gegen einen, der
//! unterwegs abbricht; ein Timeout, nach dem das Backend doch noch antwortet.
//! Genau daran entscheidet sich, ob ein Slotkredit zu frueh zurueckkommt —
//! und damit, ob eine zweite Inferenz auf eine womoeglich noch rechnende
//! Recheneinheit gelegt wird (NV-00).
//!
//! Der Actor ist derselbe wie im Betrieb. Ausgetauscht ist nur das Backend.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::HashMap;
use std::sync::Arc;
use vig_backend_triton::BackendError;
use vig_core::{
    Criticality, Instant, ModelIdx, PayloadRef, QueuePolicy, RequestDescriptor, RequestId,
    SupersessionKey,
};
use vig_gateway::executor::Executor;
use vig_gateway::testing::{FakeExecutor, request_for};
use vig_gateway::{MonotonicClock, actor};

const YAML: &str = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:59999
  slots: 1
  pipelining_depth: 0
  inference_timeout_ms: 200
models:
  detector:
    class: protected
    queue: { policy: fifo, capacity: 64 }
    contract: { deadline_ms: 10000 }
    variants:
      - id: main
        backend_model: detector_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }
";

/// Actor mit einem programmierbaren Backend.
fn actor_with(fake: Arc<FakeExecutor>) -> (vig_gateway::Handle, Arc<vig_config::schema::Resolved>) {
    let resolved = Arc::new(
        vig_config::Config::from_yaml(YAML)
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let mut backends: HashMap<String, Arc<dyn Executor>> = HashMap::new();
    backends.insert(resolved.backend_endpoint.clone(), fake);
    let handle = actor::spawn_with(
        Arc::clone(&resolved),
        backends,
        MonotonicClock::start(),
        &[],
    )
    .unwrap();
    (handle, resolved)
}

fn descriptor(id: u64, now: Instant) -> RequestDescriptor {
    RequestDescriptor {
        id: RequestId(id),
        logical_model: ModelIdx(0),
        supersession_key: SupersessionKey(0),
        generation_time: now,
        arrival_time: now,
        absolute_deadline: None,
        max_age: None,
        criticality: Criticality::Protected,
        queue_policy: QueuePolicy::Fifo,
        stateful: false,
        variant: None,
        payload: PayloadRef::default(),
        context_tokens: 0,
        decomposable: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successful_call_goes_through_the_seam() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_ok("detector_main");
    let (handle, _resolved) = actor_with(Arc::clone(&fake));

    let clock = MonotonicClock::start();
    let reply = handle
        .submit(descriptor(1, clock.now()), request_for("detector"))
        .await;
    assert!(reply.is_ok(), "{reply:?}");
    assert_eq!(fake.executed_models(), vec!["detector_main".to_owned()]);

    let metrics = handle.metrics().await.unwrap();
    assert_eq!(metrics.forwarded, 1);
    assert_eq!(metrics.backend_failures, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_that_never_started_returns_its_credit() {
    // `Unreachable` entsteht ausschliesslich beim Kanalaufbau: die
    // Recheneinheit hat nie gerechnet, der Kredit gehoert sofort zurueck.
    // Belegt dadurch, dass ein zweiter Request danach noch laeuft.
    let fake = Arc::new(FakeExecutor::default());
    for _ in 0..2 {
        fake.expect_error(
            "detector_main",
            BackendError::Unreachable {
                endpoint: "127.0.0.1:59999".to_owned(),
                cause: "connection refused".to_owned(),
            },
        );
    }
    let (handle, _resolved) = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    assert!(
        handle
            .submit(descriptor(1, clock.now()), request_for("detector"))
            .await
            .is_err()
    );
    assert!(
        handle
            .submit(descriptor(2, clock.now()), request_for("detector"))
            .await
            .is_err(),
        "der zweite Request muss ueberhaupt starten koennen"
    );
    assert_eq!(fake.executed(), 2, "sonst war der Slot noch belegt");

    let metrics = handle.metrics().await.unwrap();
    assert_eq!(metrics.backend_failures, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aborted_call_does_not_return_its_credit() {
    // `Unavailable` waehrend des Aufrufs sagt nichts ueber die
    // Recheneinheit. Der Kredit bleibt gehalten, bis ein Endnachweis
    // vorliegt — und der Nachweisweg wird deshalb hier auch begangen.
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error(
        "detector_main",
        BackendError::Rejected {
            code: tonic::Code::Unavailable,
            message: "transport closed".to_owned(),
        },
    );
    let (handle, _resolved) = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    assert!(
        handle
            .submit(descriptor(1, clock.now()), request_for("detector"))
            .await
            .is_err()
    );

    // Der Abgleich fragt das Backend nach einem Endnachweis. Dass er
    // ueberhaupt laeuft, ist der Punkt: ohne ihn bliebe der Kredit fuer
    // immer gehalten.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        fake.evidence_calls() > 0,
        "nach einem unbekannten Ausgang muss der Abgleich anlaufen"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timeout_answers_the_client_and_keeps_waiting_for_the_backend() {
    // Zwei Stufen, und das ist der Punkt: der Client bekommt nach der Frist
    // eine Antwort, der Aufruf laeuft aber weiter. Ein Abbruch wuerde
    // vortaeuschen, dass die Recheneinheit frei ist.
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_slow("detector_main", std::time::Duration::from_millis(600));
    let (handle, _resolved) = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    let started = std::time::Instant::now();
    let reply = handle
        .submit(descriptor(1, clock.now()), request_for("detector"))
        .await;
    let waited = started.elapsed();

    assert!(reply.is_err(), "der Client bekommt den Timeout zu sehen");
    assert!(
        waited < std::time::Duration::from_millis(500),
        "der Client soll nach der Frist antworten, nicht nach der Inferenz: {waited:?}"
    );

    // Und das Backend laeuft trotzdem zu Ende.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(fake.executed(), 1);
    assert_eq!(fake.pending(), 0, "die programmierte Antwort wurde geholt");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unprogrammed_backend_fails_loudly() {
    // Der Fake erfindet nichts. Ein Test, der eine Antwort erwartet, ohne sie
    // zu bestellen, soll auffallen und nicht durchgehen.
    let fake = Arc::new(FakeExecutor::default());
    let (handle, _resolved) = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();
    assert!(
        handle
            .submit(descriptor(1, clock.now()), request_for("detector"))
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_executor_owns_no_resources_of_the_governor() {
    // Der Fake haelt keinen Slot und gibt keinen Kredit frei. Dass nach zehn
    // Auftraegen genau zehn Ausfuehrungen und keine offenen Anspruecke
    // stehen, ist der Beleg fuer genau einen Besitzer je Ressource.
    let fake = Arc::new(FakeExecutor::default());
    for _ in 0..10 {
        fake.expect_ok("detector_main");
    }
    let (handle, _resolved) = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    for id in 1..=10_u64 {
        let reply = handle
            .submit(descriptor(id, clock.now()), request_for("detector"))
            .await;
        assert!(reply.is_ok(), "Request {id}: {reply:?}");
    }
    assert_eq!(fake.executed(), 10);

    let drained = handle
        .drain(std::time::Duration::from_millis(500))
        .await
        .unwrap();
    assert!(drained, "kein Anspruch darf offen bleiben");
}
