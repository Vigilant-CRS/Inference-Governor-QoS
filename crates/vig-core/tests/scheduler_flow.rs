//! End-to-End-Verhalten des Schedulers gegen ein Mock-Backend.
//!
//! Die Bausteine sind einzeln getestet (`golden_queue`, `golden_scheduling`);
//! hier wird geprueft, dass sie zusammen das erwartete Systemverhalten zeigen.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use vig_core::arrayvec::ArrayVec;
use vig_core::contract_ext::{ApprovedVariants, ContractExtension};
use vig_core::ids::VariantIdx;
use vig_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use vig_core::queue::QueueConfig;
use vig_core::request::OverflowPolicy;
use vig_core::scheduler::{Action, Event, Scheduler};
use vig_core::slots::SlotSet;
use vig_core::{
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
                semantics: vig_core::semantics::VariantSemantics::default(),
                preprocess: Duration::from_nanos_unbounded(0),
            })
            .unwrap();
    }
    ModelContract {
        variants_interchangeable: true,
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
        extension: None,
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
        context_tokens: 0,
        decomposable: false,
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
    pending: Vec<(Instant, RequestId, vig_core::SlotIdx)>,
    dispatched: Vec<RequestId>,
    terminated: Vec<(RequestId, RequestState)>,
    observed: Vec<u64>,
    /// NV-02: welche Variante tatsaechlich gelaufen ist.
    variants: Vec<vig_core::ids::VariantIdx>,
    /// NV-24: welches Modell, in Dispatchreihenfolge.
    dispatched_models: Vec<ModelIdx>,
}

impl Backend {
    fn collect(&mut self, now: Instant, actions: &[Action]) {
        for action in actions {
            match *action {
                Action::Dispatch {
                    request,
                    slot,
                    predicted_runtime,
                    variant,
                    model,
                    ..
                } => {
                    self.dispatched.push(request);
                    self.variants.push(variant);
                    self.dispatched_models.push(model);
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

    fn due(&mut self, now: Instant) -> Vec<(RequestId, vig_core::SlotIdx)> {
        let (due, rest): (Vec<_>, Vec<_>) = self
            .pending
            .iter()
            .partition(|(finish, _, _)| *finish <= now);
        self.pending = rest;
        due.into_iter().map(|(_, r, s)| (r, s)).collect()
    }

    fn variants_used(&self) -> &[vig_core::ids::VariantIdx] {
        &self.variants
    }

    /// Welche Modelle in Dispatchreihenfolge gelaufen sind (NV-24).
    fn dispatched_models(&self) -> &[ModelIdx] {
        &self.dispatched_models
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
        .observations(ModelIdx(0), vig_core::VariantIdx(0), 0);
    let beside_one = scheduler
        .estimator()
        .observations(ModelIdx(0), vig_core::VariantIdx(0), 1);

    assert!(
        alone > 0,
        "bei einem einzigen Modell auf einem Slot laeuft jede Inferenz allein; \
         Zelle 0 haette {alone} Beobachtungen, Zelle 1 hat {beside_one}"
    );
    assert_eq!(beside_one, 0, "es lief nie etwas parallel");
}

/// Eine Last, die den Vertrag dauerhaft sprengt, muss sichtbar werden.
///
/// Der Dauerlauf hat gezeigt, dass der Governor in diesem Fall still
/// degradiert: er verwirft mehr Frames, die Abdeckung faellt, und nichts sagt
/// warum (`docs/benchmark/soak.md`). Dieser Test haelt fest, dass der Befund
/// jetzt bis in den Metrikabzug durchkommt.
#[test]
fn a_load_that_breaks_the_contract_becomes_visible() {
    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        150,
        100,
        &[10],
    );
    let mut scheduler = build(vec![detector.clone()], 1);
    let mut backend = Backend::default();

    // Der Vertrag nennt 33 ms; die Quelle liefert alle 12 ms.
    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 4_000, |t| {
        if t % 12 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });

    assert_eq!(
        scheduler.arrival_exceeds_contract(ModelIdx(0)),
        Some(true),
        "eine fast dreimal so schnelle Quelle muss als Befund erscheinen"
    );

    let m = scheduler.metrics();
    assert_eq!(m.contract_period_us[0], 33_000);
    let observed = m.arrival_period_us[0];
    assert!(
        (11_000..=13_000).contains(&observed),
        "beobachteter Abstand {observed} us liegt nicht bei den gesendeten 12 ms"
    );
}

/// Die Gegenprobe: wer seinen Vertrag einhaelt, loest nichts aus.
#[test]
fn a_load_within_the_contract_stays_silent() {
    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        150,
        100,
        &[10],
    );
    let mut scheduler = build(vec![detector.clone()], 1);
    let mut backend = Backend::default();

    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 4_000, |t| {
        if t % 33 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });

    assert_eq!(scheduler.arrival_exceeds_contract(ModelIdx(0)), Some(false));
}

// ---------------------------------------------------------------------------
// NV-02 — Freigabe ist keine Qualitaetsschwelle
// ---------------------------------------------------------------------------

/// Ein Vertrag mit drei Varianten, von denen nur die genannten freigegeben sind.
fn with_approved(mut contract: ModelContract, approved: &[u16]) -> ModelContract {
    let indices: Vec<VariantIdx> = approved.iter().copied().map(VariantIdx).collect();
    contract.extension = Some(ContractExtension {
        approved_variants: ApprovedVariants::from_indices(&indices),
        ..ContractExtension::default()
    });
    contract
}

#[test]
fn an_unapproved_variant_is_never_dispatched() {
    // Drei Varianten, aber nur die langsame mittlere ist freigegeben. Die
    // beste waere qualitativ und zeitlich besser — und bleibt trotzdem aus.
    let detector = with_approved(
        contract(
            Criticality::Protected,
            QueuePolicy::Latest,
            Some(33),
            33,
            66,
            &[5, 8, 12],
        ),
        &[1],
    );
    assert!(detector.validate().is_ok());

    let mut scheduler = build(vec![detector.clone()], 1);
    let mut backend = Backend::default();
    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 1_000, |t| {
        if t % 33 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });

    assert!(
        !backend.variants_used().is_empty(),
        "es muss ueberhaupt etwas gelaufen sein"
    );
    assert!(
        backend.variants_used().iter().all(|v| *v == VariantIdx(1)),
        "gelaufen sind {:?}; freigegeben war nur v1",
        backend.variants_used()
    );
}

#[test]
fn a_contract_that_approves_nothing_dispatches_nothing() {
    let detector = with_approved(
        contract(
            Criticality::Protected,
            QueuePolicy::Latest,
            Some(33),
            33,
            66,
            &[5],
        ),
        &[],
    );
    // Eine leere Freigabeliste ergibt die leere Maske — der Vertrag ist
    // ungueltig und wird beim Start abgelehnt, nicht im Feld entdeckt.
    assert!(detector.validate().is_err());
}

#[test]
fn approval_narrows_the_choice_without_touching_min_quality() {
    // Ohne Zusatz waehlt der Governor frei zwischen drei Varianten.
    let open = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        33,
        66,
        &[5, 8, 12],
    );
    assert!(open.variant_usable(VariantIdx(0)));
    assert!(open.variant_usable(VariantIdx(2)));

    // Mit Zusatz bleibt die Mindestqualitaet unveraendert; nur die Freigabe
    // schraenkt ein.
    let narrowed = with_approved(open.clone(), &[0, 1]);
    assert_eq!(narrowed.min_quality, open.min_quality);
    assert!(
        narrowed.meets_min_quality(VariantIdx(2)),
        "qualitativ genuegt sie"
    );
    assert!(
        !narrowed.variant_usable(VariantIdx(2)),
        "freigegeben ist sie nicht"
    );
}

/// Ein Vertrag mit Weakly-hard-Bedingung.
fn with_miss_budget(mut c: ModelContract, m: u32, k: u32, l: Option<u32>) -> ModelContract {
    c.extension = Some(ContractExtension {
        consumer_period: Some(ms(33)),
        miss_budget: Some(vig_core::contract_ext::MissBudget {
            max_misses: m,
            window_cycles: k,
            max_consecutive: l,
        }),
        ..ContractExtension::default()
    });
    c
}

#[test]
fn a_stream_that_is_supplied_holds_its_miss_budget() {
    let detector = with_miss_budget(
        contract(
            Criticality::Protected,
            QueuePolicy::Latest,
            Some(33),
            33,
            66,
            &[5],
        ),
        2,
        10,
        Some(1),
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
    assert_eq!(
        scheduler.weakly_hard(ModelIdx(0)),
        Some(vig_core::contract_ext::WeaklyHardStatus::Holding)
    );
}

#[test]
fn the_contract_tick_keeps_running_when_nothing_arrives() {
    // Die Abnahme von NV-02: der Vertragstakt kommt aus dem Vertrag. Wer
    // nichts schickt, haelt keinen Vertrag ein — er erzeugt nur keine
    // Requests.
    let detector = with_miss_budget(
        contract(
            Criticality::Protected,
            QueuePolicy::Latest,
            Some(33),
            33,
            66,
            &[5],
        ),
        2,
        10,
        None,
    );
    let mut scheduler = build(vec![detector.clone()], 1);
    let mut backend = Backend::default();
    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 2_000, |t| {
        // Nur in der ersten halben Sekunde kommt etwas; danach Stille.
        if t < 500 && t % 33 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });
    assert!(
        matches!(
            scheduler.weakly_hard(ModelIdx(0)),
            Some(vig_core::contract_ext::WeaklyHardStatus::Violated { .. })
        ),
        "Stille ist keine Vertragserfuellung, sondern eine Versorgungsluecke: {:?}",
        scheduler.weakly_hard(ModelIdx(0))
    );
    assert_eq!(scheduler.metrics().weakly_hard_violated[0], 1);
}

