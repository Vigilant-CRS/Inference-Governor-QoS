//! Was eine Scheduling-Entscheidung kostet (`decision-bench`).
//!
//! Der Kern ist unter Emulation fuer `aarch64` gebaut und getestet — und
//! Emulation sagt nichts ueber Laufzeit. Dieses Modul liefert, was eine
//! Messung auf echter Hardware braucht: Szenarien mit realistischem
//! Vertragssatz, die Einteilung der Ereignisse nach Art und eine
//! Perzentilzusammenfassung, die ohne Gleitkomma und ohne Abhaengigkeiten
//! auskommt und damit auf jedem Zielsystem dieselben Zahlen liefert.
//!
//! Gemessen wird der **Kern**, nicht das Geraet: keine Inferenz, kein Netz,
//! kein Datenpfad. Die Frage ist, ob die Entscheidung selbst auf einer
//! schwachen CPU in einem Bruchteil der Periode faellt, die sie schuetzt.

use crate::scenario::{Scenario, StreamSpec, VariantSpec};
use vig_core::scheduler::Event;
use vig_core::{Criticality, Duration, QueuePolicy};

const fn us(v: u64) -> Duration {
    Duration::from_nanos_unbounded(v.saturating_mul(1_000))
}

const fn ms(v: u64) -> Duration {
    us(v.saturating_mul(1_000))
}

/// Der Vertragssatz aus `examples/gate_m3/vig.yaml`.
///
/// Die Laufzeitverhaeltnisse sind die dort kalibrierten (RF-DETR, Pose,
/// Tiefe, VLM). Mit ihren Medianen laege die Auslastung bei rund 86 %; das
/// Szenario wird deshalb auf 103 % skaliert — die Zahl, die `gate-m3` als
/// geschuetzte serialisierte Auslastung meldet. Erst ueber der Saettigung
/// liegen Verdraengung, Verwerfen und Abwertung im gemessenen Pfad. Dazu kommt
/// eine zweite, kleinere Detektorvariante, damit die Variantenwahl etwas zu
/// waehlen hat; Gate M3 selbst faehrt nur eine.
#[must_use]
pub fn gate_m3(duration: Duration) -> Scenario {
    Scenario {
        name: "gate-m3",
        duration,
        slots: 1,
        pipelining: 0,
        streams: vec![
            StreamSpec {
                name: "detector",
                criticality: Criticality::Protected,
                policy: QueuePolicy::Latest,
                period: ms(33),
                jitter: ms(2),
                transport: ms(3),
                phase: Duration::ZERO,
                deadline: ms(33),
                max_age: ms(66),
                capacity: 1,
                variants: vec![
                    VariantSpec {
                        quality_milli: 1_000,
                        p50: us(15_639),
                        p99: us(19_596),
                    },
                    VariantSpec {
                        quality_milli: 800,
                        p50: us(9_000),
                        p99: us(11_500),
                    },
                ],
            },
            StreamSpec {
                name: "pose",
                criticality: Criticality::High,
                policy: QueuePolicy::Latest,
                period: ms(33),
                jitter: ms(2),
                transport: ms(3),
                phase: ms(11),
                deadline: ms(33),
                max_age: ms(66),
                capacity: 1,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: us(4_082),
                    p99: us(5_690),
                }],
            },
            StreamSpec {
                name: "depth",
                criticality: Criticality::High,
                policy: QueuePolicy::Latest,
                period: ms(66),
                jitter: ms(3),
                transport: ms(4),
                phase: ms(7),
                deadline: ms(66),
                max_age: ms(132),
                capacity: 1,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: us(9_286),
                    p99: us(11_453),
                }],
            },
            StreamSpec {
                name: "vlm",
                criticality: Criticality::BestEffort,
                policy: QueuePolicy::Fifo,
                period: ms(800),
                jitter: ms(20),
                transport: ms(5),
                phase: ms(50),
                deadline: ms(800),
                max_age: ms(1_500),
                capacity: 4,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: us(97_902),
                    p99: us(109_996),
                }],
            },
        ],
    }
    .at_load(1_030)
}

/// Stromnamen fuer das Szenario mit vielen Modellen.
///
/// Statisch, weil [`StreamSpec::name`] ein `&'static str` ist; 32 ist die
/// Obergrenze des Kerns ([`vig_core::ids::MAX_MODELS`]).
const NAMES: [&str; 32] = [
    "m00", "m01", "m02", "m03", "m04", "m05", "m06", "m07", "m08", "m09", "m10", "m11", "m12",
    "m13", "m14", "m15", "m16", "m17", "m18", "m19", "m20", "m21", "m22", "m23", "m24", "m25",
    "m26", "m27", "m28", "m29", "m30", "m31",
];

