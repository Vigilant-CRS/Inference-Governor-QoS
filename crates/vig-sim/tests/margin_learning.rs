//! ADR-0038: die Planung kalibriert sich an der Karte — gegen den echten
//! Scheduler, deterministisch.
//!
//! Die Last ist die der Lastrampe (`vig-bench/src/bin/load-ramp.rs`):
//! Detektor und Pose alle 23 ms, Tiefe alle 46 ms bei 100 %, ein Slot,
//! dieselben Profile. Der Governor bekommt ein Profil, das um einen Faktor
//! von der simulierten Karte abweicht; die Karte rechnet, was sie rechnet.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::print_stdout,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use vig_core::learning::MarginLearning;
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::profile::SafetyMargin;
use vig_core::request::{Criticality, QueuePolicy};
use vig_core::scheduler::{Event, Scheduler};
use vig_core::slots::SlotSet;
use vig_core::{Duration, Instant, ModelIdx};
use vig_sim::harness::{Governor, RunResult, baseline, run, run_observed, vig};
use vig_sim::scenario::{Scenario, StreamSpec, VariantSpec};

fn us(v: u64) -> Duration {
    Duration::from_nanos_unbounded(v * 1_000)
}

/// Die Lastrampe bei `load_percent`, mit Laufzeiten mal `runtime_permille`.
///
/// Wie im Benchmark aendert die Last die Perioden, nicht die Modelle; Frist
/// und Hoechstalter wachsen mit der Periode.
fn ramp(load_percent: u64, runtime_permille: u64) -> Scenario {
    let base: [(&'static str, Criticality, u64, u64, u64); 3] = [
        ("detector", Criticality::Protected, 23_000, 14_916, 17_470),
        ("pose", Criticality::High, 23_000, 3_987, 5_506),
        ("depth", Criticality::High, 46_000, 7_908, 9_534),
    ];
    let streams = base
        .iter()
        .enumerate()
        .map(|(i, (name, class, period, p50, p99))| {
            let period = period * 100 / load_percent;
            StreamSpec {
                name,
                criticality: *class,
                policy: QueuePolicy::Latest,
                period: us(period),
                jitter: us(500),
                transport: us(1_000),
                phase: us(i as u64 * 7_000),
                deadline: us(period * 3 / 2),
                max_age: us(period * 2),
                capacity: 1,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: us(p50 * runtime_permille / 1_000),
                    p99: us(p99 * runtime_permille / 1_000),
                }],
            }
        })
        .collect();
    Scenario {
        name: "load-ramp",
        duration: us(20_000_000),
        slots: 1,
        pipelining: 0,
        streams,
    }
}

/// Der Governor, mit dem Profil `profile` gebaut.
fn vigilant(profile: &Scenario, margin_percent: u32, learning: Option<MarginLearning>) -> Governor {
    let slots = SlotSet::homogeneous(profile.slots, profile.pipelining).unwrap();
    let overload = OverloadController::new(OverloadConfig::default(), Instant::ZERO).unwrap();
    let mut scheduler = Scheduler::new(
        profile.vig_contracts(),
        slots,
        overload,
        SafetyMargin::from_percent(margin_percent).unwrap(),
    )
    .unwrap();
    scheduler.set_margin_learning(learning);
    Governor::Vigilant(Box::new(scheduler))
}

/// Ein Lauf, und was der Governor am Ende gelernt hatte.
struct Observed {
    result: RunResult,
    /// Die wirksame Marge je Modell am Ende, in Prozent.
    margins: [u32; 3],
    /// Der Geraetefaktor am Ende, in Prozent; null ohne Kalibrierung.
    device: u32,
    /// Nach wie vielen Fertigstellungen die Marge des Detektors zuerst
    /// `threshold` erreichte.
    reached: Option<u64>,
}

