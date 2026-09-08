use vig_core::arrayvec::ArrayVec;
use vig_core::model::{Cooperative, ModelContract, Quality, QualityValue, Variant};
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use vig_core::queue::{ModelQueue, QueueConfig};
use vig_core::request::OverflowPolicy;
use vig_core::scheduler::{Action, Event, Scheduler};
use vig_core::slots::SlotSet;
use vig_core::*;
use std::sync::Arc;

#[path = "../../../../../crates/vig-gateway/tests/mock_backend.rs"]
mod mock_backend;

fn ms(v: u64) -> Duration {
    Duration::from_millis(v).unwrap()
}
fn at(v: u64) -> Instant {
    Instant::ZERO.checked_add(ms(v)).unwrap()
}
fn contract(class: Criticality, policy: QueuePolicy, runtimes: &[u64]) -> ModelContract {
    let mut variants = ArrayVec::new();
    for (i, runtime) in runtimes.iter().enumerate() {
        variants
            .push(Variant {
                quality: QualityValue::measured(
                    Quality::from_milli(1000 - 100 * i as u16).unwrap(),
                ),
                profile: VariantProfile::solo(RuntimeProfile::exact(ms(*runtime))),
            })
            .unwrap();
    }
    ModelContract {
        variants_interchangeable: true,
        criticality: class,
        queue: QueueConfig {
            policy,
            capacity: 64,
            overflow: OverflowPolicy::RejectNew,
        },
        period: None,
        deadline: ms(1000),
        max_age: None,
        stateful: false,
        min_quality: None,
        variant_dwell: Duration::ZERO,
        variants,
        cooperative: None,
    }
}
fn frame(id: u64, model: u16, generated: u64, c: &ModelContract) -> RequestDescriptor {
    RequestDescriptor {
        id: RequestId(id),
        logical_model: ModelIdx(model),
        supersession_key: SupersessionKey(0),
        generation_time: at(generated),
        arrival_time: at(generated),
        absolute_deadline: at(generated).checked_add(c.deadline),
        max_age: c.max_age,
        criticality: c.criticality,
        queue_policy: c.queue.policy,
        stateful: c.stateful,
        variant: None,
        payload: PayloadRef(id),
    }
}
fn scheduler(contracts: &[ModelContract], slots: SlotSet) -> Scheduler {
    let mut list = ArrayVec::new();
    for c in contracts {
        list.push(c.clone()).unwrap();
    }
    Scheduler::new(
        list,
        slots,
        OverloadController::new(OverloadConfig::default(), at(0)).unwrap(),
        SafetyMargin::NONE,
    )
    .unwrap()
}
fn event(s: &mut Scheduler, time: u64, e: Event) -> Vec<Action> {
    let mut actions = Vec::new();
    s.on_event(at(time), e, &mut |a| actions.push(a));
    actions
}
fn dispatched(actions: &[Action]) -> Vec<RequestId> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Dispatch { request, .. } => Some(*request),
            _ => None,
        })
        .collect()
}

#[test]
fn latest_scope_is_model_even_when_client_keys_differ() {
    let c = contract(Criticality::Normal, QueuePolicy::Latest, &[1]);
    let mut q = ModelQueue::new(c.queue, false).unwrap();
    let mut old = frame(1, 0, 0, &c);
    old.supersession_key = SupersessionKey(11);
    let mut new = frame(2, 0, 1, &c);
    new.supersession_key = SupersessionKey(22);
    q.push(old);
    q.push(new);
    assert_eq!(
        q.len(),
        1,
        "LATEST should retain only one request for a model"
    );
}

#[test]
fn every_stale_request_gets_a_terminal_action() {
    let blocker = contract(Criticality::Protected, QueuePolicy::Fifo, &[1000]);
    let queued = contract(Criticality::Normal, QueuePolicy::Fifo, &[1]);
    // Frames queue without optimistic pre-dispatch dropping while the slot is full.
    let mut s = scheduler(
        &[blocker.clone(), queued.clone()],
        SlotSet::homogeneous(1, 0).unwrap(),
    );
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &blocker)));
    for id in 2..=65 {
        let mut d = frame(id, 1, 0, &queued);
        d.max_age = Some(ms(10));
        event(&mut s, 0, Event::Arrival(d));
    }
    let actions = event(&mut s, 11, Event::Tick);
    let terminal = actions
        .iter()
        .filter(|a| matches!(a, Action::Terminate { .. }))
        .count();
    assert_eq!(
        terminal, 64,
        "all 64 stale requests left the queue and require replies"
    );
}

