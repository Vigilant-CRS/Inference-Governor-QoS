//! Mindestlaufzeit neben vier Kameras (ADR-0046).
//!
//! Die Form der Demo vom 15.09. (`examples/demo/krakow-cams4.yaml`): eine
//! geschuetzte Frontkamera und drei `normal`-Kameras, je 33 ms Takt und rund
//! 19 ms Laufzeit, daneben ein Sprachbildmodell mit 150-ms-Bloecken als
//! `best_effort`, zwei Slots. Auf der GPU antwortete das Sprachmodell unter
//! dem Governor zweimal in 40 Sekunden.
//!
//! Gemessen wird dasselbe Szenario ohne und mit 300 ms Budget je Sekunde:
//! wie viel Ausfuehrungszeit das Sprachmodell bekommt, wie frisch die
//! Frontkamera bleibt und was die `normal`-Kameras dafuer abgeben.
//!
//! Die Tabelle entsteht mit
//! `cargo test -p vig-sim --test runtime_budget -- --nocapture`.

#![allow(
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_precision_loss
)]

use std::collections::HashMap;
use vig_core::arrayvec::ArrayVec;
use vig_core::ids::MAX_MODELS;
use vig_core::model::ModelContract;
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::profile::SafetyMargin;
use vig_core::request::{Criticality, QueuePolicy};
use vig_core::runtime_budget::RuntimeBudget;
use vig_core::scheduler::{Action, Event, Scheduler};
use vig_core::slots::SlotSet;
use vig_core::{Duration, Instant};
use vig_sim::harness::{self, Governor, RunResult};
use vig_sim::scenario::{Scenario, StreamSpec, VariantSpec};

fn ms(v: u64) -> Duration {
    Duration::from_nanos_unbounded(v * 1_000_000)
}

const VLM: u16 = 4;

fn camera(name: &'static str, criticality: Criticality, phase_ms: u64) -> StreamSpec {
    StreamSpec {
        name,
        criticality,
        policy: QueuePolicy::Latest,
        period: ms(33),
        jitter: ms(1),
        transport: ms(1),
        phase: ms(phase_ms),
        deadline: ms(66),
        max_age: ms(100),
        capacity: 1,
        variants: vec![VariantSpec {
            quality_milli: 1_000,
            p50: ms(19),
            p99: ms(22),
        }],
    }
}

fn four_cameras() -> Scenario {
    Scenario {
        name: "vier-kameras-und-vlm",
        duration: ms(40_000),
        slots: 2,
        pipelining: 0,
        streams: vec![
            camera("front", Criticality::Protected, 0),
            camera("left", Criticality::Normal, 8),
            camera("right", Criticality::Normal, 16),
            camera("rear", Criticality::Normal, 24),
            StreamSpec {
                name: "vlm",
                criticality: Criticality::BestEffort,
                policy: QueuePolicy::Fifo,
                // Die Demo fragt, sobald eine Antwort da ist; ein fester Takt
                // von 100 ms haelt ebenso immer eine Frage bereit.
                period: ms(100),
                jitter: ms(0),
                transport: ms(1),
                phase: ms(5),
                deadline: ms(5_000),
                max_age: ms(8_000),
                capacity: 2,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: ms(150),
                    p99: ms(170),
                }],
            },
        ],
    }
}

/// Der Governor mit der Marge der Demo (110 %), wahlweise mit Budget.
fn governor(scenario: &Scenario, budget: Option<RuntimeBudget>) -> Governor {
    let mut contracts: ArrayVec<ModelContract, MAX_MODELS> = scenario.vig_contracts();
    if let Some(vlm) = contracts.get_mut(usize::from(VLM)) {
        vlm.min_runtime = budget;
    }
    let scheduler = Scheduler::new(
        contracts,
        SlotSet::homogeneous(scenario.slots, scenario.pipelining).unwrap(),
        OverloadController::new(OverloadConfig::default(), Instant::ZERO).unwrap(),
        SafetyMargin::from_percent(110).unwrap(),
    )
    .unwrap();
    Governor::Vigilant(Box::new(scheduler))
}

struct Outcome {
    run: RunResult,
    /// Tatsaechlich gerechnete Zeit des Sprachmodells, Dispatch bis Ende.
    vlm_executed: Duration,
    vlm_calls: u64,
}

