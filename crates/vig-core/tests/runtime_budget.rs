//! Mindestlaufzeit fuer nachrangige Arbeit (ADR-0046).
//!
//! Drei Aussagen: Ein Budget hebt wartende Arbeit ueber `normal`, solange es
//! im Fenster nicht aufgebraucht ist, und faellt danach in die eigene Klasse
//! zurueck. Es verspaetet nie geschuetzte Arbeit ueber ihren spaetesten Start
//! hinaus. Ohne Budget ist die Ordnung die alte.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use vig_core::arrayvec::ArrayVec;
use vig_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use vig_core::queue::QueueConfig;
use vig_core::request::OverflowPolicy;
use vig_core::runtime_budget::RuntimeBudget;
use vig_core::scheduler::{Action, Event, Scheduler, SchedulerError};
use vig_core::slots::SlotSet;
use vig_core::{
    Criticality, Duration, Instant, ModelIdx, PayloadRef, QueuePolicy, RequestDescriptor,
    RequestId, RequestState, SlotIdx, SupersessionKey,
};

fn ms(v: u64) -> Duration {
    Duration::from_millis(v).unwrap()
}

fn at(v: u64) -> Instant {
    Instant::ZERO.checked_add(ms(v)).unwrap()
}

fn contract(
    criticality: Criticality,
    period_ms: Option<u64>,
    deadline_ms: u64,
    max_age_ms: u64,
    runtime_ms: u64,
) -> ModelContract {
    let mut variants = ArrayVec::new();
    variants
        .push(Variant {
            quality: QualityValue {
                value: Quality::FULL,
                source: QualitySource::Measured,
            },
            profile: VariantProfile::solo(RuntimeProfile::exact(ms(runtime_ms))),
            semantics: vig_core::semantics::VariantSemantics::default(),
            preprocess: Duration::ZERO,
        })
        .unwrap();
    ModelContract {
        variants_interchangeable: true,
        criticality,
        queue: QueueConfig {
            policy: if period_ms.is_some() {
                QueuePolicy::Latest
            } else {
                QueuePolicy::Fifo
            },
            capacity: if period_ms.is_some() { 1 } else { 2 },
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
        extension: None,
        min_runtime: None,
        objective: None,
    }
}

fn with_budget(mut c: ModelContract, budget_ms: u64, window_ms: u64) -> ModelContract {
    c.min_runtime = Some(RuntimeBudget {
        budget: ms(budget_ms),
        window: ms(window_ms),
    });
    c
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
        context_tokens: 0,
        decomposable: false,
    }
}

fn build(contracts: &[ModelContract], slots: usize) -> Result<Scheduler, SchedulerError> {
    let mut list = ArrayVec::new();
    for c in contracts {
        list.push(c.clone()).unwrap();
    }
    Scheduler::new(
        list,
        SlotSet::homogeneous(slots, 0).unwrap(),
        OverloadController::new(OverloadConfig::default(), at(0)).unwrap(),
        SafetyMargin::NONE,
    )
}

/// Ein Mock-Backend, das jeden Auftrag nach seiner geplanten Laufzeit
/// fertigmeldet und mitschreibt, was wann gestartet ist.
#[derive(Default)]
struct Backend {
    pending: Vec<(Instant, RequestId, SlotIdx)>,
    /// `(Startzeit in ms, Modell)`, in Dispatchreihenfolge.
    starts: Vec<(u64, u16)>,
}

impl Backend {
    fn due(&mut self, now: Instant) -> Vec<(RequestId, SlotIdx)> {
        let (due, rest): (Vec<_>, Vec<_>) = self.pending.iter().partition(|(f, _, _)| *f <= now);
        self.pending = rest;
        due.into_iter().map(|(_, r, s)| (r, s)).collect()
    }

    fn collect(&mut self, t: u64, now: Instant, actions: &[Action]) {
        for action in actions {
            if let Action::Dispatch {
                request,
                model,
                slot,
                predicted_runtime,
                ..
            } = *action
            {
                self.starts.push((t, model.0));
                self.pending
                    .push((now.checked_add(predicted_runtime).unwrap(), request, slot));
            }
        }
    }

    fn models(&self) -> Vec<u16> {
        self.starts.iter().map(|(_, m)| *m).collect()
    }

    fn count(&self, model: u16) -> usize {
        self.starts.iter().filter(|(_, m)| *m == model).count()
    }
}

