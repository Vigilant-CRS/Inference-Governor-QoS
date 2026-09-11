//! Der Abschlussabgleich ueber Neustarts, Endpunkte und Versionen hinweg
//! (Review 11.09., R01 und R02: `docs/reviews/2026-09-11-runtime/`).
//!
//! Ein Slotkredit, dessen Aufruf abbrach, endet nur durch einen Nachweis aus
//! der Statistik des Backends. Diese Tests fahren die Faelle, in denen der
//! Nachweis frueher falsch gefuehrt wurde:
//!
//! * Ein Backend startet neu. Der Neustart belegt das Ende der Aufrufe, die
//!   mit dem alten Prozess starben — und **keines** Aufrufs, der danach
//!   entstand oder dessen Verbindung noch offen ist.
//! * Zwei Server bieten dasselbe Modell an. Der Nachweis kommt von dem, der
//!   den Aufruf bekam.
//! * Ein Modell hat mehrere Versionen. Gezaehlt wird ueber alle.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod mock_backend;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use vig_backend_triton::{BackendError, TritonClient};
use vig_core::{
    Criticality, Instant, ModelIdx, PayloadRef, QueuePolicy, RequestDescriptor, RequestId,
    SupersessionKey,
};
use vig_gateway::executor::Executor;
use vig_gateway::testing::{FakeExecutor, request_for};
use vig_gateway::{MonotonicClock, actor};

const ENDPOINT_A: &str = "127.0.0.1:59996";
const ENDPOINT_B: &str = "127.0.0.1:59997";

/// Ein Modell, vier Slots: genug, dass gehaltene Kredite nicht jeden
/// weiteren Request abweisen.
const ONE_MODEL: &str = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:59996
  slots: 4
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

/// Zwei logische Modelle, **ein** Backendname, zwei Server derselben GPU.
const TWO_SERVERS: &str = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:59996
  slots: 4
  pipelining_depth: 0
  inference_timeout_ms: 200
models:
  det_a:
    class: protected
    queue: { policy: fifo, capacity: 64 }
    contract: { deadline_ms: 10000 }
    variants:
      - id: main
        backend_model: detector_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }
  det_b:
    class: protected
    backend_endpoint: 127.0.0.1:59997
    queue: { policy: fifo, capacity: 64 }
    contract: { deadline_ms: 10000 }
    variants:
      - id: main
        backend_model: detector_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }
";

/// Zwei Kameras auf **dasselbe** Backendmodell am selben Server.
const TWO_ALIASES: &str = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:59996
  slots: 4
  pipelining_depth: 0
  inference_timeout_ms: 200
models:
  cam_left:
    class: protected
    queue: { policy: fifo, capacity: 64 }
    contract: { deadline_ms: 10000 }
    variants:
      - id: main
        backend_model: detector_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }
  cam_right:
    class: protected
    queue: { policy: fifo, capacity: 64 }
    contract: { deadline_ms: 10000 }
    variants:
      - id: main
        backend_model: detector_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }
";

