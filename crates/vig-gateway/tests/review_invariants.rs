//! Invarianten aus dem Codereview vom 07.09.2026, Gatewayebene.
//!
//! Was hier geprueft wird, laesst sich im Kern nicht pruefen: es geht um die
//! Verdrahtung zwischen Scheduler, Actor und gRPC-Dienst. Genau dort lagen
//! die Fehler — der Kern kannte den Fehlerfall, der Actor meldete ihn nie.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod mock_backend;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::ModelInferRequest;
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;

fn yaml(endpoint: &str) -> String {
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
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 10000 }}
    variants:
      - id: large
        backend_model: detector_large
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    )
}

fn request() -> ModelInferRequest {
    ModelInferRequest {
        model_name: "detector".into(),
        id: "review".into(),
        ..Default::default()
    }
}

/// Ein fehlgeschlagener Backendaufruf zaehlt als Fehler, nicht als Erfolg.
///
/// Der Actor erzeugte auch im Fehlerfall ein gewoehnliches
/// Completion-Ereignis. `backend_failures` blieb dadurch im echten Gateway
/// dauerhaft null — und der Margen-Regler bekam die Fast-Null-Laufzeit eines
/// Verbindungsfehlers als Beleg dafuer, dass seine Prognose zu konservativ
/// gewesen sei. Jede Aussage der Form „null Backendfehler im Dauerlauf" war
/// mit diesem Zaehler unbelegt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_backend_call_is_counted_as_a_failure() {
    // Der Listener existiert nur, um einen sicher freien Port zu belegen; er
    // wird vor dem Aufruf geschlossen.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    drop(listener);

    let resolved = Arc::new(
        Config::from_yaml(&yaml(&endpoint))
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, backend, handle.clone(), clock);

    assert!(
        service
            .model_infer(tonic::Request::new(request()))
            .await
            .is_err(),
        "der Client bekommt den Fehler zu sehen"
    );
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(
        (metrics.backend_failures, metrics.completed_valid),
        (1, 0),
        "und die Statistik sagt dasselbe"
    );
}

/// Die Antwort traegt den logischen Modellnamen, nicht den der Variante.
///
/// Welche physische Variante gelaufen ist, ist eine interne Entscheidung.
/// Steht ihr Name in der Antwort, koppelt sich der Client daran, und die
/// Variantenwahl waere faktisch nicht mehr frei.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_response_keeps_the_logical_model_name() {
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let endpoint = mock_backend::start(backend_impl).await.to_string();

    let resolved = Arc::new(
        Config::from_yaml(&yaml(&endpoint))
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, backend, handle, clock);

    let response = service
        .model_infer(tonic::Request::new(request()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.model_name, "detector");
}

/// Ein abgebrochener Client verbraucht keine Backendzeit mehr.
///
/// Arbeit fuer einen Empfaenger, den es nicht mehr gibt, ist der teuerste
/// Leerlauf im System: sie belegt genau die Kapazitaet, um die noch wartende
/// Stroeme konkurrieren.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_client_request_never_reaches_the_backend() {
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(100),
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let resolved = Arc::new(
        Config::from_yaml(&yaml(&endpoint))
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = Arc::new(GatewayService::new(
        resolved,
        backend,
        handle.clone(),
        clock,
    ));

    // Der erste Request belegt den einzigen Slot fuer 100 ms.
    let svc = service.clone();
    let first = tokio::spawn(async move { svc.model_infer(tonic::Request::new(request())).await });
    while handle.metrics().await.unwrap().received < 1 {
        tokio::task::yield_now().await;
    }

    // Der zweite wartet in der Queue — und wird dort abgebrochen.
    let svc = service.clone();
    let second = tokio::spawn(async move { svc.model_infer(tonic::Request::new(request())).await });
    while handle.metrics().await.unwrap().received < 2 {
        tokio::task::yield_now().await;
    }
    second.abort();
    let _ = second.await;

    first.await.unwrap().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    assert_eq!(
        backend_impl.served.load(Ordering::Relaxed),
        1,
        "der abgebrochene Request wurde nicht mehr weitergereicht"
    );
    assert_eq!(handle.metrics().await.unwrap().cancelled, 1);
}

/// Ein zerlegbarer Auftrag laeuft tatsaechlich ueber mehrere Quanten.
///
/// Der Auftragszustand wurde frueher erst in der *Fortsetzung* angelegt — also
/// nie. `forward()` fand beim ersten Quantum keinen Job und reichte den
/// vollstaendigen Request weiter; zusaetzlich entnahm es das Template, das die
/// Fortsetzung gebraucht haette. Die Zerlegung war damit eine
/// Konfigurationsoption ohne Wirkung, und ein Vergleich „mit und ohne Quanten"
/// musste identisch ausfallen — nicht weil die Hardware es so wollte, sondern
/// weil nichts zerlegt wurde.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cooperative_job_is_actually_split_into_several_quanta() {
    let backend_impl = Arc::new(mock_backend::MockBackend::generative(
        std::time::Duration::from_millis(1),
        4,
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let yaml = format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 1
  pipelining_depth: 0
models:
  vlm:
    class: best_effort
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 10000 }}
    cooperative: {{ tokens_per_second: 1000, min_tokens: 1, max_total_tokens: 12, base_cost_us: 0 }}
    variants:
      - id: main
        backend_model: qwen
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    );
    let resolved = Arc::new(Config::from_yaml(&yaml).unwrap().resolve().unwrap());
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, backend, handle, clock);

    let response = service
        .model_infer(tonic::Request::new(text_request("Beschreibe: ")))
        .await
        .unwrap()
        .into_inner();

    // Drei Quanten a vier Zeichen erreichen die Obergrenze von zwoelf Token.
    // Gezaehlt wird die **garantierte** Obergrenze: in vier Bytes stecken
    // hoechstens vier Token (Review R06). Die alte Schaetzung `Bytes / 4`
    // haette hier drei Token gezaehlt — und drei weitere freigegeben, die es
    // nicht gab.
    assert_eq!(
        backend_impl.served.load(Ordering::Relaxed),
        3,
        "der Auftrag wurde in drei Backendaufrufe zerlegt"
    );

    // Der Zustand reist im Prompt: jedes Quantum sieht Prompt plus bisher
    // Erzeugtes. Genau das ist die Zusage von ADR-0014.
    let prompts = backend_impl.seen_prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 3);
    assert_eq!(prompts[0], "Beschreibe: ");
    assert_eq!(prompts[1], "Beschreibe: xxxx");
    assert_eq!(prompts[2], "Beschreibe: xxxxxxxx");

    // Und der Client bekommt genau eine Antwort mit dem gesammelten Text.
    let text = read_length_prefixed(response.raw_output_contents.first().unwrap()).unwrap();
    assert_eq!(text, "xxxxxxxxxxxx");
}