#[test]
fn a_still_fresh_result_supplies_a_quiet_cycle() {
    // `latest_state`: ein Ergebnis von vor 33 ms versorgt bei einem
    // Hoechstalter von 66 ms auch den Zyklus, in dem nichts Neues ankam.
    // Wuerde der Monitor nur den letzten Zeitpunkt kennen, zaehlte er hier
    // Misses, die keine sind.
    let detector = with_miss_budget(
        contract(
            Criticality::Protected,
            QueuePolicy::Latest,
            Some(66),
            66,
            66,
            &[5],
        ),
        0,
        10,
        None,
    );
    let mut scheduler = build(vec![detector.clone()], 1);
    let mut backend = Backend::default();
    let mut next_id = 0_u64;
    // Nur alle 66 ms ein Frame, aber der Verbraucher tastet alle 33 ms ab.
    run(&mut scheduler, &mut backend, 2_000, |t| {
        if t % 66 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });
    assert_eq!(
        scheduler.weakly_hard(ModelIdx(0)),
        Some(vig_core::contract_ext::WeaklyHardStatus::Holding),
        "M=0 haelt nur, wenn ruhige Zyklen aus dem Bestand versorgt zaehlen"
    );
}

// ---------------------------------------------------------------------------
// NV-06 — zustandsabhaengige Prognose
// ---------------------------------------------------------------------------

#[test]
fn the_predictor_starts_in_the_shadow_and_changes_nothing() {
    use vig_core::predictor::Mode;

    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        33,
        66,
        &[5],
    );
    let mut scheduler = build(vec![detector.clone()], 1);
    assert_eq!(scheduler.predictor_mode(), Mode::Shadow);
    // Ohne beobachteten Zustand gibt es gar keine Prognose — die
    // Voreinstellung ist „Takt unbekannt", nicht „Takt voll".
    scheduler.observe_hardware(
        vig_core::predictor::StateClass {
            occupancy: 0,
            throttle: vig_core::predictor::ThrottleClass::Nominal,
            clock: vig_core::predictor::ClockClass::Full,
        },
        1,
    );

    let mut backend = Backend::default();
    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 3_000, |t| {
        if t % 33 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });

    let ledger = scheduler.predictor_ledger();
    assert!(
        ledger.comparisons > 0,
        "der Schatten muss ueberhaupt vergleichen"
    );
    assert!(
        ledger.decisive() > 0,
        "und irgendwann eine belegte Zelle haben: {ledger:?}"
    );
}

#[test]
fn a_hardware_state_change_invalidates_the_learned_cells() {
    use vig_core::predictor::{ClockClass, StateClass, ThrottleClass};

    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        33,
        66,
        &[5],
    );
    let mut scheduler = build(vec![detector.clone()], 1);
    scheduler.observe_hardware(
        StateClass {
            occupancy: 0,
            throttle: ThrottleClass::Nominal,
            clock: ClockClass::Full,
        },
        1,
    );

    let mut backend = Backend::default();
    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 3_000, |t| {
        if t % 33 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });
    let before = scheduler.predictor_ledger().decisive();
    assert!(before > 0);

    // Die Karte laeuft in eine Drosselung: die gelernten Zellen gelten nicht
    // mehr, und der naechste Vergleich faellt zurueck.
    scheduler.observe_hardware(
        StateClass {
            occupancy: 0,
            throttle: ThrottleClass::Limited,
            clock: ClockClass::Reduced,
        },
        1,
    );
    let fallbacks_before = scheduler.predictor_ledger().fallbacks;
    run(&mut scheduler, &mut backend, 200, |t| {
        if t % 33 == 0 {
            next_id = next_id.saturating_add(1);
            vec![frame(next_id, 0, t, &detector)]
        } else {
            Vec::new()
        }
    });
    assert!(
        scheduler.predictor_ledger().fallbacks > fallbacks_before,
        "nach dem Zustandswechsel gibt es zunaechst keine belegte Zelle mehr"
    );
}

// ---------------------------------------------------------------------------
// NV-24 — das Missbudget in Entscheidungen einbeziehen
// ---------------------------------------------------------------------------

#[test]
fn the_miss_aware_policy_is_off_by_default() {
    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        33,
        66,
        &[5],
    );
    let scheduler = build(vec![detector], 1);
    assert!(
        !scheduler.miss_aware_policy(),
        "eine empirische Policy schaltet sich nicht selbst scharf"
    );
}

