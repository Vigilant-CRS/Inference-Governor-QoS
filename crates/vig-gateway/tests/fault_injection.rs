//! Fehlerinjektion ohne GPU (NV-20, Gate G4).
//!
//! Gate G4 verlangt Fault Injection auf dem freizugebenden Stand. Bis hierher
//! war das nur mit echtem Backend zu machen — und die interessanten Fehler
//! treten dort nicht auf Kommando ein. Mit der Backendnaht aus NV-07 gehen
//! sie im Testlauf.
//!
//! Geprueft wird nicht, dass nichts schiefgeht, sondern **was der Governor
//! tut, wenn es schiefgeht**: bleibt ein Slotkredit gehalten, wo er gehalten
//! bleiben muss; wird der Client beantwortet; meldet die Bereitschaftspruefung
//! den Ausfall; kommt der Prozess ohne offene Anspruechen zum Ende.

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
use vig_gateway::{MonotonicClock, actor, exporter};

const YAML: &str = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:59998
  slots: 2
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

fn actor_with(fake: Arc<FakeExecutor>) -> vig_gateway::Handle {
    let resolved = Arc::new(
        vig_config::Config::from_yaml(YAML)
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let mut backends: HashMap<String, Arc<dyn Executor>> = HashMap::new();
    backends.insert(resolved.backend_endpoint.clone(), fake);
    actor::spawn_with(resolved, backends, MonotonicClock::start(), &[]).unwrap()
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
    }
}

/// **Fehlerbild: das Backend ist weg.**
///
/// Der Verbindungsaufbau schlaegt fehl. Der Client bekommt einen Fehler, der
/// Slotkredit kommt sofort zurueck — und die Bereitschaftspruefung meldet
/// rot, obwohl kein einziger Slot in Quarantaene ist. Genau dieser Fall wurde
/// vor der Ergaenzung um `consecutive_transport_failures` uebersehen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vanished_backend_turns_readiness_red() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error(
        "detector_main",
        BackendError::Unreachable {
            endpoint: "127.0.0.1:59998".to_owned(),
            cause: "connection refused".to_owned(),
        },
    );
    let handle = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    assert!(
        handle
            .submit(descriptor(1, clock.now()), request_for("detector"))
            .await
            .is_err()
    );

    let metrics = handle.metrics().await.unwrap();
    assert_eq!(metrics.quarantined, 0, "kein Slot haengt");
    assert!(
        exporter::readiness(&metrics).is_err(),
        "und die Bereitschaft muss trotzdem rot sein"
    );
}

/// **Fehlerbild: das Backend antwortet wieder.**
///
/// Ein Alarm, der nicht von selbst verstummt, wird abgeschaltet und schuetzt
/// dann gar nichts mehr.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recovered_backend_turns_readiness_green_again() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error(
        "detector_main",
        BackendError::Unreachable {
            endpoint: "127.0.0.1:59998".to_owned(),
            cause: "connection refused".to_owned(),
        },
    );
    fake.expect_ok("detector_main");
    let handle = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    let _ = handle
        .submit(descriptor(1, clock.now()), request_for("detector"))
        .await;
    assert!(exporter::readiness(&handle.metrics().await.unwrap()).is_err());

    assert!(
        handle
            .submit(descriptor(2, clock.now()), request_for("detector"))
            .await
            .is_ok()
    );
    assert!(
        exporter::readiness(&handle.metrics().await.unwrap()).is_ok(),
        "ein Erfolg setzt die Kette zurueck"
    );
}

/// **Fehlerbild: ein Modellfehler.**
///
/// Er sagt etwas ueber einen Request und nichts ueber die Erreichbarkeit. Er
/// darf den Governor nicht aus der Rotation nehmen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_error_does_not_take_the_governor_out_of_rotation() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error(
        "detector_main",
        BackendError::UnknownModel {
            model: "detector_main".to_owned(),
        },
    );
    let handle = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    assert!(
        handle
            .submit(descriptor(1, clock.now()), request_for("detector"))
            .await
            .is_err()
    );
    let metrics = handle.metrics().await.unwrap();
    assert!(
        exporter::readiness(&metrics).is_ok(),
        "ein falscher Modellname ist ein Konfigurationsfehler, kein Ausfall"
    );
}

