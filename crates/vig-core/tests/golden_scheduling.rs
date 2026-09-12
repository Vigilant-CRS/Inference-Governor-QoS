//! Golden Tests fuer Deadline, Slack, Slots, Varianten und Ueberlast
//! (Spec 28: G-005 bis G-009; WP3, WP5, WP6; ADR-0002, ADR-0004, ADR-0007).

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use vig_core::arrayvec::ArrayVec;
use vig_core::estimator::RuntimeEstimator;
use vig_core::feasibility::{
    ExpectedArrival, GuardVerdict, absolute_deadline, evaluate, guard_protected,
};
use vig_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use vig_core::overload::{
    OverloadConfig, OverloadConfigError, OverloadController, OverloadState, PressureSample,
};
use vig_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use vig_core::queue::QueueConfig;
use vig_core::request::OverflowPolicy;
use vig_core::slots::{SlotError, SlotSet};
use vig_core::variant::{PlanningContext, Resolution, VariantState, resolve};
use vig_core::{Criticality, Duration, Instant, ModelIdx, QueuePolicy, RequestId, SlotIdx};

fn ms(v: u64) -> Duration {
    Duration::from_millis(v).unwrap()
}

fn at(v: u64) -> Instant {
    Instant::ZERO.checked_add(ms(v)).unwrap()
}

const M0: ModelIdx = ModelIdx(0);
const M1: ModelIdx = ModelIdx(1);

/// Baut einen Vertrag aus `(Qualitaet in Tausendsteln, p99 in ms)`,
/// absteigend nach Qualitaet.
fn contract(
    variants: &[(u16, u64)],
    deadline_ms: u64,
    criticality: Criticality,
    dwell_ms: u64,
    source: QualitySource,
) -> ModelContract {
    let mut list = ArrayVec::new();
    for (quality, p99) in variants {
        list.push(Variant {
            quality: QualityValue {
                value: Quality::from_milli(*quality).unwrap(),
                source,
            },
            profile: VariantProfile::solo(RuntimeProfile::exact(ms(*p99))),
            semantics: vig_core::semantics::VariantSemantics::default(),
            preprocess: Duration::from_nanos_unbounded(0),
        })
        .unwrap();
    }
    ModelContract {
        variants_interchangeable: true,
        criticality,
        queue: QueueConfig {
            policy: QueuePolicy::Latest,
            capacity: 1,
            overflow: OverflowPolicy::RejectNew,
        },
        period: Some(ms(33)),
        deadline: ms(deadline_ms),
        max_age: Some(ms(66)),
        stateful: false,
        min_quality: None,
        variant_dwell: ms(dwell_ms),
        variants: list,
        cooperative: None,
        extension: None,
    }
}

/// Ruft den Resolver mit einem leeren Schaetzer auf.
///
/// Die Golden Tests pruefen die Auswahlregel gegen die Offline-Profile; ein
/// gefuellter Schaetzer waere hier eine zweite Variable und wuerde die Aussage
/// verwaessern. Sein Verhalten ist in `estimator::tests` eigens geprueft.
fn resolve_with(
    contract: &ModelContract,
    state: &VariantState,
    slots: &SlotSet,
    model: ModelIdx,
    now: Instant,
    deadline: Option<Instant>,
    margin: SafetyMargin,
) -> Resolution {
    let estimator = RuntimeEstimator::new();
    resolve(
        contract,
        state,
        model,
        deadline,
        &PlanningContext {
            degrade: false,
            slots,
            estimator: &estimator,
            predictor: &vig_core::predictor::Predictor::new(),
            state: vig_core::predictor::StateClass::default(),
            profile_revision: 0,
            margin,
            now,
            residual: vig_core::Duration::ZERO,
        },
    )
}

// ---------------------------------------------------------------------------
// G-005 - Deadline from generation time
// ---------------------------------------------------------------------------

/// Ein bereits 25 ms alter Frame mit 30-ms-Deadline erhaelt nicht noch einmal
/// 30 ms ab Gateway-Ankunft.
#[test]
fn g005_deadline_is_anchored_at_capture_not_at_arrival() {
    let generation = at(0);
    let arrival = at(25);
    let deadline = absolute_deadline(generation, ms(30)).unwrap();

    assert_eq!(deadline, at(30));
    assert_eq!(
        deadline.saturating_since(arrival),
        ms(5),
        "ab Ankunft bleiben 5 ms, nicht 30"
    );

    // Und dieselbe Rechnung ab Ankunft waere um 25 ms zu optimistisch.
    let wrong = absolute_deadline(arrival, ms(30)).unwrap();
    assert_eq!(wrong.saturating_since(deadline), ms(25));
}