fn run(scenario: &Scenario, budget: Option<RuntimeBudget>, seed: u64) -> Outcome {
    let mut started: HashMap<u64, Instant> = HashMap::new();
    let mut executed = 0_u64;
    let mut calls = 0_u64;
    let run = harness::run_observed(
        scenario,
        governor(scenario, budget),
        if budget.is_some() { "budget" } else { "ohne" }.into(),
        seed,
        |g, now, event, actions| {
            if let Event::Completion { request, .. } = event
                && let Some(start) = started.remove(&request.0)
            {
                executed += now.saturating_since(start).as_nanos();
                calls += 1;
            }
            let before = actions.0.len();
            g.on_event(now, event, actions);
            for action in &actions.0[before..] {
                if let Action::Dispatch { request, model, .. } = *action
                    && model.0 == VLM
                {
                    started.insert(request.0, now);
                }
            }
        },
    );
    Outcome {
        run,
        vlm_executed: Duration::from_nanos_unbounded(executed),
        vlm_calls: calls,
    }
}

/// Mittlere unabgedeckte Verbraucherzyklen der drei `normal`-Kameras.
fn normal_uncovered(run: &RunResult) -> u64 {
    run.coverage[1..4]
        .iter()
        .map(|(_, c)| c.consumer_uncovered_permille())
        .sum::<u64>()
        / 3
}

#[test]
fn the_language_model_gets_its_budget_and_the_front_camera_pays_nothing() {
    let scenario = four_cameras();
    let budget = RuntimeBudget {
        budget: ms(300),
        window: ms(1_000),
    };
    let seconds = scenario.duration.as_millis() / 1_000;

    println!(
        "| Seed | Lauf | VLM-Aufrufe | VLM-Rechenzeit je s | Front nicht frisch (Verbraucher) | Front Fenster ohne neue Lieferung | Front Antwortalter p50 / p99 | Front laengste Luecke | Front Deadline-Misses | Normal nicht frisch (Mittel) |"
    );
    println!("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|");
    for seed in [1_u64, 2, 3] {
        let without = run(&scenario, None, seed);
        let with = run(&scenario, Some(budget), seed);
        for outcome in [&without, &with] {
            let front = outcome.run.coverage[0].1;
            println!(
                "| {seed} | {} | {} | {} ms | {} ‰ | {} ‰ | {} / {} ms | {} ms | {} | {} ‰ |",
                outcome.run.label,
                outcome.vlm_calls,
                outcome.vlm_executed.as_millis() / seconds,
                front.consumer_uncovered_permille(),
                front.uncovered_permille(),
                front.response_age_p50_ns / 1_000_000,
                front.response_age_p99_ns / 1_000_000,
                front.longest_gap_ns / 1_000_000,
                outcome.run.metrics.protected_deadline_misses,
                normal_uncovered(&outcome.run),
            );
        }

        // Das Budget: mindestens 300 ms je Sekunde, ueber den ganzen Lauf.
        let share = with.vlm_executed.as_millis() / seconds;
        assert!(share >= 300, "Seed {seed}: {share} ms je Sekunde");
        assert!(with.vlm_calls > without.vlm_calls * 5);

        // Die Frontkamera, gemessen wie in der Demo: zu jedem Takt liegt ein
        // Ergebnis vor, das hoechstens `max_age` alt ist — mit Budget nicht
        // seltener als ohne. Und keine verpasste Deadline mehr.
        //
        // Bewusst **nicht** zugesichert: dass in jedem 33-ms-Fenster etwas
        // Neues ankommt. Das verspricht der Vertrag nicht (Deadline 66 ms,
        // Hoechstalter 100 ms), und genau dort zahlt die Front mit: laeuft das
        // Sprachmodell auf einem Slot, teilt sie den anderen mit den
        // `normal`-Kameras, und ihre Ergebnisse kommen spaeter innerhalb der
        // Frist. Die Tabelle zeigt den Preis.
        let front_with = with.run.coverage[0].1;
        let front_without = without.run.coverage[0].1;
        assert!(
            front_with.consumer_uncovered_permille() <= front_without.consumer_uncovered_permille(),
            "Seed {seed}: Front mit {} ‰, ohne {} ‰",
            front_with.consumer_uncovered_permille(),
            front_without.consumer_uncovered_permille()
        );
        assert!(
            with.run.metrics.protected_deadline_misses
                <= without.run.metrics.protected_deadline_misses
        );
    }
}