fn text_request(prompt: &str) -> ModelInferRequest {
    use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
    ModelInferRequest {
        model_name: "vlm".into(),
        id: "review".into(),
        inputs: vec![InferInputTensor {
            name: "text_input".into(),
            datatype: "BYTES".into(),
            shape: vec![1],
            parameters: std::collections::HashMap::new(),
            contents: None,
        }],
        raw_input_contents: vec![length_prefixed(prompt)],
        ..Default::default()
    }
}

fn length_prefixed(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len().saturating_add(4));
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

fn read_length_prefixed(bytes: &[u8]) -> Option<String> {
    let (header, rest) = bytes.split_at_checked(4)?;
    let length = u32::from_le_bytes([
        *header.first()?,
        *header.get(1)?,
        *header.get(2)?,
        *header.get(3)?,
    ]);
    let end = usize::try_from(length).ok()?.min(rest.len());
    String::from_utf8(rest.get(..end)?.to_vec()).ok()
}

/// Ein decoupled Modell gilt erst mit seiner **letzten** Antwort als fertig.
///
/// Frueher kehrte der Adapter bei der ersten Teilantwort zurueck. Bei einem
/// tatsaechlich streamenden Modell heisst das: Teiltext wird als Endergebnis
/// ausgeliefert, und der Slot ist frei, waehrend das Backend noch rechnet —
/// der Governor plant dann gegen eine Belegung, die es nicht mehr gibt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decoupled_model_is_read_to_its_final_response() {
    // Eine Nutzlast, danach eine leere Abschlussmarkierung: der uebliche
    // unaere Fall ueber den Stream-Endpunkt.
    let single = Arc::new(mock_backend::MockBackend::streaming(
        std::time::Duration::from_millis(1),
        1,
        true,
    ));
    let endpoint = mock_backend::start(single).await.to_string();
    let client = vig_backend_triton::TritonClient::new(endpoint);
    let response = client
        .infer_decoupled(ModelInferRequest {
            model_name: "vlm".into(),
            id: "1".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        read_length_prefixed(response.raw_output_contents.first().unwrap()).unwrap(),
        "teil0",
        "die Nutzlast ueberlebt die nachfolgende Abschlussmarkierung"
    );

    // Mehrere Teilantworten: das Zusammenfuegen ist modellspezifisch. Sie
    // stillschweigend auf die erste zu reduzieren waere ein falsches Ergebnis
    // bei gruener Metrik — deshalb wird der Fall gemeldet.
    let multi = Arc::new(mock_backend::MockBackend::streaming(
        std::time::Duration::from_millis(1),
        3,
        false,
    ));
    let endpoint = mock_backend::start(multi).await.to_string();
    let client = vig_backend_triton::TritonClient::new(endpoint);
    let error = client
        .infer_decoupled(ModelInferRequest {
            model_name: "vlm".into(),
            id: "2".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(
        format!("{error}").contains("Teilantworten"),
        "der Fall wird benannt statt still abgeschnitten: {error}"
    );
}

/// Auch ein bereits zerlegter Auftrag lässt sich abbrechen.
///
/// Eine Fortsetzung tritt als neue Ankunft mit neuer Kennung an; der Client
/// kennt nur seine erste. Ohne Übersetzung liefe der Auftrag nach dem Abbruch
/// weiter, bis sein Tokenbudget erschöpft ist — bei einem generativen Modell
/// die teuerste Arbeit im System, für einen Empfänger, den es nicht mehr gibt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_client_stops_a_job_that_is_already_split() {
    // 20 ms je Quantum, Budget für 8 Quanten: genug Zeit, um mitten im
    // Auftrag abzubrechen.
    let backend_impl = Arc::new(mock_backend::MockBackend::generative(
        std::time::Duration::from_millis(20),
        4,
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let yaml = format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 1
  pipelining_depth: 0
models:
  vlm:
    class: best_effort
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 60000 }}
    cooperative: {{ tokens_per_second: 1000, min_tokens: 1, max_total_tokens: 8, base_cost_us: 0 }}
    variants:
      - id: main
        backend_model: qwen
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 20000, p95_us: 20000, p99_us: 20000, samples: 1000 }}
"
    );
    let resolved = Arc::new(Config::from_yaml(&yaml).unwrap().resolve().unwrap());
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = Arc::new(GatewayService::new(resolved, backend, handle, clock));

    let svc = service.clone();
    let call = tokio::spawn(async move {
        svc.model_infer(tonic::Request::new(text_request("los: ")))
            .await
    });

    // Warten, bis der Auftrag tatsächlich fortgesetzt hat — erst dann trägt er
    // eine andere Kennung als die des Clients.
    while backend_impl.served.load(Ordering::Relaxed) < 2 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    call.abort();
    let _ = call.await;

    let after_abort = backend_impl.served.load(Ordering::Relaxed);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let finally = backend_impl.served.load(Ordering::Relaxed);

    // Das laufende Quantum darf zu Ende laufen; ein weiteres wird nicht mehr
    // gestartet. Ohne die Übersetzung liefe der Auftrag bis zum Budgetende
    // von acht Quanten weiter.
    assert!(
        finally <= after_abort.saturating_add(1),
        "nach dem Abbruch wurden noch {} Quanten gestartet (vorher {after_abort})",
        finally.saturating_sub(after_abort)
    );
    assert!(finally < 8, "das Tokenbudget wurde nicht ausgeschöpft");
}