#[test]
fn fifo_stateful_sequence_is_not_reordered_by_edf() {
    let mut c = contract(Criticality::Protected, QueuePolicy::Fifo, &[5]);
    c.stateful = true;
    let mut s = scheduler(&[c.clone()], SlotSet::homogeneous(1, 0).unwrap());
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &c)));
    event(&mut s, 1, Event::Arrival(frame(2, 0, 1, &c)));
    let mut third = frame(3, 0, 2, &c);
    third.absolute_deadline = Some(at(20));
    event(&mut s, 2, Event::Arrival(third));
    let a = event(
        &mut s,
        5,
        Event::Completion {
            request: RequestId(1),
            slot: SlotIdx(0),
        },
    );
    assert_eq!(
        dispatched(&a),
        vec![RequestId(2)],
        "FIFO must dispatch the older queued sequence element"
    );
}

#[test]
fn corun_veto_remains_until_actual_completion() {
    let mut slots = SlotSet::homogeneous(2, 0).unwrap();
    slots.forbid_corun(ModelIdx(0), ModelIdx(1));
    slots
        .dispatch(SlotIdx(0), RequestId(1), ModelIdx(0), at(0), ms(5))
        .unwrap();
    assert!(!slots.corun_allowed(ModelIdx(1)));
    assert!(
        slots.ready_slot(ModelIdx(1), at(6)).is_none(),
        "prediction elapsed, but forbidden work remains in flight"
    );
}

#[test]
fn blocked_candidate_does_not_idle_an_independent_slot() {
    let a = contract(Criticality::Protected, QueuePolicy::Fifo, &[100]);
    let b = contract(Criticality::High, QueuePolicy::Fifo, &[1]);
    let c = contract(Criticality::Normal, QueuePolicy::Fifo, &[1]);
    let mut slots = SlotSet::homogeneous(2, 0).unwrap();
    slots.forbid_corun(ModelIdx(0), ModelIdx(1));
    let mut s = scheduler(&[a.clone(), b.clone(), c.clone()], slots);
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &a)));
    event(&mut s, 1, Event::Arrival(frame(2, 1, 1, &b)));
    let actions = event(&mut s, 2, Event::Arrival(frame(3, 2, 2, &c)));
    assert_eq!(
        dispatched(&actions),
        vec![RequestId(3)],
        "C can use the idle slot without harming A or B"
    );
}

#[test]
fn infeasible_fallback_chooses_fastest_runtime_not_lowest_quality() {
    let mut c = contract(Criticality::Protected, QueuePolicy::Fifo, &[10, 20]);
    c.deadline = ms(1);
    let mut s = scheduler(&[c.clone()], SlotSet::homogeneous(1, 0).unwrap());
    let actions = event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &c)));
    let chosen = actions.iter().find_map(|a| match a {
        Action::Dispatch { variant, .. } => Some(*variant),
        _ => None,
    });
    assert_eq!(
        chosen,
        Some(VariantIdx(0)),
        "higher quality may also be faster on this hardware"
    );
}

#[test]
fn old_age_hint_does_not_turn_stale_input_fresh() {
    use vig_protocol_oip::params::{GenerationHint, VigParams, resolve_generation};
    let params = VigParams {
        generation: Some(GenerationHint::AgeMicros(2_000_000)),
        ..Default::default()
    };
    let (generation, _) = resolve_generation(&params, at(10_000), ms(1000));
    assert!(
        at(10_000).saturating_since(generation) >= ms(1000),
        "an explicit 2-second age must not become fresh"
    );
}

#[test]
fn aoi_includes_time_without_deliveries() {
    let mut tracker =
        vig_sim::coverage::CoverageTracker::new(ms(10), ms(1000), at(0), ms(1000));
    tracker.record_delivery(at(1), at(0));
    assert!(
        tracker.finish().peak_aoi_ns >= 900_000_000,
        "over the next 999 ms the consumer's information grows old; delivery latency alone is not time-sampled AoI"
    );
}

#[test]
fn quantum_is_sized_before_guarding_full_job_runtime() {
    let mut protected = contract(Criticality::Protected, QueuePolicy::Fifo, &[2]);
    protected.period = Some(ms(20));
    protected.deadline = ms(5);
    let mut llm = contract(Criticality::BestEffort, QueuePolicy::Fifo, &[100]);
    llm.cooperative = Some(Cooperative {
        tokens_per_second: 1000,
        min_tokens: 1,
        max_total_tokens: 100,
        base_cost: Duration::ZERO,
    });
    let mut s = scheduler(
        &[protected.clone(), llm.clone()],
        SlotSet::homogeneous(1, 0).unwrap(),
    );
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &protected)));
    event(
        &mut s,
        2,
        Event::Completion {
            request: RequestId(1),
            slot: SlotIdx(0),
        },
    );
    let actions = event(&mut s, 3, Event::Arrival(frame(2, 1, 3, &llm)));
    assert_eq!(
        dispatched(&actions),
        vec![RequestId(2)],
        "a 17-ms quantum fits before the next protected arrival"
    );
}

