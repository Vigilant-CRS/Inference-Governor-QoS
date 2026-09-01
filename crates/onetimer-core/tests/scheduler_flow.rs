//! End-to-End-Verhalten des Schedulers gegen ein Mock-Backend.
//!
//! Die Bausteine sind einzeln getestet (`golden_queue`, `golden_scheduling`);
//! hier wird geprueft, dass sie zusammen das erwartete Systemverhalten zeigen.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use onetimer_core::arrayvec::ArrayVec;
use onetimer_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use onetimer_core::overload::{OverloadConfig, OverloadController};
use onetimer_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use onetimer_core::queue::QueueConfig;
use onetimer_core::request::OverflowPolicy;
use onetimer_core::scheduler::{Action, Event, Scheduler};
use onetimer_core::slots::SlotSet;
use onetimer_core::{
    Criticality, Duration, Instant, ModelIdx, PayloadRef, QueuePolicy, RequestDescriptor,
    RequestId, RequestState, SupersessionKey,
};

fn ms(v: u64) -> Duration {
    Duration::from_millis(v).unwrap()
}

fn at(v: u64) -> Instant {
    Instant::ZERO.checked_add(ms(v)).unwrap()
}

fn contract(
    criticality: Criticality,
    policy: QueuePolicy,
    period_ms: Option<u64>,
    deadline_ms: u64,
    max_age_ms: u64,
    runtimes_ms: &[u64],
) -> ModelContract {
    let mut variants = ArrayVec::new();
    for (i, r) in runtimes_ms.iter().enumerate() {
        let quality = 1_000_u16.saturating_sub(u16::try_from(i).unwrap_or(0).saturating_mul(70));
        variants
            .push(Variant {
                quality: QualityValue {
                    value: Quality::from_milli(quality).unwrap(),
                    source: QualitySource::Measured,
                },
                profile: VariantProfile::solo(RuntimeProfile::exact(ms(*r))),
            })
            .unwrap();
    }
    ModelContract {
        criticality,
        queue: QueueConfig {
            policy,
            capacity: 2,
            overflow: OverflowPolicy::RejectNew,
        },
        period: period_ms.map(ms),
        deadline: ms(deadline_ms),
        max_age: Some(ms(max_age_ms)),
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
        supersession_key: SupersessionKey::DEFAULT,
        generation_time: at(generated),
        arrival_time: at(generated),
        absolute_deadline: at(generated).checked_add(c.deadline),
        max_age: c.max_age,
        criticality: c.criticality,
        queue_policy: c.queue.policy,
        stateful: false,
        variant: None,
        payload: PayloadRef(id),
    }
}

fn build(contracts: Vec<ModelContract>, slots: usize) -> Scheduler {
    let mut list = ArrayVec::new();
    for c in contracts {
        list.push(c).unwrap();
    }
    Scheduler::new(
        list,
        SlotSet::homogeneous(slots, 0).unwrap(),
        OverloadController::new(OverloadConfig::default(), at(0)).unwrap(),
        SafetyMargin::NONE,
    )
    .unwrap()
}

/// Ein Mock-Backend: nimmt Dispatches entgegen und meldet sie nach der
/// prognostizierten Laufzeit als fertig zurueck.
#[derive(Default)]
struct Backend {
    pending: Vec<(Instant, RequestId, onetimer_core::SlotIdx)>,
    dispatched: Vec<RequestId>,
    terminated: Vec<(RequestId, RequestState)>,
    observed: Vec<u64>,
}

impl Backend {
    fn collect(&mut self, now: Instant, actions: &[Action]) {
        for action in actions {
            match *action {
                Action::Dispatch {
                    request,
                    slot,
                    predicted_runtime,
                    ..
                } => {
                    self.dispatched.push(request);
                    let finish = now.checked_add(predicted_runtime).unwrap();
                    self.pending.push((finish, request, slot));
                }
                Action::Terminate { request, state } => self.terminated.push((request, state)),
                Action::ObservedRuntime { runtime, .. } => {
                    self.observed.push(runtime.as_millis());
                }
                Action::WakeAt(_) => {}
            }
        }
    }