#[test]
fn an_exhausted_budget_wins_inside_its_criticality_class() {
    use vig_core::contract_ext::{CycleOutcome, MissBudget};

    // Zwei gleichwertige Stroeme, gleiche Klasse, gleiche Deadline. Einer hat
    // sein Missbudget aufgebraucht, der andere nicht. Mit Policy geht der
    // erschoepfte vor.
    let with_budget = |m: u32| {
        let mut c = contract(
            Criticality::Protected,
            QueuePolicy::Fifo,
            Some(33),
            100,
            200,
            &[10],
        );
        c.extension = Some(ContractExtension {
            consumer_period: Some(ms(33)),
            miss_budget: Some(MissBudget {
                max_misses: m,
                window_cycles: 10,
                max_consecutive: None,
            }),
            ..ContractExtension::default()
        });
        c
    };
    let tight = with_budget(0);
    let loose = with_budget(5);

    // Ein dritter, langer Auftrag belegt den einen Slot, damit beide
    // Kandidaten gleichzeitig in der Queue stehen, wenn er frei wird. Ohne
    // ihn entscheidet die Ankunftsreihenfolge, weil der Scheduler bei jedem
    // Ereignis sofort plant — und dann prueft der Test nichts.
    let blocker = contract(
        Criticality::Protected,
        QueuePolicy::Fifo,
        None,
        400,
        800,
        &[40],
    );
    let mut scheduler = build(vec![tight.clone(), loose.clone(), blocker.clone()], 1);
    scheduler.set_miss_aware_policy(true);

    // Modell 0 hat kein Budget: sein naechster Zyklus ist immer Pflicht.
    assert!(scheduler.next_cycle_is_mandatory(ModelIdx(0)));
    assert!(!scheduler.next_cycle_is_mandatory(ModelIdx(1)));

    let mut backend = Backend::default();
    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 120, |t| {
        next_id = next_id.saturating_add(1);
        match t {
            0 => vec![frame(next_id, 2, t, &blocker)],
            // Waehrend der Slot belegt ist, in umgekehrter Reihenfolge: der
            // Strom mit Spielraum kommt zuerst an.
            5 => vec![frame(next_id, 1, t, &loose)],
            6 => vec![frame(next_id, 0, t, &tight)],
            _ => Vec::new(),
        }
    });

    let after_blocker: Vec<ModelIdx> = backend
        .dispatched_models()
        .iter()
        .copied()
        .filter(|m| *m != ModelIdx(2))
        .collect();
    assert_eq!(
        after_blocker.first(),
        Some(&ModelIdx(0)),
        "der Strom mit erschoepftem Budget muss zuerst laufen, obwohl er \
         spaeter ankam: {:?}",
        backend.dispatched_models()
    );
    let _ = CycleOutcome::Supplied;
}

#[test]
fn without_the_policy_the_arrival_order_decides_as_before() {
    use vig_core::contract_ext::MissBudget;

    let with_budget = |m: u32| {
        let mut c = contract(
            Criticality::Protected,
            QueuePolicy::Fifo,
            Some(33),
            100,
            200,
            &[10],
        );
        c.extension = Some(ContractExtension {
            consumer_period: Some(ms(33)),
            miss_budget: Some(MissBudget {
                max_misses: m,
                window_cycles: 10,
                max_consecutive: None,
            }),
            ..ContractExtension::default()
        });
        c
    };
    let tight = with_budget(0);
    let loose = with_budget(5);
    let blocker = contract(
        Criticality::Protected,
        QueuePolicy::Fifo,
        None,
        400,
        800,
        &[40],
    );
    // Policy aus — die Voreinstellung.
    let mut scheduler = build(vec![tight.clone(), loose.clone(), blocker.clone()], 1);

    let mut backend = Backend::default();
    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 120, |t| {
        next_id = next_id.saturating_add(1);
        match t {
            0 => vec![frame(next_id, 2, t, &blocker)],
            5 => vec![frame(next_id, 1, t, &loose)],
            6 => vec![frame(next_id, 0, t, &tight)],
            _ => Vec::new(),
        }
    });

    let after_blocker: Vec<ModelIdx> = backend
        .dispatched_models()
        .iter()
        .copied()
        .filter(|m| *m != ModelIdx(2))
        .collect();
    assert_eq!(
        after_blocker.first(),
        Some(&ModelIdx(1)),
        "ohne Policy gilt die Generationszeit wie bisher: {:?}",
        backend.dispatched_models()
    );
}

