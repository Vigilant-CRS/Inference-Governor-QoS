//! Independent counterexamples for the review snapshot; temporarily install as gateway integration test.
#![allow(clippy::all, missing_docs)]
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as WallDuration;
use vig_backend_triton::{BackendError, TritonClient};
use vig_core::{Criticality, Duration, Instant, ModelIdx, PayloadRef, QueuePolicy, RequestDescriptor, RequestId, SupersessionKey};
use vig_core::scheduler::{Action, Event, Scheduler};
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_gateway::{actor, GatewayService, MonotonicClock};
use vig_gateway::executor::Executor;
use vig_gateway::testing::{FakeExecutor, request_for};
use vig_protocol_oip::inference::{ModelInferRequest, ModelInferResponse};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;

const YAML: &str = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:59998
  slots: 2
  pipelining_depth: 0
  inference_timeout_ms: 50
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

fn resolved() -> vig_config::schema::Resolved {
    let mut cfg = vig_config::Config::from_yaml(YAML).unwrap().resolve().unwrap();
    cfg.max_inflight_bytes = 16;
    cfg
}
fn desc(id: u64, now: Instant) -> RequestDescriptor {
    RequestDescriptor {
        id: RequestId(id), logical_model: ModelIdx(0), supersession_key: SupersessionKey(0),
        generation_time: now, arrival_time: now, absolute_deadline: None, max_age: None,
        criticality: Criticality::Protected, queue_policy: QueuePolicy::Fifo, stateful: false,
        variant: None, payload: PayloadRef::default(), context_tokens: 0, decomposable: false,
    }
}
fn unknown() -> BackendError {
    BackendError::Rejected { code: tonic::Code::Unavailable, message: "RPC lost; execution may continue".into() }
}
async fn actor_with(fake: Arc<FakeExecutor>) -> (vig_gateway::Handle, MonotonicClock) {
    let cfg = Arc::new(resolved());
    let mut backends: HashMap<String, Arc<dyn Executor>> = HashMap::new();
    backends.insert(cfg.backend_endpoint.clone(), fake);
    let clock = MonotonicClock::start();
    let h = actor::spawn_with(cfg, backends, clock, &[]).unwrap();
    for _ in 0..100 {
        if h.metrics().await.unwrap().reconcile_baseline_missing == 0 { return (h, clock); }
        tokio::time::sleep(WallDuration::from_millis(5)).await;
    }
    panic!("baseline not acquired");
}

#[tokio::test]
async fn later_completion_must_not_prove_earlier_unknown_request_finished() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error("detector_main", unknown());
    fake.expect_ok("detector_main");
    let (h, clock) = actor_with(fake.clone()).await;
    assert!(h.submit(desc(1, clock.now()), request_for("detector")).await.is_err());
    assert_eq!(h.metrics().await.unwrap().quarantined, 1);
    assert!(h.submit(desc(2, clock.now()), request_for("detector")).await.is_ok());
    // Only request 2 has completed. Request 1 may still be executing.
    fake.set_evidence(1);
    tokio::time::sleep(WallDuration::from_millis(550)).await;
    let m = h.metrics().await.unwrap();
    assert_eq!(m.quarantined, 1, "request 2 was incorrectly accepted as proof for request 1; reconciled={}", m.reconciled);
}

#[tokio::test]
async fn never_started_request_must_not_raise_future_completion_target() {
    let fake = Arc::new(FakeExecutor::default());
    fake.expect_error("detector_main", BackendError::Unreachable { endpoint: "unused".into(), cause: "refused before submission".into() });
    fake.expect_error("detector_main", unknown());
    let (h, clock) = actor_with(fake.clone()).await;
    assert!(h.submit(desc(1, clock.now()), request_for("detector")).await.is_err());
    assert!(h.submit(desc(2, clock.now()), request_for("detector")).await.is_err());
    // All work that ever reached this exclusive backend has now completed.
    fake.set_evidence(1);
    tokio::time::sleep(WallDuration::from_millis(550)).await;
    assert_eq!(h.metrics().await.unwrap().quarantined, 0, "a request that never reached the backend made the reconciliation target unreachable");
}

fn at(ms: u64) -> Instant { Instant::ZERO.checked_add(Duration::from_millis(ms).unwrap()).unwrap() }