fn yaml(endpoint: &str) -> String {
    format!(
        r#"
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
"#
    )
}

#[test]
fn invalid_cooperative_limits_are_rejected_before_start() {
    let mut c = vig_config::Config::from_yaml(&yaml("127.0.0.1:1")).unwrap();
    c.models.get_mut("detector").unwrap().cooperative =
        Some(vig_config::schema::CooperativeConfig {
            tokens_per_second: 1000,
            min_tokens: 8,
            max_total_tokens: 1,
            base_cost_us: 0,
        });
    assert!(
        c.resolve().is_err(),
        "min_tokens > max_total_tokens reaches u32::clamp and panics in dispatch"
    );
}

#[test]
fn out_of_range_max_age_is_rejected_not_disabled() {
    let mut c = vig_config::Config::from_yaml(&yaml("127.0.0.1:1")).unwrap();
    c.models.get_mut("detector").unwrap().contract.max_age_ms = Some(u64::MAX);
    assert!(
        c.resolve().is_err(),
        "unrepresentable max_age must not silently become None"
    );
}

fn request() -> vig_protocol_oip::inference::ModelInferRequest {
    vig_protocol_oip::inference::ModelInferRequest {
        model_name: "detector".into(),
        id: "review".into(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_backend_call_is_counted_as_failure() {
    use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;
    // The listener exists for port ownership but is deliberately closed before the call.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    drop(listener);
    let resolved = Arc::new(
        vig_config::Config::from_yaml(&yaml(&endpoint))
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let clock = vig_gateway::MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = vig_gateway::actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = vig_gateway::GatewayService::new(resolved, backend, handle.clone(), clock);
    assert!(
        service
            .model_infer(tonic::Request::new(request()))
            .await
            .is_err()
    );
    let metrics = handle.metrics().await.unwrap();
    assert_eq!((metrics.backend_failures, metrics.completed_valid), (1, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_inference_response_keeps_logical_model_name() {
    use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(1),
    ));
    let endpoint = mock_backend::start(backend_impl).await.to_string();
    let resolved = Arc::new(
        vig_config::Config::from_yaml(&yaml(&endpoint))
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let clock = vig_gateway::MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = vig_gateway::actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = vig_gateway::GatewayService::new(resolved, backend, handle, clock);
    let response = service
        .model_infer(tonic::Request::new(request()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.model_name, "detector");
}

#[test]
fn guard_reserves_combined_demand_of_expected_requests() {
    use vig_core::feasibility::{ExpectedArrival, GuardVerdict, guard_protected};
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let forecast = [
        ExpectedArrival {
            model: ModelIdx(0),
            criticality: Criticality::Protected,
            at: at(10),
            deadline: at(20),
            runtime: ms(5),
        },
        ExpectedArrival {
            model: ModelIdx(1),
            criticality: Criticality::Protected,
            at: at(10),
            deadline: at(20),
            runtime: ms(5),
        },
    ];
    // Without candidate: [10,15] and [15,20]. With candidate [0,11]: [11,16] and [16,21].
    let verdict = guard_protected(
        &slots,
        ModelIdx(2),
        Criticality::BestEffort,
        ms(11),
        at(0),
        &forecast,
        ms(100),
    );
    assert!(
        matches!(verdict, GuardVerdict::WouldEndanger { .. }),
        "both protected jobs fit without candidate; together they do not fit with it: {verdict:?}"
    );
}

#[test]
fn accepted_config_does_not_panic_during_dispatch() {
    let mut c = contract(Criticality::Normal, QueuePolicy::Fifo, &[1]);
    c.cooperative = Some(Cooperative {
        tokens_per_second: 1000,
        min_tokens: 8,
        max_total_tokens: 1,
        base_cost: Duration::ZERO,
    });
    assert!(c.validate().is_err(), "such a contract must not be accepted");
    let mut c = c;
    c.cooperative = Some(Cooperative {
        tokens_per_second: 1000,
        min_tokens: 1,
        max_total_tokens: 8,
        base_cost: Duration::ZERO,
    });
    c.validate().unwrap();
    let d = frame(1, 0, 0, &c);
    let mut s = scheduler(&[c], SlotSet::homogeneous(1, 0).unwrap());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        event(&mut s, 0, Event::Arrival(d))
    }));
    assert!(
        result.is_ok(),
        "accepted configuration panics; release uses panic=abort"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_queued_request_is_not_dispatched() {
    use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;
    use std::sync::atomic::Ordering;
    let backend_impl = Arc::new(mock_backend::MockBackend::new(
        std::time::Duration::from_millis(100),
    ));
    let endpoint = mock_backend::start(backend_impl.clone()).await.to_string();
    let resolved = Arc::new(
        vig_config::Config::from_yaml(&yaml(&endpoint))
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let clock = vig_gateway::MonotonicClock::start();
    let backend = Arc::new(vig_backend_triton::TritonClient::new(endpoint));
    let handle = vig_gateway::actor::spawn(resolved.clone(), &backend, clock, &[]).unwrap();
    let service = Arc::new(vig_gateway::GatewayService::new(
        resolved,
        backend,
        handle.clone(),
        clock,
    ));
    let svc = service.clone();
    let first = tokio::spawn(async move { svc.model_infer(tonic::Request::new(request())).await });
    while handle.metrics().await.unwrap().received < 1 {
        tokio::task::yield_now().await;
    }
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
        "client cancelled before dispatch, but backend still executes it"
    );
}

#[test]
fn control_latest_same_key_supersedes() {
    let c = contract(Criticality::Normal, QueuePolicy::Latest, &[1]);
    let mut q = ModelQueue::new(c.queue, false).unwrap();
    q.push(frame(1, 0, 0, &c));
    let result = q.push(frame(2, 0, 1, &c));
    assert_eq!(q.len(), 1);
    assert_eq!(result.evicted.len(), 1);
}

#[test]
fn control_corun_veto_works_before_prediction_expires() {
    let mut slots = SlotSet::homogeneous(2, 0).unwrap();
    slots.forbid_corun(ModelIdx(0), ModelIdx(1));
    slots
        .dispatch(SlotIdx(0), RequestId(1), ModelIdx(0), at(0), ms(5))
        .unwrap();
    assert!(slots.ready_slot(ModelIdx(1), at(4)).is_none());
}

#[test]
fn control_valid_relative_age_is_retained() {
    use vig_protocol_oip::params::{GenerationHint, VigParams, resolve_generation};
    let params = VigParams {
        generation: Some(GenerationHint::AgeMicros(20_000)),
        ..Default::default()
    };
    let (generation, _) = resolve_generation(&params, at(1000), ms(100));
    assert_eq!(at(1000).saturating_since(generation), ms(20));
}

#[test]
fn counterexample_runtime_longer_than_period_does_not_imply_starvation() {
    let mut p = contract(Criticality::Protected, QueuePolicy::Fifo, &[1]);
    p.period = Some(ms(20));
    p.deadline = ms(100);
    let b = contract(Criticality::BestEffort, QueuePolicy::Fifo, &[30]);
    let mut s = scheduler(&[p.clone(), b.clone()], SlotSet::homogeneous(1, 0).unwrap());
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &p)));
    event(
        &mut s,
        1,
        Event::Completion {
            request: RequestId(1),
            slot: SlotIdx(0),
        },
    );
    let actions = event(&mut s, 2, Event::Arrival(frame(2, 1, 2, &b)));
    assert_eq!(dispatched(&actions), vec![RequestId(2)]);
}

#[test]
fn guard_uses_learned_protected_runtime() {
    let mut p = contract(Criticality::Protected, QueuePolicy::Fifo, &[1]);
    p.period = Some(ms(100));
    p.deadline = ms(40);
    let b = contract(Criticality::BestEffort, QueuePolicy::Fifo, &[30]);
    let mut s = scheduler(&[p.clone(), b.clone()], SlotSet::homogeneous(1, 0).unwrap());
    for k in 0..16 {
        let id = RequestId(k + 1);
        event(&mut s, k * 100, Event::Arrival(frame(id.0, 0, k * 100, &p)));
        event(
            &mut s,
            k * 100 + 10,
            Event::Completion {
                request: id,
                slot: SlotIdx(0),
            },
        );
    }
    assert_eq!(
        s.estimator().observed_p95(ModelIdx(0), VariantIdx(0), 0),
        Some(ms(10))
    );
    let actions = event(&mut s, 1592, Event::Arrival(frame(100, 1, 1592, &b)));
    assert!(
        dispatched(&actions).is_empty(),
        "B ends at 1622; the learned protected runtime (10 ms observed, margin applied) \
         no longer fits before the 1640 deadline. With the 1-ms profile runtime it would."
    );
}

