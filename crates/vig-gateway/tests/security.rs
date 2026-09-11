//! Security-Review vom 11.09.2026, Gatewayebene — und NV-17.
//!
//! Jede Pruefung hier sitzt an einer Naht, die der Kern nicht sieht: wer
//! anfragt, welche Region wem gehoert, was an einer Zugangspruefung vorbei
//! durchgereicht wird. Die Befundnummern stehen in `docs/security.md`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod mock_backend;

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tonic::Code;
use vig_config::Config;
use vig_gateway::auth::Tokens;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService as _;
use vig_protocol_oip::inference::infer_parameter::ParameterChoice;
use vig_protocol_oip::inference::model_infer_request::{
    InferInputTensor, InferRequestedOutputTensor,
};
use vig_protocol_oip::inference::{
    InferParameter, ModelInferRequest, RepositoryModelLoadRequest, RepositoryModelUnloadRequest,
    ServerLiveRequest, ServerReadyRequest, SystemSharedMemoryRegisterRequest,
    SystemSharedMemoryUnregisterRequest,
};

const ALPHA: &str = "alpha-0123456789abcdef";
const BETA: &str = "beta-0123456789abcdef";
const ROOT: &str = "root-0123456789abcdef";
const CONTRACT: &str = "{ deadline_ms: 10000 }";

fn yaml(endpoint: &str, extra_backend: &str, models: usize, contract: &str) -> String {
    let mut text = format!(
        "
version: 1
backend:
  type: triton
  grpc_endpoint: {endpoint}
  slots: 1
  pipelining_depth: 0
{extra_backend}models:
"
    );
    for m in 0..models {
        write!(
            text,
            "  m{m}:
    class: normal
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {contract}
    variants:
      - id: main
        backend_model: detector_{m}
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
        )
        .unwrap();
    }
    text
}

fn service(endpoint: &str, extra_backend: &str, models: usize, contract: &str) -> GatewayService {
    let resolved = Arc::new(
        Config::from_yaml(&yaml(endpoint, extra_backend, models, contract))
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint.to_owned()));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    GatewayService::new(resolved, backend, handle, clock)
}

async fn backend(compute: Duration) -> (Arc<mock_backend::MockBackend>, String) {
    let mock = Arc::new(mock_backend::MockBackend::new(compute));
    let endpoint = mock_backend::start(mock.clone()).await.to_string();
    (mock, endpoint)
}

async fn hanging_backend() -> String {
    let mock = Arc::new(mock_backend::MockBackend::hanging());
    mock_backend::start(mock).await.to_string()
}

fn tokens() -> Tokens {
    Tokens::parse(&format!("alpha:{ALPHA}\nbeta:{BETA}\n")).unwrap()
}

fn admin() -> Tokens {
    Tokens::parse(&format!("root:{ROOT}\n")).unwrap()
}

fn with_token<T>(message: T, token: Option<&str>) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    if let Some(token) = token {
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    request
}

fn reason(status: &tonic::Status) -> Option<&str> {
    status
        .metadata()
        .get("vig-reason")
        .and_then(|v| v.to_str().ok())
}

fn string_param(value: &str) -> InferParameter {
    InferParameter {
        parameter_choice: Some(ParameterChoice::StringParam(value.to_owned())),
    }
}

// ---------------------------------------------------------------------------
// H2 / M4 / N7 — Shared Memory
// ---------------------------------------------------------------------------

fn register(
    name: &str,
    key: &str,
    offset: u64,
    byte_size: u64,
    token: &str,
) -> tonic::Request<SystemSharedMemoryRegisterRequest> {
    with_token(
        SystemSharedMemoryRegisterRequest {
            name: name.to_owned(),
            key: key.to_owned(),
            offset,
            byte_size,
        },
        Some(token),
    )
}

fn unregister(name: &str, token: &str) -> tonic::Request<SystemSharedMemoryUnregisterRequest> {
    with_token(
        SystemSharedMemoryUnregisterRequest {
            name: name.to_owned(),
        },
        Some(token),
    )
}