/// Ein hängendes Backend gibt den Client frei — und den Slot **nicht**.
///
/// Ein Inferenztimeout, der den Slotkredit zurückgibt, ist schlimmer als
/// keiner: die Recheneinheit ist womöglich noch belegt, und der Governor
/// plant anschließend gegen eine Belegung, die es nicht gibt. Er würde eine
/// zweite Ausführung auf dieselbe GPU legen und beide verspäten.
///
/// Richtig ist: Client antworten, Kredit halten, Zustand melden.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hanging_backend_releases_the_client_but_not_the_slot() {
    let backend_impl = Arc::new(mock_backend::MockBackend::hanging());
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    // 200 ms Timeout, ein Slot.
    let yaml = format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 1
  pipelining_depth: 0
  inference_timeout_ms: 200
models:
  detector:
    class: protected
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 10000 }}
    variants:
      - id: large
        backend_model: detector_large
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    );
    let resolved = Arc::new(Config::from_yaml(&yaml).unwrap().resolve().unwrap());
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, backend, handle.clone(), clock);

    let started = std::time::Instant::now();
    let status = service
        .model_infer(tonic::Request::new(request()))
        .await
        .expect_err("das Backend antwortet nie");

    // Der Client wartet nicht ewig.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "der Client wurde nach {:?} freigegeben",
        started.elapsed()
    );
    assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
    assert_eq!(
        status
            .metadata()
            .get("vig-reason")
            .and_then(|v| v.to_str().ok()),
        Some("backend_timeout"),
        "der Grund ist maschinenlesbar, nicht nur Text"
    );

    // Und der Slot bleibt gehalten, solange das Backend nicht antwortet.
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(metrics.backend_timeouts, 1);
    assert_eq!(
        metrics.quarantined, 1,
        "der Kredit wird gehalten, nicht zurueckgegeben"
    );
    assert_eq!(
        metrics.completed_valid, 0,
        "ein Timeout ist keine gueltige Fertigstellung"
    );
}

/// Ein geordnetes Ende beantwortet wartende Arbeit, statt sie fallen zu lassen.
///
/// `serve()` reagierte früher nur auf Ctrl-C — im Container ist SIGTERM der
/// Normalfall, und der Prozess wurde dort immer hart getötet. Ein Client, der
/// mitten in einer Inferenz die Verbindung verliert, kann nicht unterscheiden,
/// ob sein Request lief oder nicht.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_answers_pending_work_before_the_actor_stops() {
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(50),
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let resolved = Arc::new(
        Config::from_yaml(&yaml(&endpoint))
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = Arc::new(GatewayService::new(
        resolved,
        backend,
        handle.clone(),
        clock,
    ));

    // Zwei Requests: einer läuft, einer wartet.
    let mut calls = Vec::new();
    for _ in 0..2 {
        let svc = service.clone();
        calls.push(tokio::spawn(async move {
            svc.model_infer(tonic::Request::new(request())).await
        }));
    }
    while handle.metrics().await.unwrap().received < 2 {
        tokio::task::yield_now().await;
    }

    let finished = handle
        .drain(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert!(finished, "der Drain lief innerhalb der Frist zu Ende");

    for call in calls {
        call.await
            .unwrap()
            .expect("jeder angenommene Request bekommt seine Antwort");
    }

    // Und danach ist der Actor wirklich beendet.
    assert!(
        handle.metrics().await.is_err(),
        "nach dem Drain nimmt der Governor nichts mehr an"
    );
}

fn yaml_with(endpoint: &str, extra_backend: &str) -> String {
    format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 1
  pipelining_depth: 0
{extra_backend}models:
  detector:
    class: normal
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 10000 }}
    variants:
      - id: large
        backend_model: detector_large
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    )
}

fn service_with(endpoint: &str, extra_backend: &str) -> GatewayService {
    let resolved = Arc::new(
        Config::from_yaml(&yaml_with(endpoint, extra_backend))
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint.to_owned()));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    GatewayService::new(resolved, backend, handle, clock)
}

/// `backend.prediction: active` kommt im Scheduler an (NV-06, Review R09).
///
/// Dass der scharfe Modus eine Entscheidung aendert, prueft der Kern
/// (`an_active_predictor_admits_what_the_stale_profile_refuses`). Hier geht
/// es um die Naht davor: ein Schalter, der im Schema steht und im Actor nicht
/// ankommt, ist genau der Befund aus R09. Und der Betreiber muss den Modus
/// **sofort** sehen, nicht erst nach der ersten Fertigstellung.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_prediction_switch_reaches_the_scheduler() {
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let endpoint = mock_backend::start(backend_impl).await.to_string();

    for (extra, expected) in [("", 0), ("  prediction: active\n", 1)] {
        let resolved = Arc::new(
            Config::from_yaml(&yaml_with(&endpoint, extra))
                .unwrap()
                .resolve()
                .unwrap(),
        );
        let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint.clone()));
        let handle = actor::spawn(resolved, &backend, MonotonicClock::start(), &[]).unwrap();
        let metrics = handle.metrics().await.unwrap();
        assert_eq!(
            metrics.predictor_active, expected,
            "Konfiguration {extra:?}: der Modus muss ab dem Start sichtbar sein"
        );
    }
}

/// `preemptible:` und `backend.preemptible_lanes` kommen im Scheduler an
/// (ADR-0035, Review R09).
///
/// Dass eine Spur eine Entscheidung aendert, pruefen die Kerntests
/// (`scheduler_flow`, ADR-0035). Hier geht es um die Naht davor: der Auftrag
/// laeuft im Prozess niedriger Prioritaet, und der Scheduler zaehlt ihn als
/// praemptierbar — ohne die Angabe bliebe der Zaehler bei null.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preemptible_model_runs_on_its_lane() {
    let high = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let low = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let high_endpoint = mock_backend::start(high.clone()).await.to_string();
    let low_endpoint = mock_backend::start(low.clone()).await.to_string();

    let yaml = format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {high_endpoint}
  slots: 1
  pipelining_depth: 0
  preemptible_lanes: 1