/// `n` Kamerastroeme mit je zwei Varianten, bei 110 % Angebotslast.
///
/// Die Mischung ist bewusst gemischt: ein Viertel `protected`, die Haelfte
/// `high`, der Rest `normal`; Perioden von 33 bis 100 ms; versetzte Phasen,
/// damit nicht alle Stroeme im selben Tick eintreffen. 110 % sorgt dafuer,
/// dass Verdraengung, Verwerfen und Abwertung im Pfad liegen und nicht nur
/// der ruhige Fall gemessen wird.
///
/// `n` wird auf `1..=32` begrenzt.
#[must_use]
pub fn many_models(n: usize, duration: Duration) -> Scenario {
    let count = n.clamp(1, NAMES.len());
    let streams = NAMES
        .iter()
        .take(count)
        .enumerate()
        .map(|(i, name)| {
            let period_ms = [33_u64, 50, 66, 100].get(i % 4).copied().unwrap_or(33);
            let criticality = match i % 4 {
                0 => Criticality::Protected,
                1 | 2 => Criticality::High,
                _ => Criticality::Normal,
            };
            let large = 4_000 + (i as u64 % 5) * 1_000;
            StreamSpec {
                name,
                criticality,
                policy: QueuePolicy::Latest,
                period: ms(period_ms),
                jitter: ms(2),
                transport: ms(3),
                phase: ms((i as u64).saturating_mul(7) % period_ms),
                deadline: ms(period_ms),
                max_age: ms(period_ms.saturating_mul(2)),
                capacity: 1,
                variants: vec![
                    VariantSpec {
                        quality_milli: 1_000,
                        p50: us(large),
                        p99: us(large * 13 / 10),
                    },
                    VariantSpec {
                        quality_milli: 800,
                        p50: us(large * 6 / 10),
                        p99: us(large * 8 / 10),
                    },
                ],
            }
        })
        .collect();
    Scenario {
        name: "many-models",
        duration,
        slots: 1,
        pipelining: 0,
        streams,
    }
    .at_load(1_100)
}

/// Die Art eines Ereignisses.
///
/// Getrennt berichtet, weil die Arten verschieden viel Arbeit bedeuten und
/// verschieden oft vorkommen: der Takt feuert jede Millisekunde und ist
/// meist billig, die Ankunft ist die eigentliche Entscheidung. Ein gemeinsames
/// Perzentil ueber alle wuerde vom Takt beherrscht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Ein Auftrag trifft ein: Zulassung, Verdraengung, Variantenwahl, Dispatch.
    Arrival,
    /// Ein Auftrag ist fertig: Slot frei, naechster Dispatch.
    Completion,
    /// Das Backend meldet einen Fehler.
    Failure,
    /// Der Client zieht einen Auftrag zurueck.
    Cancel,
    /// Der Weckruf: Ablaeufe, Verbraucherzyklen, Look-ahead.
    Tick,
}

impl EventKind {
    /// Alle Arten, in Berichtsreihenfolge.
    pub const ALL: [Self; 5] = [
        Self::Arrival,
        Self::Completion,
        Self::Failure,
        Self::Cancel,
        Self::Tick,
    ];

    /// Die Art eines Ereignisses.
    #[must_use]
    pub const fn of(event: &Event) -> Self {
        match event {
            Event::Arrival(_) => Self::Arrival,
            Event::Completion { .. } => Self::Completion,
            Event::BackendFailure { .. } => Self::Failure,
            Event::Cancel { .. } => Self::Cancel,
            Event::Tick => Self::Tick,
        }
    }

    /// Ein fester Index fuer Tabellen, `0..5`.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Arrival => 0,
            Self::Completion => 1,
            Self::Failure => 2,
            Self::Cancel => 3,
            Self::Tick => 4,
        }
    }

    /// Der Name im Bericht.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Arrival => "arrival",
            Self::Completion => "completion",
            Self::Failure => "failure",
            Self::Cancel => "cancel",
            Self::Tick => "tick",
        }
    }
}

/// Die Zusammenfassung einer Latenzstichprobe, in Nanosekunden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    /// Anzahl Messwerte.
    pub count: u64,
    /// Median.
    pub p50: u64,
    /// 99-%-Quantil.
    pub p99: u64,
    /// 99,9-%-Quantil.
    pub p999: u64,
    /// Groesster Wert.
    pub max: u64,
    /// Mittelwert, abgerundet.
    pub mean: u64,
    /// Summe aller Werte.
    pub total: u64,
}

/// Sortiert die Stichprobe und fasst sie zusammen.
///
/// Quantile nach der Nearest-Rank-Methode: der kleinste Messwert, unter oder
/// auf dem mindestens der verlangte Anteil der Stichprobe liegt. Keine
/// Interpolation — ein Quantil ist dann immer ein tatsaechlich gemessener
/// Wert, und das Ergebnis haengt nicht von Gleitkommarundung ab.
#[must_use]
pub fn summarize(samples: &mut [u64]) -> Summary {
    if samples.is_empty() {
        return Summary::default();
    }
    samples.sort_unstable();
    let total = samples.iter().fold(0_u64, |acc, v| acc.saturating_add(*v));
    let count = samples.len() as u64;
    Summary {
        count,
        p50: nearest_rank(samples, 500),
        p99: nearest_rank(samples, 990),
        p999: nearest_rank(samples, 999),
        max: samples.last().copied().unwrap_or(0),
        mean: total / count,
        total,
    }
}