#[test]
fn g005_a_frame_that_is_already_too_old_is_infeasible_on_arrival() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let generation = at(0);
    let deadline = absolute_deadline(generation, ms(30)).unwrap();
    let now = at(25);

    let f = evaluate(&slots, M0, now, ms(10), Some(deadline)).unwrap();
    assert!(!f.is_feasible(), "10 ms Arbeit passen nicht in 5 ms Rest");
    assert_eq!(f.slack.unwrap().as_nanos(), -5_000_000);
}

#[test]
fn g012_absurd_client_deadline_cannot_overflow_the_schedule() {
    let generation = Instant::from_nanos(u64::MAX - 10);
    assert!(absolute_deadline(generation, ms(30)).is_none());
    assert!(Duration::from_millis(u64::MAX).is_none());
}

// ---------------------------------------------------------------------------
// G-006 - Variant
// ---------------------------------------------------------------------------

/// Restbudget 14 ms; large p99 20, medium p99 12, small p99 7 -> medium.
#[test]
fn g006_picks_the_highest_quality_variant_that_still_fits() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let c = contract(
        &[(1_000, 20), (930, 12), (850, 7)],
        14,
        Criticality::Protected,
        0,
        QualitySource::Measured,
    );
    c.validate().unwrap();

    let now = at(0);
    let deadline = now.checked_add(ms(14)).unwrap();
    let state = VariantState::default();

    let r = resolve_with(
        &c,
        &state,
        &slots,
        M0,
        now,
        Some(deadline),
        SafetyMargin::NONE,
    );
    match r {
        Resolution::Feasible(sel) => {
            assert_eq!(
                sel.variant.get(),
                1,
                "medium ist die beste passende Variante"
            );
            assert_eq!(sel.feasibility.finish, at(12));
            assert_eq!(sel.feasibility.slack.unwrap().as_nanos(), 2_000_000);
        }
        other => panic!("erwartet Feasible, war {other:?}"),
    }
}

#[test]
fn g006_when_nothing_fits_the_fastest_variant_is_named() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let c = contract(
        &[(1_000, 20), (930, 12), (850, 7)],
        5,
        Criticality::Protected,
        0,
        QualitySource::Measured,
    );
    let now = at(0);
    let deadline = now.checked_add(ms(5)).unwrap();

    match resolve_with(
        &c,
        &VariantState::default(),
        &slots,
        M0,
        now,
        Some(deadline),
        SafetyMargin::NONE,
    ) {
        Resolution::Infeasible { fastest } => {
            assert_eq!(fastest.variant.get(), 2, "small ist die schnellste");
            assert!(!fastest.feasibility.is_feasible());
        }
        other => panic!("erwartet Infeasible, war {other:?}"),
    }
}

/// ADR-0007: unbekannte Qualitaetsherkunft deaktiviert die automatische Wahl.
#[test]
fn unknown_quality_provenance_disables_automatic_variant_selection() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let c = contract(
        &[(1_000, 20), (930, 12)],
        14,
        Criticality::Protected,
        0,
        QualitySource::Unknown,
    );
    assert!(!c.auto_variant_selection());

    let now = at(0);
    let deadline = now.checked_add(ms(14)).unwrap();
    // Die beste Variante passt nicht — es wird trotzdem nicht auf eine
    // Variante mit geratener Qualitaet ausgewichen.
    match resolve_with(
        &c,
        &VariantState::default(),
        &slots,
        M0,
        now,
        Some(deadline),
        SafetyMargin::NONE,
    ) {
        Resolution::Infeasible { fastest } => assert_eq!(fastest.variant.get(), 0),
        other => panic!("erwartet Infeasible ohne Ausweichen, war {other:?}"),
    }
}