fn actor_with(yaml: &str, fakes: &[(&str, &Arc<FakeExecutor>)]) -> vig_gateway::Handle {
    let resolved = Arc::new(
        vig_config::Config::from_yaml(yaml)
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let mut backends: HashMap<String, Arc<dyn Executor>> = HashMap::new();
    for (endpoint, fake) in fakes {
        let executor: Arc<dyn Executor> = Arc::clone(*fake) as Arc<dyn Executor>;
        backends.insert((*endpoint).to_owned(), executor);
    }
    actor::spawn_with(resolved, backends, MonotonicClock::start(), &[]).unwrap()
}

fn descriptor(id: u64, model: u16, now: Instant) -> RequestDescriptor {
    RequestDescriptor {
        id: RequestId(id),
        logical_model: ModelIdx(model),
        supersession_key: SupersessionKey(id),
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

/// Ein abgebrochener Aufruf: unterwegs gewesen, Ende unbekannt.
fn aborted() -> BackendError {
    BackendError::Rejected {
        code: tonic::Code::Unavailable,
        message: "RPC verloren; die Ausfuehrung kann weiterlaufen".to_owned(),
    }
}

async fn submit(handle: &vig_gateway::Handle, id: u64, model: u16, logical: &str) -> bool {
    let clock = MonotonicClock::start();
    handle
        .submit(
            descriptor(id, model, clock.now()),
            request_for(logical),
            vig_gateway::budget::PayloadPermit::untracked(),
        )
        .await
        .is_ok()
}

async fn quarantined(handle: &vig_gateway::Handle) -> u64 {
    handle.metrics().await.unwrap().quarantined
}

/// Wartet, bis die Zahl gehaltener Kredite `want` ist; `false` nach der Frist.
async fn settles_at(handle: &vig_gateway::Handle, want: u64, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if quarantined(handle).await == want {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn await_baselines(handle: &vig_gateway::Handle) {
    for _ in 0..80 {
        if handle.metrics().await.unwrap().reconcile_baseline_missing == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "die Basislinien kamen nicht: {} fehlen",
        handle.metrics().await.unwrap().reconcile_baseline_missing
    );
}

// ---------------------------------------------------------------------------
// R01: ein Neustart belegt das Ende der Aufrufe, die mit ihm starben
// ---------------------------------------------------------------------------

/// Reset, dann ein neuer Aufruf mit Timeout, dann die verspaetete Antwort.
///
/// Vorher merkte sich der Abgleich den hoechsten Zaehlerstand je Poller und
/// nannte **jeden** niedrigeren Stand einen Neustart. Nach 100 → 0 → 0 galt
/// deshalb auch die dritte Meldung noch als Neustart, und sie gab den Kredit
/// eines Aufrufs frei, der erst nach dem Reset entstanden war und dessen
/// Verbindung noch offen stand. Die GPU rechnete, und der Governor plante
/// mit einem freien Slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_ends_only_the_calls_that_died_with_it() {
    let fake = Arc::new(FakeExecutor::default());
    fake.set_evidence(100);
    fake.expect_error("detector_main", aborted());
    fake.expect_slow("detector_main", Duration::from_millis(1_500));
    let handle = actor_with(ONE_MODEL, &[(ENDPOINT_A, &fake)]);
    await_baselines(&handle).await;

    assert!(
        !submit(&handle, 1, 0, "detector").await,
        "der Aufruf bricht ab"
    );
    assert_eq!(quarantined(&handle).await, 1);

    // Das Backend startet neu: der Zaehler faellt. Der abgebrochene Aufruf
    // lief im alten Prozess, und den gibt es nicht mehr.
    fake.set_evidence(0);
    assert!(
        settles_at(&handle, 0, Duration::from_secs(2)).await,
        "der Neustart belegt das Ende des abgebrochenen Aufrufs"
    );

    // Ein neuer Aufruf nach dem Reset. Er ueberschreitet das Timeout, aber
    // seine Verbindung steht: das Backend rechnet ihn.
    assert!(
        !submit(&handle, 2, 0, "detector").await,
        "der Client bekommt das Timeout"
    );
    assert_eq!(quarantined(&handle).await, 1);

    // Mehrere Abgleichsrunden lang: der Zaehler bleibt bei 0, und das ist
    // kein zweiter Neustart.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        quarantined(&handle).await,
        1,
        "ein Aufruf mit offener Verbindung endet durch seine Antwort, nicht \
         durch den Neustart von vorhin"
    );

    // Die verspaetete Antwort ist der Nachweis.
    assert!(
        settles_at(&handle, 0, Duration::from_secs(3)).await,
        "die Antwort des Backends gibt den Kredit frei"
    );
}

/// Nach einem Neustart zaehlt der neue Zaehler — und nur fuer neue Arbeit.
///
/// Vorher blieben Basislinie und Auslieferungssumme ueber den Reset hinweg
/// stehen. Das Ziel war danach unerreichbar, und der einzige Weg zum
/// Nachweis war die Neustartmeldung, die jeden Kredit freigab — auch den
/// eines Aufrufs, der im neuen Prozess noch rechnen konnte.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_a_restart_new_work_is_proven_by_the_new_counter() {
    let fake = Arc::new(FakeExecutor::default());
    fake.set_evidence(100);
    fake.expect_error("detector_main", aborted());
    fake.expect_error("detector_main", aborted());
    let handle = actor_with(ONE_MODEL, &[(ENDPOINT_A, &fake)]);
    await_baselines(&handle).await;

    assert!(!submit(&handle, 1, 0, "detector").await);
    fake.set_evidence(0);
    assert!(settles_at(&handle, 0, Duration::from_secs(2)).await);

    // Abgebrochen im neuen Prozess: das Ende ist unbekannt.
    assert!(!submit(&handle, 2, 0, "detector").await);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        quarantined(&handle).await,
        1,
        "solange der neue Zaehler steht, ist nichts belegt"
    );

    // Eine Inferenz abgeschlossen, eine ausgeliefert: Ruhe im neuen Prozess.
    fake.set_evidence(1);
    assert!(
        settles_at(&handle, 0, Duration::from_secs(2)).await,
        "der neue Zaehler belegt das Ende"
    );
}

/// Alles zusammen: ein offener Aufruf ueber den Reset hinweg, ein
/// abgebrochener davor, einer danach, und die verspaetete Antwort.
///
/// Der offene Aufruf kann im neuen Prozess laufen. Er wandert deshalb in die
/// neue Epoche und zaehlt dort mit: ein einzelner Abschluss im neuen Prozess
/// koennte seiner sein und belegt den abgebrochenen Aufruf danach nicht.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_call_across_a_restart_counts_in_the_new_epoch() {
    let fake = Arc::new(FakeExecutor::default());
    fake.set_evidence(100);
    fake.expect_slow("detector_main", Duration::from_millis(3_000));
    fake.expect_error("detector_main", aborted());
    fake.expect_error("detector_main", aborted());
    let handle = actor_with(ONE_MODEL, &[(ENDPOINT_A, &fake)]);
    await_baselines(&handle).await;

    // Auftrag 1: Timeout, Verbindung offen. Auftrag 2: abgebrochen.
    assert!(!submit(&handle, 1, 0, "detector").await);
    assert!(!submit(&handle, 2, 0, "detector").await);
    assert_eq!(quarantined(&handle).await, 2);

    // Reset: Auftrag 2 starb mit dem alten Prozess, Auftrag 1 wartet.
    fake.set_evidence(0);
    assert!(
        settles_at(&handle, 1, Duration::from_secs(2)).await,
        "nur der abgebrochene Aufruf endet mit dem Neustart"
    );

    // Auftrag 3 bricht im neuen Prozess ab. Ein Abschluss im neuen Zaehler
    // kann der von Auftrag 1 sein.
    assert!(!submit(&handle, 3, 0, "detector").await);
    fake.set_evidence(1);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        quarantined(&handle).await,
        2,
        "ein Abschluss belegt nicht zwei Aufrufe"
    );

    // Die Antwort von Auftrag 1 kommt; Auftrag 3 bleibt offen, bis der
    // Zaehler beide Aufrufe der neuen Epoche traegt.
    assert!(settles_at(&handle, 1, Duration::from_secs(4)).await);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(quarantined(&handle).await, 1);
    fake.set_evidence(2);
    assert!(
        settles_at(&handle, 0, Duration::from_secs(2)).await,
        "Ruhe in der neuen Epoche"
    );
}