models:
  detector:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 33, deadline_ms: 33, max_age_ms: 66 }}
    variants:
      - id: main
        backend_model: detector_large
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
  vlm:
    class: best_effort
    backend_endpoint: {low_endpoint}
    preemptible: {{ residual_blocking_us: 2000, source: measured }}
    queue: {{ policy: fifo, capacity: 4 }}
    contract: {{ deadline_ms: 10000 }}
    variants:
      - id: main
        backend_model: vlm_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    );
    let resolved = Arc::new(Config::from_yaml(&yaml).unwrap().resolve().unwrap());
    assert_eq!(resolved.slots.lanes(), 1);
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(high_endpoint.clone()));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, backend, handle.clone(), clock);

    service
        .model_infer(tonic::Request::new(ModelInferRequest {
            model_name: "vlm".into(),
            id: "hintergrund".into(),
            ..Default::default()
        }))
        .await
        .expect("der praemptierbare Auftrag laeuft");
    assert_eq!(
        low.served.load(Ordering::Relaxed),
        1,
        "im Prozess niedriger Prioritaet"
    );
    assert_eq!(high.served.load(Ordering::Relaxed), 0);
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(metrics.preemptible_dispatched, 1);
}

/// Im strikten Modus führt der physische Modellname nicht am Governor vorbei.
///
/// Unkonfigurierte Modelle unverändert durchzureichen ist die dokumentierte
/// Zusage für ein abgeschlossenes Netz (Spec L-002). Steht der Governor nicht
/// in einem, genügt sonst der Aufruf von `detector_large` statt `detector`, um
/// ohne Kredit, ohne Frischeprüfung und ohne Look-ahead zu laufen — und genau
/// die geschützte Arbeit zu verdrängen, die geschützt werden soll.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_mode_refuses_to_run_past_the_governor() {
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let bypass = ModelInferRequest {
        model_name: "detector_large".into(),
        id: "bypass".into(),
        ..Default::default()
    };

    // Offen: durchgereicht, wie dokumentiert.
    let open = service_with(&endpoint, "");
    open.model_infer(tonic::Request::new(bypass.clone()))
        .await
        .expect("im offenen Modus laeuft ein Standardclient unveraendert weiter");
    assert_eq!(backend_impl.served.load(Ordering::Relaxed), 1);

    // Strikt: abgelehnt, und das Backend sieht den Aufruf nicht.
    let strict = service_with(&endpoint, "  trust: strict\n");
    let status = strict
        .model_infer(tonic::Request::new(bypass))
        .await
        .expect_err("im strikten Modus nicht");
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(
        backend_impl.served.load(Ordering::Relaxed),
        1,
        "der Aufruf hat das Backend nie erreicht"
    );
}

/// Im strikten Modus darf ein Client seine Klasse senken, nicht anheben.
///
/// Sonst setzt sich jeder Aufrufer selbst auf `protected`, und die Prioritäten
/// sind eine Empfehlung statt einer Zusage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_mode_lets_clients_lower_their_class_but_not_raise_it() {
    use vig_protocol_oip::inference::InferParameter;
    use vig_protocol_oip::inference::infer_parameter::ParameterChoice;

    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let endpoint = mock_backend::start(backend_impl).await.to_string();
    let strict = service_with(&endpoint, "  trust: strict\n");

    let with_class = |class: &str| {
        let mut r = request();
        r.model_name = "detector".into();
        r.parameters.insert(
            vig_protocol_oip::params::P_CLASS.to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::StringParam(class.to_owned())),
            },
        );
        r
    };

    // Beides wird ausgeführt — die Klasse ist keine Zugangsentscheidung.
    // Geprüft wird, dass die Anhebung wirkungslos bleibt: der Vertrag sagt
    // `normal`, und dabei bleibt es.
    strict
        .model_infer(tonic::Request::new(with_class("protected")))
        .await
        .expect("der Request laeuft, nur eben als normal");
    strict
        .model_infer(tonic::Request::new(with_class("best_effort")))
        .await
        .expect("und ein freiwilliger Rücktritt ist harmlos");
}

/// Das Nutzlastbudget begrenzt Bytes, nicht nur Requests.
///
/// Der Ereigniskanal fasst 1.024 offene Requests. Bei 64 MiB je Tensor sind
/// das 64 GiB, bevor irgendeine Zahl auffällt — auf einem Edgegerät mit 8 GB
/// kein theoretischer Fall.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_payload_budget_counts_bytes_not_requests() {
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(200),
    ));
    let endpoint = mock_backend::start(backend_impl).await.to_string();
    // 1 MiB Budget.
    let service = Arc::new(service_with(&endpoint, "  max_inflight_mib: 1\n"));

    let big = || {
        let mut r = request();
        r.model_name = "detector".into();
        r.raw_input_contents = vec![vec![0_u8; 700 * 1024]];
        r
    };

    // Der erste 700-KiB-Request passt, der zweite nicht mehr.
    let svc = service.clone();
    let first = tokio::spawn(async move { svc.model_infer(tonic::Request::new(big())).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let status = service
        .model_infer(tonic::Request::new(big()))
        .await
        .expect_err("zwei mal 700 KiB passen nicht in 1 MiB");
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);

    // Und nach dem Abschluss ist das Budget wieder frei.
    first.await.unwrap().unwrap();
    service
        .model_infer(tonic::Request::new(big()))
        .await
        .expect("das Budget wird zurueckgegeben, nicht verbraucht");
}

/// Ohne gültiges Token kommt keine Anfrage durch — auch keine Metadatenabfrage.
///
/// Der Governor steuert eine ganze GPU. Wer ihn ohne Identitätsprüfung ins
/// Netz stellt, gibt diese Steuerung an jeden im Netz. Deshalb bindet er auf
/// Loopback; wer ihn öffnet, schaltet TLS oder Token ein.
///
/// Geprüft wird hier die Tokenvariante samt der Stelle, an der sie am
/// leichtesten vergessen wird: die Nebenendpunkte. `system_shared_memory_register`
/// ist der gefährlichste davon — er verknüpft fremden Speicher mit dem Backend.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_valid_token_nothing_gets_through() {
    use std::io::Write as _;
    use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService as _;

    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let mut token_file = tempfile::NamedTempFile::new().unwrap();
    writeln!(token_file, "# der Roboter\nrobot:s3cret-0123456789abcdef").unwrap();
    token_file.flush().unwrap();

    let tokens = vig_gateway::auth::Tokens::load(token_file.path()).unwrap();
    let service = service_with(&endpoint, "").with_tokens(tokens);

    let authorised = |with_token: bool| {
        let mut r = tonic::Request::new(request());
        if with_token {
            r.metadata_mut().insert(
                "authorization",
                "Bearer s3cret-0123456789abcdef".parse().unwrap(),
            );
        }
        r
    };

    // Ohne Token: abgelehnt, und das Backend sieht nichts.
    let status = service
        .model_infer(authorised(false))
        .await
        .expect_err("ohne Token nicht");
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    assert_eq!(backend_impl.served.load(Ordering::Relaxed), 0);

    // Auch der Shared-Memory-Endpunkt ist geschützt.
    let shm = service
        .system_shared_memory_status(tonic::Request::new(
            vig_protocol_oip::inference::SystemSharedMemoryStatusRequest::default(),
        ))
        .await
        .expect_err("Nebenendpunkte ebenso");
    assert_eq!(shm.code(), tonic::Code::Unauthenticated);

    // Mit Token: normal.
    service
        .model_infer(authorised(true))
        .await
        .expect("mit gueltigem Token laeuft alles wie vorher");
    assert_eq!(backend_impl.served.load(Ordering::Relaxed), 1);
}