/// H2/M4/N7: eine Registrierung ueber den Governor nennt nur Schluessel unter
/// dem Praefix, eine gueltige Ausdehnung und eine Region, die niemand anderem
/// gehoert — und nicht mehr Regionen als erlaubt. Abgelehntes erreicht das
/// Backend nie.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shm_registration_is_confined_to_its_prefix_owner_and_bound() {
    let (mock, endpoint) = backend(Duration::from_millis(1)).await;
    let service = service(
        &endpoint,
        "  security:\n    max_shm_regions: 2\n",
        1,
        CONTRACT,
    )
    .with_tokens(tokens())
    .with_admin_tokens(admin());

    for (name, key, offset, size, what) in [
        ("r1", "/other", 0, 1024, "fremder Praefix"),
        ("r1", "/vig_a/b", 0, 1024, "ein zweites '/'"),
        ("r1", "/vig_a", 0, 0, "leere Region"),
        ("r1", "/vig_a", u64::MAX, 2, "Ueberlauf"),
        ("", "/vig_a", 0, 1024, "ohne Namen"),
    ] {
        let status = service
            .system_shared_memory_register(register(name, key, offset, size, ALPHA))
            .await
            .expect_err(what);
        assert_eq!(status.code(), Code::InvalidArgument, "{what}: {status:?}");
    }
    assert_eq!(
        mock.shm_calls.load(Ordering::Relaxed),
        0,
        "nichts erreicht das Backend"
    );

    service
        .system_shared_memory_register(register("r1", "/vig_a", 0, 1024, ALPHA))
        .await
        .expect("unter dem Praefix, mit Ausdehnung");
    assert_eq!(mock.shm_calls.load(Ordering::Relaxed), 1);

    // Ein anderer Aufrufer kann die Region weder uebernehmen noch abmelden.
    let taken = service
        .system_shared_memory_register(register("r1", "/vig_b", 0, 1024, BETA))
        .await
        .expect_err("fremde Region");
    assert_eq!(taken.code(), Code::PermissionDenied);
    let foreign = service
        .system_shared_memory_unregister(unregister("r1", BETA))
        .await
        .expect_err("fremde Abmeldung");
    assert_eq!(foreign.code(), Code::PermissionDenied);

    // Die Obergrenze gilt ueber alle Aufrufer.
    service
        .system_shared_memory_register(register("r2", "/vig_c", 0, 1024, ALPHA))
        .await
        .expect("zweite Region");
    let full = service
        .system_shared_memory_register(register("r3", "/vig_d", 0, 1024, BETA))
        .await
        .expect_err("dritte Region");
    assert_eq!(full.code(), Code::ResourceExhausted);

    // „Alle abmelden" (leerer Name) nur mit Administrationstoken.
    let all = service
        .system_shared_memory_unregister(unregister("", ALPHA))
        .await
        .expect_err("ein Inferenzclient meldet nicht alle ab");
    assert_eq!(all.code(), Code::PermissionDenied);
    assert_eq!(service.shm_registry().len(), 2);
    service
        .system_shared_memory_unregister(unregister("", ROOT))
        .await
        .expect("der Betreiber darf");
    assert!(service.shm_registry().is_empty());

    // Der Besitzer meldet seine eigene Region ab.
    service
        .system_shared_memory_register(register("r4", "/vig_e", 0, 1024, BETA))
        .await
        .unwrap();
    service
        .system_shared_memory_unregister(unregister("r4", BETA))
        .await
        .expect("eigene Region");
    assert!(service.shm_registry().is_empty());
}

fn referencing(input_region: Option<&str>, output_region: Option<&str>) -> ModelInferRequest {
    let mut request = vig_gateway::testing::request_for("m0");
    request.id = "shm".into();
    if let Some(region) = input_region {
        let mut input = InferInputTensor {
            name: "image".into(),
            datatype: "UINT8".into(),
            shape: vec![1],
            ..Default::default()
        };
        input
            .parameters
            .insert("shared_memory_region".into(), string_param(region));
        request.inputs.push(input);
    }
    if let Some(region) = output_region {
        let mut output = InferRequestedOutputTensor {
            name: "boxes".into(),
            ..Default::default()
        };
        output
            .parameters
            .insert("shared_memory_region".into(), string_param(region));
        request.outputs.push(output);
    }
    request
}