/// Ein Abgleich je Backendidentitaet, und er endet mit dem Nachweis.
///
/// Vorher startete jeder abgebrochene Aufruf einen eigenen Poller, und keiner
/// endete je. Drei Verbindungsabbrueche hiessen drei Poller fuer die
/// Lebensdauer des Prozesses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_poller_per_backend_however_often_the_transport_breaks() {
    let fake = Arc::new(FakeExecutor::default());
    fake.set_evidence(0);
    for _ in 0..3 {
        fake.expect_error("detector_main", aborted());
    }
    let handle = actor_with(ONE_MODEL, &[(ENDPOINT_A, &fake)]);
    await_baselines(&handle).await;

    for id in 1..=3 {
        assert!(!submit(&handle, id, 0, "detector").await);
    }
    assert_eq!(quarantined(&handle).await, 3);

    let before = fake.evidence_calls();
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let polls = fake.evidence_calls() - before;
    assert!(
        polls <= 6,
        "ein Poller fragt alle 250 ms; {polls} Abfragen in einer Sekunde \
         heissen mehrere"
    );

    fake.set_evidence(3);
    assert!(settles_at(&handle, 0, Duration::from_secs(2)).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = fake.evidence_calls();
    tokio::time::sleep(Duration::from_millis(750)).await;
    assert!(
        fake.evidence_calls() - after <= 1,
        "ohne gehaltenen Kredit fragt niemand mehr nach"
    );
}