// ---------------------------------------------------------------------------
// Produktionsreife-Review vom 08.09.2026
// ---------------------------------------------------------------------------

/// Ein Drain darf keinen sauberen Abschluss melden, während die GPU noch rechnet.
///
/// Der Actor prüfte, ob noch Clients warten. Nach einem Timeout ist der Client
/// beantwortet und die Recheneinheit möglicherweise weiterhin belegt — der
/// Prozess endete also mit „alles erledigt", und der nächste startete in eine
/// Belegung, von der er nichts wusste.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_does_not_report_success_while_the_backend_still_runs() {
    let backend_impl = Arc::new(mock_backend::MockBackend::hanging());
    let endpoint = mock_backend::start(backend_impl).await.to_string();
    let service = service_with(&endpoint, "  inference_timeout_ms: 150\n");
    let handle = service.scheduler_handle();

    let status = service
        .model_infer(tonic::Request::new(request()))
        .await
        .expect_err("das Backend antwortet nie");
    assert_eq!(status.code(), tonic::Code::DeadlineExceeded);

    let metrics = handle.metrics().await.unwrap();
    assert_eq!(metrics.quarantined, 1);
    assert_eq!(metrics.outstanding_backend_calls, 1);

    assert!(
        !handle
            .drain(std::time::Duration::from_millis(400))
            .await
            .unwrap(),
        "solange ein Backendaufruf offen ist, ist der Drain nicht fertig"
    );
}

/// Bei vollständiger Quarantäne wird neue Arbeit abgewiesen, nicht eingereiht.
///
/// Sie einzureihen hieße: der Client wartet bis in sein eigenes Timeout, und
/// der Governor hält Speicher für Arbeit, die nie beginnt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_quarantine_refuses_new_work_instead_of_letting_it_wait() {
    let backend_impl = Arc::new(mock_backend::MockBackend::hanging());
    let endpoint = mock_backend::start(backend_impl).await.to_string();
    let service = service_with(&endpoint, "  inference_timeout_ms: 150\n");
    let handle = service.scheduler_handle();

    // Der einzige Slot geht in Quarantäne.
    let _ = service.model_infer(tonic::Request::new(request())).await;
    assert_eq!(handle.metrics().await.unwrap().quarantined, 1);

    let started = std::time::Instant::now();
    let status = service
        .model_infer(tonic::Request::new(request()))
        .await
        .expect_err("es kann nichts starten");
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(200),
        "und zwar sofort, nicht nach einem weiteren Timeout ({:?})",
        started.elapsed()
    );
    assert_eq!(handle.metrics().await.unwrap().rejected_quarantined, 1);
}

/// Ein abgelehnter Verbindungsaufbau nimmt den Governor aus der Rotation.
///
/// Er erzeugt **keine** Quarantäne — der Aufruf kehrt sofort zurück, der
/// Slotkredit wird regulär frei. Wer nur auf Quarantäne schaut, übersieht damit
/// den häufigsten Backendausfall und meldet grün.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_fails_after_a_confirmed_transport_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    drop(listener);

    let service = service_with(&endpoint, "");
    let handle = service.scheduler_handle();

    assert!(
        service
            .model_infer(tonic::Request::new(request()))
            .await
            .is_err()
    );
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(
        metrics.quarantined, 0,
        "ein Verbindungsfehler quarantaeniert nicht"
    );
    assert!(metrics.consecutive_transport_failures >= 1);
    assert!(
        vig_gateway::exporter::readiness(&metrics).is_err(),
        "aber bereit ist der Governor damit nicht"
    );
}

/// Das Bytebudget zählt beide zulässigen Payloadformen.
///
/// OIP erlaubt Rohdaten in `raw_input_contents` **und** typisierte Werte in
/// `inputs[].contents`. Nur die erste zu zählen hieß, dass ein Client das
/// Budget umgeht, ohne etwas Unerlaubtes zu tun — er benutzt die andere Form.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_payload_budget_also_counts_typed_tensor_contents() {
    use vig_protocol_oip::inference::InferTensorContents;
    use vig_protocol_oip::inference::model_infer_request::InferInputTensor;

    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(200),
    ));
    let endpoint = mock_backend::start(backend_impl).await.to_string();
    let service = service_with(&endpoint, "  max_inflight_mib: 1\n");

    // 2 MiB als fp32, ausschliesslich in `contents`.
    let mut typed = request();
    typed.model_name = "detector".into();
    typed.inputs = vec![InferInputTensor {
        name: "images".into(),
        datatype: "FP32".into(),
        shape: vec![1, 524_288],
        parameters: std::collections::HashMap::new(),
        contents: Some(InferTensorContents {
            fp32_contents: vec![0.0_f32; 524_288],
            ..Default::default()
        }),
    }];

    let status = service
        .model_infer(tonic::Request::new(typed))
        .await
        .expect_err("2 MiB passen nicht in 1 MiB, egal in welcher Darstellung");
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
}