/// Spec 12.4: Abwertung sofort, Aufwertung erst nach stabiler Verweildauer.
#[test]
fn variant_hysteresis_is_asymmetric() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let c = contract(
        &[(1_000, 20), (930, 12)],
        30,
        Criticality::Protected,
        50, // 50 ms Mindestverweildauer
        QualitySource::Measured,
    );

    // Aktuell laeuft die kleine Variante, seit t=0.
    let mut state = VariantState::default();
    state.record(vig_core::VariantIdx(1), at(0));

    // Bei t=10 waere die grosse Variante machbar - zu frueh fuer eine Aufwertung.
    let d = at(10).checked_add(ms(30)).unwrap();
    match resolve_with(&c, &state, &slots, M0, at(10), Some(d), SafetyMargin::NONE) {
        Resolution::Feasible(sel) => {
            assert_eq!(
                sel.variant.get(),
                1,
                "Aufwertung erst nach der Verweildauer"
            );
        }
        other => panic!("erwartet Feasible, war {other:?}"),
    }

    // Bei t=60 ist die Verweildauer abgelaufen.
    let d = at(60).checked_add(ms(30)).unwrap();
    match resolve_with(&c, &state, &slots, M0, at(60), Some(d), SafetyMargin::NONE) {
        Resolution::Feasible(sel) => assert_eq!(sel.variant.get(), 0, "jetzt aufwerten"),
        other => panic!("erwartet Feasible, war {other:?}"),
    }

    // Abwertung wirkt dagegen sofort: enges Budget bei t=10.
    let mut high = VariantState::default();
    high.record(vig_core::VariantIdx(0), at(0));
    let d = at(10).checked_add(ms(14)).unwrap();
    match resolve_with(&c, &high, &slots, M0, at(10), Some(d), SafetyMargin::NONE) {
        Resolution::Feasible(sel) => assert_eq!(sel.variant.get(), 1, "sofort abwerten"),
        other => panic!("erwartet Feasible, war {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// G-009 - Runtime degradation
// ---------------------------------------------------------------------------

/// Steigt die beobachtete Laufzeit, waehlt der Scheduler eine kleinere Variante.
#[test]
fn g009_a_growing_safety_margin_forces_a_smaller_variant() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let c = contract(
        &[(1_000, 10), (930, 6)],
        12,
        Criticality::Protected,
        0,
        QualitySource::Measured,
    );
    let now = at(0);
    let deadline = now.checked_add(ms(12)).unwrap();

    let normal = SafetyMargin::from_percent(110).unwrap();
    match resolve_with(
        &c,
        &VariantState::default(),
        &slots,
        M0,
        now,
        Some(deadline),
        normal,
    ) {
        Resolution::Feasible(sel) => assert_eq!(sel.variant.get(), 0, "11 ms passen in 12 ms"),
        other => panic!("erwartet Feasible, war {other:?}"),
    }

    // Das Backend wird langsamer; der Estimator hebt die Marge auf 200 %.
    let degraded = SafetyMargin::from_percent(200).unwrap();
    match resolve_with(
        &c,
        &VariantState::default(),
        &slots,
        M0,
        now,
        Some(deadline),
        degraded,
    ) {
        Resolution::Feasible(sel) => {
            assert_eq!(
                sel.variant.get(),
                1,
                "20 ms passen nicht mehr, 12 ms gerade noch"
            );
        }
        other => panic!("erwartet Feasible, war {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// G-007 - Known future protected arrival (Spec 10.7, absichtliches Idle)
// ---------------------------------------------------------------------------

fn detector_arrival(arrival_ms: u64, deadline_ms: u64, runtime_ms: u64) -> ExpectedArrival {
    ExpectedArrival {
        model: M0,
        criticality: Criticality::Protected,
        at: at(arrival_ms),
        deadline: at(deadline_ms),
        supply: None,
        runtime: ms(runtime_ms),
    }
}

#[test]
fn g007_best_effort_waits_for_an_expected_protected_arrival() {
    // Das Beispiel aus Spec 10.7: ein Slot, VLM 50 ms, Detector erwartet bei
    // t=8 ms mit 20 ms Deadline nach Ankunft.
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let forecast = [detector_arrival(8, 28, 10)];

    let verdict = guard_protected(
        &slots,
        M1,
        Criticality::BestEffort,
        ms(50),
        at(0),
        &forecast,
    );

    match verdict {
        GuardVerdict::WouldEndanger { model, retry_after } => {
            assert_eq!(model, M0);
            assert_eq!(
                retry_after,
                at(50),
                "erst nach Ende der Blockade erneut versuchen"
            );
        }
        GuardVerdict::Clear => panic!("der 50-ms-Job wuerde den Detector sicher verdraengen"),
    }
}

#[test]
fn g007_short_best_effort_work_is_not_blocked() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let forecast = [detector_arrival(8, 28, 10)];

    // 5 ms Arbeit ist bei t=5 fertig, der Detector startet weiterhin bei t=8.
    let verdict = guard_protected(&slots, M1, Criticality::BestEffort, ms(5), at(0), &forecast);
    assert_eq!(verdict, GuardVerdict::Clear);
}

/// Ein Veto, das nichts rettet, waere schlechter als FIFO: es wuerde die GPU
/// leer laufen lassen, ohne die geschuetzte Arbeit zu sichern.
#[test]
fn g007_no_veto_when_the_protected_work_was_doomed_anyway() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    // Deadline 12 ms bei Ankunft 8 ms und 10 ms Laufzeit: schon ohne
    // Konkurrenz nicht zu halten.
    let forecast = [detector_arrival(8, 12, 10)];

    let verdict = guard_protected(
        &slots,
        M1,
        Criticality::BestEffort,
        ms(50),
        at(0),
        &forecast,
    );
    assert_eq!(verdict, GuardVerdict::Clear, "kein Idle ohne Nutzen");
}

#[test]
fn g007_a_second_slot_removes_the_conflict() {
    // Mit zwei Slots blockiert der VLM den Detector nicht mehr. Genau das ist
    // der Grund, warum das Backend als Slot-Menge modelliert wird (ADR-0004).
    let slots = SlotSet::homogeneous(2, 0).unwrap();
    let forecast = [detector_arrival(8, 28, 10)];

    let verdict = guard_protected(
        &slots,
        M1,
        Criticality::BestEffort,
        ms(50),
        at(0),
        &forecast,
    );
    assert_eq!(verdict, GuardVerdict::Clear);
}

#[test]
fn guard_ignores_arrivals_of_equal_or_lower_criticality() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let mut forecast = [detector_arrival(8, 28, 10)];
    forecast[0].criticality = Criticality::BestEffort;

    let verdict = guard_protected(
        &slots,
        M1,
        Criticality::BestEffort,
        ms(50),
        at(0),
        &forecast,
    );
    assert_eq!(
        verdict,
        GuardVerdict::Clear,
        "Best-Effort schuetzt kein Best-Effort"
    );
}

/// Eine Ankunft nach dem Ende des Kandidaten kann er nicht verspaeten.
#[test]
fn guard_ignores_arrivals_after_the_candidate_has_finished() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let forecast = [detector_arrival(500, 520, 10)];

    let verdict = guard_protected(
        &slots,
        M1,
        Criticality::BestEffort,
        ms(50),
        at(0),
        &forecast,
    );
    assert_eq!(verdict, GuardVerdict::Clear);
}

