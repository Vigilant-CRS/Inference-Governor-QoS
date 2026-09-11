//! Die zwei Schwaechen aus der Messkette vom 11.09., im Simulator
//! nachgestellt (docs/analysis/bursts-and-frontier.md).
//!
//! 1. **Frontier.** Die automatische Variantenwahl verfehlte bei 110 und
//!    125 % Last 66 bzw. 84 ‰ der Perioden, die kleine Variante allein
//!    keine. Der Simulator trifft beide Zahlen (71 und 84 ‰) mit denselben
//!    Profilen und Vertraegen wie `frontier`. Ursache: die Wahl pruefte je
//!    Frame die Deadline (`1,5 P`), nicht die Versorgung (`max_age - P`).
//! 2. **Lastspitzen.** Die Fenstersicht des Detektors verfehlte unter
//!    Spitzen fuer beide Seiten viel, die Verbrauchersicht nichts: dieselbe
//!    Kamera liefert waehrend einer Spitze schneller, danach liegen Aufnahmen
//!    und Fenstergrenzen gegeneinander verschoben. Die Fenstersicht misst
//!    dort die Phase, nicht die Versorgung.
//!
//! Die Tabellen der Analyse entstehen mit
//! `cargo test -p vig-sim --test bursts_and_frontier -- --ignored --nocapture`.

#![allow(
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::indexing_slicing
)]

use vig_core::profile::SafetyMargin;
use vig_core::request::{Criticality, QueuePolicy};
use vig_core::{Duration, Instant};
use vig_sim::harness::{self, RunResult};
use vig_sim::scenario::{Scenario, StreamSpec, VariantSpec};

fn us(v: u64) -> Duration {
    Duration::from_nanos_unbounded(v * 1_000)
}

fn margin() -> SafetyMargin {
    SafetyMargin::from_percent(110).unwrap()
}

// ---------------------------------------------------------------------------
// Frontier
// ---------------------------------------------------------------------------

/// Die Kalibrierung aus dem sauberen Lauf vom 11.09. (measure-nachlauf-e):
/// `detector_large` p50 12 720 / p99 13 354 us, `detector_small` p50 5 102 /
/// p99 5 325 us.
const LARGE: VariantSpec = VariantSpec {
    quality_milli: 1_000,
    p50: Duration::from_nanos_unbounded(12_720_000),
    p99: Duration::from_nanos_unbounded(13_354_000),
};
const SMALL: VariantSpec = VariantSpec {
    quality_milli: 800,
    p50: Duration::from_nanos_unbounded(5_102_000),
    p99: Duration::from_nanos_unbounded(5_325_000),
};

/// Der Vertrag aus `frontier`: Deadline `1,5 P`, `max_age = 2 P`.
fn frontier(period_ms: u64, variants: Vec<VariantSpec>) -> Scenario {
    let p = period_ms * 1_000;
    Scenario {
        name: "frontier",
        duration: us(15_000_000),
        slots: 1,
        pipelining: 0,
        streams: vec![StreamSpec {
            name: "detector",
            criticality: Criticality::Protected,
            policy: QueuePolicy::Latest,
            period: us(p),
            jitter: us(200),
            transport: Duration::ZERO,
            phase: Duration::ZERO,
            deadline: us((p * 3).div_ceil(2)),
            max_age: us(p * 2),
            capacity: 1,
            variants,
        }],
    }
}

fn run_frontier(period_ms: u64, variants: Vec<VariantSpec>, seed: u64) -> RunResult {
    let s = frontier(period_ms, variants);
    harness::run(&s, harness::vig(&s, margin()), "vig".into(), seed)
}

/// Unter Ueberlast versorgt die automatische Wahl so gut wie die kleine
/// Variante allein. Vor der Versorgungsfrist verfehlte sie bei 12 ms Periode
/// (`frontier` 110 %) 71 ‰ der Fenster und 191 ‰ der Abtastzeitpunkte.
#[test]
fn under_overload_the_automatic_choice_supplies_like_the_small_variant() {
    for period in [12, 10] {
        for seed in 1..=3 {
            let auto = run_frontier(period, vec![LARGE, SMALL], seed);
            let (_, c) = auto.coverage[0];
            assert!(
                c.consumer_uncovered_permille() <= 5 && c.uncovered_permille() <= 5,
                "P {period} ms, Seed {seed}: auto verfehlt {} ‰ Fenster, {} ‰ Abtastungen",
                c.uncovered_permille(),
                c.consumer_uncovered_permille()
            );
        }
    }
}