fn observe(actual: &Scenario, governor: Governor, seed: u64, threshold: Option<u32>) -> Observed {
    let mut margins = [0_u32; 3];
    let mut device = 0;
    let mut completions = 0_u64;
    let mut reached = None;
    let result = run_observed(
        actual,
        governor,
        "vigilant".to_owned(),
        seed,
        |g, now, event, actions| {
            let completion = matches!(event, Event::Completion { .. });
            g.on_event(now, event, actions);
            if let Governor::Vigilant(s) = g {
                for (i, m) in margins.iter_mut().enumerate() {
                    *m = s.margin_of(ModelIdx(i as u16)).as_percent();
                }
                device = s.metrics().learned_device_factor_percent;
                if completion {
                    completions += 1;
                    if reached.is_none() && threshold.is_some_and(|t| margins[0] >= t) {
                        reached = Some(completions);
                    }
                }
            }
        },
    );
    Observed {
        result,
        margins,
        device,
        reached,
    }
}

fn worst_high(result: &RunResult) -> u64 {
    result
        .uncovered_permille("pose")
        .max(result.uncovered_permille("depth"))
}

/// Ohne Kalibrierung ist alles wie vorher: dieselben Zaehler, dieselbe
/// Abdeckung, Ereignis fuer Ereignis.
#[test]
fn without_learning_the_scheduler_is_bit_identical() {
    let actual = ramp(100, 1_000);
    let plain = run(
        &actual,
        vig(&actual, SafetyMargin::DEFAULT),
        "a".to_owned(),
        5,
    );
    let switched_off = run(&actual, vigilant(&actual, 110, None), "b".to_owned(), 5);
    assert_eq!(plain.metrics, switched_off.metrics);
    assert_eq!(plain.coverage, switched_off.coverage);
}

/// Ein pessimistisches Profil — die Karte rechnet in der Haelfte der Zeit:
/// der Faktor sinkt unter 100 % und findet das p99 dieser Karte, fuer jedes
/// Modell, obwohl das Profil es verdoppelt.
#[test]
fn a_pessimistic_profile_is_learned_below_one() {
    let actual = ramp(90, 1_000);
    let profile = ramp(90, 2_000);
    let learning = MarginLearning::new(25, 1_000, 48).unwrap();
    let run = observe(&actual, vigilant(&profile, 110, Some(learning)), 3, None);
    for (name, margin) in ["detector", "pose", "depth"].iter().zip(run.margins) {
        assert!(
            (45..=62).contains(&margin),
            "{name}: gelernt {margin} %, erwartet rund 50 % (das p99 der Karte ist das halbe \
             Profil-p99)"
        );
    }
    assert!(run.device < 100, "Geraetefaktor {} %", run.device);
}

/// Ein optimistisches Profil — die Karte braucht das Doppelte: der Faktor
/// steigt innerhalb der ersten Fertigstellungen, vorsichtiger wird er sofort.
#[test]
fn an_optimistic_profile_is_corrected_within_the_first_completions() {
    let actual = ramp(70, 1_000);
    let profile = ramp(70, 500);
    let run = observe(
        &actual,
        vigilant(&profile, 110, Some(MarginLearning::default_range())),
        3,
        Some(180),
    );
    let reached = run.reached.expect("180 % nie erreicht");
    assert!(
        reached <= 200,
        "erst nach {reached} Fertigstellungen bei 180 %"
    );
    for (name, margin) in ["detector", "pose", "depth"].iter().zip(run.margins) {
        assert!(
            (180..=250).contains(&margin),
            "{name}: gelernt {margin} %, erwartet rund 200 %"
        );
    }
}

/// Der Befund hinter ADR-0038: an der Kante entscheidet die Marge nichts.
///
/// Bei 100 % Last und einem Profil, das die Karte um 10 % ueberschaetzt,
/// liefern feste 110 %, feste 100 % und die gelernte Marge dieselbe
/// Abdeckung. Kein Veto und kein verworfener Frame haengt dort am Plan; der
/// Verlust ist Kapazitaet, nicht Vorsicht. Wer die Kante verbessern will,
/// muss woanders ansetzen.
#[test]
fn at_the_edge_the_margin_decides_nothing() {
    let actual = ramp(100, 1_000);
    let profile = ramp(100, 1_100);
    let results: Vec<(u64, u64, u64)> = [
        (110, None),
        (100, None),
        (110, Some(MarginLearning::default_range())),
    ]
    .into_iter()
    .map(|(margin, learning)| {
        let r = run(
            &actual,
            vigilant(&profile, margin, learning),
            "vigilant".to_owned(),
            1,
        );
        (
            r.uncovered_permille("detector"),
            worst_high(&r),
            r.metrics.deferred_for_protected,
        )
    })
    .collect();
    assert!(
        results.windows(2).all(|w| w[0] == w[1]),
        "Detektor, high-Strom, Vetos je Marge: {results:?}"
    );
    assert_eq!(results[0].2, 0, "kein Veto an der Kante");
}