/// H2: im strikten Modus nennt eine Inferenz nur Regionen, die derselbe
/// Aufrufer ueber den Governor registriert hat — als Eingabe laese sie sonst
/// fremde Frames, als Ausgabe schriebe das Backend in fremden Speicher.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_mode_only_lets_a_caller_name_its_own_regions() {
    let (mock, endpoint) = backend(Duration::from_millis(1)).await;
    let service = service(&endpoint, "  trust: strict\n", 1, CONTRACT).with_tokens(tokens());
    service
        .system_shared_memory_register(register("frames", "/vig_frames", 0, 1024, ALPHA))
        .await
        .unwrap();

    service
        .model_infer(with_token(
            referencing(Some("frames"), Some("frames")),
            Some(ALPHA),
        ))
        .await
        .expect("die eigene Region");
    assert_eq!(mock.served.load(Ordering::Relaxed), 1);

    for (request, token, what) in [
        (referencing(Some("frames"), None), BETA, "fremde Eingabe"),
        (referencing(None, Some("frames")), BETA, "fremde Ausgabe"),
        (
            referencing(Some("elsewhere"), None),
            ALPHA,
            "nie ueber den Governor registriert",
        ),
    ] {
        let status = service
            .model_infer(with_token(request, Some(token)))
            .await
            .expect_err(what);
        assert_eq!(status.code(), Code::PermissionDenied, "{what}: {status:?}");
    }
    assert_eq!(
        mock.served.load(Ordering::Relaxed),
        1,
        "abgelehnt heisst: nie gerechnet"
    );

    // Und eine unbekannte Region meldet im strikten Modus niemand ab.
    let status = service
        .system_shared_memory_unregister(unregister("elsewhere", ALPHA))
        .await
        .expect_err("unbekannte Region");
    assert_eq!(status.code(), Code::PermissionDenied);
}

// ---------------------------------------------------------------------------
// H3 — Administrationsendpunkte
// ---------------------------------------------------------------------------

/// H3: Modelle laden und entladen ist gesperrt, bis eine
/// Administrationstokendatei hinterlegt ist — und dann nur mit deren Token.
/// Ein Administrationstoken oeffnet auch die gewoehnlichen Endpunkte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_endpoints_are_closed_until_an_admin_token_is_presented() {
    let (_mock, endpoint) = backend(Duration::from_millis(1)).await;

    // Voreinstellung: kein Token, keine Administration.
    let default = service(&endpoint, "", 1, CONTRACT);
    let status = default
        .repository_model_unload(tonic::Request::new(RepositoryModelUnloadRequest::default()))
        .await
        .expect_err("gesperrt ohne Administrationsdatei");
    assert_eq!(status.code(), Code::PermissionDenied);

    let service = service(&endpoint, "", 1, CONTRACT)
        .with_tokens(tokens())
        .with_admin_tokens(admin());
    let status = service
        .repository_model_load(with_token(
            RepositoryModelLoadRequest::default(),
            Some(ALPHA),
        ))
        .await
        .expect_err("ein Inferenztoken ist kein Administrationstoken");
    assert_eq!(status.code(), Code::PermissionDenied);

    // Mit Administrationstoken erreicht der Aufruf das Backend (der Mock
    // kennt den Endpunkt nicht und sagt das).
    let status = service
        .repository_model_load(with_token(
            RepositoryModelLoadRequest::default(),
            Some(ROOT),
        ))
        .await
        .expect_err("der Mock laedt nichts");
    assert_eq!(status.code(), Code::Unimplemented, "{status:?}");

    service
        .model_infer(with_token(
            vig_gateway::testing::request_for("m0"),
            Some(ROOT),
        ))
        .await
        .expect("das Administrationstoken authentifiziert auch");
}

// ---------------------------------------------------------------------------
// M1 / M2 / N3
// ---------------------------------------------------------------------------