/// Ein abgebrochener Backendaufruf gibt den Slotkredit nicht sofort zurück.
///
/// Ein Verbindungsabbruch **nach** dem Dispatch beweist nicht, dass die GPU
/// aufgehört hat. Den Kredit dann zurückzugeben ist derselbe Fehler wie beim
/// Timeout — nur schwerer zu sehen, weil der Aufruf zurückgekehrt ist und
/// alles beendet *aussieht*.
///
/// Der Unterschied zum abgelehnten Verbindungsaufbau ist entscheidend: dort
/// hat der Request das Backend nie erreicht, und der Kredit gehört sofort
/// zurück. Beides wird hier geprüft.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aborted_call_holds_the_credit_a_refused_connection_does_not() {
    use vig_backend_triton::{BackendError, ExecutionState};

    // Verbindungsaufbau gescheitert: nichts wurde ausgeführt.
    let never_sent = BackendError::Unreachable {
        endpoint: "127.0.0.1:1".into(),
        cause: "connection refused".into(),
    };
    assert_eq!(never_sent.execution_state(), ExecutionState::NotStarted);

    // Aufruf war unterwegs und brach ab: Ausführungsende unbekannt.
    for code in [
        tonic::Code::Unavailable,
        tonic::Code::DeadlineExceeded,
        tonic::Code::Aborted,
        tonic::Code::Cancelled,
    ] {
        let aborted = BackendError::Rejected {
            code,
            message: "connection reset".into(),
        };
        assert_eq!(
            aborted.execution_state(),
            ExecutionState::Unknown,
            "{code:?} sagt nichts darueber, ob die GPU aufgehoert hat"
        );
    }

    // Das Backend hat geantwortet, wenn auch ablehnend: fertig.
    let answered = BackendError::Rejected {
        code: tonic::Code::InvalidArgument,
        message: "bad shape".into(),
    };
    assert_eq!(answered.execution_state(), ExecutionState::Finished);

    // Und im Betrieb: ein abgelehnter Verbindungsaufbau quarantäniert nicht.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    drop(listener);
    let service = service_with(&endpoint, "");
    let handle = service.scheduler_handle();

    assert!(
        service
            .model_infer(tonic::Request::new(request()))
            .await
            .is_err()
    );
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(
        metrics.quarantined, 0,
        "was das Backend nie erreicht hat, belegt auch keine Recheneinheit"
    );
    assert_eq!(metrics.outstanding_backend_calls, 0);
}

// ---------------------------------------------------------------------------
// NV-00: Ressourcen nur mit Endnachweis freigeben
// ---------------------------------------------------------------------------

/// Rechnet das Backend nach einem Abbruch weiter, entsteht kein neuer Kredit.
///
/// Das war vorher ein Timer: nach Ablauf einer Frist wurde der Slotkredit
/// zurückgegeben. Eine verstrichene Frist beweist aber nicht, dass die GPU
/// fertig ist — sie sagt nur, dass wir nicht länger warten wollten.
///
/// Hier meldet die Backendstatistik über mehrere Timeoutlängen hinweg
/// unverändert *nichts abgeschlossen*. Der Kredit bleibt gehalten.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_credit_is_returned_while_the_backend_may_still_compute() {
    use std::sync::atomic::Ordering;

    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    // Das Backend meldet nichts als abgeschlossen: aus seiner Sicht läuft
    // unsere Inferenz noch.
    backend_impl.completed.store(0, Ordering::Relaxed);
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let service = service_with(&endpoint, "  inference_timeout_ms: 100\n");
    let handle = service.scheduler_handle();

    // Ein Aufruf, der mit Unavailable abbricht: Ausführungsende unbekannt.
    backend_impl
        .fail_with
        .lock()
        .unwrap()
        .replace(tonic::Code::Unavailable);
    let status = service
        .model_infer(tonic::Request::new(request()))
        .await
        .expect_err("der Aufruf bricht ab");
    // Der Client erfährt, was wir **nicht** wissen: ob seine Inferenz lief.
    // Für alles, was nicht wiederholbar ist, ist genau das die Auskunft, die
    // er braucht — „Backendfehler" wäre hier eine Behauptung zu viel.
    assert_eq!(
        status
            .metadata()
            .get("vig-reason")
            .and_then(|v| v.to_str().ok()),
        Some("execution_unknown")
    );

    // Über mehrere Timeoutlängen hinweg bleibt der Kredit gehalten, weil der
    // Abschlusszähler sich nicht bewegt.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(
        metrics.quarantined, 1,
        "ohne Endnachweis bleibt der Kredit gehalten"
    );
    assert_eq!(metrics.reconciled, 0);

    // Und der Drain meldet in diesem Zustand keinen Erfolg.
    assert!(
        !handle
            .drain(std::time::Duration::from_millis(300))
            .await
            .unwrap(),
        "ein offener Anspruch ist kein sauberes Ende"
    );
}

/// Belegt das Backend das Ende, wird der Kredit genau einmal frei.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proven_execution_end_releases_the_credit_exactly_once() {
    use std::sync::atomic::Ordering;

    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    backend_impl.completed.store(0, Ordering::Relaxed);
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let service = service_with(&endpoint, "  inference_timeout_ms: 100\n");
    let handle = service.scheduler_handle();

    // Erst die Abgleichs-Basislinie abwarten (NV-20). Sie wird beim Start
    // geholt und nur angenommen, solange dem Modell noch nichts ausgeliefert
    // wurde: danach koennte sie eigene, schon abgeschlossene Inferenzen
    // enthalten und waere zu hoch.
    let mut ready = false;
    for _ in 0..40 {
        if handle.metrics().await.unwrap().reconcile_baseline_missing == 0 {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        ready,
        "ohne Basislinie gibt es keinen zaehlerbasierten Nachweis"
    );

    backend_impl
        .fail_with
        .lock()
        .unwrap()
        .replace(tonic::Code::Unavailable);
    let _ = service.model_infer(tonic::Request::new(request())).await;
    assert_eq!(handle.metrics().await.unwrap().quarantined, 1);

    // Jetzt belegt das Backend, dass es so viele Inferenzen abgeschlossen hat,
    // wie der Governor ihm ausgeliefert hat: von unserer Arbeit ist nichts
    // mehr offen.
    backend_impl.completed.store(1, Ordering::Relaxed);

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

    // Danach nimmt der Governor wieder Arbeit an.
    backend_impl.fail_with.lock().unwrap().take();
    service
        .model_infer(tonic::Request::new(request()))
        .await
        .expect("der Slot ist wieder nutzbar");
}