/// **Fehlerbild: das Backend rechnet nach dem Timeout weiter.**
///
/// Der Client wird nach der Frist beantwortet, der Slotkredit bleibt
/// gehalten. Beide Slots so zu belegen heisst: es startet nichts mehr, und die
/// Bereitschaftspruefung sagt das.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_hanging_calls_quarantine_every_slot() {
    let fake = Arc::new(FakeExecutor::default());
    for _ in 0..2 {
        fake.expect_slow("detector_main", std::time::Duration::from_secs(30));
    }
    // Kein Endnachweis: der Kredit bleibt gehalten, so lange es dauert.
    fake.set_capabilities(vig_gateway::Capabilities {
        completion_evidence: false,
        decoupled_endpoint: true,
    });
    let handle = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    let first = {
        let handle = handle.clone();
        let now = clock.now();
        tokio::spawn(async move {
            handle
                .submit(descriptor(1, now), request_for("detector"))
                .await
        })
    };
    let second = {
        let handle = handle.clone();
        let now = clock.now();
        tokio::spawn(async move {
            handle
                .submit(descriptor(2, now), request_for("detector"))
                .await
        })
    };

    assert!(first.await.unwrap().is_err(), "Timeout beim Client");
    assert!(second.await.unwrap().is_err());

    let metrics = handle.metrics().await.unwrap();
    assert_eq!(
        metrics.quarantined, 2,
        "beide Kredite bleiben gehalten, solange kein Ende belegt ist"
    );
    assert!(
        exporter::readiness(&metrics).is_err(),
        "es kann nichts mehr starten, und das gehoert nach aussen"
    );
}

/// **Fehlerbild: Herunterfahren, waehrend das Backend haengt.**
///
/// Ein Drain, das nicht endet, ist keines. Er muss innerhalb der Frist
/// zurueckkehren und `false` melden — und **nicht** Erfolg, weil die
/// Recheneinheit womoeglich noch belegt ist.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drain_over_a_hanging_backend_reports_failure_not_success() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_slow("detector_main", std::time::Duration::from_secs(30));
    fake.set_capabilities(vig_gateway::Capabilities {
        completion_evidence: false,
        decoupled_endpoint: true,
    });
    let handle = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    let pending = {
        let handle = handle.clone();
        let now = clock.now();
        tokio::spawn(async move {
            handle
                .submit(descriptor(1, now), request_for("detector"))
                .await
        })
    };
    // Warten, bis der Aufruf tatsaechlich unterwegs ist.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let drained = handle
        .drain(std::time::Duration::from_millis(400))
        .await
        .unwrap();
    assert!(
        !drained,
        "ein sauberes Ende zu melden, waehrend die GPU noch rechnet, waere \
         die schlimmste der moeglichen Antworten: der naechste Prozess \
         startet dann in eine Belegung, von der er nichts weiss"
    );
    let _ = pending.await;
}

/// **Fehlerbild: sauberes Ende.**
///
/// Der Gegentest zum vorigen. Ohne ihn belegt der obere nur, dass `drain`
/// manchmal `false` sagt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_over_a_healthy_backend_reports_success() {
    let fake = Arc::new(FakeExecutor::default());
    for _ in 0..3 {
        fake.expect_ok("detector_main");
    }
    let handle = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();
    for id in 1..=3_u64 {
        assert!(
            handle
                .submit(descriptor(id, clock.now()), request_for("detector"))
                .await
                .is_ok()
        );
    }
    assert!(
        handle
            .drain(std::time::Duration::from_millis(500))
            .await
            .unwrap()
    );
}

/// **Fehlerbild: das Backend bricht mitten im Aufruf ab.**
///
/// Der Zustand, ueber den niemand etwas weiss. Der Kredit bleibt gehalten,
/// und der Abgleich laeuft an — auch wenn das Backend keinen Nachweis liefern
/// kann, denn dann bleibt er gehalten, und das ist die richtige Antwort.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aborted_call_starts_the_reconciliation() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error(
        "detector_main",
        BackendError::Rejected {
            code: tonic::Code::Unavailable,
            message: "transport closed mid-call".to_owned(),
        },
    );
    let handle = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    assert!(
        handle
            .submit(descriptor(1, clock.now()), request_for("detector"))
            .await
            .is_err()
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        fake.evidence_calls() > 0,
        "ohne Abgleich bliebe der Kredit fuer immer gehalten, ohne dass \
         jemand nachfragt"
    );
}

/// **Fehlerbild: der Endnachweis kommt spaeter.**
///
/// Ein abgebrochener Aufruf, dessen Ende das Backend anschliessend belegt.
/// Danach muss der Slot wieder benutzbar sein.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_proof_of_completion_frees_the_slot() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error(
        "detector_main",
        BackendError::Rejected {
            code: tonic::Code::Unavailable,
            message: "transport closed mid-call".to_owned(),
        },
    );
    fake.expect_ok("detector_main");
    // Das Backend meldet eine abgeschlossene Inferenz — der Nachweis.
    fake.set_evidence(1);
    let handle = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    assert!(
        handle
            .submit(descriptor(1, clock.now()), request_for("detector"))
            .await
            .is_err()
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert!(
        handle
            .submit(descriptor(2, clock.now()), request_for("detector"))
            .await
            .is_ok(),
        "nach dem Nachweis muss wieder etwas starten koennen"
    );
    assert_eq!(fake.executed(), 2);
}