/// M1: die Zugangspruefung steht als Interceptor vor dem Dekodieren; ein
/// Client ohne Token kostet keinen Nachrichtenpuffer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gate_refuses_before_anything_is_decoded() {
    use tonic::service::Interceptor as _;

    let (_mock, endpoint) = backend(Duration::from_millis(1)).await;
    let mut gate = service(&endpoint, "", 1, CONTRACT)
        .with_tokens(tokens())
        .gate();
    let status = gate.call(tonic::Request::new(())).unwrap_err();
    assert_eq!(status.code(), Code::Unauthenticated);
    assert!(
        gate.call(with_token((), Some("wrong-0123456789abcdef")))
            .is_err()
    );
    assert!(gate.call(with_token((), Some(BETA))).is_ok());

    // Ohne Tokendatei laesst er alles durch — die Loopback-Voreinstellung.
    let mut open = service(&endpoint, "", 1, CONTRACT).gate();
    assert!(open.call(tonic::Request::new(())).is_ok());
}

/// M2: auch was am Governor vorbei durchgereicht wird (offener Modus,
/// unkonfiguriertes Modell), zaehlt gegen das Nutzlastbudget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passthrough_is_bounded_by_the_payload_budget() {
    let (_mock, endpoint) = backend(Duration::from_millis(200)).await;
    let service = Arc::new(service(&endpoint, "  max_inflight_mib: 1\n", 1, CONTRACT));

    let big = || {
        let mut r = vig_gateway::testing::request_for("not_configured");
        r.raw_input_contents = vec![vec![0_u8; 700 * 1024]];
        tonic::Request::new(r)
    };

    let svc = service.clone();
    let first = tokio::spawn(async move { svc.model_infer(big()).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let status = service
        .model_infer(big())
        .await
        .expect_err("zwei mal 700 KiB passen nicht in 1 MiB — auch nicht durchgereicht");
    assert_eq!(status.code(), Code::ResourceExhausted);

    first.await.unwrap().expect("der erste laeuft durch");
    service
        .model_infer(big())
        .await
        .expect("und gibt das Budget zurueck");
}

/// N3: Liveness beantwortet der Governor selbst — auch wenn das Backend
/// nicht erreichbar ist. Er ist kein Verstaerker fuer Tritons Health-Endpunkt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn liveness_is_answered_locally() {
    // Auf Port 1 lauscht niemand.
    let service = service("127.0.0.1:1", "", 1, CONTRACT).with_tokens(tokens());

    let live = tokio::time::timeout(
        Duration::from_secs(1),
        service.server_live(with_token(ServerLiveRequest::default(), Some(ALPHA))),
    )
    .await
    .expect("ohne Backendanfrage")
    .unwrap()
    .into_inner();
    assert!(live.live);

    let ready = tokio::time::timeout(
        Duration::from_secs(2),
        service.server_ready(with_token(ServerReadyRequest::default(), Some(ALPHA))),
    )
    .await
    .expect("aus dem eigenen Zustand")
    .unwrap()
    .into_inner();
    assert!(!ready.ready, "ein unerreichbares Backend ist nicht bereit");

    let status = service
        .server_live(tonic::Request::new(ServerLiveRequest::default()))
        .await
        .expect_err("auch Liveness nur mit Token, wenn Token eingerichtet sind");
    assert_eq!(status.code(), Code::Unauthenticated);
}

/// N5: ein Client mit falscher Zeitbasis fuellte das Log mit einer Warnung
/// je Request. Jetzt zaehlt der Governor jede, meldet aber hoechstens eine je
/// zehn Sekunden — und der Request laeuft trotzdem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn implausible_client_timestamps_are_counted_not_each_logged() {
    let (mock, endpoint) = backend(Duration::from_millis(1)).await;
    let service = service(&endpoint, "", 1, CONTRACT);

    for id in 0..5 {
        let mut request = vig_gateway::testing::request_for("m0");
        request.id = format!("future-{id}");
        request.parameters.insert(
            "vig_generation_ns".to_owned(),
            InferParameter {
                // Rund 127 Jahre nach dem Start der Uhr.
                parameter_choice: Some(ParameterChoice::Int64Param(4_000_000_000_000_000_000)),
            },
        );
        service
            .model_infer(tonic::Request::new(request))
            .await
            .expect("mit Ankunftszeit statt Clientzeit");
    }
    assert_eq!(service.generation_warnings(), 5, "jede zaehlt");
    assert_eq!(mock.served.load(Ordering::Relaxed), 5);
}

// ---------------------------------------------------------------------------
// NV-17 und N2 — der Abhaengigkeitsgraph im Gateway
// ---------------------------------------------------------------------------