/// ADR-0036: kein fester Horizont. Reicht der Kandidat bis an eine Ankunft in
/// 500 ms heran, ist sie geschuetzt — bis NV-23 lag sie jenseits von 100 ms
/// und zaehlte nicht.
#[test]
fn guard_protects_an_arrival_however_far_away_the_candidate_reaches() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let forecast = [detector_arrival(500, 520, 10)];

    let verdict = guard_protected(
        &slots,
        M1,
        Criticality::BestEffort,
        ms(515),
        at(0),
        &forecast,
    );
    assert!(matches!(verdict, GuardVerdict::WouldEndanger { .. }));
}

// ---------------------------------------------------------------------------
// ADR-0002 / L-021 - Backend In-Flight Control
// ---------------------------------------------------------------------------

#[test]
fn l021_dispatch_is_bounded_by_slot_credits() {
    let mut slots = SlotSet::homogeneous(2, 0).unwrap();

    assert!(
        slots
            .dispatch(SlotIdx(0), RequestId(1), M0, at(0), ms(10))
            .is_ok()
    );
    assert!(
        slots
            .dispatch(SlotIdx(1), RequestId(2), M0, at(0), ms(10))
            .is_ok()
    );
    assert_eq!(slots.in_flight(), 2);
    assert_eq!(slots.occupancy(), 2);

    // Ohne Pipelining ist jetzt Schluss: die Backend-Queue bleibt leer.
    assert_eq!(
        slots.dispatch(SlotIdx(0), RequestId(3), M0, at(0), ms(10)),
        Err(SlotError::NoCredit(SlotIdx(0)))
    );
    assert!(!slots.has_credit(M0), "kein Slot mit freiem Kredit");
    assert!(slots.ready_slot(M0, at(0)).is_none());
    // Die Planung sieht trotzdem, wann wieder Kapazitaet entsteht - Kredit ist
    // eine Live-Schranke, keine Kapazitaetsaussage (ADR-0002 vs. ADR-0004).
    assert_eq!(slots.projected_start(M0, at(0)).unwrap().1, at(10));

    // Nach einer Fertigstellung ist der Kredit wieder da.
    assert!(slots.complete(SlotIdx(0), RequestId(1)).is_some());
    assert_eq!(slots.in_flight(), 1);
    assert!(
        slots
            .dispatch(SlotIdx(0), RequestId(3), M0, at(10), ms(10))
            .is_ok()
    );
}

