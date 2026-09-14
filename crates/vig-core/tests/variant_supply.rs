//! Die Variantenwahl haelt den Verbraucher versorgt, nicht nur die Deadline.
//!
//! Befund aus `frontier` (docs/analysis/bursts-and-frontier.md): bei 110 und
//! 125 % Last verfehlte die automatische Wahl 66 bzw. 84 ‰ der Perioden, die
//! kleine Variante allein keine. Jeder einzelne Frame der grossen Variante
//! hielt seine Deadline (`1,5 P`) — aber das Ergebnis des vorigen Frames lief
//! schon nach `max_age - P = P` ab.

#![allow(clippy::unwrap_used, clippy::panic, clippy::arithmetic_side_effects)]

use vig_core::arrayvec::ArrayVec;
use vig_core::estimator::RuntimeEstimator;
use vig_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use vig_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use vig_core::queue::QueueConfig;
use vig_core::request::{Criticality, OverflowPolicy, QueuePolicy};
use vig_core::slots::SlotSet;
use vig_core::variant::{PlanningContext, Resolution, VariantState, resolve};
use vig_core::{Duration, Instant, ModelIdx, VariantIdx};

fn us(v: u64) -> Duration {
    Duration::from_nanos_unbounded(v * 1_000)
}

fn at_us(v: u64) -> Instant {
    Instant::ZERO.checked_add(us(v)).unwrap()
}

/// Das Laufzeitpaar aus `frontier`: gross plant mit 14,69 ms (p99 13,35 ms
/// mal 110 %), klein mit 5,86 ms.
const LARGE_US: u64 = 14_690;
const SMALL_US: u64 = 5_860;

fn contract(period_us: u64, deadline_us: u64, max_age_us: u64) -> ModelContract {
    let mut variants = ArrayVec::new();
    for (quality, runtime) in [(1_000, LARGE_US), (800, SMALL_US)] {
        variants
            .push(Variant {
                quality: QualityValue {
                    value: Quality::from_milli(quality).unwrap(),
                    source: QualitySource::UserDeclared,
                },
                profile: VariantProfile::solo(RuntimeProfile::exact(us(runtime))),
                semantics: vig_core::semantics::VariantSemantics::default(),
                preprocess: Duration::from_nanos_unbounded(0),
            })
            .unwrap();
    }
    ModelContract {
        variants_interchangeable: true,
        criticality: Criticality::Protected,
        queue: QueueConfig {
            policy: QueuePolicy::Latest,
            capacity: 1,
            overflow: OverflowPolicy::RejectNew,
        },
        period: Some(us(period_us)),
        deadline: us(deadline_us),
        max_age: Some(us(max_age_us)),
        stateful: false,
        min_quality: None,
        variant_dwell: us(100_000),
        variants,
        cooperative: None,
        extension: None,
    }
}

/// Waehlt fuer einen Frame, der jetzt aufgenommen wurde und sofort ansteht.
fn chosen(contract: &ModelContract, state: &VariantState, now: Instant) -> VariantIdx {
    contract.validate().unwrap();
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let estimator = RuntimeEstimator::new();
    let deadline = now.checked_add(contract.deadline);
    match resolve(
        contract,
        state,
        ModelIdx(0),
        deadline,
        &PlanningContext {
            degrade: false,
            slots: &slots,
            estimator: &estimator,
            predictor: &vig_core::predictor::Predictor::new(),
            state: vig_core::predictor::StateClass::default(),
            profile_revision: 0,
            margin: SafetyMargin::NONE,
            now,
            residual: Duration::ZERO,
        },
    ) {
        Resolution::Feasible(sel) => sel.variant,
        other => panic!("erwartet Feasible, war {other:?}"),
    }
}

/// `frontier` bei 90 %: Periode 14 ms, Deadline 21, max_age 28. Die grosse
/// Variante haelt die Deadline (14,69 < 21), versorgt aber nicht: das
/// vorige Ergebnis laeuft 14 ms nach dieser Aufnahme ab.
#[test]
fn a_variant_that_meets_its_deadline_but_leaves_a_gap_is_passed_over() {
    let c = contract(14_000, 21_000, 28_000);
    assert_eq!(chosen(&c, &VariantState::default(), at_us(0)).get(), 1);
}