/// Faehrt `duration_ms` in 1-ms-Schritten: Fertigstellungen, dann Ankuenfte.
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
            scheduler.on_event(now, Event::Completion { request, slot }, &mut |a| {
                actions.push(a);
            });
        }
        for descriptor in arrivals(t) {
            scheduler.on_event(now, Event::Arrival(descriptor), &mut |a| actions.push(a));
        }
        scheduler.on_event(now, Event::Tick, &mut |a| actions.push(a));
        backend.collect(t, now, &actions);
    }
}

const NORMAL: u16 = 0;
const BACKGROUND: u16 = 1;

/// Ein Slot, `normal` und `best_effort` mit je 40 ms. Neue `normal`-Arbeit
/// trifft 1 ms vor jeder Fertigstellung ein, es wartet also immer welche.
fn normal_and_background(budget: Option<(u64, u64)>) -> (Scheduler, Backend) {
    let normal = contract(Criticality::Normal, None, 1_000, 2_000, 40);
    let mut background = contract(Criticality::BestEffort, None, 5_000, 8_000, 40);
    if let Some((b, w)) = budget {
        background = with_budget(background, b, w);
    }
    let mut scheduler = build(&[normal.clone(), background.clone()], 1).unwrap();
    let mut backend = Backend::default();
    let mut id = 0_u64;
    run(&mut scheduler, &mut backend, 2_000, |t| {
        let mut out = Vec::new();
        if t == 0 || t % 40 == 39 {
            id += 1;
            out.push(frame(id, NORMAL, t, &normal));
        }
        if t % 40 == 1 {
            id += 1;
            out.push(frame(id, BACKGROUND, t, &background));
        }
        out
    });
    (scheduler, backend)
}

/// (a) Mit 100 ms je Sekunde laeuft der Hintergrund dreimal vor der
/// wartenden `normal`-Arbeit (40 + 40 + 40 ms, erst die dritte Buchung
/// erreicht das Budget), faellt dann zurueck und kommt erst wieder dran, wenn
/// die erste Buchung aus dem Fenster gleitet.
#[test]
fn a_budget_lifts_background_above_waiting_normal_work_until_it_is_spent() {
    let (scheduler, backend) = normal_and_background(Some((100, 1_000)));
    let models = backend.models();
    assert_eq!(
        models[..5],
        [NORMAL, BACKGROUND, BACKGROUND, BACKGROUND, NORMAL],
        "Starts: {:?}",
        &backend.starts[..8]
    );

    // Aufgebraucht: bis das Fenster gleitet, bekommt `normal` jeden Slot.
    let background_between = backend
        .starts
        .iter()
        .filter(|(t, m)| *m == BACKGROUND && *t > 120 && *t < 960)
        .count();
    assert_eq!(background_between, 0, "{:?}", backend.starts);

    // Die Buchung von 40 ms faellt nach 15/16 bis 16/16 des Fensters heraus;
    // am naechsten freien Slot geht der Hintergrund wieder vor.
    assert!(
        backend
            .starts
            .iter()
            .any(|(t, m)| *m == BACKGROUND && (960..=1_040).contains(t)),
        "{:?}",
        backend.starts
    );

    let m = scheduler.metrics();
    assert_eq!(m.runtime_budget_granted_us[1], 100_000);
    assert_eq!(m.runtime_budget_window_us[1], 1_000_000);
    assert!(m.runtime_budget_dispatches[1] >= 4);
    assert_eq!(m.runtime_budget_dispatches[0], 0);
    // Gleichmaessig ueber zwei Sekunden: 100 ms Budget je Fenster, um
    // hoechstens einen Auftrag ueberzogen.
    let background = backend.count(BACKGROUND);
    assert!(
        (6..=10).contains(&background),
        "{background} Hintergrundstarts in 2 s"
    );
}

/// (c) Ohne Budget bekommt `best_effort` keinen Slot, solange `normal` wartet
/// — die Ordnung aus Spec 10.6, unveraendert.
#[test]
fn without_a_budget_the_order_is_the_old_one() {
    let (scheduler, backend) = normal_and_background(None);
    assert_eq!(backend.count(BACKGROUND), 0, "{:?}", backend.starts);
    assert!(backend.count(NORMAL) >= 49);
    let m = scheduler.metrics();
    assert_eq!(m.runtime_budget_granted_us, [0; vig_core::ids::MAX_MODELS]);
    assert_eq!(m.runtime_budget_used_us, [0; vig_core::ids::MAX_MODELS]);
    assert_eq!(m.runtime_budget_dispatches, [0; vig_core::ids::MAX_MODELS]);
    assert_eq!(
        scheduler.runtime_budget_used(ModelIdx(BACKGROUND), at(0)),
        None
    );
}