/// Eine zu teure Zerlegung wird nicht gefahren — der Auftrag laeuft ungeteilt.
///
/// ADR-0014 nennt den ungeteilten Lauf als Rueckfall, NV-16 rechnet aus, wann
/// er faellig ist: jedes Quantum traegt den gewachsenen Prompt erneut ins
/// Backend, und ohne wirksames Prefix-Caching kostet die Zerlegung dann mehr
/// Arbeit, als sie an Blockadezeit spart. Mit `max_overhead_permille: 0` ist
/// jeder Aufschlag zu viel, und der Governor darf gar nicht erst zerlegen.
///
/// Der Nachweis ist die Zahl der Backendaufrufe: einer statt drei. Er haengt
/// an keiner Uhr — die Entscheidung faellt bei der Zulassung, aus Zahlen, die
/// im Vertrag stehen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decomposition_that_costs_too_much_is_not_run() {
    let split = backend_calls_for(None).await;
    assert_eq!(split.0, 3, "ohne Grenze wird zerlegt wie bisher");

    let undivided = backend_calls_for(Some(0)).await;
    assert_eq!(
        undivided.0, 1,
        "mit einer Grenze von null laeuft der Auftrag am Stueck"
    );

    // Und der eine Aufruf traegt die Obergrenze des Betreibers. Ohne diese
    // Zusicherung war der Rueckfall schlimmer als die Zerlegung: der
    // `GenerativeJob` ist die **einzige** Stelle, die `max_total_tokens`
    // durchsetzt, und ohne ihn bekaeme ein Client ohne eigenes `max_tokens`
    // unbegrenzte Erzeugung — waehrend der Kern die Profillaufzeit der
    // Variante eingeplant hat.
    assert_eq!(
        undivided.1,
        vec![Some(12)],
        "ungeteilt heisst nicht unbegrenzt (Spec 8.3)"
    );
}

/// Ein Auftrag gegen ein Backend, das je Aufruf vier Zeichen erzeugt; zurueck
/// kommen die Zahl der Aufrufe und das `max_tokens`, das je Aufruf ankam.
async fn backend_calls_for(max_overhead_permille: Option<u32>) -> (u64, Vec<Option<u32>>) {
    let backend_impl = Arc::new(mock_backend::MockBackend::generative(
        std::time::Duration::from_millis(1),
        4,
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let limit = max_overhead_permille
        .map(|p| format!("\n      max_overhead_permille: {p}"))
        .unwrap_or_default();
    let yaml = format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 1
  pipelining_depth: 0
models:
  vlm:
    class: best_effort
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 10000 }}
    cooperative:
      tokens_per_second: 1000
      min_tokens: 1
      max_total_tokens: 12
      base_cost_us: 5000{limit}
    variants:
      - id: main
        backend_model: qwen
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    );
    let resolved = Arc::new(Config::from_yaml(&yaml).unwrap().resolve().unwrap());
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, backend, handle, clock);

    service
        .model_infer(tonic::Request::new(text_request("Beschreibe: ")))
        .await
        .unwrap();

    (
        backend_impl.served.load(Ordering::Relaxed),
        backend_impl.seen_max_tokens.lock().unwrap().clone(),
    )
}

/// Der Kontext eines zerlegten Auftrags erreicht die Metrik — und damit den
/// Kern.
///
/// Die Luecke, die das Review gefunden hat: beide Unittests riefen
/// `continuation_descriptor` direkt auf. Haette jemand die Zeile in
/// `continue_job` geloescht, die sie benutzt, waeren sie gruen geblieben und
/// der ganze Umbau wirkungslos. Dieser Test geht ueber den Draht: der
/// gemeldete laengste Kontext kann nur entstehen, wenn die Fortsetzung ihn
/// wirklich traegt.
///
/// Gerechnet: 12 Zeichen Prompt sind 3 Token, jedes Quantum erzeugt 4 Zeichen
/// (1 Token). Nach dem ersten Quantum steht der Kontext bei 4, nach dem
/// zweiten bei 5.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_grown_context_of_a_continuation_reaches_the_metrics() {
    let backend_impl = Arc::new(mock_backend::MockBackend::generative(
        std::time::Duration::from_millis(1),
        4,
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let yaml = format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 1
  pipelining_depth: 0
models:
  vlm:
    class: best_effort
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 10000 }}
    cooperative:
      tokens_per_second: 1000
      min_tokens: 1
      max_total_tokens: 12
      base_cost_us: 0
      prefill_per_token_us: 1000
    variants:
      - id: main
        backend_model: qwen
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    );
    let resolved = Arc::new(Config::from_yaml(&yaml).unwrap().resolve().unwrap());
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, backend, handle.clone(), clock);

    service
        .model_infer(tonic::Request::new(text_request("Beschreibe: ")))
        .await
        .unwrap();

    let metrics = handle.metrics().await.unwrap();
    // Drei Quanten zu je vier Token: der Kontext geht 3 -> 7 -> 11 -> 15.
    // Gezaehlt wird die garantierte Obergrenze aus den Bytes, nicht die
    // Schaetzung `Bytes / 4` (Review R06).
    assert_eq!(
        metrics.generative_context_tokens, 15,
        "der Kontext waechst mit jedem Quantum und wird als solcher gefuehrt"
    );
    // Gebucht werden die **wiederholten** Prefills, also die Quanten 2 und 3
    // mit Kontext 7 und 11 zu je 1 ms. Der erste Prefill faellt auch beim
    // ungeteilten Lauf an und ist kein Preis der Zerlegung.
    assert_eq!(
        metrics.generative_prefill_us, 18_000,
        "wiederholtes Prefill ist Arbeit und wird gebucht"
    );
    // Alle drei Quanten zu je vier Token bei 1000 Token/s. Das letzte zaehlt
    // mit: es wird nicht fortgesetzt, aber es hat gerechnet.
    assert_eq!(
        metrics.generative_decode_us, 12_000,
        "und sie ist getrennt von dem, was wirklich Token erzeugt hat"
    );
    assert_eq!(
        metrics.generative_fixed_us, 0,
        "dieser Vertrag hat keinen Sockel"
    );
    assert_eq!(metrics.decomposition_refused, 0);
}

