//! Die Messung einer Zusage im Governor (ADR-0047, Schritt 2).
//!
//! Vier Aussagen: Gebucht wird nur ein Ergebnis, das beim Verbraucher noch
//! trug. Anteil und Luecke stehen danach im Metrikabzug. Ohne Zusage bleibt
//! alles null. Und der Zaehler entsteht beim ersten Ereignis — ein Strom, der
//! noch nie dran war, ist nicht im Rueckstand.
//!
//! Gesteuert wird hier noch nichts: dieser Schritt misst nur.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use vig_core::arrayvec::ArrayVec;
use vig_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use vig_core::objective::Objective;
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use vig_core::queue::QueueConfig;
use vig_core::request::OverflowPolicy;
use vig_core::scheduler::{Action, Event, Scheduler, SchedulerError};
use vig_core::slots::SlotSet;
use vig_core::time::Slack;
use vig_core::{
    Criticality, Duration, Instant, ModelIdx, PayloadRef, QueuePolicy, RequestDescriptor,
    RequestId, SlotIdx, SupersessionKey,
};

fn ms(v: u64) -> Duration {
    Duration::from_millis(v).unwrap()
}

fn at(v: u64) -> Instant {
    Instant::ZERO.checked_add(ms(v)).unwrap()
}

fn contract(period_ms: u64, max_age_ms: u64, runtime_ms: u64) -> ModelContract {
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
        criticality: Criticality::Protected,
        queue: QueueConfig {
            policy: QueuePolicy::Latest,
            capacity: 1,
            overflow: OverflowPolicy::RejectNew,
        },
        period: Some(ms(period_ms)),
        deadline: ms(max_age_ms),
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

fn with_objective(
    mut c: ModelContract,
    permille: u16,
    window_ms: u64,
    gap_ms: Option<u64>,
) -> ModelContract {
    c.objective = Some(Objective {
        coverage_permille: permille,
        window: ms(window_ms),
        max_gap: gap_ms.map(ms),
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

/// Ein Backend, das jeden Auftrag nach seiner geplanten Laufzeit fertigmeldet
/// und mitschreibt, welches Modell wann gestartet ist.
///
/// Die Reihenfolge ist fuer die Steuerung die eigentliche Aussage: dass etwas
/// lief, sagt nichts — wer zuerst drankam, sagt alles.
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

    /// Wie oft dieses Modell gestartet wurde.
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

/// Eine Kamera mit 33 ms Takt, 100 ms Hoechstalter, 10 ms Laufzeit und der
/// Zusage „98 % der Zyklen, nie laenger als eine Sekunde nichts".
#[test]
fn what_arrived_fresh_is_counted_and_visible() {
    let camera = with_objective(contract(33, 100, 10), 980, 10_000, Some(1_000));
    let mut scheduler = build(std::slice::from_ref(&camera), 1).unwrap();
    let mut backend = Backend::default();

    run(&mut scheduler, &mut backend, 1_000, |t| {
        if t % 33 == 0 {
            vec![frame(t, 0, t, &camera)]
        } else {
            Vec::new()
        }
    });

    let metrics = scheduler.metrics();
    // Ein Slot, 10 ms je Bild, 33 ms Takt: das haelt die Zusage muehelos.
    assert!(
        metrics.objective_coverage_permille[0] >= 900,
        "Anteil {} ‰",
        metrics.objective_coverage_permille[0]
    );
    // Die Luecke ist hoechstens ein Takt plus Laufzeit.
    assert!(
        metrics.objective_gap_us[0] <= 50_000,
        "Luecke {} us",
        metrics.objective_gap_us[0]
    );
    // Und die Zusage haelt: Luft ja, Rueckstand nein.
    assert!(metrics.objective_slack_us[0] > 0);
    assert_eq!(metrics.objective_deficit_us[0], 0);
}

/// Ohne Zusage misst der Governor nichts und aendert nichts.
#[test]
fn without_an_objective_everything_stays_zero() {
    let camera = contract(33, 100, 10);
    let mut scheduler = build(std::slice::from_ref(&camera), 1).unwrap();
    let mut backend = Backend::default();

    run(&mut scheduler, &mut backend, 500, |t| {
        if t % 33 == 0 {
            vec![frame(t, 0, t, &camera)]
        } else {
            Vec::new()
        }
    });

    let metrics = scheduler.metrics();
    assert_eq!(metrics.objective_coverage_permille[0], 0);
    assert_eq!(metrics.objective_gap_us[0], 0);
    assert_eq!(metrics.objective_slack_us[0], 0);
    assert_eq!(metrics.objective_deficit_us[0], 0);
    assert_eq!(scheduler.objective_slack(ModelIdx(0), at(500)), None);
}

/// Ein Strom, der noch nie dran war, ist nicht im Rueckstand: der Zaehler
/// entsteht beim ersten Ereignis, nicht beim Bau.
#[test]
fn a_stream_that_never_ran_is_not_behind() {
    let camera = with_objective(contract(33, 100, 10), 980, 10_000, Some(1_000));
    let scheduler = build(&[camera], 1).unwrap();
    assert_eq!(scheduler.objective_slack(ModelIdx(0), at(5_000)), None);
    assert_eq!(scheduler.metrics().objective_deficit_us[0], 0);
}

/// Bleibt der Nachschub aus, faellt die Zusage — und der Rueckstand wird
/// sichtbar, ohne dass jemand etwas buchen muesste.
#[test]
fn silence_turns_slack_into_deficit() {
    let camera = with_objective(contract(33, 100, 10), 980, 1_000, Some(200));
    let mut scheduler = build(std::slice::from_ref(&camera), 1).unwrap();
    let mut backend = Backend::default();

    // Eine halbe Sekunde Betrieb, dann nichts mehr.
    run(&mut scheduler, &mut backend, 500, |t| {
        if t < 200 && t % 33 == 0 {
            vec![frame(t, 0, t, &camera)]
        } else {
            Vec::new()
        }
    });

    let slack = scheduler.objective_slack(ModelIdx(0), at(500)).unwrap();
    assert!(
        slack < Slack::ZERO,
        "nach 300 ms Stille bei 200 ms Luecke: {slack}"
    );
    assert!(scheduler.metrics().objective_deficit_us[0] > 0);
    assert_eq!(scheduler.metrics().objective_slack_us[0], 0);
}

/// Ein Ergebnis, das bei der Auslieferung schon zu alt war, hat nicht
/// versorgt — und wird deshalb auch nicht gebucht.
#[test]
fn a_result_that_no_longer_carried_is_not_counted() {
    // 10 ms Hoechstalter, aber 40 ms Laufzeit: was ankommt, ist immer zu alt.
    let camera = with_objective(contract(33, 10, 40), 500, 1_000, None);
    let mut scheduler = build(std::slice::from_ref(&camera), 1).unwrap();
    let mut backend = Backend::default();

    run(&mut scheduler, &mut backend, 500, |t| {
        if t % 33 == 0 {
            vec![frame(t, 0, t, &camera)]
        } else {
            Vec::new()
        }
    });

    assert_eq!(
        scheduler.metrics().objective_coverage_permille[0],
        0,
        "ein Ergebnis, das nie trug, darf keine Zusage erfuellen"
    );
}

// ---------------------------------------------------------------------------
// ADR-0047, Schritt 4 — die Steuerung
// ---------------------------------------------------------------------------

/// Ein nachrangiger Strom mit frei waehlbarer Zusage.
fn lower(period_ms: u64, max_age_ms: u64, runtime_ms: u64) -> ModelContract {
    let mut c = contract(period_ms, max_age_ms, runtime_ms);
    c.criticality = Criticality::Normal;
    c
}

/// Faehrt zwei gleichrangige Stroeme auf einem Slot und gibt zurueck, wie oft
/// jeder gestartet ist. Modell 1 traegt die Zusage, falls eine vereinbart ist.
///
/// Modell 1 bewusst: bei Gleichstand entscheidet zuletzt der Modellindex, und
/// Modell 0 gewaenne. Was Modell 1 darueber hinaus bekommt, kann nur von der
/// Zusage kommen.
fn two_streams(objective: Option<(u16, u64, Option<u64>)>) -> (usize, usize) {
    let plain = lower(50, 200, 30);
    let second = match objective {
        None => lower(50, 200, 30),
        Some((permille, window_ms, gap_ms)) => {
            with_objective(lower(50, 200, 30), permille, window_ms, gap_ms)
        }
    };
    let mut scheduler = build(&[plain.clone(), second.clone()], 1).unwrap();
    let mut backend = Backend::default();

    run(&mut scheduler, &mut backend, 600, |t| {
        if t % 50 == 0 {
            vec![frame(t, 0, t, &plain), frame(1_000 + t, 1, t, &second)]
        } else {
            Vec::new()
        }
    });
    (backend.count(0), backend.count(1))
}

/// Derselbe Strom mit zwei Varianten: volle Qualitaet langsam, halbe schnell.
fn two_variants(slow_ms: u64, fast_ms: u64) -> ModelContract {
    let mut c = lower(50, 200, slow_ms);
    c.variants
        .push(Variant {
            quality: QualityValue {
                value: Quality::from_milli(500).unwrap(),
                source: QualitySource::Measured,
            },
            profile: VariantProfile::solo(RuntimeProfile::exact(ms(fast_ms))),
            semantics: vig_core::semantics::VariantSemantics::default(),
            preprocess: Duration::ZERO,
        })
        .unwrap();
    c
}

/// Faehrt den Strom mit Zusage, wahlweise neben einem geschuetzten Stoerer,
/// und gibt zurueck, wie oft jede Variante gewaehlt wurde.
fn variant_choice(with_disturber: bool) -> (u64, u64) {
    let promised = with_objective(two_variants(45, 10), 900, 2_000, Some(100));
    let disturber = contract(50, 100, 40);
    let contracts: Vec<ModelContract> = if with_disturber {
        vec![promised.clone(), disturber.clone()]
    } else {
        vec![promised.clone()]
    };
    let mut scheduler = build(&contracts, 1).unwrap();
    let mut backend = Backend::default();

    run(&mut scheduler, &mut backend, 800, |t| {
        if t % 50 != 0 {
            return Vec::new();
        }
        let mut frames = vec![frame(t, 0, t, &promised)];
        if with_disturber {
            frames.push(frame(1_000 + t, 1, t, &disturber));
        }
        frames
    });

    let metrics = scheduler.metrics();
    (metrics.variant_selected[0], metrics.variant_selected[1])
}

/// Randfall 7: Wer hinter seiner Zusage liegt, senkt zuerst die eigene
/// Qualitaet — statt sofort einen Nachbarn zu verdraengen.
///
/// Auch hier erzeugt der Test seine eigene Referenz: allein haelt der Strom
/// seine Zusage und bleibt auf der besseren Variante; neben einem
/// geschuetzten Stoerer geraet er in Rueckstand und weicht aus.
#[test]
fn a_stream_behind_its_promise_lowers_its_own_quality_first() {
    let (alone_full, alone_fast) = variant_choice(false);
    let (busy_full, busy_fast) = variant_choice(true);

    assert_eq!(
        alone_fast, 0,
        "allein haelt der Strom seine Zusage und braucht keine Abwertung \
         (voll {alone_full}, schnell {alone_fast})"
    );
    assert!(
        busy_fast > 0,
        "im Rueckstand muss die schnellere Variante zum Zug kommen \
         (voll {busy_full}, schnell {busy_fast})"
    );
}

/// Eine Zusage verschiebt die Bedienung zum versprochenen Strom.
///
/// Der Test erzeugt seine eigene Referenz: derselbe Aufbau einmal ohne und
/// einmal mit Zusage. Nur der Vergleich gegen den Referenzlauf trennt die
/// Wirkung der Zusage von der Asymmetrie, die zwei gleich getaktete Stroeme
/// auf einem Slot ohnehin haben — ohne ihn waere jede Zahl wertlos.
#[test]
fn an_objective_shifts_service_towards_the_promised_stream() {
    let (plain_a, plain_b) = two_streams(None);
    let (with_a, with_b) = two_streams(Some((900, 2_000, Some(100))));

    assert!(
        with_b > plain_b,
        "mit Zusage {with_b} Starts gegen {plain_b} ohne — die Zusage muss wirken \
         (Referenz {plain_a}:{plain_b}, mit Zusage {with_a}:{with_b})"
    );
}

/// Und ohne Zusage aendert sich nichts: derselbe Aufbau, zweimal gefahren,
/// ergibt dieselbe Verteilung. Das sichert die Referenz selbst ab.
#[test]
fn without_objectives_the_run_is_reproducible() {
    assert_eq!(two_streams(None), two_streams(None));
}

/// Absicherung gegen einen Test, der nichts prueft: die Last muss ueberhaupt
/// gueltige Ergebnisse erzeugen, sonst waeren alle Aussagen oben wertlos.
///
/// Gezaehlt wird am Metrikabzug, nicht an der Form der Aktionen — die Frage
/// lautet „kam etwas Gueltiges an", nicht „wie heisst die Variante gerade".
#[test]
fn the_test_load_actually_produces_valid_results() {
    let camera = with_objective(contract(33, 100, 10), 980, 10_000, None);
    let mut scheduler = build(std::slice::from_ref(&camera), 1).unwrap();
    let mut backend = Backend::default();

    run(&mut scheduler, &mut backend, 300, |t| {
        if t % 33 == 0 {
            vec![frame(t, 0, t, &camera)]
        } else {
            Vec::new()
        }
    });

    let valid = scheduler.metrics().completed_valid;
    assert!(valid >= 8, "nur {valid} gueltige Ergebnisse");
}
