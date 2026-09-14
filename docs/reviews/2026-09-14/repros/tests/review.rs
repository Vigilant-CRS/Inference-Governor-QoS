//! Gegenproben gegen die echten Bibliotheken. Die Assertions beschreiben
//! das geforderte Verhalten und schlagen auf dem geprueften Stand fehl.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as WallDuration;
use vig_core::arrayvec::ArrayVec;
use vig_core::contract_ext::{ApprovedVariants, ContractExtension};
use vig_core::variant::{PlanningContext, resolve};
use vig_core::*;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n).unwrap()
}
fn at(n: u64) -> Instant {
    Instant::ZERO.checked_add(ms(n)).unwrap()
}
fn contract(class: Criticality, period: u64, deadline: u64, times: &[u64]) -> ModelContract {
    let mut variants = ArrayVec::new();
    for (i, time) in times.iter().enumerate() {
        variants
            .push(Variant {
                quality: QualityValue {
                    value: Quality::from_milli(1000 - i as u16 * 100).unwrap(),
                    source: QualitySource::Measured,
                },
                profile: VariantProfile::solo(RuntimeProfile::exact(ms(*time))),
                semantics: vig_core::semantics::VariantSemantics::default(),
                preprocess: Duration::ZERO,
            })
            .unwrap();
    }
    ModelContract {
        variants_interchangeable: true,
        criticality: class,
        queue: QueueConfig {
            policy: QueuePolicy::Latest,
            capacity: 2,
            overflow: OverflowPolicy::RejectNew,
        },
        period: Some(ms(period)),
        deadline: ms(deadline),
        max_age: Some(ms(period * 2)),
        stateful: false,
        min_quality: None,
        variant_dwell: Duration::ZERO,
        variants,
        cooperative: None,
        extension: None,
    }
}
fn frame(id: u64, model: u16, time: u64, c: &ModelContract) -> RequestDescriptor {
    RequestDescriptor {
        id: RequestId(id),
        logical_model: ModelIdx(model),
        supersession_key: SupersessionKey::DEFAULT,
        generation_time: at(time),
        arrival_time: at(time),
        absolute_deadline: at(time).checked_add(c.deadline),
        max_age: c.max_age,
        criticality: c.criticality,
        queue_policy: c.queue.policy,
        stateful: false,
        variant: None,
        payload: PayloadRef::default(),
        context_tokens: 0,
        decomposable: false,
    }
}

#[test]
fn lookahead_must_use_an_approved_variant() {
    let mut protected = contract(Criticality::Protected, 30, 20, &[5, 20]);
    protected.extension = Some(ContractExtension {
        approved_variants: ApprovedVariants::from_indices(&[VariantIdx(1)]),
        ..ContractExtension::default()
    });
    let background = contract(Criticality::BestEffort, 100, 100, &[20]);
    let mut contracts = ArrayVec::new();
    contracts.push(protected.clone()).unwrap();
    contracts.push(background.clone()).unwrap();
    let mut scheduler = Scheduler::new(
        contracts,
        SlotSet::homogeneous(1, 0).unwrap(),
        OverloadController::new(OverloadConfig::default(), at(0)).unwrap(),
        SafetyMargin::NONE,
    )
    .unwrap();
    let mut actions = Vec::new();
    scheduler.on_event(
        at(0),
        Event::Arrival(frame(1, 0, 0, &protected)),
        &mut |a| actions.push(a),
    );
    let slot = actions
        .iter()
        .find_map(|a| match a {
            Action::Dispatch { slot, variant, .. } => {
                assert_eq!(*variant, VariantIdx(1));
                Some(*slot)
            }
            _ => None,
        })
        .unwrap();
    scheduler.on_event(
        at(20),
        Event::Completion {
            request: RequestId(1),
            slot,
        },
        &mut |_| {},
    );
    actions.clear();
    scheduler.on_event(
        at(25),
        Event::Arrival(frame(2, 1, 25, &background)),
        &mut |a| actions.push(a),
    );
    assert!(
        !actions.iter().any(|a| matches!(
            a,
            Action::Dispatch {
                request: RequestId(2),
                ..
            }
        )),
        "Hintergrund 25..45 verspaetet die einzige freigegebene Variante: 45+20 > Deadline 50; Aktionen: {actions:?}"
    );
}

#[test]
fn hysteresis_must_not_hold_a_variant_that_breaks_supply() {
    let mut c = contract(Criticality::Protected, 10, 20, &[5, 15]);
    c.variant_dwell = ms(100);
    let mut state = VariantState::default();
    state.record(VariantIdx(1), at(0));
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let estimator = RuntimeEstimator::new();
    let predictor = vig_core::predictor::Predictor::new();
    let selected = resolve(
        &c,
        &state,
        ModelIdx(0),
        Some(at(21)),
        &PlanningContext {
            degrade: false,
            slots: &slots,
            estimator: &estimator,
            predictor: &predictor,
            state: vig_core::predictor::StateClass::default(),
            profile_revision: 0,
            margin: SafetyMargin::NONE,
            now: at(1),
            residual: Duration::ZERO,
        },
    );
    let Resolution::Feasible(selected) = selected else {
        panic!("{selected:?}")
    };
    assert_eq!(
        selected.variant,
        VariantIdx(0),
        "5 ms haelt die Versorgung bis 11 ms; 15 ms haelt nur die Deadline bei 21 ms"
    );
}

