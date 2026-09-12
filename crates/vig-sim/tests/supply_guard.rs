//! Der Look-ahead schuetzt die Deadline, nicht die Versorgung (ADR-0041).
//!
//! Befund der Rampe vom 12.09. (`measure-morgen-2026-09-12`): mit fester
//! Marge von 110 % verfehlte der Detektor bei 125 % Last 0 ‰, mit gelernter
//! Marge 205 ‰ — ohne eine einzige verletzte Deadline. Je genauer der Plan,
//! desto weniger Vetos, desto mehr Hintergrundarbeit, desto mehr
//! Versorgungsluecken.
//!
//! Hier steht dieselbe Mechanik ohne Marge und ohne Zufall: ein geschuetzter
//! Strom mit `deadline = 1,5 T` und `max_age = 2 T`, daneben ein
//! Hintergrundstrom, dessen Auftraege in den Deadline-Spielraum passen — aber
//! nicht in den Versorgungsspielraum.
//!
//! Die Tabelle der Analyse entsteht mit
//! `cargo test -p vig-sim --test supply_guard -- --ignored --nocapture`.

#![allow(
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]

use vig_core::Duration;
use vig_core::profile::SafetyMargin;
use vig_core::request::{Criticality, QueuePolicy};
use vig_sim::harness::{self, RunResult};
use vig_sim::scenario::{Scenario, StreamSpec, VariantSpec};

fn ms(v: u64) -> Duration {
    Duration::from_nanos_unbounded(v * 1_000_000)
}

/// Ohne Aufschlag: der Plan trifft die Laufzeit. Genau das ist der Fall, den
/// die gelernte Marge herstellt (ADR-0038).
fn exact() -> SafetyMargin {
    SafetyMargin::from_percent(100).unwrap()
}

/// Ein geschuetzter Strom mit 33 ms Takt und ein Hintergrundstrom, dessen
/// Auftraege in den Deadline-Spielraum passen.
///
/// Der Detektor rechnet 22 ms, seine Deadline liegt 49 ms nach der Aufnahme,
/// sein Hoechstalter bei 66 ms. Das Ergebnis des vorigen Frames laeuft also
/// 33 ms nach dieser Aufnahme ab: alles, was den Detektor mehr als 11 ms
/// verzoegert, hinterlaesst eine Luecke, ohne eine Deadline zu reissen.
fn supply_scenario(background_ms: u64) -> Scenario {
    Scenario {
        name: "supply-guard",
        duration: ms(20_000),
        slots: 1,
        pipelining: 0,
        streams: vec![
            StreamSpec {
                name: "detector",
                criticality: Criticality::Protected,
                policy: QueuePolicy::Latest,
                period: ms(33),
                jitter: ms(1),
                transport: ms(1),
                phase: Duration::ZERO,
                deadline: ms(49),
                max_age: ms(66),
                capacity: 1,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: ms(21),
                    p99: ms(22),
                }],
            },
            StreamSpec {
                name: "background",
                criticality: Criticality::BestEffort,
                policy: QueuePolicy::Latest,
                period: ms(60),
                jitter: ms(3),
                transport: ms(1),
                phase: ms(17),
                deadline: ms(600),
                max_age: ms(1_200),
                capacity: 1,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: ms(background_ms),
                    p99: ms(background_ms),
                }],
            },
        ],
    }
}

fn without(scenario: &Scenario, seed: u64) -> RunResult {
    harness::run(
        scenario,
        harness::vig(scenario, exact()),
        "deadline".into(),
        seed,
    )
}

fn with(scenario: &Scenario, seed: u64) -> RunResult {
    harness::run(
        scenario,
        harness::vig_protecting_supply(scenario, exact()),
        "versorgung".into(),
        seed,
    )
}

/// Abdeckung des Verbrauchers und versorgte Hintergrundfenster.
fn cells(run: &RunResult) -> (u64, u64, u64, u64) {
    let detector = run.coverage[0].1;
    let background = run.coverage[1].1;
    (
        detector.consumer_uncovered_permille(),
        detector.uncovered_permille(),
        background.covered,
        run.metrics.deferred_for_protected,
    )
}

