//! Gate S — die simulierte Falsifikation (ADR-0001).
//!
//! Faehrt die Kernvergleiche A und B aus Spec 19.5/19.6 vollstaendig simuliert
//! gegen eine FIFO-Baseline mit modelluebergreifender Prioritaet und schreibt
//! einen Report nach `docs/benchmark/gate-s-report.md`.
//!
//! Der Simulator ist die **Best-Case-Welt** fuer Vigilant: keine
//! Proxy-Kosten, keine zweite Backend-Queue, keine Profilfehler. Ein Effekt,
//! der hier nicht gross ist, kann real nur kleiner werden. Gate S kann die
//! Produkthypothese daher falsifizieren, aber nicht bestaetigen.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::integer_division
)]

use std::fmt::Write as _;
use vig_sim::harness::{RunResult, baseline, run, vig};
use vig_sim::scenario::{self, Scenario};

/// Angebotslastpunkte aus Spec 19.4.
const LOADS: [u64; 7] = [500, 750, 900, 1_000, 1_100, 1_250, 1_500];

/// Queue-Tiefen der Baseline. Flach wirft Arbeit weg, tief erzeugt Rueckstand;
/// beide Enden werden gefahren, damit die Baseline nicht an einer schlecht
/// gewaehlten Zahl scheitert (Spec 19.1).
const BASELINE_DEPTHS: [usize; 3] = [1, 4, 16];

/// Seeds je Lastpunkt. Der Median wird berichtet.
const SEEDS: [u64; 5] = [
    0x5EED_0001,
    0x5EED_0002,
    0x5EED_0003,
    0x5EED_0004,
    0x5EED_0005,
];

fn median(mut values: Vec<u64>) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

impl Point {
    /// Lebendigkeitsbedingung (ADR-0009).
    ///
    /// Ein Governor, der weniger nuetzliche Ergebnisse liefert als die
    /// Baseline, hat den Vergleich nicht gewonnen — gleichgueltig, wie gut
    /// seine uebrigen Kennzahlen aussehen. Ohne diese Bedingung wertet der
    /// Report Verhungern als Erfolg: wer nichts rechnet, verschwendet nichts.
    fn alive(&self) -> bool {
        self.vig_valid.saturating_mul(100) >= self.baseline_best.valid.saturating_mul(95)
    }
}

struct Point {
    load: u64,
    vig_uncovered: u64,
    vig_stale: u64,
    vig_valid: u64,
    baseline_best: BaselinePoint,
}

#[derive(Clone, Copy)]
struct BaselinePoint {
    depth: usize,
    uncovered: u64,
    stale: u64,
    valid: u64,
}

fn guarded_uncovered(scenario: &Scenario, result: &RunResult) -> u64 {
    scenario
        .streams
        .iter()
        .filter(|s| s.criticality.is_guarded())
        .map(|s| result.uncovered_permille(s.name))
        .max()
        .unwrap_or(0)
}

fn measure(scenario: &Scenario) -> Vec<Point> {
    let mut points = Vec::new();

    for load in LOADS {
        let scaled = scenario.at_load(load);

        let mut ot_unc = Vec::new();
        let mut ot_stale = Vec::new();
        let mut ot_valid = Vec::new();
        for seed in SEEDS {
            let r = run(
                &scaled,
                vig(&scaled, vig_core::profile::SafetyMargin::DEFAULT),
                "vig".to_owned(),
                seed,
            );
            ot_unc.push(guarded_uncovered(&scaled, &r));
            ot_stale.push(u64::from(r.metrics.stale_compute_permille()));
            ot_valid.push(r.metrics.completed_valid);
        }

        // Fuer die Baseline gewinnt die beste Queue-Tiefe: der Vergleich wird
        // gegen ihr staerkstes Ergebnis gefuehrt, nicht gegen ihr schwaechstes.
        let mut best: Option<BaselinePoint> = None;
        for depth in BASELINE_DEPTHS {
            let mut unc = Vec::new();
            let mut stale = Vec::new();
            let mut valid = Vec::new();
            for seed in SEEDS {
                let r = run(
                    &scaled,
                    baseline(&scaled, depth),
                    "baseline".to_owned(),
                    seed,
                );
                unc.push(guarded_uncovered(&scaled, &r));
                stale.push(u64::from(r.metrics.stale_compute_permille()));
                valid.push(r.metrics.completed_valid);
            }
            let candidate = BaselinePoint {
                depth,
                uncovered: median(unc),
                stale: median(stale),
                valid: median(valid),
            };
            best = match best {
                Some(b) if b.uncovered <= candidate.uncovered => Some(b),
                _ => Some(candidate),
            };
        }

        points.push(Point {
            load,
            vig_uncovered: median(ot_unc),
            vig_stale: median(ot_stale),
            vig_valid: median(ot_valid),
            baseline_best: best.unwrap_or(BaselinePoint {
                depth: 0,
                uncovered: 0,
                stale: 0,
                valid: 0,
            }),
        });
    }
    points
}

