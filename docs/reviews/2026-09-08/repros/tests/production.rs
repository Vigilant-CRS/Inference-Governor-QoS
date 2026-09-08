use std::sync::Arc;
use std::time::Duration;
use tonic::Request;
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;
use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{InferTensorContents, ModelInferRequest};

#[path = "../../../../../crates/vig-gateway/tests/mock_backend.rs"]
mod mock_backend;

fn setup(endpoint: &str) -> (Arc<GatewayService>, actor::Handle) {
    let yaml = format!(
        r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "{endpoint}"
  slots: 1
  pipelining_depth: 0
  inference_timeout_ms: 60
  max_inflight_mib: 1
  trust: strict
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
"#
    );
    let resolved = Arc::new(Config::from_yaml(&yaml).unwrap().resolve().unwrap());
    let clock = MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = Arc::new(GatewayService::new(
        resolved,
        backend,
        handle.clone(),
        clock,
    ));
    (service, handle)
}

fn request() -> ModelInferRequest {
    ModelInferRequest {
        model_name: "detector".into(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_must_not_report_success_while_backend_is_quarantined() {
    let backend = Arc::new(mock_backend::MockBackend::hanging());
    let endpoint = mock_backend::start(backend).await.to_string();
    let (service, handle) = setup(&endpoint);
    let status = service
        .model_infer(Request::new(request()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
    assert_eq!(handle.metrics().await.unwrap().quarantined, 1);
    let drained = handle.drain(Duration::from_millis(100)).await.unwrap();
    assert!(
        !drained,
        "drain reported success although the backend has never completed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_must_fail_when_backend_connection_is_refused() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    drop(listener);
    let (service, handle) = setup(&endpoint);
    let status = service
        .model_infer(Request::new(request()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unavailable);
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(metrics.backend_failures, 1);
    assert!(
        vig_gateway::exporter::readiness(&metrics).is_err(),
        "readiness says ready after a confirmed backend connection failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn payload_budget_must_also_count_typed_tensor_contents() {
    let backend = Arc::new(mock_backend::MockBackend::new(Duration::ZERO));
    let endpoint = mock_backend::start(backend).await.to_string();
    let (service, _) = setup(&endpoint);
    let mut r = request();
    r.inputs.push(InferInputTensor {
        name: "input".into(),
        datatype: "BYTES".into(),
        shape: vec![1],
        contents: Some(InferTensorContents {
            bytes_contents: vec![vec![0u8; 2 * 1024 * 1024]],
            ..Default::default()
        }),
        ..Default::default()
    });
    let outcome = service.model_infer(Request::new(r)).await;
    assert_eq!(
        outcome.err().map(|s| s.code()),
        Some(tonic::Code::ResourceExhausted),
        "2 MiB of typed tensor data bypassed the configured 1 MiB budget"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_raw_tensor_over_budget_is_rejected() {
    let backend = Arc::new(mock_backend::MockBackend::new(Duration::ZERO));
    let endpoint = mock_backend::start(backend).await.to_string();
    let (service, _) = setup(&endpoint);
    let mut r = request();
    r.raw_input_contents = vec![vec![0u8; 2 * 1024 * 1024]];
    assert_eq!(
        service
            .model_infer(Request::new(r))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::ResourceExhausted
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_quarantined_must_not_leave_new_client_waiting_past_its_deadline() {
    let backend = Arc::new(mock_backend::MockBackend::hanging());
    let endpoint = mock_backend::start(backend).await.to_string();
    let (service, handle) = setup(&endpoint);
    service
        .model_infer(Request::new(request()))
        .await
        .unwrap_err();
    assert_eq!(handle.metrics().await.unwrap().quarantined, 1);
    let mut next = request();
    next.parameters.insert(
        "vig_deadline_us".into(),
        vig_protocol_oip::inference::InferParameter {
            parameter_choice: Some(
                vig_protocol_oip::inference::infer_parameter::ParameterChoice::Int64Param(100_000),
            ),
        },
    );
    let answer = tokio::time::timeout(
        Duration::from_millis(300),
        service.model_infer(Request::new(next)),
    )
    .await;
    assert!(
        answer.is_ok(),
        "new work has a 100-ms deadline but is still unanswered after 300 ms; all credits are quarantined"
    );
}

#[test]
fn delivery_outside_measurement_window_must_not_inflate_peak_aoi() {
    use vig_core::{Duration as D, Instant};
    let ms = |n| D::from_millis(n).unwrap();
    let at = |n| Instant::ZERO.checked_add(ms(n)).unwrap();
    let mut tracker = vig_sim::coverage::CoverageTracker::new(ms(10), ms(20), at(0), ms(100));
    tracker.record_delivery(at(1000), at(990));
    let coverage = tracker.finish();
    assert!(
        coverage.peak_aoi_ns <= ms(100).as_nanos(),
        "100-ms measurement reports {} ns peak from a delivery at 1000 ms",
        coverage.peak_aoi_ns
    );
}

#[test]
fn control_quarantine_blocks_readiness() {
    let mut metrics = vig_core::Metrics::default();
    metrics.slots = 1;
    metrics.quarantined = 1;
    assert!(vig_gateway::exporter::readiness(&metrics).is_err());
}

#[test]
fn cooperative_quantum_must_preserve_clients_total_token_limit() {
    let request = vig_backend_triton::text_request("qwen", "hello", 4);
    let job = vig_gateway::cooperative::GenerativeJob::from_request(&request, 64).unwrap();
    let quantum = job.build_quantum(&request, 32);
    let raw = &quantum.raw_input_contents[1];
    let parameters = std::str::from_utf8(&raw[4..]).unwrap();
    assert!(
        parameters.contains("\"max_tokens\": 4"),
        "client requested at most 4 tokens but first quantum uses {parameters}"
    );
}