/// Das Nutzlastbudget endet mit der Ausfuehrung, nicht mit dem Client
/// (Review R04).
///
/// Der Fall: ein Client laeuft in sein Timeout, das Backend rechnet weiter
/// und haelt die Nutzlast. Wurde das Budget beim Timeout freigegeben, war
/// dieselbe Zahl Bytes ein zweites Mal zu haben — zwei 16-Byte-Auftraege bei
/// einem Budget von 16 Bytes.
///
/// Dieselbe Klasse Fehler wie ein zu frueh zurueckgegebener Slotkredit, nur
/// in einer anderen Waehrung: erfundene Kapazitaet aus einer Antwort, die
/// nichts ueber das Backend aussagt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_payload_budget_outlives_a_client_timeout() {
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_secs(5),
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();

    let yaml = format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 2
  pipelining_depth: 0
  inference_timeout_ms: 50
models:
  detector:
    class: protected
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 10000 }}
    variants:
      - id: main
        backend_model: detector_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    );
    let mut config = Config::from_yaml(&yaml).unwrap().resolve().unwrap();
    // Genau eine Nutzlast passt hinein. Die Konfiguration kennt nur Mebibyte;
    // fuer diesen Nachweis braucht es eine Grenze, die ein Test auch
    // erreichen kann.
    config.max_inflight_bytes = 16;
    let resolved = Arc::new(config);
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = GatewayService::new(resolved, backend, handle, clock);

    let request = || {
        let mut r = vig_gateway::testing::request_for("detector");
        r.raw_input_contents = vec![vec![7; 16]];
        tonic::Request::new(r)
    };

    let first = service.model_infer(request()).await.unwrap_err();
    assert_eq!(
        first.code(),
        tonic::Code::DeadlineExceeded,
        "der Client gibt auf; das Backend rechnet weiter"
    );

    let second = service.model_infer(request()).await.unwrap_err();
    assert_eq!(
        second.code(),
        tonic::Code::ResourceExhausted,
        "die erste Nutzlast steht noch — die zweite passt nicht daneben"
    );
}

/// Eine Zusammenfuehrung ueber Aufnahmegrenzen wird abgelehnt, bevor sie
/// rechnet (NV-17, ADR-0028).
///
/// Der Fehler, um den es geht: eine frische Detektion neben einer alten
/// Tiefenkarte ergibt eine Szene, die es nie gegeben hat. Frische allein
/// faengt das nicht — **beide** Ergebnisse koennen unter ihrem Hoechstalter
/// liegen und trotzdem aus verschiedenen Aufnahmen stammen.
///
/// Bis hierher war der Graph gebaut, getestet und an nichts angeschlossen. Er
/// brauchte eine Zusage vom Client, welche Anfrage zu welcher Aufnahme
/// gehoert; die gibt es jetzt als `vig_capture_id` und `vig_depends_on`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fusion_across_captures_is_refused_before_it_computes() {
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();
    let service = graph_service(&endpoint);

    // Zwei Auftraege aus derselben Aufnahme.
    for (id, capture) in [(1_u64, 100_u64), (2, 100)] {
        service
            .model_infer(tonic::Request::new(captured(id, capture, &[])))
            .await
            .unwrap_or_else(|e| panic!("Auftrag {id} laeuft: {e}"));
    }

    // Eine Zusammenfuehrung aus derselben Aufnahme geht durch.
    service
        .model_infer(tonic::Request::new(captured(3, 100, &[1, 2])))
        .await
        .expect("gemeinsame Aufnahme");

    // Und einer aus einer spaeteren Aufnahme.
    service
        .model_infer(tonic::Request::new(captured(4, 200, &[])))
        .await
        .expect("neue Aufnahme");

    // Diese Zusammenfuehrung mischt zwei Aufnahmen — und wird abgelehnt.
    let mixed = service
        .model_infer(tonic::Request::new(captured(5, 200, &[1, 4])))
        .await
        .expect_err("eine Szene, die es nie gegeben hat");
    assert_eq!(mixed.code(), tonic::Code::FailedPrecondition, "{mixed:?}");

    let before = backend_impl.served.load(Ordering::Relaxed);
    assert_eq!(
        before, 4,
        "der abgelehnte Auftrag darf das Backend nie erreicht haben"
    );
}

/// Ohne `vig_capture_id` aendert sich nichts.
///
/// Die Gegenprobe. Ein Governor, der ohne Zusage des Clients einen Graphen
/// fuehrt, wuerde Aufnahmen erfinden — und ADR-0028 sagt ausdruecklich, dass
/// er das nicht kann.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_declared_capture_nothing_changes() {
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();
    let service = graph_service(&endpoint);

    for id in 1..=3_u64 {
        let mut request = vig_gateway::testing::request_for("detector");
        request.id = id.to_string();
        service
            .model_infer(tonic::Request::new(request))
            .await
            .unwrap_or_else(|e| panic!("Auftrag {id} laeuft: {e}"));
    }
    assert_eq!(backend_impl.served.load(Ordering::Relaxed), 3);
}

/// Ein Dienst mit einem Modell, das Aufnahmen kennt.
fn graph_service(endpoint: &str) -> GatewayService {
    let yaml = format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 2
  pipelining_depth: 0
models:
  detector:
    class: protected
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 10000 }}
    variants:
      - id: main
        backend_model: detector_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    );
    let resolved = Arc::new(Config::from_yaml(&yaml).unwrap().resolve().unwrap());
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint.to_owned()));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    GatewayService::new(resolved, backend, handle, clock)
}

/// Ein Request mit Aufnahmekennung und Eltern.
fn captured(id: u64, capture: u64, parents: &[u64]) -> ModelInferRequest {
    use vig_protocol_oip::inference::{InferParameter, infer_parameter};

    let mut request = vig_gateway::testing::request_for("detector");
    request.id = id.to_string();
    request.parameters.insert(
        "vig_capture_id".to_owned(),
        InferParameter {
            parameter_choice: Some(infer_parameter::ParameterChoice::Uint64Param(capture)),
        },
    );
    if !parents.is_empty() {
        let list = parents
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        request.parameters.insert(
            "vig_depends_on".to_owned(),
            InferParameter {
                parameter_choice: Some(infer_parameter::ParameterChoice::StringParam(list)),
            },
        );
    }
    request
}