#[test]
fn the_policy_never_reorders_across_criticality_classes() {
    use vig_core::contract_ext::MissBudget;

    // Ein best-effort-Strom mit erschoepftem Budget darf **nicht** vor einen
    // protected-Strom mit Spielraum. Der Vorrang zwischen Klassen ist die
    // Betreiberpolicy und bleibt es.
    let mut starving = contract(
        Criticality::BestEffort,
        QueuePolicy::Fifo,
        Some(33),
        100,
        200,
        &[10],
    );
    starving.extension = Some(ContractExtension {
        consumer_period: Some(ms(33)),
        miss_budget: Some(MissBudget {
            max_misses: 0,
            window_cycles: 10,
            max_consecutive: None,
        }),
        ..ContractExtension::default()
    });
    let protected = contract(
        Criticality::Protected,
        QueuePolicy::Fifo,
        Some(33),
        100,
        200,
        &[10],
    );

    let blocker = contract(
        Criticality::Protected,
        QueuePolicy::Fifo,
        None,
        400,
        800,
        &[40],
    );
    let mut scheduler = build(
        vec![starving.clone(), protected.clone(), blocker.clone()],
        1,
    );
    scheduler.set_miss_aware_policy(true);
    assert!(scheduler.next_cycle_is_mandatory(ModelIdx(0)));

    let mut backend = Backend::default();
    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 120, |t| {
        next_id = next_id.saturating_add(1);
        match t {
            0 => vec![frame(next_id, 2, t, &blocker)],
            // Der ausgehungerte best-effort-Strom kommt zuerst an.
            5 => vec![frame(next_id, 0, t, &starving)],
            6 => vec![frame(next_id, 1, t, &protected)],
            _ => Vec::new(),
        }
    });

    let after_blocker: Vec<ModelIdx> = backend
        .dispatched_models()
        .iter()
        .copied()
        .filter(|m| *m != ModelIdx(2))
        .collect();
    assert_eq!(
        after_blocker.first(),
        Some(&ModelIdx(1)),
        "protected geht vor, auch wenn best_effort ausgehungert ist und \
         frueher ankam: {:?}",
        backend.dispatched_models()
    );
}

#[test]
fn the_slack_is_reported_per_model() {
    use vig_core::contract_ext::MissBudget;

    let mut detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        33,
        66,
        &[5],
    );
    detector.extension = Some(ContractExtension {
        consumer_period: Some(ms(33)),
        miss_budget: Some(MissBudget {
            max_misses: 3,
            window_cycles: 10,
            max_consecutive: None,
        }),
        ..ContractExtension::default()
    });
    let mut scheduler = build(vec![detector.clone()], 1);
    let slack = scheduler.budget_slack(ModelIdx(0)).unwrap();
    assert_eq!(slack.misses_left, 3);
    assert!(!slack.observed);

    // Nichts liefern: das Budget wird aufgebraucht.
    let mut backend = Backend::default();
    run(&mut scheduler, &mut backend, 1_000, |_| Vec::new());
    assert_eq!(
        scheduler.budget_slack(ModelIdx(0)).unwrap().misses_left,
        0,
        "Schweigen kostet Budget"
    );
    assert_eq!(scheduler.metrics().weakly_hard_misses_left[0], 0);
}

// ---------------------------------------------------------------------------
// NV-18 — Anwendungshinweise innerhalb freigegebener Grenzen
// ---------------------------------------------------------------------------

#[test]
fn without_a_policy_the_governor_hears_nobody() {
    use vig_core::hints::{Authority, Hint, HintKind, Rejection};

    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        33,
        66,
        &[5],
    );
    let mut scheduler = build(vec![detector], 1);
    let hint = Hint {
        model: ModelIdx(0),
        authority: Authority(1),
        kind: HintKind::Elevated { max_age: ms(20) },
        issued_at: at(0),
        ttl: ms(100),
    };
    assert_eq!(
        scheduler.offer_hint(hint, at(0)),
        Err(Rejection::NoAuthorityConfigured),
        "die Voreinstellung ist geschlossen"
    );
    assert_eq!(
        scheduler.effective_max_age(ModelIdx(0), at(0)),
        Some(ms(66)),
        "der Grundvertrag bleibt unberuehrt"
    );
}

#[test]
fn an_authorised_hint_tightens_the_effective_max_age() {
    use vig_core::hints::{Authority, Hint, HintKind, HintPolicy};

    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        33,
        66,
        &[5],
    );
    let mut scheduler = build(vec![detector], 1);
    scheduler.set_hint_policy(HintPolicy {
        authority: Some(Authority(7)),
        allow_loosening: false,
        approved_modes: 0,
        min_max_age: Some(ms(10)),
        max_action_horizon: None,
    });

    let hint = Hint {
        model: ModelIdx(0),
        authority: Authority(7),
        kind: HintKind::Elevated { max_age: ms(20) },
        issued_at: at(0),
        ttl: ms(100),
    };
    assert!(scheduler.offer_hint(hint, at(0)).is_ok());
    assert_eq!(
        scheduler.effective_max_age(ModelIdx(0), at(0)),
        Some(ms(20))
    );
    assert_eq!(
        scheduler.effective_max_age(ModelIdx(0), at(101)),
        Some(ms(66)),
        "nach der Frist gilt wieder der Vertrag, nicht der letzte Zustand"
    );
}