/// (c) Ein Budget, das nie Vorrang braucht, aendert keine Entscheidung: mit
/// nur einem wartenden Strom ist die Aktionsfolge dieselbe wie ohne.
#[test]
fn a_budget_that_never_competes_changes_no_action() {
    let trace = |budget: bool| {
        let mut background = contract(Criticality::BestEffort, None, 5_000, 8_000, 30);
        if budget {
            background = with_budget(background, 100, 1_000);
        }
        let mut scheduler = build(&[background.clone()], 1).unwrap();
        let mut actions = Vec::new();
        let mut pending: Vec<(Instant, RequestId, SlotIdx)> = Vec::new();
        for t in 0..1_000_u64 {
            let now = at(t);
            let (due, rest): (Vec<_>, Vec<_>) = pending.iter().partition(|(f, _, _)| *f <= now);
            pending = rest;
            let mut step = Vec::new();
            for (_, request, slot) in due {
                scheduler.on_event(now, Event::Completion { request, slot }, &mut |a| {
                    step.push(a);
                });
            }
            if t % 50 == 0 {
                scheduler.on_event(
                    now,
                    Event::Arrival(frame(t + 1, 0, t, &background)),
                    &mut |a| step.push(a),
                );
            }
            for a in &step {
                if let Action::Dispatch {
                    request,
                    slot,
                    predicted_runtime,
                    ..
                } = *a
                {
                    pending.push((now.checked_add(predicted_runtime).unwrap(), request, slot));
                }
            }
            actions.extend(step);
        }
        actions
    };
    assert_eq!(trace(false), trace(true));
}

/// Das Budget hebt nie ueber `high`: wartet bewachte Arbeit, geht sie vor.
#[test]
fn a_budget_never_lifts_above_high() {
    let high = contract(Criticality::High, None, 1_000, 2_000, 40);
    let background = with_budget(
        contract(Criticality::BestEffort, None, 5_000, 8_000, 40),
        1_000,
        1_000,
    );
    let mut scheduler = build(&[high.clone(), background.clone()], 1).unwrap();
    let mut backend = Backend::default();
    let mut id = 0_u64;
    run(&mut scheduler, &mut backend, 1_000, |t| {
        let mut out = Vec::new();
        if t == 0 || t % 40 == 39 {
            id += 1;
            out.push(frame(id, 0, t, &high));
        }
        if t % 40 == 1 {
            id += 1;
            out.push(frame(id, 1, t, &background));
        }
        out
    });
    assert_eq!(backend.count(1), 0, "{:?}", backend.starts);
}

/// (b) Ein Slot, ein geschuetzter Detektor mit 33 ms Takt, daneben ein
/// 200-ms-Block mit einem Budget ueber das ganze Fenster: der Block passt nie
/// vor die naechste geschuetzte Ankunft, und das Budget schafft keine Luecke.
/// Er startet nie, der Detektor verfehlt keine Deadline.
#[test]
fn a_budget_does_not_create_a_gap_next_to_protected_work() {
    let detector = contract(Criticality::Protected, Some(33), 30, 66, 10);
    let vlm = with_budget(
        contract(Criticality::BestEffort, None, 800, 1_500, 200),
        1_000,
        1_000,
    );
    let mut scheduler = build(&[detector.clone(), vlm.clone()], 1).unwrap();
    let mut backend = Backend::default();
    let mut id = 0_u64;
    run(&mut scheduler, &mut backend, 2_000, |t| {
        let mut out = Vec::new();
        if t % 33 == 0 {
            id += 1;
            out.push(frame(id, 0, t, &detector));
        }
        if t % 250 == 10 {
            id += 1;
            out.push(frame(id, 1, t, &vlm));
        }
        out
    });
    let m = scheduler.metrics();
    assert_eq!(m.protected_deadline_misses, 0);
    assert!(m.deferred_for_protected > 0, "der Look-ahead hat vetoiert");
    assert_eq!(backend.count(1), 0, "{:?}", backend.starts);
    assert_eq!(
        m.runtime_budget_used_us[1], 0,
        "nichts gerechnet, nichts gebucht"
    );
}