#[test]
fn consumer_age_must_start_at_capture_not_completion() {
    use vig_core::contract_ext::{ContractExtension, MissBudget};
    let mut cfg = resolved();
    let c = cfg.contracts.get_mut(0).unwrap();
    c.max_age = Some(Duration::from_millis(66).unwrap());
    c.extension = Some(ContractExtension {
        consumer_period: Some(Duration::from_millis(10).unwrap()),
        miss_budget: Some(MissBudget { max_misses: 9, window_cycles: 10, max_consecutive: None }),
        ..Default::default()
    });
    let mut scheduler = Scheduler::new(cfg.contracts, cfg.slots,
        OverloadController::new(OverloadConfig::default(), at(0)).unwrap(), cfg.margin).unwrap();
    let mut actions = Vec::new();
    scheduler.on_event(at(0), Event::Tick, &mut |a| actions.push(a));
    let mut d = desc(1, at(0));
    d.arrival_time = at(40);
    d.max_age = Some(Duration::from_millis(66).unwrap());
    scheduler.on_event(at(40), Event::Arrival(d), &mut |a| actions.push(a));
    let slot = actions.iter().find_map(|a| match a { Action::Dispatch { slot, .. } => Some(*slot), _ => None }).unwrap();
    scheduler.on_event(at(50), Event::Completion { request: RequestId(1), slot }, &mut |_| {});
    scheduler.on_event(at(60), Event::Tick, &mut |_| {});
    let before = scheduler.metrics().weakly_hard_misses[0];
    scheduler.on_event(at(70), Event::Tick, &mut |_| {});
    let after = scheduler.metrics().weakly_hard_misses[0];
    assert_eq!(after, before + 1, "capture=0, completion=50, sample=70, max_age=66: stale result counted as supplied");
}

#[tokio::test]
async fn payload_budget_must_remain_reserved_after_client_timeout() {
    let fake = Arc::new(FakeExecutor::default());
    for _ in 0..2 { fake.expect_slow("detector_main", WallDuration::from_secs(5)); }
    let (h, clock) = actor_with(fake.clone()).await;
    let service = GatewayService::new(Arc::new(resolved()), Arc::new(TritonClient::new("127.0.0.1:59998")), h.clone(), clock);
    let request = || {
        let mut r = request_for("detector");
        r.raw_input_contents = vec![vec![7; 16]];
        tonic::Request::new(r)
    };
    let first = service.model_infer(request()).await.unwrap_err();
    assert_eq!(first.code(), tonic::Code::DeadlineExceeded);
    let second = service.model_infer(request()).await.unwrap_err();
    assert_eq!(second.code(), tonic::Code::ResourceExhausted, "first backend execution is still running, but second payload was admitted; dispatched={}", fake.executed());
}

fn bytes(text: &str) -> Vec<u8> {
    let mut b = (text.len() as u32).to_le_bytes().to_vec(); b.extend_from_slice(text.as_bytes()); b
}

#[test]
fn real_token_budget_cannot_be_enforced_by_dividing_utf8_bytes_by_four() {
    use vig_gateway::cooperative::GenerativeJob;
    use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
    let request = ModelInferRequest {
        inputs: vec![InferInputTensor { name: "text_input".into(), datatype: "BYTES".into(), shape: vec![1], ..Default::default() }],
        raw_input_contents: vec![bytes("Prompt:")], ..Default::default()
    };
    let mut job = GenerativeJob::from_request(&request, 4).unwrap();
    // A backend may emit four single-character tokens, e.g. four digit tokens.
    let response = ModelInferResponse { raw_output_contents: vec![bytes("1234")], ..Default::default() };
    assert!(job.absorb(&response), "four actual tokens consumed, but counter={} and remaining={}", job.tokens, job.remaining_tokens());
}

#[test]
fn observing_a_clock_below_the_promised_floor_is_not_confirmation() {
    use vig_platform::{Actuation, Actuator, ActuationError, ActuationPolicy, ClockMhz};
    #[derive(Debug)] struct Fake;
    impl Actuator for Fake {
        fn request(&mut self, _: ClockMhz) -> Result<(), ActuationError> { Ok(()) }
        fn restore(&mut self) -> Result<(), ActuationError> { Ok(()) }
    }
    let mut act = Actuation::disabled(ActuationPolicy {
        platform_min: ClockMhz(300), platform_max: ClockMhz(2100), promised_floor: ClockMhz(1500),
        dwell_ms: 0, tolerance_mhz: 50, settle_ms: 0,
    });
    act.enable(Box::new(Fake));
    let observation = || Some(vig_platform::collector::parse_output(
        "0, GPU, 580.173.02, 8.6, 8192, 80, 1470, 2100, 129.55, [N/A], P0, Disabled, 0x4", 1000));
    assert!(act.request(ClockMhz(1500), 1000, &observation, 0).is_err(), "1470 < promised 1500, yet accepted as valid planning point");
}

#[test]
fn unavailable_proven_contract_must_not_be_silently_accepted() {
    let yaml = YAML.replace("contract: { deadline_ms: 10000 }", "contract:\n      deadline_ms: 10000\n      extension:\n        evidence_required: proven");
    let result = vig_config::Config::from_yaml(&yaml).unwrap().resolve();
    assert!(result.is_err(), "proven requested, but no proof/capability rejection exists at configuration admission");
}