/// Wo die grosse Variante versorgt, bleibt sie die Wahl: die Regel kostet
/// dort keine Qualitaet (`frontier` 50 und 75 %).
#[test]
fn where_the_large_variant_supplies_it_is_kept() {
    for period in [25, 17] {
        let auto = run_frontier(period, vec![LARGE, SMALL], 1);
        let selected = auto.metrics.variant_selected;
        assert_eq!(selected[1], 0, "P {period} ms: keine Abwertung erwartet");
        assert!(selected[0] > 0);
        assert_eq!(auto.coverage[0].1.consumer_uncovered_permille(), 0);
    }
}

/// Die grosse Variante allein kann nicht versorgen, wenn ihre Laufzeit an
/// die Periode heranreicht: das Ergebnis des vorigen Frames laeuft nach
/// `max_age - P = P` ab, und jede Laufzeit knapp ueber `P` hinterlaesst eine
/// Luecke — auch ohne einen einzigen verworfenen Frame. Das ist die
/// Physik dieses Vertrags, kein Planungsfehler: die feste grosse Variante
/// verfehlte im Messlauf bei 90 % 105 ‰.
#[test]
fn a_fixed_large_variant_cannot_supply_when_its_runtime_reaches_the_period() {
    let gross = run_frontier(13, vec![LARGE], 1);
    let (_, c) = gross.coverage[0];
    assert_eq!(gross.metrics.stale + gross.metrics.superseded, 0);
    assert!(c.consumer_uncovered_permille() > 50, "{c:?}");
}

// ---------------------------------------------------------------------------
// Lastspitzen
// ---------------------------------------------------------------------------

/// `load-ramp`: (logisch, Basisperiode ms bei 100 %, Basis-max_age ms, p50,
/// p99, Klasse). Profile wie dort, gemessen am 01.09.
const RAMP: [(&str, u64, u64, u64, u64, Criticality); 3] = [
    ("detector", 23, 46, 14_916, 17_470, Criticality::Protected),
    ("pose", 23, 46, 3_987, 5_506, Criticality::High),
    ("depth", 46, 92, 7_908, 9_534, Criticality::High),
];

const BURST_SECONDS: u64 = 20;

fn scaled(base_ms: u64, load: u64) -> u64 {
    (base_ms * 100 / load).max(5)
}

/// Die Konfiguration der Grundlast, wie `load-ramp bursts` sie dem Governor
/// gibt: der Betreiber hat fuer den Normalbetrieb konfiguriert.
fn ramp(load: u64, transport_us: u64) -> Scenario {
    Scenario {
        name: "bursts",
        duration: us(BURST_SECONDS * 1_000_000),
        slots: 1,
        pipelining: 0,
        streams: RAMP
            .iter()
            .map(|(name, period, age, p50, p99, class)| {
                let p = scaled(*period, load);
                StreamSpec {
                    name,
                    criticality: *class,
                    policy: QueuePolicy::Latest,
                    period: us(p * 1_000),
                    jitter: Duration::ZERO,
                    transport: us(transport_us),
                    phase: Duration::ZERO,
                    deadline: us(p * 3 / 2 * 1_000),
                    max_age: us(scaled(*age, load) * 1_000),
                    capacity: 1,
                    variants: vec![VariantSpec {
                        quality_milli: 1_000,
                        p50: us(*p50),
                        p99: us(*p99),
                    }],
                }
            })
            .collect(),
    }
}

/// Aufnahmezeitpunkte wie `workload::run_stream` mit `Burst`: alle `every`
/// beginnt eine Spitze der Laenge `length`, darin liefert die Kamera mit der
/// Periode der Spitzenlast.
fn burst_captures(base: u64, peak: u64, length_ms: u64, every_ms: u64) -> Vec<Vec<Instant>> {
    RAMP.iter()
        .map(|(_, period, _, _, _, _)| {
            let base_period = scaled(*period, base) * 1_000_000;
            let peak_period = scaled(*period, peak) * 1_000_000;
            let mut out = Vec::new();
            let mut t = 0_u64;
            while t < BURST_SECONDS * 1_000_000_000 {
                out.push(Instant::from_nanos(t));
                let phase = t % (every_ms * 1_000_000);
                t += if phase < length_ms * 1_000_000 {
                    peak_period
                } else {
                    base_period
                };
            }
            out
        })
        .collect()
}