fn ratio(baseline_value: u64, vig_value: u64) -> String {
    if vig_value == 0 {
        return if baseline_value == 0 {
            "—".to_owned()
        } else {
            "∞".to_owned()
        };
    }
    format!("{:.2}x", baseline_value as f64 / vig_value as f64)
}

fn section(out: &mut String, scenario: &Scenario, points: &[Point]) {
    let _ = writeln!(out, "\n## Szenario `{}`\n", scenario.name);
    let _ = writeln!(
        out,
        "Slots: {} · Pipelining: {} · Messdauer: {} ms · Streams: {}\n",
        scenario.slots,
        scenario.pipelining,
        scenario.duration.as_millis(),
        scenario
            .streams
            .iter()
            .map(|s| s.name)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(
        out,
        "| Last | unabged. Perioden OT | Baseline (beste Tiefe) | Faktor | stale compute OT | Baseline | Faktor | gueltige Ergebnisse OT / Baseline |"
    );
    let _ = writeln!(out, "|---:|---:|---:|---:|---:|---:|---:|---:|");
    for p in points {
        let b = p.baseline_best;
        let _ = writeln!(
            out,
            "| {} % | {} ‰ | {} ‰ (d={}) | **{}** | {} ‰ | {} ‰ | **{}** | {} / {}{} |",
            p.load / 10,
            p.vig_uncovered,
            b.uncovered,
            b.depth,
            ratio(b.uncovered, p.vig_uncovered),
            p.vig_stale,
            b.stale,
            ratio(b.stale, p.vig_stale),
            p.vig_valid,
            b.valid,
            if p.alive() { "" } else { " ⚠" },
        );
    }
}

fn verdict(points: &[Point]) -> (bool, String) {
    // Bewertet wird bei Ueberlast - dort, wo die Produktthese greift.
    let overload: Vec<&Point> = points.iter().filter(|p| p.load >= 1_100).collect();

    let coverage_ok = overload.iter().any(|p| {
        p.vig_uncovered > 0 && p.baseline_best.uncovered >= p.vig_uncovered.saturating_mul(2)
    }) || overload
        .iter()
        .any(|p| p.vig_uncovered == 0 && p.baseline_best.uncovered > 0);

    let stale_ok = overload.iter().any(|p| {
        p.baseline_best.stale > 0
            && p.vig_stale.saturating_mul(10) <= p.baseline_best.stale.saturating_mul(7)
    });

    let alive = points.iter().all(Point::alive);
    let passed = alive && (coverage_ok || stale_ok);
    let mut text = String::new();
    let _ = writeln!(
        text,
        "- Lebendigkeit (mind. 95 % der nuetzlichen Ergebnisse der Baseline, an \
         jedem Lastpunkt): **{}**",
        if alive { "erfuellt" } else { "VERLETZT" }
    );
    let _ = writeln!(
        text,
        "- Ziel A' (mind. 2x weniger unabgedeckte Perioden bei >= 110 % Last): **{}**",
        if coverage_ok {
            "erreicht"
        } else {
            "nicht erreicht"
        }
    );
    let _ = writeln!(
        text,
        "- Ziel B (mind. 30 % weniger stale compute bei >= 110 % Last): **{}**",
        if stale_ok {
            "erreicht"
        } else {
            "nicht erreicht"
        }
    );
    if !alive {
        let _ = writeln!(
            text,
            "\n> Die Lebendigkeitsbedingung ist verletzt. Die uebrigen Kennzahlen \
             sind damit **nicht** aussagekraeftig: eine Politik, die kaum noch \
             Arbeit ausfuehrt, erreicht null verschwendete Rechenzeit, ohne \
             irgendetwas zu leisten."
        );
    }
    (passed, text)
}

fn main() {
    let mut out = String::new();
    let _ = writeln!(out, "# Gate-S-Report");
    let _ = writeln!(
        out,
        "\n> Simulierte Falsifikation nach ADR-0001. **Kein Messwert gegen echte \
         Hardware.** Der Simulator ist die Best-Case-Welt fuer Vigilant: keine \
         Proxy-Kosten, keine zweite Backend-Queue, keine Profilfehler. Ein Effekt, \
         der hier nicht gross ist, kann real nur kleiner werden. Gate S kann die \
         Produkthypothese daher **falsifizieren, aber nicht bestaetigen** — \
         Milestone M3 gegen eine getunte Triton-Baseline bleibt das entscheidende \
         Gate."
    );

    let _ = writeln!(out, "\n## Versuchsaufbau\n");
    let _ = writeln!(
        out,
        "Beide Governoren bekommen denselben Ankunftsprozess, dieselben Slots, \
         dieselben Vertraege, dieselben Laufzeitprofile und dieselben Weckrufe \
         (1 ms). Die tatsaechliche Backendlaufzeit eines Frames wird aus \
         `(seed, request_id, variant)` gezogen und ist damit unabhaengig von der \
         Reihenfolge, in der ein Governor Arbeit startet."
    );
    let _ = writeln!(out, "\n| Parameter | Wert |");
    let _ = writeln!(out, "|---|---|");
    let _ = writeln!(
        out,
        "| Laufzeitverteilung | lognormal, an p50/p99 kalibriert, geklemmt auf `[p50/2, p99*3]` |"
    );
    let _ = writeln!(
        out,
        "| Planungsgrundlage des Schedulers | `p99 * 1.10` (Spec 13.2) |"
    );
    let _ = writeln!(
        out,
        "| Angebotslast | {LOADS:?} Promille der Slot-Kapazitaet |"
    );
    let _ = writeln!(
        out,
        "| Baseline-Queue-Tiefen | {BASELINE_DEPTHS:?}, beste je Lastpunkt gewertet |"
    );
    let _ = writeln!(out, "| Seeds | {SEEDS:02x?}, Median berichtet |");
    let _ = writeln!(
        out,
        "| Baseline-Politik | bounded FIFO mit modelluebergreifender Prioritaet (= Triton + Rate Limiter) |"
    );
    let _ = writeln!(
        out,
        "| Erfolgsmetrik | unabgedeckte Perioden nach ADR-0005, nicht Deadline-Misses pro Request |"
    );
    let _ = writeln!(
        out,
        "| Nebenbedingung | Lebendigkeit nach ADR-0009: mind. 95 % der nuetzlichen Ergebnisse der Baseline |"
    );

    let mut all_passed = true;
    let mut verdicts = String::new();

    for scenario in [scenario::freshness(), scenario::protected_vs_best_effort()] {
        let points = measure(&scenario);
        section(&mut out, &scenario, &points);
        let (passed, text) = verdict(&points);
        all_passed = all_passed && passed;
        let _ = writeln!(verdicts, "\n### `{}`\n\n{text}", scenario.name);
    }

    let _ = writeln!(out, "\n## Bewertung\n{verdicts}");
    let _ = writeln!(
        out,
        "\n**Gate S: {}**",
        if all_passed {
            "bestanden"
        } else {
            "NICHT bestanden"
        }
    );
    if !all_passed {
        let _ = writeln!(
            out,
            "\nNach ADR-0001 ist die Positionierung zu ueberpruefen, bevor Phase 2 \
             beginnt. Parameter duerfen dafuer **nicht** nachtraeglich zugunsten des \
             Ergebnisses angepasst werden."
        );
    }

    println!("{out}");

    // Der Report gehoert ins Repository: ADR-0001 verlangt, dass Parameter und
    // Ergebnis gemeinsam nachvollziehbar bleiben.
    let path = std::path::Path::new("docs/benchmark/gate-s-report.md");
    match std::fs::write(path, &out) {
        Ok(()) => println!("\n-> geschrieben nach {}", path.display()),
        Err(e) => println!("\n-> konnte {} nicht schreiben: {e}", path.display()),
    }
}