    fn due(&mut self, now: Instant) -> Vec<(RequestId, onetimer_core::SlotIdx)> {
        let (due, rest): (Vec<_>, Vec<_>) = self
            .pending
            .iter()
            .partition(|(finish, _, _)| *finish <= now);
        self.pending = rest;
        due.into_iter().map(|(_, r, s)| (r, s)).collect()
    }

    fn count(&self, state: RequestState) -> usize {
        self.terminated.iter().filter(|(_, s)| *s == state).count()
    }
}

/// Faehrt eine Simulation ueber `duration_ms` mit 1-ms-Schritten.
fn run(
    scheduler: &mut Scheduler,
    backend: &mut Backend,
    duration_ms: u64,
    mut arrivals: impl FnMut(u64) -> Vec<RequestDescriptor>,
) {
    for t in 0..duration_ms {
        let now = at(t);
        let mut actions = Vec::new();

        for (request, slot) in backend.due(now) {
            scheduler.on_event(
                now,
                Event::Completion { request, slot },
                &mut |a: Action| {
                    actions.push(a);
                },
            );
        }
        for descriptor in arrivals(t) {
            scheduler.on_event(now, Event::Arrival(descriptor), &mut |a: Action| {
                actions.push(a);
            });
        }
        scheduler.on_event(now, Event::Tick, &mut |a: Action| actions.push(a));
        backend.collect(now, &actions);
    }
}

/// Spec 1.1: Bei 30 FPS Eingang und 20 Inferenzen/s Kapazitaet darf kein
/// Rueckstand entstehen; stattdessen wird der jeweils aelteste wartende Frame
/// durch den neueren ersetzt.
#[test]
fn latest_policy_prevents_backlog_under_sustained_overload() {
    // Deadline bewusst grosszuegig (150 ms): so bleiben wartende Frames
    // machbar und werden tatsaechlich durch neuere ersetzt, statt schon vorher
    // als unmachbar abgelehnt zu werden. Genau diesen Unterschied soll der
    // Test zeigen - Supersession, nicht Admission.
    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        150,
        100,
        &[50],
    );
    let mut scheduler = build(vec![detector.clone()], 1);
    let mut backend = Backend::default();

    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 2_000, |t| {
        if t % 33 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });

    let m = scheduler.metrics();
    // Die beobachtete Laufzeit muss der geplanten entsprechen. Weicht sie ab,
    // stimmt die Prognose nicht mit dem ueberein, was tatsaechlich passiert —
    // und der Online-Schaetzer wuerde eine Rueckkopplung aufbauen, die niemand
    // beabsichtigt hat.
    assert!(
        backend.observed.iter().all(|ms| *ms == 50),
        "geplante 50 ms, beobachtet {:?}",
        backend.observed.iter().take(8).collect::<Vec<_>>()
    );
    assert!(m.received >= 60, "rund 60 Frames in 2 s");
    assert!(
        m.forwarded >= 30,
        "die GPU wird ausgelastet, waren {}",
        m.forwarded
    );
    assert!(
        m.superseded > 0,
        "veraltete Frames muessen verdraengt werden"
    );

    // Die Kernaussage: fast jede ausgefuehrte Inferenz war noch aktuell.
    assert!(
        m.useful_inference_permille() >= 900,
        "useful inference {} permille zu niedrig",
        m.useful_inference_permille()
    );
    assert_eq!(
        backend.count(RequestState::Superseded),
        usize::try_from(m.superseded).unwrap()
    );
}

/// Spec 10.7 / G-007: Ein langer Best-Effort-Job darf einen erwarteten
/// Protected-Request nicht verdraengen.
#[test]
fn a_long_best_effort_job_defers_for_expected_protected_work() {
    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        30,
        66,
        &[10],
    );
    let vlm = contract(
        Criticality::BestEffort,
        QueuePolicy::Fifo,
        None,
        800,
        1_500,
        &[200],
    );
    let mut scheduler = build(vec![detector.clone(), vlm.clone()], 1);
    let mut backend = Backend::default();

    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 1_000, |t| {
        let mut out = Vec::new();
        if t % 33 == 0 {
            next_id = next_id.saturating_add(1);
            out.push(frame(next_id, 0, t, &detector));
        }
        if t % 250 == 10 {
            next_id = next_id.saturating_add(1);
            out.push(frame(next_id, 1, t, &vlm));
        }
        out
    });

    let m = scheduler.metrics();
    assert!(
        m.deferred_for_protected > 0,
        "der Look-ahead muss mindestens einmal absichtlich gewartet haben"
    );
    assert_eq!(
        m.protected_deadline_misses, 0,
        "kein Protected-Request darf seine Deadline verlieren"
    );
}