/// Das Profil, bei dem Vigilant im Messlauf schlechter aussah (75 → 150 %,
/// 1 s alle 4 s): im Simulator verfehlen Governor **und** FIFO in der
/// Fenstersicht ein Vielfaches dessen, was ihnen in der Verbrauchersicht
/// fehlt — naemlich nichts.
#[test]
fn under_bursts_the_window_metric_measures_phase_not_supply() {
    let s = ramp(75, 200);
    let captures = burst_captures(75, 150, 1_000, 4_000);
    for seed in 1..=2 {
        for governor in [harness::vig(&s, margin()), harness::baseline(&s, 1)] {
            let r = harness::run_captures(&s, governor, "x".into(), seed, &captures);
            let (_, detector) = r.coverage[0];
            assert_eq!(
                detector.consumer_uncovered_permille(),
                0,
                "Verbrauchersicht: {detector:?}"
            );
            assert!(
                detector.uncovered_permille() > 30,
                "Fenstersicht: {detector:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tabellen fuer die Analyse
// ---------------------------------------------------------------------------

fn cells(r: &RunResult) -> String {
    r.coverage
        .iter()
        .map(|(name, c)| {
            format!(
                "{name} {}/{}",
                c.uncovered_permille(),
                c.consumer_uncovered_permille()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
#[ignore = "druckt die Tabellen der Analyse"]
fn tables() {
    println!("Frontier (Fenster/Verbraucher in Promille, Seed 1..3):");
    for period in [25, 17, 14, 13, 12, 10] {
        for (label, variants) in [
            ("gross", vec![LARGE]),
            ("klein", vec![SMALL]),
            ("auto", vec![LARGE, SMALL]),
        ] {
            let row: Vec<String> = (1..=3)
                .map(|seed| {
                    let r = run_frontier(period, variants.clone(), seed);
                    format!("{} [gross {}]", cells(&r), r.metrics.variant_selected[0])
                })
                .collect();
            println!("  P {period:>2} ms {label:<5}: {}", row.join(" | "));
        }
    }

    println!("\nLastspitzen (Fenster/Verbraucher in Promille, Transport 0,2 / 0,7 ms):");
    for (base, peak, length, every) in [
        (90, 150, 200, 2_000),
        (90, 150, 500, 2_000),
        (75, 150, 1_000, 4_000),
    ] {
        let captures = burst_captures(base, peak, length, every);
        for transport in [200, 700] {
            let s = ramp(base, transport);
            let v =
                harness::run_captures(&s, harness::vig(&s, margin()), "vig".into(), 1, &captures);
            let b =
                harness::run_captures(&s, harness::baseline(&s, 1), "fifo".into(), 1, &captures);
            println!(
                "  {base} -> {peak} %, {length}/{every} ms, {transport} us: vig [{}] fifo [{}]",
                cells(&v),
                cells(&b)
            );
        }
    }

    println!("\nRampe mit Dispatchluecke nur auf Governorseite (Laufzeit + Luecke):");
    for load in [90, 95, 100, 105] {
        for gap in [0, 300, 600] {
            let with_gap = ramp_with(load, gap);
            let plain = ramp_with(load, 0);
            let v = harness::run(
                &with_gap,
                harness::vig(&with_gap, margin()),
                "vig".into(),
                1,
            );
            let b = harness::run(&plain, harness::baseline(&plain, 8), "fifo".into(), 1);
            println!(
                "  {load:>3} %, Luecke {gap:>3} us: vig [{}] fifo [{}]",
                cells(&v),
                cells(&b)
            );
        }
    }
}

/// Die stationaere Rampe mit leicht versetzten Phasen und einer Luecke je
/// Auftrag, die nur die Governorseite hat (`pipelining_depth: 0`: der
/// naechste Auftrag geht erst nach der Rueckmeldung des vorigen raus).
fn ramp_with(load: u64, gap_us: u64) -> Scenario {
    let mut s = ramp(load, 200);
    s.duration = us(15_000_000);
    for (i, stream) in s.streams.iter_mut().enumerate() {
        stream.jitter = us(300);
        stream.phase = us(i as u64 * 1_700);
        for v in &mut stream.variants {
            v.p50 = v.p50.checked_add(us(gap_us)).unwrap();
            v.p99 = v.p99.checked_add(us(gap_us)).unwrap();
        }
    }
    s
}