/// Nach ADR-0036 gibt der Look-ahead eine geschuetzte Ankunft nicht mehr
/// auf. Ein Profil, das die Karte doppelt so langsam macht, hungert dann
/// jeden anderen Strom aus: jeder Auftrag gefaehrdet laut Plan den Detektor.
/// Die Kalibrierung gibt ihnen Arbeit zurueck — und kostet den Detektor dabei
/// hoechstens, was ihn auch ein richtiges Profil kostet.
#[test]
fn a_strongly_pessimistic_profile_starves_the_other_streams_until_it_is_learned() {
    let actual = ramp(110, 1_000);
    let profile = ramp(110, 2_000);
    let fixed = run(&actual, vigilant(&profile, 110, None), "fest".to_owned(), 1);
    let learned = run(
        &actual,
        vigilant(&profile, 110, Some(MarginLearning::default_range())),
        "gelernt".to_owned(),
        1,
    );
    let correct = run(
        &actual,
        vigilant(&actual, 110, None),
        "richtig".to_owned(),
        1,
    );
    let row = |r: &RunResult| (r.uncovered_permille("detector"), worst_high(r));
    let (fixed, learned, correct) = (row(&fixed), row(&learned), row(&correct));
    assert!(
        fixed.1 >= 990,
        "feste Marge, falsches Profil: high-Stroeme {} ‰ statt ausgehungert",
        fixed.1
    );
    assert!(
        learned.1 + 150 <= fixed.1,
        "gelernt gibt den high-Stroemen Arbeit zurueck: {} ‰ gegen {} ‰",
        learned.1,
        fixed.1
    );
    assert!(
        learned.0 <= correct.0,
        "Detektor gelernt {} ‰, mit richtigem Profil {} ‰",
        learned.0,
        correct.0
    );
}

/// Was die Kalibrierung verspricht: dasselbe Ergebnis wie mit einem
/// richtigen Profil, gleich in welche Richtung das Profil falsch ist.
#[test]
fn a_learned_plan_behaves_like_a_correct_profile() {
    for load in [105, 110] {
        let actual = ramp(load, 1_000);
        let correct = run(
            &actual,
            vigilant(&actual, 110, None),
            "richtig".to_owned(),
            1,
        );
        for scale in [700, 1_100] {
            let learned = run(
                &actual,
                vigilant(
                    &ramp(load, scale),
                    110,
                    Some(MarginLearning::default_range()),
                ),
                "gelernt".to_owned(),
                1,
            );
            for (what, a, b) in [
                (
                    "Detektor",
                    learned.uncovered_permille("detector"),
                    correct.uncovered_permille("detector"),
                ),
                ("high", worst_high(&learned), worst_high(&correct)),
            ] {
                assert!(
                    a.abs_diff(b) <= 25,
                    "{load} %, Profil x{scale} Promille: {what} gelernt {a} ‰, richtig {b} ‰"
                );
            }
        }
    }
}