/// `frontier` bei 75 %: Periode 17 ms, Deadline 26, max_age 34. Die grosse
/// Variante versorgt (14,69 < 17) und bleibt die Wahl — die Regel kostet dort
/// keine Qualitaet.
#[test]
fn where_the_large_variant_supplies_it_stays_the_choice() {
    let c = contract(17_000, 26_000, 34_000);
    assert_eq!(chosen(&c, &VariantState::default(), at_us(0)).get(), 0);
}

/// Versorgt keine Variante, bleibt es bei der bisherigen Regel: die beste,
/// die ihre Deadline haelt. Die Versorgungsfrist verschaerft nur, wo sie
/// erreichbar ist; sonst wuerde sie die Qualitaet ohne Gegenwert senken.
#[test]
fn if_no_variant_supplies_the_deadline_rule_decides() {
    // max_age - P = 2 ms: das schafft keine.
    let c = contract(14_000, 21_000, 16_000);
    assert_eq!(chosen(&c, &VariantState::default(), at_us(0)).get(), 0);
}

/// Ohne Hoechstalter ueber der Periode gibt es keine Versorgungsfrist, und die
/// Wahl ist unveraendert die beste machbare.
#[test]
fn without_a_supply_window_nothing_changes() {
    let c = contract(14_000, 21_000, 14_000);
    assert_eq!(chosen(&c, &VariantState::default(), at_us(0)).get(), 0);
}

/// Die Versorgungsfrist haengt an der Aufnahme, nicht an der Ankunft: ein
/// Frame, der 3 ms alt ansteht, hat 3 ms weniger. Bei Periode 17 ms passt die
/// grosse dann nicht mehr (3 + 14,69 > 17).
#[test]
fn the_supply_deadline_is_anchored_at_capture() {
    let c = contract(17_000, 26_000, 34_000);
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let estimator = RuntimeEstimator::new();
    let capture = at_us(0);
    let now = at_us(3_000);
    let r = resolve(
        &c,
        &VariantState::default(),
        ModelIdx(0),
        capture.checked_add(c.deadline),
        &PlanningContext {
            degrade: false,
            slots: &slots,
            estimator: &estimator,
            predictor: &vig_core::predictor::Predictor::new(),
            state: vig_core::predictor::StateClass::default(),
            profile_revision: 0,
            margin: SafetyMargin::NONE,
            now,
            residual: Duration::ZERO,
        },
    );
    match r {
        Resolution::Feasible(sel) => assert_eq!(sel.variant.get(), 1),
        other => panic!("erwartet Feasible, war {other:?}"),
    }
}

/// Eine Abwertung, die die Versorgung rettet, wirkt sofort — auch innerhalb
/// der Verweildauer der grossen Variante (Spec 12.4: abwerten sofort).
#[test]
fn the_downgrade_for_supply_is_immediate() {
    let c = contract(14_000, 21_000, 28_000);
    let mut state = VariantState::default();
    state.record(VariantIdx(0), at_us(0));
    assert_eq!(chosen(&c, &state, at_us(1_000)).get(), 1);
}

/// Qualitaet ist nicht monoton in der Laufzeit: auch eine Aufwertung kann
/// die einzige Variante sein, die die Versorgung rettet (Review 14.09.).
#[test]
fn a_faster_upgrade_can_rescue_supply_during_the_dwell_time() {
    let mut c = contract(10_000, 20_000, 20_000);
    c.variants.get_mut(0).unwrap().profile = VariantProfile::solo(RuntimeProfile::exact(us(5_000)));
    c.variants.get_mut(1).unwrap().profile =
        VariantProfile::solo(RuntimeProfile::exact(us(15_000)));
    let mut state = VariantState::default();
    state.record(VariantIdx(1), at_us(0));
    assert_eq!(chosen(&c, &state, at_us(1_000)), VariantIdx(0));
}

/// Ohne rettbare Versorgung bleibt die bestehende Verweildauer wirksam.
#[test]
fn an_upgrade_that_cannot_rescue_supply_still_observes_the_dwell_time() {
    let mut c = contract(10_000, 20_000, 12_000);
    c.variants.get_mut(0).unwrap().profile = VariantProfile::solo(RuntimeProfile::exact(us(5_000)));
    c.variants.get_mut(1).unwrap().profile =
        VariantProfile::solo(RuntimeProfile::exact(us(15_000)));
    let mut state = VariantState::default();
    state.record(VariantIdx(1), at_us(0));
    assert_eq!(chosen(&c, &state, at_us(1_000)), VariantIdx(1));
}