#[test]
fn a_duplicate_completion_does_not_invent_credit() {
    let mut slots = SlotSet::homogeneous(1, 0).unwrap();
    slots
        .dispatch(SlotIdx(0), RequestId(1), M0, at(0), ms(10))
        .unwrap();

    assert!(slots.complete(SlotIdx(0), RequestId(1)).is_some());
    assert!(
        slots.complete(SlotIdx(0), RequestId(1)).is_none(),
        "zweite Meldung ist wirkungslos"
    );
    assert_eq!(slots.in_flight(), 0);
}

#[test]
fn pipelining_depth_allows_exactly_one_queued_request_per_slot() {
    let mut slots = SlotSet::homogeneous(1, 1).unwrap();
    assert!(
        slots
            .dispatch(SlotIdx(0), RequestId(1), M0, at(0), ms(10))
            .is_ok()
    );
    let queued = slots
        .dispatch(SlotIdx(0), RequestId(2), M0, at(0), ms(10))
        .unwrap();
    assert_eq!(
        queued.expected_finish,
        at(20),
        "der zweite startet erst nach dem ersten"
    );
    assert_eq!(
        slots.dispatch(SlotIdx(0), RequestId(3), M0, at(0), ms(10)),
        Err(SlotError::NoCredit(SlotIdx(0))),
        "die Backend-Queue bleibt bei hoechstens eins"
    );
}

#[test]
fn slot_configuration_is_validated() {
    assert_eq!(SlotSet::homogeneous(0, 0).unwrap_err(), SlotError::NoSlots);
    assert_eq!(
        SlotSet::homogeneous(99, 0).unwrap_err(),
        SlotError::TooManySlots { requested: 99 }
    );
    assert_eq!(
        SlotSet::homogeneous(1, 9).unwrap_err(),
        SlotError::PipeliningTooDeep { requested: 9 }
    );
}

// ---------------------------------------------------------------------------
// ADR-0006 - Co-Run-Veto statt Interferenzmatrix
// ---------------------------------------------------------------------------

#[test]
fn forbidden_corun_delays_the_start_instead_of_blocking_the_slot() {
    let mut slots = SlotSet::homogeneous(2, 0).unwrap();
    slots.forbid_corun(M0, M1);

    slots
        .dispatch(SlotIdx(0), RequestId(1), M1, at(0), ms(50))
        .unwrap();

    assert!(
        !slots.corun_allowed(M0),
        "der Detector darf nicht neben dem VLM laufen"
    );
    let (_, start) = slots.projected_start(M0, at(0)).unwrap();
    assert_eq!(start, at(50), "er startet, sobald der VLM fertig ist");

    // Ein unbeteiligtes Modell bleibt unbehelligt.
    assert!(slots.corun_allowed(ModelIdx(2)));
}

#[test]
fn a_model_may_always_corun_with_itself() {
    let mut slots = SlotSet::homogeneous(2, 0).unwrap();
    slots.forbid_corun(M0, M1);
    slots
        .dispatch(SlotIdx(0), RequestId(1), M0, at(0), ms(50))
        .unwrap();
    assert!(slots.corun_allowed(M0));
    assert_eq!(slots.projected_start(M0, at(0)).unwrap().1, at(0));
}

// ---------------------------------------------------------------------------
// WP5 - Overload state machine (Spec 14)
// ---------------------------------------------------------------------------

fn drive(controller: &mut OverloadController, from_ms: u64, to_ms: u64, violated: bool) {
    let mut t = from_ms;
    while t < to_ms {
        controller.observe(
            at(t),
            PressureSample {
                guarded: true,
                violated,
            },
        );
        controller.evaluate(at(t));
        t = t.saturating_add(5);
    }
}

