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
        context_tokens: 0,
        decomposable: false,
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
            .submit(
                descriptor(1, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
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
        .submit(
            descriptor(1, clock.now()),
            request_for("detector"),
            vig_gateway::budget::PayloadPermit::untracked(),
        )
        .await;
    assert!(exporter::readiness(&handle.metrics().await.unwrap()).is_err());

    assert!(
        handle
            .submit(
                descriptor(2, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
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
            .submit(
                descriptor(1, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
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
                .submit(
                    descriptor(1, now),
                    request_for("detector"),
                    vig_gateway::budget::PayloadPermit::untracked(),
                )
                .await
        })
    };
    let second = {
        let handle = handle.clone();
        let now = clock.now();
        tokio::spawn(async move {
            handle
                .submit(
                    descriptor(2, now),
                    request_for("detector"),
                    vig_gateway::budget::PayloadPermit::untracked(),
                )
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
                .submit(
                    descriptor(1, now),
                    request_for("detector"),
                    vig_gateway::budget::PayloadPermit::untracked(),
                )
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
                .submit(
                    descriptor(id, clock.now()),
                    request_for("detector"),
                    vig_gateway::budget::PayloadPermit::untracked()
                )
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
            .submit(
                descriptor(1, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
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
            .submit(
                descriptor(1, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
            .await
            .is_err()
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert!(
        handle
            .submit(
                descriptor(2, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
            .await
            .is_ok(),
        "nach dem Nachweis muss wieder etwas starten koennen"
    );
    assert_eq!(fake.executed(), 2);
}

/// Wartet, bis die Abgleichs-Basislinie steht.
async fn await_baseline(handle: &vig_gateway::Handle) {
    for _ in 0..40 {
        if handle.metrics().await.unwrap().reconcile_baseline_missing == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("die Basislinie kam nicht");
}

/// **Fehlerbild: Governor-Neustart gegen ein lange laufendes Backend.**
///
/// Das ist der Normalfall bei jedem Update, und er war falsch. Tritons
/// Statistikzaehler laeuft ueber die Lebensdauer des Triton-Prozesses, und der
/// ueberlebt den Governor. Ohne Basislinie war „das Backend meldet mindestens
/// so viele Abschluesse wie wir ausgeliefert haben" beim **ersten** Request
/// sofort wahr — der Abgleich gab einen Slotkredit frei, waehrend die
/// Recheneinheit womoeglich noch rechnete. Genau das, was NV-00 ausschliessen
/// sollte.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_against_a_long_running_backend_does_not_free_a_credit() {
    let fake = Arc::new(FakeExecutor::default());
    // Das Backend laeuft seit Stunden: fuenftausend abgeschlossene Inferenzen.
    fake.set_evidence(5_000);
    fake.expect_error(
        "detector_main",
        BackendError::Rejected {
            code: tonic::Code::Unavailable,
            message: "transport closed mid-call".to_owned(),
        },
    );
    let handle = actor_with(Arc::clone(&fake));
    await_baseline(&handle).await;

    let clock = MonotonicClock::start();
    assert!(
        handle
            .submit(
                descriptor(1, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
            .await
            .is_err()
    );
    assert_eq!(handle.metrics().await.unwrap().quarantined, 1);

    // Der Zaehler bewegt sich nicht. Ohne Basislinie waere 5000 >= 1 sofort
    // wahr gewesen; mit Basislinie ist das Ziel 5001.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(
        metrics.quarantined, 1,
        "der Kredit gehoert gehalten, solange kein Ende belegt ist"
    );
    assert_eq!(metrics.reconciled, 0);

    // Erst wenn das Backend eine Inferenz **mehr** meldet, ist es belegt.
    fake.set_evidence(5_001);
    let mut released = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let m = handle.metrics().await.unwrap();
        if m.quarantined == 0 {
            assert_eq!(m.reconciled, 1, "genau einmal freigegeben");
            released = true;
            break;
        }
    }
    assert!(released, "mit Nachweis endet der Anspruch");
}

/// **Fehlerbild: das Backend liefert keine Statistik.**
///
/// Dann gibt es keine Basislinie und damit keinen zaehlerbasierten Nachweis.
/// Der Kredit bleibt gehalten — unbequem und richtig — und die Metrik sagt es,
/// damit der Betreiber den Governor bei erreichbarem Backend neu starten kann.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backend_without_statistics_reports_the_missing_baseline() {
    let fake = Arc::new(FakeExecutor::default());
    fake.set_capabilities(vig_gateway::Capabilities {
        completion_evidence: false,
        decoupled_endpoint: true,
    });
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
            .submit(
                descriptor(1, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
            .await
            .is_err()
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let metrics = handle.metrics().await.unwrap();
    assert!(
        metrics.reconcile_baseline_missing > 0,
        "der Betreiber soll den Grund sehen, nicht nur den gehaltenen Kredit"
    );
    assert_eq!(
        metrics.quarantined, 1,
        "ohne Nachweisweg bleibt der Kredit gehalten"
    );
}

/// **Fehlerbild: die Basislinie kommt zu spaet.**
///
/// Nach der ersten Auslieferung wird sie nicht mehr angenommen: der Zaehler
/// koennte eigene, schon abgeschlossene Inferenzen enthalten, die Basislinie
/// waere zu hoch und das Ziel unerreichbar. Eine zu hohe Basislinie ist nicht
/// die sichere Seite, sondern eine andere Art, kaputt zu sein.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_baseline_is_refused_and_reported() {
    let fake = Arc::new(FakeExecutor::default());
    // Erst keine Statistik — die Basislinie kommt nicht durch.
    fake.set_capabilities(vig_gateway::Capabilities {
        completion_evidence: false,
        decoupled_endpoint: true,
    });
    fake.expect_ok("detector_main");
    let handle = actor_with(Arc::clone(&fake));
    let clock = MonotonicClock::start();

    // Eine Auslieferung, bevor die Basislinie da ist.
    assert!(
        handle
            .submit(
                descriptor(1, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
            .await
            .is_ok()
    );

    // Jetzt liefert das Backend Statistik — zu spaet.
    fake.set_capabilities(vig_gateway::Capabilities {
        completion_evidence: true,
        decoupled_endpoint: true,
    });
    fake.set_evidence(9_999);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    assert!(
        handle.metrics().await.unwrap().reconcile_baseline_missing > 0,
        "eine Basislinie nach der ersten Auslieferung wird nicht angenommen"
    );
}

// ---------------------------------------------------------------------------
// Ein aggregierter Zaehler ist kein Einzelnachweis (Review R01)
// ---------------------------------------------------------------------------

/// Ein spaeterer Abschluss belegt nicht, dass ein frueherer fertig ist.
///
/// Das Fehlerbild, und es reicht **ein** Governor als Client: Auftrag A
/// laeuft, seine Verbindung bricht ab, Auftrag B wird fertig. Der gemeinsame
/// Abschlusszaehler steigt um eins — und der Abgleich las das als „A ist
/// fertig" und gab dessen Slotkredit frei. A koennte noch rechnen, und ab da
/// stimmte die Kapazitaetsrechnung nicht mehr.
///
/// Was ein aggregierter Zaehler tragen kann, ist eine **Ruhe-Aussage**: hat
/// das Modell mindestens so viele Inferenzen abgeschlossen, wie ihm insgesamt
/// zugestellt wurden, ist von unserer Arbeit nichts mehr offen. Ein einzelner
/// Abschluss belegt keinen einzelnen Auftrag.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_later_completion_does_not_prove_an_earlier_request_finished() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error("detector_main", aborted());
    fake.expect_ok("detector_main");
    let handle = actor_with(Arc::clone(&fake));
    await_baseline(&handle).await;
    let clock = MonotonicClock::start();

    assert!(
        handle
            .submit(
                descriptor(1, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
            .await
            .is_err(),
        "der erste Aufruf bricht ab; sein Ende ist unbekannt"
    );
    assert_eq!(handle.metrics().await.unwrap().quarantined, 1);

    assert!(
        handle
            .submit(
                descriptor(2, clock.now()),
                request_for("detector"),
                vig_gateway::budget::PayloadPermit::untracked()
            )
            .await
            .is_ok(),
        "der zweite laeuft durch"
    );

    // Genau ein Abschluss ist belegt — und er gehoert zum zweiten Auftrag.
    fake.set_evidence(1);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(
        metrics.quarantined, 1,
        "der Kredit des ersten Auftrags gehoert weiter gehalten; \
         reconciled={}",
        metrics.reconciled
    );

    // Die Gegenprobe: sind **beide** Auslieferungen abgeschlossen, ist von
    // unserer Arbeit nichts mehr offen — und der Kredit kommt zurueck.
    fake.set_evidence(2);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(
        handle.metrics().await.unwrap().quarantined,
        0,
        "Ruhe ist belegt, also endet der Anspruch"
    );
}

/// Ein Aufruf, der das Backend nie erreicht hat, hebt das Ziel nicht an.
///
/// Die Gegenrichtung desselben Fehlers. Ein Request, der schon am
/// Kanalaufbau scheiterte, wird nie eine Fertigstellung erzeugen. Zaehlte er
/// im Abgleichsziel mit, waere das Ziel unerreichbar — und ein spaeter
/// tatsaechlich abgeschlossener Auftrag bliebe dauerhaft in Quarantaene.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_that_never_reached_the_backend_does_not_raise_the_target() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error(
        "detector_main",
        BackendError::Unreachable {
            endpoint: "unbenutzt".to_owned(),
            cause: "vor der Zustellung abgelehnt".to_owned(),
        },
    );
    fake.expect_error("detector_main", aborted());
    let handle = actor_with(Arc::clone(&fake));
    await_baseline(&handle).await;
    let clock = MonotonicClock::start();

    for id in [1, 2] {
        assert!(
            handle
                .submit(
                    descriptor(id, clock.now()),
                    request_for("detector"),
                    vig_gateway::budget::PayloadPermit::untracked()
                )
                .await
                .is_err()
        );
    }

    // Alles, was dieses Backend je erreicht hat, ist jetzt abgeschlossen.
    fake.set_evidence(1);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(
        handle.metrics().await.unwrap().quarantined,
        0,
        "ein nie zugestellter Aufruf darf das Ziel nicht unerreichbar machen"
    );
}

/// Ein abgebrochener Aufruf: unterwegs gewesen, Ende unbekannt.
fn aborted() -> BackendError {
    BackendError::Rejected {
        code: tonic::Code::Unavailable,
        message: "RPC verloren; die Ausfuehrung kann weiterlaufen".to_owned(),
    }
}

/// Der Governor erholt sich ohne Verkehr (Review R11).
///
/// Der Fall, gegen den die aktive Probe existiert: das Backend faellt aus,
/// ein Loadbalancer nimmt daraufhin allen Verkehr weg — und damit den
/// einzigen Ausloeser, der die Bereitschaft je wieder gruen machen koennte.
/// Vorher blieb der Governor rot, bis jemand von aussen eine Inferenz
/// schickte, die er selbst als „nicht bereit" abgelehnt hatte.
///
/// Hier wird **keine einzige** Inferenz gefahren. Nur die Probe laeuft.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_recovers_without_any_traffic() {
    let fake = Arc::new(FakeExecutor::default());
    let handle = actor_with(Arc::clone(&fake));

    // Erst muss die Probe ueberhaupt einmal geantwortet haben: vor der ersten
    // Antwort ist die Lieferfaehigkeit ungeprueft.
    await_reachable(&handle, true).await;

    // Das Backend faellt aus.
    fake.set_unreachable(Some(BackendError::Unreachable {
        endpoint: "127.0.0.1:59999".to_owned(),
        cause: "Verbindung abgelehnt".to_owned(),
    }));
    await_reachable(&handle, false).await;
    assert!(
        vig_gateway::exporter::readiness(&handle.metrics().await.unwrap()).is_err(),
        "ein nicht antwortendes Backend nimmt den Governor aus der Rotation"
    );

    // Es kommt zurueck — und niemand schickt eine Inferenz.
    fake.set_unreachable(None);
    await_reachable(&handle, true).await;
    assert!(
        vig_gateway::exporter::readiness(&handle.metrics().await.unwrap()).is_ok(),
        "die Erholung braucht keinen Verkehr"
    );
    assert_eq!(fake.executed(), 0, "es lief keine einzige Inferenz");
}

/// Wartet, bis die Erreichbarkeitsprobe den erwarteten Stand meldet.
async fn await_reachable(handle: &vig_gateway::Handle, expected: bool) {
    for _ in 0..80 {
        let metrics = handle.metrics().await.unwrap();
        if (metrics.backends_reachable == metrics.backends && metrics.backends > 0) == expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("die Probe meldete nie {expected}");
}