fn captured(model: &str, id: u64, capture: u64, parents: &[u64]) -> ModelInferRequest {
    let mut request = vig_gateway::testing::request_for(model);
    request.id = id.to_string();
    request.parameters.insert(
        "vig_capture_id".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Uint64Param(capture)),
        },
    );
    if !parents.is_empty() {
        let list = parents
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        request
            .parameters
            .insert("vig_depends_on".to_owned(), string_param(&list));
    }
    request
}

/// NV-17: nach 256 Aufnahmen lehnte das Gateway jeden weiteren Frame ab. Der
/// Graph fasst 256 Knoten, und kein Pfad setzte einen Knoten je auf einen
/// Endzustand — `collect()` fand nie etwas. Hier 2.000 Aufnahmen mit je einer
/// Erkennung und einer Zusammenfuehrung, nacheinander, und alle kommen an.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thousands_of_captures_are_all_delivered() {
    let (mock, endpoint) = backend(Duration::ZERO).await;
    let service = service(&endpoint, "", 1, CONTRACT);

    for capture in 1..=2_000_u64 {
        let detection = capture * 2 - 1;
        service
            .model_infer(tonic::Request::new(captured("m0", detection, capture, &[])))
            .await
            .unwrap_or_else(|e| panic!("Erkennung der Aufnahme {capture}: {e:?}"));
        service
            .model_infer(tonic::Request::new(captured(
                "m0",
                capture * 2,
                capture,
                &[detection],
            )))
            .await
            .unwrap_or_else(|e| panic!("Zusammenfuehrung der Aufnahme {capture}: {e:?}"));
    }
    assert_eq!(mock.served.load(Ordering::Relaxed), 4_000);
}

/// NV-17: „ein Parent mit zwei Verbrauchern bleibt bis zum letzten gueltig".
/// Der erste Verbraucher ist fertig, bevor der zweite kommt — der Parent
/// darf trotzdem nicht eingesammelt sein. Und nach seiner Haltefrist (dem
/// Hoechstalter des Modells) ist er es: sonst waechst der Graph wieder voll.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parent_outlives_its_first_consumer() {
    let (_mock, endpoint) = backend(Duration::from_millis(1)).await;
    let service = service(&endpoint, "", 1, "{ deadline_ms: 300, max_age_ms: 400 }");

    service
        .model_infer(tonic::Request::new(captured("m0", 1, 100, &[])))
        .await
        .expect("der Parent");
    service
        .model_infer(tonic::Request::new(captured("m0", 2, 100, &[1])))
        .await
        .expect("der erste Verbraucher");
    tokio::time::sleep(Duration::from_millis(150)).await;
    service
        .model_infer(tonic::Request::new(captured("m0", 3, 100, &[1])))
        .await
        .expect("der zweite Verbraucher findet den Parent noch");

    tokio::time::sleep(Duration::from_millis(900)).await;
    let late = service
        .model_infer(tonic::Request::new(captured("m0", 4, 100, &[1])))
        .await
        .expect_err("nach der Haltefrist ist der Parent eingesammelt");
    assert_eq!(late.code(), Code::FailedPrecondition, "{late:?}");
    assert_eq!(reason(&late), Some("unknown_parent"));
}