/// Der Wert mit Rang `ceil(n · q)` einer sortierten Stichprobe, `q` in Promille.
fn nearest_rank(sorted: &[u64], per_mille: u64) -> u64 {
    let n = sorted.len() as u64;
    let rank = n.saturating_mul(per_mille).div_ceil(1_000).max(1);
    let index = usize::try_from(rank.saturating_sub(1)).unwrap_or(usize::MAX);
    sorted
        .get(index)
        .or_else(|| sorted.last())
        .copied()
        .unwrap_or(0)
}

/// Der Median einer Reihe, fuer die Zusammenfassung ueber Wiederholungen.
///
/// Bei gerader Anzahl der obere der beiden mittleren Werte — dieselbe
/// Konvention wie in `gate-s` und `load-ramp`.
#[must_use]
pub fn median(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted.get(sorted.len() / 2).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;
    use crate::harness::{run, run_observed, vig};
    use vig_core::profile::SafetyMargin;

    #[test]
    fn quantiles_follow_the_nearest_rank_rule() {
        let mut samples: Vec<u64> = (1..=1_000).rev().collect();
        let s = summarize(&mut samples);
        assert_eq!(s.count, 1_000);
        assert_eq!(s.p50, 500);
        assert_eq!(s.p99, 990);
        assert_eq!(s.p999, 999);
        assert_eq!(s.max, 1_000);
        assert_eq!(s.mean, 500);
        assert_eq!(s.total, 500_500);
    }

    #[test]
    fn a_small_sample_takes_its_largest_values_for_the_tail() {
        let mut samples = vec![7, 3, 5];
        let s = summarize(&mut samples);
        assert_eq!(s.p50, 5);
        assert_eq!(s.p99, 7, "ceil(3 · 0,99) = 3: der groesste Wert");
        assert_eq!(s.p999, 7);
    }

    #[test]
    fn an_empty_sample_is_all_zero() {
        assert_eq!(summarize(&mut []), Summary::default());
        assert_eq!(median(&[]), 0);
    }

    #[test]
    fn the_median_of_repeats_is_the_upper_middle() {
        assert_eq!(median(&[3, 1, 2]), 2);
        assert_eq!(median(&[4, 1, 3, 2]), 3);
    }

    #[test]
    fn the_gate_m3_set_is_overloaded_like_the_real_gate() {
        let s = gate_m3(ms(1_000));
        assert_eq!(s.streams.len(), 4);
        // Auf 103 % skaliert, die Zahl aus gate-m3; ganzzahlige Skalierung
        // laesst ein Promille Spiel.
        let u = s.utilization_permille();
        assert!((1_020..=1_040).contains(&u), "{u}");
    }

    #[test]
    fn many_models_is_bounded_and_loaded() {
        for n in [1, 16, 32, 99] {
            let s = many_models(n, ms(1_000));
            assert_eq!(s.streams.len(), n.clamp(1, 32));
            let u = s.utilization_permille();
            assert!((1_050..=1_150).contains(&u), "n={n}: {u}");
            assert!(s.streams.iter().all(|st| st.variants.len() == 2));
        }
    }

    /// `run_observed` faehrt dieselbe Ereignisfolge wie `run`: dieselben
    /// Zaehler am Ende, und jedes Ereignis kommt genau einmal durch `step`.
    #[test]
    fn observing_does_not_change_the_run() {
        let scenario = gate_m3(ms(5_000));
        let plain = run(
            &scenario,
            vig(&scenario, SafetyMargin::DEFAULT),
            "a".into(),
            7,
        );
        let mut seen = [0_u64; 5];
        let observed = run_observed(
            &scenario,
            vig(&scenario, SafetyMargin::DEFAULT),
            "b".into(),
            7,
            |g, now, event, actions| {
                seen[EventKind::of(&event).index()] += 1;
                g.on_event(now, event, actions);
            },
        );
        assert_eq!(plain.metrics, observed.metrics);
        assert!(seen[EventKind::Arrival.index()] > 0);
        assert!(seen[EventKind::Tick.index()] >= 4_999);
        assert_eq!(
            seen[EventKind::Arrival.index()],
            observed.metrics.received,
            "jede Ankunft genau einmal"
        );
    }

    /// Die Last ist echt: Verdraengung und Variantenwahl kommen im Pfad vor.
    #[test]
    fn the_gate_m3_trace_exercises_supersession_and_variants() {
        let scenario = gate_m3(ms(20_000));
        let result = run(
            &scenario,
            vig(&scenario, SafetyMargin::DEFAULT),
            "v".into(),
            11,
        );
        assert!(result.metrics.forwarded > 0);
        assert!(
            result.metrics.superseded > 0 || result.metrics.stale > 0,
            "bei ueber 100 % Last muss etwas verworfen werden: {:?}",
            result.metrics
        );
        assert!(
            result
                .metrics
                .variant_selected
                .iter()
                .filter(|c| **c > 0)
                .count()
                >= 2,
            "beide Detektorvarianten sollten gewaehlt werden: {:?}",
            result.metrics.variant_selected
        );
    }
}