/// Der Befund: Hintergrundarbeit wird zugelassen, jede Deadline haelt, und
/// der Verbraucher hat trotzdem Luecken.
///
/// Der Effekt ist klein, und das gehoert dazu: eine Luecke von rund 15 ms in
/// einem 33-ms-Fenster trifft nicht jede Abtastung. Gemessen ueber drei
/// Seeds: 13 ‰ der Abtastungen, mit Schutz keine.
#[test]
fn work_that_keeps_every_deadline_still_costs_the_consumer_its_supply() {
    let scenario = supply_scenario(25);
    let mut worst = 0;
    for seed in 1..=3 {
        let run = without(&scenario, seed);
        let (consumer, _, _, _) = cells(&run);
        assert_eq!(
            run.metrics.protected_deadline_misses, 0,
            "Seed {seed}: keine verletzte Deadline erwartet"
        );
        worst = worst.max(consumer);
    }
    assert!(
        worst > 0,
        "erwartet wurden Versorgungsluecken ohne Deadlineverletzung, gemessen {worst} ‰"
    );
}

/// Mit Versorgungsschutz verschwinden die Luecken.
#[test]
fn the_supply_guard_closes_the_gaps() {
    let scenario = supply_scenario(25);
    for seed in 1..=3 {
        let (consumer, windows, _, _) = cells(&with(&scenario, seed));
        assert_eq!(
            consumer, 0,
            "Seed {seed}: mit Schutz erwartet 0 ‰, gemessen {consumer} ‰ ({windows} ‰ Fenster)"
        );
    }
}

/// Und er kostet Hintergrundfortschritt — in diesem Szenario den ganzen.
///
/// Bei 25-ms-Auftraegen versorgt der Hintergrund ohne Schutz 333 Fenster, mit
/// Schutz keines: der Guard vetoiert jeden Auftrag, der laenger dauert als
/// der Abstand zwischen zwei geschuetzten Ankuenften. Das ist kein
/// Nebeneffekt, sondern der Preis (Spec 10.7) — und der Grund, warum der
/// Schalter in der Voreinstellung aus ist.
#[test]
fn the_supply_guard_costs_background_progress() {
    let scenario = supply_scenario(25);
    let plain = cells(&without(&scenario, 1));
    let guarded = cells(&with(&scenario, 1));
    assert!(
        guarded.2 < plain.2,
        "Hintergrund versorgt: ohne {}, mit {}",
        plain.2,
        guarded.2
    );
    assert!(
        guarded.3 > plain.3,
        "mehr Vetos erwartet: ohne {}, mit {}",
        plain.3,
        guarded.3
    );
}

/// Er zahlt auch, wo nichts zu retten war: bei 20-ms-Auftraegen hat der
/// Verbraucher ohne Schutz keine einzige Luecke, und der Schutz kostet
/// trotzdem Hintergrundfenster. Der Guard kennt nur die Prognose, nicht den
/// Ausgang — er reserviert gegen den schlechtesten Fall.
#[test]
fn the_supply_guard_also_pays_where_nothing_had_to_be_rescued() {
    let scenario = supply_scenario(20);
    let plain = cells(&without(&scenario, 1));
    let guarded = cells(&with(&scenario, 1));
    assert_eq!(plain.0, 0, "ohne Schutz schon lueckenlos");
    assert_eq!(guarded.0, 0);
    assert!(
        guarded.2 < plain.2,
        "Hintergrund versorgt: ohne {}, mit {}",
        plain.2,
        guarded.2
    );
}

/// Ohne Hoechstalter ueber der Periode gibt es keine Versorgungsfrist, und
/// der Schutz aendert nichts — bitgleich dieselben Zaehler.
#[test]
fn without_a_supply_window_nothing_changes() {
    let mut scenario = supply_scenario(25);
    scenario.streams[0].max_age = ms(33);
    let plain = without(&scenario, 1);
    let guarded = with(&scenario, 1);
    assert_eq!(plain.metrics.forwarded, guarded.metrics.forwarded);
    assert_eq!(
        plain.metrics.deferred_for_protected,
        guarded.metrics.deferred_for_protected
    );
}

/// Die Tabelle fuer ADR-0041.
#[test]
#[ignore = "Messmatrix, Ausgabe fuer das ADR"]
fn the_supply_matrix() {
    println!(
        "Hintergrundauftrag | Verbraucher ohne/mit | Fenster ohne/mit | \
         Hintergrund versorgt ohne/mit | Vetos ohne/mit"
    );
    for background in [10, 15, 20, 25, 30] {
        let scenario = supply_scenario(background);
        let plain = cells(&without(&scenario, 1));
        let guarded = cells(&with(&scenario, 1));
        println!(
            "  {background:>3} ms | {:>3} / {:>3} ‰ | {:>3} / {:>3} ‰ | {:>4} / {:>4} | {:>4} / {:>4}",
            plain.0, guarded.0, plain.1, guarded.1, plain.2, guarded.2, plain.3, guarded.3
        );
    }
}