/// Flutet den Graphen mit offenen Knoten (das Backend antwortet nie) und
/// gibt die erste Graphablehnung zurueck.
async fn first_graph_rejection(
    service: Arc<GatewayService>,
    token: Option<&'static str>,
    models: usize,
    per_model: u64,
) -> Option<tonic::Status> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut id = 0_u64;
    for _ in 0..per_model {
        for m in 0..models {
            id += 1;
            let svc = service.clone();
            let tx = tx.clone();
            let request = with_token(captured(&format!("m{m}"), id, id, &[]), token);
            tokio::spawn(async move {
                let result = svc.model_infer(request).await;
                let _ignored = tx.send(result);
            });
        }
    }
    drop(tx);
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(result) = rx.recv().await {
            if let Err(status) = result
                && reason(&status).is_some_and(|r| r.starts_with("graph_"))
            {
                return Some(status);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// NV-17 (b): ein voller Graph sagt `graph_full` — die ROS-2-Bruecke kennt
/// den Wert und kann ihn von einer fachlichen Ablehnung unterscheiden.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_graph_answers_graph_full() {
    let endpoint = hanging_backend().await;
    // Acht Modelle mit je 40 wartenden Auftraegen: 320 offene Knoten, mehr
    // als der Graph fasst.
    let service = Arc::new(service(&endpoint, "", 8, CONTRACT));
    let status = first_graph_rejection(service, None, 8, 40)
        .await
        .expect("der Graph laeuft voll");
    assert_eq!(reason(&status), Some("graph_full"), "{status:?}");
    assert_eq!(status.code(), Code::ResourceExhausted);
}

/// N2: eine authentifizierte Identitaet belegt hoechstens die Haelfte des
/// Graphen; ein anderer Aufrufer kommt danach noch hinein.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_identity_cannot_fill_the_graph_alone() {
    let endpoint = hanging_backend().await;
    let service = Arc::new(service(&endpoint, "", 8, CONTRACT).with_tokens(tokens()));
    let status = first_graph_rejection(service.clone(), Some(ALPHA), 8, 40)
        .await
        .expect("das Kontingent greift");
    assert_eq!(reason(&status), Some("graph_quota"), "{status:?}");
    assert_eq!(status.code(), Code::ResourceExhausted);

    let other = tokio::time::timeout(
        Duration::from_millis(300),
        service.model_infer(with_token(captured("m0", 1, 1, &[]), Some(BETA))),
    )
    .await;
    if let Ok(Err(status)) = other {
        assert!(
            !reason(&status).is_some_and(|r| r.starts_with("graph_")),
            "ein anderer Aufrufer wird nicht vom Graphen abgewiesen: {status:?}"
        );
    }
}

/// N2: Anfragekennungen gelten je Aufrufer. Ein anderer kann weder einen
/// fremden Knoten als Elternteil nennen noch eine offene Kennung doppelt
/// belegen; eine abgeschlossene darf wiederverwendet werden.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_ids_are_namespaced_per_caller() {
    let (_mock, endpoint) = backend(Duration::from_millis(1)).await;
    let service = service(&endpoint, "", 1, CONTRACT).with_tokens(tokens());

    service
        .model_infer(with_token(captured("m0", 7, 100, &[]), Some(ALPHA)))
        .await
        .unwrap();
    let foreign = service
        .model_infer(with_token(captured("m0", 8, 100, &[7]), Some(BETA)))
        .await
        .expect_err("ein fremder Knoten ist kein Elternteil");
    assert_eq!(reason(&foreign), Some("unknown_parent"), "{foreign:?}");
    assert_eq!(foreign.code(), Code::FailedPrecondition);
    service
        .model_infer(with_token(captured("m0", 9, 100, &[7]), Some(ALPHA)))
        .await
        .expect("der eigene schon");
    service
        .model_infer(with_token(captured("m0", 7, 100, &[]), Some(ALPHA)))
        .await
        .expect("eine abgeschlossene Kennung darf wiederkommen");
}

/// N2: eine noch offene Kennung ein zweites Mal ist ein Fehler des Clients —
/// sonst zeigte ein spaeterer Verbraucher auf den falschen Auftrag.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_request_id_cannot_be_used_twice() {
    let endpoint = hanging_backend().await;
    let service = Arc::new(service(&endpoint, "", 1, CONTRACT).with_tokens(tokens()));

    let svc = service.clone();
    let _hanging = tokio::spawn(async move {
        svc.model_infer(with_token(captured("m0", 1, 100, &[]), Some(ALPHA)))
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let duplicate = service
        .model_infer(with_token(captured("m0", 1, 100, &[]), Some(ALPHA)))
        .await
        .expect_err("die Kennung ist noch offen");
    assert_eq!(reason(&duplicate), Some("duplicate_id"), "{duplicate:?}");

    let other = tokio::time::timeout(
        Duration::from_millis(300),
        service.model_infer(with_token(captured("m0", 1, 100, &[]), Some(BETA))),
    )
    .await;
    if let Ok(Err(status)) = other {
        assert_ne!(
            reason(&status),
            Some("duplicate_id"),
            "dieselbe Zahl eines anderen Aufrufers ist eine andere Kennung"
        );
    }
}