/// Der Beleg, warum gelernt bei doppelt so langsamem Profil den Detektor
/// mehr kostet als die feste Marge: der Plan ist die Groesse, mit der der
/// Look-ahead Arbeit neben dem Detektor zulaesst. Je naeher er am echten p99
/// liegt, desto weniger Vetos, desto mehr andere Arbeit, desto oefter trifft
/// deren Streuung den Detektor. Der harte Boden `min_factor_percent` haelt
/// den Plan hier kuenstlich ueber dem echten p99 (Faktor 50 % ist es genau).
///
/// `cargo test --release -p vig-sim --test margin_learning -- --ignored --nocapture`
#[test]
#[ignore = "Beleg, ~10 s im Release-Build"]
fn the_plan_size_trade_off() {
    for load in [100, 110, 125] {
        let actual = ramp(load, 1_000);
        let profile = ramp(load, 2_000);
        println!(
            "{load} %, Profil doppelt so langsam: Detektor / high Promille, Vetos, \
             Fristverletzungen des Detektors, weitergereicht"
        );
        let print = |label: String, o: &Observed| {
            let m = o.result.metrics;
            println!(
                "  {label:<26} {:>4} / {:>4}   veto {:>5}   Frist {:>4}   weiter {:>5}   Faktor {:?}",
                o.result.uncovered_permille("detector"),
                worst_high(&o.result),
                m.deferred_for_protected,
                m.protected_deadline_misses,
                m.forwarded,
                o.margins,
            );
        };
        print(
            "fest 110 %".to_owned(),
            &observe(&actual, vigilant(&profile, 110, None), 1, None),
        );
        for min in [100, 75, 60, 55, 50] {
            let learning = MarginLearning::new(min, 1_000, 48).unwrap();
            print(
                format!(
                    "gelernt, Boden {min} % (Plan x{:.2})",
                    f64::from(min) / 50.0
                ),
                &observe(&actual, vigilant(&profile, 110, Some(learning)), 1, None),
            );
        }
        print(
            "richtiges Profil, 110 %".to_owned(),
            &observe(&actual, vigilant(&actual, 110, None), 1, None),
        );
    }
}

/// Die Messmatrix hinter ADR-0038: welcher Mechanismus kostet an der Kante
/// Arbeit, und was die Kalibrierung daran aendert.
///
/// `cargo test --release -p vig-sim --test margin_learning -- --ignored --nocapture`
#[test]
#[ignore = "Messmatrix, ~1 min im Release-Build"]
fn the_edge_matrix() {
    let learning = MarginLearning::default_range();
    println!(
        "Profil = Karte mal Faktor. Promille unabgedeckter Perioden, Median ueber 3 Seeds: \
         Detektor / schlechtester high-Strom. Marge am Ende: Detektor/Pose/Tiefe, Geraet."
    );
    for scale in [1_000, 1_100, 2_000, 700] {
        for load in [90, 95, 100, 105, 110, 125] {
            let actual = ramp(load, 1_000);
            let profile = ramp(load, scale);
            let mut rows: Vec<String> = Vec::new();

            let mut base_det = Vec::new();
            let mut base_high = Vec::new();
            for seed in 1..=3 {
                let (mut det, mut high) = (u64::MAX, u64::MAX);
                for cap in [1, 8] {
                    let r = run(&actual, baseline(&profile, cap), "fifo".to_owned(), seed);
                    det = det.min(r.uncovered_permille("detector"));
                    high = high.min(worst_high(&r));
                }
                base_det.push(det);
                base_high.push(high);
            }
            rows.push(format!(
                "FIFO {:>4}/{:<4}",
                median(base_det),
                median(base_high)
            ));

            for (label, margin, learn) in [
                ("110", 110, None),
                ("100", 100, None),
                ("lernt", 110, Some(learning)),
            ] {
                let mut det = Vec::new();
                let mut high = Vec::new();
                let mut last = None;
                for seed in 1..=3 {
                    let o = observe(&actual, vigilant(&profile, margin, learn), seed, None);
                    det.push(o.result.uncovered_permille("detector"));
                    high.push(worst_high(&o.result));
                    last = Some((o.margins, o.device, o.result.metrics));
                }
                let (margins, device, m) = last.unwrap();
                rows.push(format!(
                    "{label} {:>4}/{:<4} [{}/{}/{} %, G {} %; stale {}, veto {}, abgelehnt {}]",
                    median(det),
                    median(high),
                    margins[0],
                    margins[1],
                    margins[2],
                    device,
                    m.stale,
                    m.deferred_for_protected,
                    m.rejected_infeasible,
                ));
            }
            println!(
                "x{:.1} {load:>3} %  {}",
                scale as f64 / 1_000.0,
                rows.join("  |  ")
            );
        }
    }
}

fn median(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    v[v.len() / 2]
}