/// (b) Zwei Slots, ein geschuetzter Detektor, zwei `normal`-Kameras, die
/// zusammen mehr als einen Slot fuellen, und ein 100-ms-Block mit 300 ms je
/// Sekunde. Mit Budget laeuft der Block mehrfach je Sekunde; der Detektor
/// verfehlt in beiden Faellen keine Deadline, und jede seiner Ankuenfte
/// startet spaetestens zu ihrem spaetesten Start.
#[test]
fn a_budget_is_paid_by_normal_work_never_by_the_protected_stream() {
    let outcome = |budget: bool| {
        let detector = contract(Criticality::Protected, Some(33), 30, 66, 15);
        let left = contract(Criticality::Normal, Some(33), 66, 100, 15);
        let right = contract(Criticality::Normal, Some(33), 66, 100, 15);
        let mut vlm = contract(Criticality::BestEffort, None, 5_000, 8_000, 100);
        if budget {
            vlm = with_budget(vlm, 300, 1_000);
        }
        let contracts = vec![detector, left, right, vlm];
        let mut scheduler = build(&contracts, 2).unwrap();
        let mut backend = Backend::default();
        let mut id = 0_u64;
        let mut detector_frames = Vec::new();
        run(&mut scheduler, &mut backend, 3_000, |t| {
            let mut out = Vec::new();
            for (model, phase) in [(0_u16, 0_u64), (1, 11), (2, 22)] {
                if t % 33 == phase {
                    id += 1;
                    if model == 0 {
                        detector_frames.push(t);
                    }
                    out.push(frame(id, model, t, &contracts[usize::from(model)]));
                }
            }
            if t % 50 == 5 {
                id += 1;
                out.push(frame(id, 3, t, &contracts[3]));
            }
            out
        });
        // Jede Ankunft des Detektors: spaetester Start = Aufnahme + 30 - 15.
        let detector_starts: Vec<u64> = backend
            .starts
            .iter()
            .filter(|(_, m)| *m == 0)
            .map(|(t, _)| *t)
            .collect();
        let late = detector_frames
            .iter()
            .zip(detector_starts.iter())
            .filter(|(capture, start)| **start > **capture + 15)
            .count();
        (
            scheduler.metrics().protected_deadline_misses,
            detector_starts.len(),
            late,
            backend.count(3),
        )
    };

    let (misses_without, detector_without, late_without, vlm_without) = outcome(false);
    let (misses_with, detector_with, late_with, vlm_with) = outcome(true);
    assert_eq!(misses_without, 0);
    assert_eq!(misses_with, 0);
    assert_eq!(late_without, 0);
    assert_eq!(late_with, 0);
    assert!(
        detector_with >= detector_without,
        "Detektor mit Budget {detector_with}, ohne {detector_without}"
    );
    // 300 ms je Sekunde bei 100-ms-Bloecken: mindestens drei je Sekunde.
    assert!(vlm_with >= 9, "mit Budget {vlm_with} Bloecke in 3 s");
    assert!(
        vlm_with > vlm_without,
        "mit Budget {vlm_with}, ohne {vlm_without}"
    );
}

/// Ein Budget, das die Slots nicht rechnen koennen, verhindert den Start.
#[test]
fn a_budget_beyond_the_slots_is_refused_at_construction() {
    let vlm = with_budget(
        contract(Criticality::BestEffort, None, 5_000, 8_000, 100),
        1_500,
        1_000,
    );
    assert!(matches!(
        build(std::slice::from_ref(&vlm), 1),
        Err(SchedulerError::Contract { model: 0, .. })
    ));
    assert!(build(&[vlm], 2).is_ok());
}

/// Ein Budget an bewachter Arbeit ist ein Vertragsfehler, kein Schalter.
#[test]
fn a_budget_on_a_guarded_class_is_refused() {
    let detector = with_budget(
        contract(Criticality::Protected, Some(33), 30, 66, 10),
        100,
        1_000,
    );
    assert!(detector.validate().is_err());
    assert!(matches!(
        build(&[detector], 1),
        Err(SchedulerError::Contract { model: 0, .. })
    ));
    let _ = RequestState::Queued;
}