#[test]
fn a_hint_cannot_loosen_a_contract_the_operator_did_not_open() {
    use vig_core::hints::{Authority, Hint, HintKind, HintPolicy, Rejection};

    let detector = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        Some(33),
        33,
        66,
        &[5],
    );
    let mut scheduler = build(vec![detector], 1);
    scheduler.set_hint_policy(HintPolicy {
        authority: Some(Authority(7)),
        allow_loosening: false,
        approved_modes: 0,
        min_max_age: None,
        max_action_horizon: None,
    });

    let hint = Hint {
        model: ModelIdx(0),
        authority: Authority(7),
        kind: HintKind::ActionHorizon { holds_for: ms(500) },
        issued_at: at(0),
        ttl: ms(1_000),
    };
    assert_eq!(
        scheduler.offer_hint(hint, at(0)),
        Err(Rejection::WouldLoosen)
    );
    assert_eq!(
        scheduler.effective_max_age(ModelIdx(0), at(0)),
        Some(ms(66))
    );
}

#[test]
fn a_hint_does_not_change_what_the_scheduler_dispatches_by_itself() {
    use vig_core::hints::{Authority, Hint, HintKind, HintPolicy};

    // Ein Hinweis ist eine Aussage ueber Frische, kein Vorrang. Zwei gleiche
    // Stroeme bleiben gleich, auch wenn einer einen Hinweis traegt — der
    // Vorrang zwischen Stroemen ist die Betreiberpolicy (dasselbe Argument
    // wie bei NV-24).
    let a = contract(
        Criticality::Protected,
        QueuePolicy::Fifo,
        Some(33),
        100,
        200,
        &[10],
    );
    let b = a.clone();
    let blocker = contract(
        Criticality::Protected,
        QueuePolicy::Fifo,
        None,
        400,
        800,
        &[40],
    );
    let mut scheduler = build(vec![a.clone(), b.clone(), blocker.clone()], 1);
    scheduler.set_hint_policy(HintPolicy {
        authority: Some(Authority(7)),
        allow_loosening: false,
        approved_modes: 0,
        min_max_age: Some(ms(10)),
        max_action_horizon: None,
    });
    scheduler
        .offer_hint(
            Hint {
                model: ModelIdx(1),
                authority: Authority(7),
                kind: HintKind::Elevated { max_age: ms(20) },
                issued_at: at(0),
                ttl: ms(10_000),
            },
            at(0),
        )
        .unwrap();

    let mut backend = Backend::default();
    let mut next_id = 0_u64;
    run(&mut scheduler, &mut backend, 120, |t| {
        next_id = next_id.saturating_add(1);
        match t {
            0 => vec![frame(next_id, 2, t, &blocker)],
            5 => vec![frame(next_id, 0, t, &a)],
            6 => vec![frame(next_id, 1, t, &b)],
            _ => Vec::new(),
        }
    });
    let after: Vec<ModelIdx> = backend
        .dispatched_models()
        .iter()
        .copied()
        .filter(|m| *m != ModelIdx(2))
        .collect();
    assert_eq!(
        after.first(),
        Some(&ModelIdx(0)),
        "der frueher angekommene geht vor; der Hinweis aendert daran nichts: {:?}",
        backend.dispatched_models()
    );
}

// ---------------------------------------------------------------------------
// Ein Hinweis muss eine Entscheidung aendern (Review R09)
// ---------------------------------------------------------------------------

const HINT_AUTHORITY: vig_core::hints::Authority = vig_core::hints::Authority(7);

/// Ein Vertrag, dessen Laufzeit sein Hoechstalter sicher reisst.
///
/// 50 ms Laufzeit bei 10 ms Hoechstalter: jeder Request ist bei der
/// Fertigstellung veraltet und wird verworfen — solange kein Hinweis etwas
/// anderes sagt.
fn doomed_contract() -> ModelContract {
    contract(
        Criticality::BestEffort,
        QueuePolicy::Latest,
        None,
        1_000,
        10,
        &[50],
    )
}