#[test]
fn overload_escalates_under_sustained_protected_violations() {
    let mut c = OverloadController::new(OverloadConfig::default(), at(0)).unwrap();
    assert_eq!(c.state(), OverloadState::Normal);

    drive(&mut c, 0, 3_000, true);
    assert_eq!(c.pressure_permille(), 1_000);
    assert_eq!(
        c.state(),
        OverloadState::ProtectedOnly,
        "voller Druck eskaliert bis ganz oben"
    );
}

#[test]
fn overload_escalates_one_level_at_a_time() {
    let mut c = OverloadController::new(OverloadConfig::default(), at(0)).unwrap();
    let mut seen = Vec::new();
    let mut t = 0_u64;
    while t < 3_000 {
        c.observe(
            at(t),
            PressureSample {
                guarded: true,
                violated: true,
            },
        );
        let before = c.state();
        let after = c.evaluate(at(t));
        if after != before {
            seen.push(after);
            assert_eq!(
                after.level(),
                before.level().saturating_add(1),
                "kein Ueberspringen von Stufen"
            );
        }
        t = t.saturating_add(5);
    }
    assert_eq!(
        seen,
        vec![
            OverloadState::FreshnessPressure,
            OverloadState::Degraded,
            OverloadState::RejectBestEffort,
            OverloadState::ProtectedOnly,
        ]
    );
}

/// Spec 14.3: Rueckkehr darf nicht knapp unter der Eintrittsschwelle erfolgen.
#[test]
fn overload_recovers_and_does_not_oscillate() {
    let mut c = OverloadController::new(OverloadConfig::default(), at(0)).unwrap();
    drive(&mut c, 0, 3_000, true);
    assert_eq!(c.state(), OverloadState::ProtectedOnly);

    drive(&mut c, 3_000, 8_000, false);
    assert_eq!(
        c.state(),
        OverloadState::Normal,
        "ohne Druck zurueck auf NORMAL"
    );
    assert_eq!(c.pressure_permille(), 0);
}

#[test]
fn best_effort_rejection_does_not_escalate_overload() {
    let mut c = OverloadController::new(OverloadConfig::default(), at(0)).unwrap();
    let mut t = 0_u64;
    while t < 3_000 {
        // Massenhaft abgewiesene Best-Effort-Arbeit: das ist die beabsichtigte
        // Wirkung des Systems, kein Ueberlastsignal.
        c.observe(
            at(t),
            PressureSample {
                guarded: false,
                violated: true,
            },
        );
        c.evaluate(at(t));
        t = t.saturating_add(5);
    }
    assert_eq!(c.state(), OverloadState::Normal);
    assert_eq!(c.pressure_permille(), 0);
}

#[test]
fn overload_states_gate_admission_as_specified() {
    assert!(OverloadState::Normal.admits(Criticality::BestEffort));
    assert!(OverloadState::Degraded.admits(Criticality::BestEffort));
    assert!(!OverloadState::RejectBestEffort.admits(Criticality::BestEffort));
    assert!(OverloadState::RejectBestEffort.admits(Criticality::Normal));
    assert!(!OverloadState::ProtectedOnly.admits(Criticality::Normal));
    assert!(OverloadState::ProtectedOnly.admits(Criticality::High));
    assert!(OverloadState::ProtectedOnly.admits(Criticality::Protected));

    assert!(!OverloadState::FreshnessPressure.forces_degradation());
    assert!(OverloadState::Degraded.forces_degradation());
    assert!(OverloadState::FreshnessPressure.aggressive_supersession());
    assert!(!OverloadState::Normal.aggressive_supersession());
}

/// L-020: eine Konfiguration ohne echte Hysterese wird abgelehnt, nicht
/// stillschweigend korrigiert.
#[test]
fn overload_config_without_hysteresis_is_rejected() {
    let bad = OverloadConfig {
        exit: [50, 100, 200, 400],
        ..OverloadConfig::default()
    };
    assert_eq!(
        bad.validate().unwrap_err(),
        OverloadConfigError::NoHysteresis {
            level: 1,
            enter: 50,
            exit: 50
        }
    );

    // Nur die Monotonie verletzen, die Hysterese je Stufe aber einhalten.
    let unsorted = OverloadConfig {
        enter: [50, 45, 200, 400],
        exit: [20, 40, 100, 200],
        ..OverloadConfig::default()
    };
    assert_eq!(
        unsorted.validate().unwrap_err(),
        OverloadConfigError::ThresholdsNotMonotonic { level: 2 }
    );

    assert!(OverloadConfig::default().validate().is_ok());
}