#[test]
fn a_new_measurement_arm_must_not_forget_quarantined_regions() {
    use vig_bench::pilot::RegionPool;
    let regions = vec![(
        "review-region".to_owned(),
        std::path::PathBuf::from("/tmp/review-region"),
    )];
    let first_arm = RegionPool::new(regions.clone());
    first_arm.lease().unwrap().quarantine();
    assert_eq!(first_arm.free(), 0);
    // Genau wie run_arm: dieselben CameraDef.regions, neuer Pool je Arm.
    let next_arm = RegionPool::new(regions);
    assert!(
        next_arm.lease().is_none(),
        "Die alte Quarantaene verschwindet am Armwechsel"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_arm_must_not_leave_an_inference_running() {
    use vig_bench::{backend, pilot};
    use vig_sim::workload::RuntimeDistribution;
    let times = HashMap::from([(
        "detector".to_owned(),
        RuntimeDistribution::constant(ms(2000)),
    )]);
    let backend = Arc::new(backend::Backend::new(1, times, 1));
    let endpoint = backend::start(Arc::clone(&backend)).await.to_string();
    let camera = pilot::CameraDef {
        name: "cam0".into(),
        model: "detector".into(),
        sequence: Arc::new(pilot::Sequence::from_frames_with(
            "review",
            10,
            vec![vec![]; 10],
            pilot::ArrivalRule::FirstAppearance,
        )),
        frames: Arc::new(vec![128; 8 * 8 * 3 * 10]),
        size: 8,
        rate_hz: 10.0,
        regions: vec![],
        input: ("input".into(), "FP32".into(), vec![1, 3, 8, 8]),
        target_classes: vec![0],
        iou_threshold: 0.5,
        classes: 1,
        ideal: None,
    };
    let report = pilot::run_arm(pilot::ArmConfig {
        endpoint,
        via_governor: false,
        cameras: vec![camera],
        in_flight_cap: 1,
        duration: WallDuration::from_millis(100),
        llm: None,
    })
    .await
    .unwrap();
    assert!(report.cameras[0].sent > 0);
    assert_eq!(
        backend.slots.available_permits(),
        1,
        "run_arm lieferte schon ein Ergebnis, obwohl sein Backend noch rechnet: {report:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delayed_reset_must_not_release_a_new_aborted_call() {
    use vig_gateway::{
        MonotonicClock, actor,
        budget::PayloadPermit,
        executor::Executor,
        testing::{FakeExecutor, request_for},
    };
    let yaml = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:59996
  slots: 2
  pipelining_depth: 0
models:
  detector:
    class: protected
    queue: { policy: fifo, capacity: 8 }
    contract: { deadline_ms: 10000 }
    variants:
      - id: main
        backend_model: detector_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }
";
    let resolved = Arc::new(
        vig_config::Config::from_yaml(yaml)
            .unwrap()
            .resolve()
            .unwrap(),
    );
    let fake = Arc::new(FakeExecutor::default());
    fake.set_evidence(100);
    let endpoints: HashMap<String, Arc<dyn Executor>> = HashMap::from([(
        "127.0.0.1:59996".into(),
        Arc::clone(&fake) as Arc<dyn Executor>,
    )]);
    let clock = MonotonicClock::start();
    let handle = actor::spawn_with(resolved, endpoints, clock, &[]).unwrap();
    for _ in 0..100 {
        if handle.metrics().await.unwrap().reconcile_baseline_missing == 0 {
            break;
        }
        tokio::time::sleep(WallDuration::from_millis(10)).await;
    }
    assert_eq!(
        handle.metrics().await.unwrap().reconcile_baseline_missing,
        0
    );
    // Neuer Backendprozess. Der Governor hat den Reset noch nicht gesehen.
    fake.set_evidence(0);
    fake.expect_error(
        "detector_main",
        vig_backend_triton::BackendError::Rejected {
            code: tonic::Code::Unavailable,
            message: "RPC verloren, Ausfuehrung im neuen Prozess laeuft weiter".into(),
        },
    );
    let mut descriptor = frame(
        1,
        0,
        0,
        &contract(Criticality::Protected, 10000, 10000, &[1]),
    );
    descriptor.generation_time = clock.now();
    descriptor.arrival_time = clock.now();
    descriptor.absolute_deadline = None;
    descriptor.max_age = None;
    descriptor.queue_policy = QueuePolicy::Fifo;
    let _ = handle
        .submit(
            descriptor,
            request_for("detector"),
            PayloadPermit::untracked(),
        )
        .await;
    tokio::time::sleep(WallDuration::from_millis(350)).await;
    let metrics = handle.metrics().await.unwrap();
    assert_eq!(
        metrics.quarantined, 1,
        "Zaehler 100 -> 0 beendet einen neuen abgebrochenen Aufruf ohne Endnachweis; reconciled={}",
        metrics.reconciled
    );
}