/// Ein freigegebener Aktionshorizont rettet einen Request, der sonst als
/// veraltet verworfen wuerde.
///
/// Der Befund aus R09: `offer_hint`, `set_hint_policy` und
/// `effective_max_age` waren gebaut und getestet — und keine
/// Schedulingentscheidung hing daran. „Modul fertig" und „wirksam" sind
/// verschiedene Aussagen, und dieser Test prueft die zweite.
///
/// Der Aktionshorizont sagt: bis der laufende Vorgang abgeschlossen ist,
/// aendert eine neue Wahrnehmung nichts mehr. Er **lockert** und braucht
/// deshalb `allow_loosening` (ADR-0029).
#[test]
fn an_approved_action_horizon_saves_a_request_that_would_be_dropped() {
    use vig_core::hints::{Hint, HintKind, HintPolicy};

    let c = doomed_contract();
    let mut without = build(vec![c.clone()], 1);
    let mut actions = Vec::new();
    without.on_event(at(0), Event::Arrival(frame(1, 0, 0, &c)), &mut |a| {
        actions.push(a);
    });
    assert!(
        !actions.iter().any(|a| matches!(a, Action::Dispatch { .. })),
        "ohne Hinweis ist der Request bei der Fertigstellung wertlos"
    );

    let mut with = build(vec![c.clone()], 1);
    with.set_hint_policy(HintPolicy {
        authority: Some(HINT_AUTHORITY),
        allow_loosening: true,
        approved_modes: 0,
        min_max_age: None,
        max_action_horizon: Some(ms(500)),
    });
    with.offer_hint(
        Hint {
            model: ModelIdx(0),
            authority: HINT_AUTHORITY,
            kind: HintKind::ActionHorizon { holds_for: ms(200) },
            issued_at: at(0),
            ttl: ms(200),
        },
        at(0),
    )
    .unwrap();

    let mut actions = Vec::new();
    with.on_event(at(0), Event::Arrival(frame(1, 0, 0, &c)), &mut |a| {
        actions.push(a);
    });
    assert!(
        actions.iter().any(|a| matches!(a, Action::Dispatch { .. })),
        "mit dem Hinweis reicht die Frische — er aendert eine Entscheidung \
         und nicht nur einen Zaehler: {actions:?}"
    );
}

/// Ohne Freigabe des Betreibers bleibt es beim Vertrag.
///
/// Die Gegenprobe. Ein Hinweis, der ohne `allow_loosening` wirkte, waere ein
/// Hebel, mit dem sich jede Anwendung ihren eigenen Vertrag schreibt.
#[test]
fn without_the_operators_approval_a_loosening_hint_changes_nothing() {
    use vig_core::hints::{Hint, HintKind, HintPolicy};

    let c = doomed_contract();
    let mut s = build(vec![c.clone()], 1);
    s.set_hint_policy(HintPolicy {
        authority: Some(HINT_AUTHORITY),
        allow_loosening: false,
        approved_modes: 0,
        min_max_age: None,
        max_action_horizon: Some(ms(500)),
    });
    assert!(
        s.offer_hint(
            Hint {
                model: ModelIdx(0),
                authority: HINT_AUTHORITY,
                kind: HintKind::ActionHorizon { holds_for: ms(200) },
                issued_at: at(0),
                ttl: ms(200),
            },
            at(0),
        )
        .is_err(),
        "lockern braucht eine Freigabe"
    );

    let mut actions = Vec::new();
    s.on_event(at(0), Event::Arrival(frame(1, 0, 0, &c)), &mut |a| {
        actions.push(a);
    });
    assert!(
        !actions.iter().any(|a| matches!(a, Action::Dispatch { .. })),
        "und ohne sie bleibt es beim Vertrag"
    );
}

/// Eine Beobachtung landet in der Zelle des Zustands beim **Dispatch**
/// (Review R07).
///
/// Zwischen Start und Ende kann die Karte heruntergetaktet, gedrosselt oder in
/// einen anderen Leistungsmodus gegangen sein. Wird die Laufzeit dem Zustand
/// bei der Fertigstellung zugeschrieben, fuellt sie eine Zelle, in der nie
/// etwas gelaufen ist — und die Prognose liest sie spaeter als Beleg.
///
/// Der Test faehrt genau diesen Wechsel: Dispatch unter vollem Takt,
/// Fertigstellung unter gedrosseltem.
#[test]
fn an_observation_lands_in_the_cell_of_the_dispatch_state() {
    use vig_core::predictor::{ClockClass, StateClass, ThrottleClass};

    let full = StateClass {
        occupancy: 0,
        clock: ClockClass::Full,
        throttle: ThrottleClass::Nominal,
    };
    let throttled = StateClass {
        occupancy: 0,
        clock: ClockClass::Reduced,
        throttle: ThrottleClass::Limited,
    };

    let c = contract(
        Criticality::Protected,
        QueuePolicy::Fifo,
        None,
        1_000,
        1_000,
        &[10],
    );
    let mut s = build(vec![c.clone()], 1);
    s.observe_hardware(full, 1);

    let mut actions = Vec::new();
    s.on_event(at(0), Event::Arrival(frame(1, 0, 0, &c)), &mut |a| {
        actions.push(a);
    });
    let Some(slot) = actions.iter().find_map(|a| match a {
        Action::Dispatch { slot, .. } => Some(*slot),
        _ => None,
    }) else {
        panic!("der Request wurde gestartet: {actions:?}");
    };

    // Zwischen Start und Ende drosselt die Karte.
    s.observe_hardware(throttled, 1);
    s.on_event(
        at(10),
        Event::Completion {
            request: RequestId(1),
            slot,
        },
        &mut |_| {},
    );

    assert_eq!(
        s.predictor()
            .observations(ModelIdx(0), VariantIdx(0), full, 1),
        1,
        "die Laufzeit gehoert in die Zelle des Zustands beim Dispatch"
    );
    assert_eq!(
        s.predictor()
            .observations(ModelIdx(0), VariantIdx(0), throttled, 1),
        0,
        "und nicht in die des Zustands bei der Fertigstellung"
    );
}