/// Ohne den Look-ahead verliert derselbe Workload Protected-Deadlines. Der
/// Test sichert ab, dass der vorige Test tatsaechlich etwas misst.
#[test]
fn without_look_ahead_the_same_workload_would_lose_protected_deadlines() {
    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        30,
        66,
        &[10],
    );
    // Kein `period` heisst: keine Ankunftsprognose, also kein Look-ahead.
    let mut blind = detector.clone();
    blind.period = None;
    let vlm = contract(
        Criticality::BestEffort,
        QueuePolicy::Fifo,
        None,
        800,
        1_500,
        &[200],
    );

    let mut scheduler = build(vec![blind.clone(), vlm.clone()], 1);
    let mut backend = Backend::default();

    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 1_000, |t| {
        let mut out = Vec::new();
        if t % 33 == 0 {
            next_id = next_id.saturating_add(1);
            out.push(frame(next_id, 0, t, &blind));
        }
        if t % 250 == 10 {
            next_id = next_id.saturating_add(1);
            out.push(frame(next_id, 1, t, &vlm));
        }
        out
    });

    let m = scheduler.metrics();
    assert_eq!(
        m.deferred_for_protected, 0,
        "ohne Prognose gibt es kein Veto"
    );
    assert!(
        m.rejected_infeasible > 0 || m.protected_deadline_misses > 0 || m.stale > 0,
        "ohne Look-ahead muss geschuetzte Arbeit sichtbar leiden"
    );
}

/// Spec L-003 / G-008: Unter 150 % Angebotslast waechst keine Queue ueber ihre
/// Kapazitaet, und jeder Request erreicht einen terminalen Zustand.
#[test]
fn g008_overload_stays_bounded_and_every_request_terminates() {
    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(20),
        40,
        80,
        &[30],
    );
    let mut scheduler = build(vec![detector.clone()], 1);
    let mut backend = Backend::default();

    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 3_000, |t| {
        if t % 20 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });

    let m = scheduler.metrics();
    let terminal = m.superseded
        + m.stale
        + m.rejected_infeasible
        + m.rejected_capacity
        + m.completed_valid
        + m.completed_obsolete
        + m.backend_failures;
    let still_open = m.received.saturating_sub(terminal);

    assert!(
        still_open <= u64::try_from(scheduler.slots().len() + 2).unwrap(),
        "{still_open} Requests ohne terminalen Zustand; nur laufende duerfen offen sein"
    );
    assert!(m.received > 140, "rund 150 Frames in 3 s");
}

/// Der Belegungsgrad, unter dem gemessen wird, muss derselbe sein, mit dem
/// geplant wurde.
///
/// Wird er **nach** dem eigenen Dispatch erfasst, ist er um eins zu hoch: die
/// Planung fragt Zelle 0 („laeuft allein"), die Beobachtung landet in Zelle 1.
/// Der Schaetzer waere damit vorhanden, aber wirkungslos — ein Fehler, der
/// keine Meldung erzeugt und nur an ausbleibender Wirkung zu erkennen waere.
#[test]
fn observations_land_in_the_cell_that_planning_reads() {
    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(50),
        200,
        400,
        &[20],
    );
    let mut scheduler = build(vec![detector.clone()], 1);
    let mut backend = Backend::default();

    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 600, |t| {
        if t % 50 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });

    let alone = scheduler
        .estimator()
        .observations(ModelIdx(0), onetimer_core::VariantIdx(0), 0);
    let beside_one =
        scheduler
            .estimator()
            .observations(ModelIdx(0), onetimer_core::VariantIdx(0), 1);

    assert!(
        alone > 0,
        "bei einem einzigen Modell auf einem Slot laeuft jede Inferenz allein; \
         Zelle 0 haette {alone} Beobachtungen, Zelle 1 hat {beside_one}"
    );
    assert_eq!(beside_one, 0, "es lief nie etwas parallel");
}