// ---------------------------------------------------------------------------
// R02: der Nachweis kommt von dem Server, der den Aufruf bekam
// ---------------------------------------------------------------------------

/// Zwei Server auf einer GPU, beide mit einem Modell `detector_main`.
///
/// Vorher war der Abgleich nach dem Backendnamen geschluesselt, und die
/// Suche nach dem Client nahm den ersten Server mit diesem Namen. Ein
/// Aufruf an B wurde gegen die Statistik von A abgeglichen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_proof_comes_from_the_server_that_ran_the_call() {
    let a = Arc::new(FakeExecutor::default());
    let b = Arc::new(FakeExecutor::default());
    a.set_evidence(0);
    b.set_evidence(1_000);
    b.expect_error("detector_main", aborted());
    b.expect_error("detector_main", aborted());
    let handle = actor_with(TWO_SERVERS, &[(ENDPOINT_A, &a), (ENDPOINT_B, &b)]);
    await_baselines(&handle).await;

    assert!(!submit(&handle, 1, 1, "det_b").await);
    assert_eq!(quarantined(&handle).await, 1);

    // B belegt den Abschluss.
    b.set_evidence(1_001);
    assert!(
        settles_at(&handle, 0, Duration::from_secs(2)).await,
        "der Server, der den Aufruf bekam, belegt sein Ende"
    );

    // Und A belegt nichts ueber einen Aufruf an B.
    assert!(!submit(&handle, 2, 1, "det_b").await);
    a.set_evidence(5_000);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        quarantined(&handle).await,
        1,
        "ein fremder Server belegt nichts"
    );
    b.set_evidence(1_002);
    assert!(settles_at(&handle, 0, Duration::from_secs(2)).await);
}

/// Zwei Kameras auf dasselbe Backendmodell brauchen **eine** Basislinie.
///
/// Vorher zaehlte die Kennzahl logische Varianten und zog eindeutige
/// Backendnamen ab: zwei Aliase erschienen dauerhaft als eine fehlende
/// Basislinie, obwohl nichts fehlte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aliases_of_one_backend_model_need_one_baseline() {
    let fake = Arc::new(FakeExecutor::default());
    fake.set_evidence(42);
    let handle = actor_with(TWO_ALIASES, &[(ENDPOINT_A, &fake)]);
    await_baselines(&handle).await;
}

/// Mehrere Versionen eines Modells: gezaehlt wird ueber alle.
///
/// Ohne Versionsfilter liefert Triton je geladener Version einen Eintrag,
/// und die Zaehler gelten je Version. Vorher nahm der Client den ersten
/// Eintrag — lief die Arbeit auf einer anderen Version, stand der Zaehler
/// still, und der Kredit blieb fuer immer gehalten.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evidence_counts_every_version_of_the_model() {
    let backend = Arc::new(mock_backend::MockBackend::new(Duration::ZERO));
    *backend.stat_versions.lock().unwrap() = vec![("1".to_owned(), 5), ("2".to_owned(), 7)];
    let address = mock_backend::start(Arc::clone(&backend)).await;
    let client = TritonClient::new(address.to_string());

    let evidence = client.completion_evidence("detector_main").await.unwrap();
    assert_eq!(
        evidence.completed, 12,
        "Abschluesse aller Versionen, nicht nur der ersten"
    );
}