// ---------------------------------------------------------------------------
// Die gemessene Interferenz aendert eine Entscheidung (NV-11)
// ---------------------------------------------------------------------------

/// Eine gemessene Interferenz schlaegt bis in die Planung durch.
///
/// Der Befund aus dem Review: die Tabelle war gebaut, getestet und an nichts
/// angeschlossen — `vig calibrate` mass beide Richtungen, berichtete sie und
/// **warf sie weg**. Der Belegungsgrad blieb die einzige Naeherung, und der
/// weiss nicht, **wer** danebenlaeuft.
///
/// Geprueft wird die gemeldete Laufzeit: sie ist die Zahl, mit der
/// Look-ahead, Slotbelegung und Machbarkeit rechnen.
#[test]
fn measured_interference_reaches_the_planned_runtime() {
    use vig_core::interference::{ConflictKind, Interference};

    let slow = contract(
        Criticality::BestEffort,
        QueuePolicy::Fifo,
        None,
        10_000,
        10_000,
        &[200],
    );
    let fast = contract(
        Criticality::Protected,
        QueuePolicy::Fifo,
        None,
        10_000,
        10_000,
        &[10],
    );

    let planned = |table: Option<Interference>| {
        let mut s = build(vec![slow.clone(), fast.clone()], 2);
        if let Some(table) = table {
            s.set_interference(table);
        }
        // Das langsame Modell belegt einen Slot.
        let mut actions = Vec::new();
        s.on_event(at(0), Event::Arrival(frame(1, 0, 0, &slow)), &mut |a| {
            actions.push(a);
        });
        // Und jetzt der schnelle Kandidat daneben.
        let mut actions = Vec::new();
        s.on_event(at(1), Event::Arrival(frame(2, 1, 1, &fast)), &mut |a| {
            actions.push(a);
        });
        actions.iter().find_map(|a| match a {
            Action::Dispatch {
                predicted_runtime, ..
            } => Some(*predicted_runtime),
            _ => None,
        })
    };

    let Some(without) = planned(None) else {
        panic!("ohne Tabelle laeuft er");
    };

    let mut table = Interference::new();
    // Das schnelle Modell leidet unter dem langsamen: 40 ms obendrauf.
    table.record_pair(
        ModelIdx(1),
        ModelIdx(0),
        ms(40),
        ConflictKind::MemoryBandwidth,
    );
    let Some(with) = planned(Some(table)) else {
        panic!("mit Tabelle laeuft er auch");
    };

    assert_eq!(
        with.as_nanos(),
        without.as_nanos().saturating_add(ms(40).as_nanos()),
        "die gemessenen 40 ms stehen in der Zahl, mit der geplant wird"
    );
}

/// Eine ungemessene Paarung bekommt keinen erfundenen Aufschlag.
///
/// Die Gegenprobe. Eine Tabelle, die fuer unbekannte Paare etwas annimmt,
/// waere schlimmer als keine: der Belegungsgrad ist die dokumentierte
/// Naeherung (ADR-0006), und eine erfundene Zahl daneben waere eine zweite,
/// unbelegte.
#[test]
fn an_unmeasured_pairing_gets_no_invented_surcharge() {
    use vig_core::interference::{ConflictKind, Interference};

    let slow = contract(
        Criticality::BestEffort,
        QueuePolicy::Fifo,
        None,
        10_000,
        10_000,
        &[200],
    );
    let fast = contract(
        Criticality::Protected,
        QueuePolicy::Fifo,
        None,
        50,
        1_000,
        &[10],
    );

    let mut table = Interference::new();
    // Gemessen ist die **andere** Richtung.
    table.record_pair(ModelIdx(0), ModelIdx(1), ms(40), ConflictKind::Compute);

    let mut s = build(vec![slow.clone(), fast.clone()], 2);
    s.set_interference(table);
    let mut actions = Vec::new();
    s.on_event(at(0), Event::Arrival(frame(1, 0, 0, &slow)), &mut |a| {
        actions.push(a);
    });
    let mut actions = Vec::new();
    s.on_event(at(1), Event::Arrival(frame(2, 1, 1, &fast)), &mut |a| {
        actions.push(a);
    });
    assert!(
        actions.iter().any(|a| matches!(a, Action::Dispatch { .. })),
        "die Gegenrichtung ist nicht gemessen und wird nicht geraten"
    );
}
