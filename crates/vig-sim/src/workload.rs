//! Lastmodell der Simulation: Ankunftsprozesse und Laufzeitverteilungen.
//!
//! ADR-0001 verpflichtet Gate S darauf, alle Verteilungsparameter offenzulegen
//! und nicht nachtraeglich zugunsten des Ergebnisses zu veraendern. Dieses
//! Modul haelt die Parameter deshalb explizit und wenige.

use crate::rng::Pcg32;
use vig_core::{Duration, Instant};

/// Das 99-%-Quantil der Standardnormalverteilung.
const Z99: f64 = 2.326_347_874_040_841;

/// Laufzeitverteilung einer Modellvariante auf dem Backend.
///
/// Modelliert als **lognormal**, an p50 und p99 kalibriert. Begruendung:
///
/// * GPU-Inferenzlaufzeiten sind nach unten hart begrenzt (der Kernel braucht
///   seine Zeit) und nach oben schwer — Interferenz, Taktabsenkung,
///   Speicherdruck erzeugen einen rechten Schwanz. Eine Normalverteilung
///   bildet das nicht ab, eine Exponentialverteilung uebertreibt es.
/// * Spec 13.1 verlangt ohnehin p50/p95/p99 statt eines Mittelwerts. Eine
///   Verteilung, die genau an diesen Punkten kalibriert wird, ist damit
///   direkt aus realen Profilen befuellbar.
///
/// Die Ziehung wird auf `[p50 / 2, p99 * 3]` geklemmt, damit ein einzelner
/// Ausreisser einen Simulationslauf nicht dominiert.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeDistribution {
    median_ns: f64,
    sigma: f64,
    floor_ns: u64,
    ceiling_ns: u64,
}

impl RuntimeDistribution {
    /// Kalibriert die Verteilung an Median und 99-%-Quantil.
    ///
    /// Gibt `None` zurueck, wenn `p50` null ist oder `p99 < p50` — beides sind
    /// unmoegliche Profile und deuten auf einen Konfigurationsfehler hin.
    #[must_use]
    pub fn from_percentiles(p50: Duration, p99: Duration) -> Option<Self> {
        let m = p50.as_nanos();
        let q = p99.as_nanos();
        if m == 0 || q < m {
            return None;
        }
        let median_ns = m as f64;
        let sigma = (q as f64 / median_ns).ln() / Z99;
        Some(Self {
            median_ns,
            sigma,
            floor_ns: m / 2,
            ceiling_ns: q.saturating_mul(3),
        })
    }

    /// Eine deterministische Verteilung ohne Streuung.
    ///
    /// Nuetzlich fuer Golden Tests, in denen die Laufzeit exakt bekannt sein
    /// muss (Spec 28).
    #[must_use]
    pub fn constant(d: Duration) -> Self {
        Self {
            median_ns: d.as_nanos() as f64,
            sigma: 0.0,
            floor_ns: d.as_nanos(),
            ceiling_ns: d.as_nanos(),
        }
    }

    /// Zieht eine Laufzeit.
    pub fn sample(&self, rng: &mut Pcg32) -> Duration {
        if self.sigma == 0.0 {
            return Duration::from_nanos_unbounded(self.floor_ns);
        }
        let z = rng.next_standard_normal();
        let value = self.median_ns * (self.sigma * z).exp();
        let clamped = value.clamp(self.floor_ns as f64, self.ceiling_ns as f64);
        Duration::from_nanos_unbounded(clamped as u64)
    }

    /// Das analytische Quantil `q` in `(0, 1)`.
    ///
    /// Der Scheduler bekommt sein Laufzeitprofil aus dieser Funktion, nicht aus
    /// gezogenen Stichproben. So ist im Simulator sauber trennbar, ob ein
    /// Deadline-Miss aus einer falschen Prognose oder aus einer falschen
    /// Entscheidung entstand.
    #[must_use]
    pub fn quantile(&self, q: f64) -> Duration {
        if self.sigma == 0.0 || q <= 0.0 || q >= 1.0 {
            return Duration::from_nanos_unbounded(self.floor_ns);
        }
        let z = probit(q);
        let value = self.median_ns * (self.sigma * z).exp();
        let clamped = value.clamp(self.floor_ns as f64, self.ceiling_ns as f64);
        Duration::from_nanos_unbounded(clamped as u64)
    }
}

/// Inverse der Standardnormalverteilung (Acklam-Approximation).
///
/// Genauigkeit rund 1e-9 relativ — weit mehr, als fuer Laufzeitquantile
/// gebraucht wird, und ohne externe Dependency.
// Die Koeffizienten sind woertlich aus der veroeffentlichten Approximation
// uebernommen. Sie werden nicht gekuerzt, auch wenn die letzte Stelle in `f64`
// nicht mehr ankommt: eine stillschweigend veraenderte numerische Konstante
// waere spaeter nicht mehr gegen die Quelle pruefbar.
#[allow(clippy::excessive_precision)]
fn probit(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_690e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    const P_LOW: f64 = 0.024_25;

    let q;
    if p < P_LOW {
        q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - P_LOW {
        q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

/// Ein periodischer Sensorstream, wie ihn eine Kamera erzeugt.
///
/// Die Trennung von Capture-Zeit und Ankunftszeit ist wesentlich: ein Frame
/// wird zum Zeitpunkt `capture` belichtet und erreicht das Gateway erst nach
/// `transport`. Die Deadline haengt an der Capture-Zeit (Spec 10.2, L-008),
/// weshalb ein Simulator, der beide gleichsetzt, den Kerneffekt des Produkts
/// gar nicht abbilden koennte.
#[derive(Debug, Clone, Copy)]
pub struct PeriodicStream {
    /// Nominale Periode zwischen zwei Captures.
    pub period: Duration,
    /// Maximale symmetrische Abweichung der Periode.
    pub jitter: Duration,
    /// Verzoegerung zwischen Capture und Ankunft am Gateway.
    pub transport: Duration,
    /// Versatz des ersten Captures gegenueber dem Simulationsstart.
    pub phase: Duration,
}

impl PeriodicStream {
    /// Ein jitterfreier Stream ohne Transportverzoegerung.
    #[must_use]
    pub const fn ideal(period: Duration) -> Self {
        Self {
            period,
            jitter: Duration::ZERO,
            transport: Duration::ZERO,
            phase: Duration::ZERO,
        }
    }

    /// Der Capture-Zeitpunkt des `n`-ten Frames.
    ///
    /// Der Jitter wird aus `n` und dem Stream-RNG gezogen und ist damit
    /// unabhaengig von der Reihenfolge anderer Ziehungen reproduzierbar.
    #[must_use]
    pub fn capture_at(&self, n: u64, rng: &mut Pcg32) -> Instant {
        let nominal = self.period.as_nanos().saturating_mul(n);
        let base = self.phase.as_nanos().saturating_add(nominal);
        let j = self.jitter.as_nanos();
        let offset = if j == 0 {
            0_i64
        } else {
            let span = j.saturating_mul(2).min(u64::from(u32::MAX));
            i64::from(rng.next_bounded(span as u32 + 1)) - i64::try_from(j).unwrap_or(i64::MAX)
        };
        let shifted = if offset.is_negative() {
            base.saturating_sub(offset.unsigned_abs())
        } else {
            base.saturating_add(offset.unsigned_abs())
        };
        Instant::from_nanos(shifted)
    }

    /// Der Ankunftszeitpunkt des `n`-ten Frames am Gateway.
    #[must_use]
    pub fn arrival_at(&self, capture: Instant) -> Instant {
        capture.checked_add(self.transport).unwrap_or(capture)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    #[test]
    fn distribution_matches_its_calibration_points() {
        let d = RuntimeDistribution::from_percentiles(ms(10), ms(20)).unwrap();
        assert_eq!(d.quantile(0.5).as_millis(), 10, "Median muss p50 treffen");
        let p99 = d.quantile(0.99).as_nanos();
        let want = ms(20).as_nanos();
        let err = p99.abs_diff(want) as f64 / want as f64;
        assert!(err < 0.001, "p99 weicht um {err} ab");
    }

    #[test]
    fn sampled_distribution_matches_analytic_quantile() {
        let d = RuntimeDistribution::from_percentiles(ms(10), ms(25)).unwrap();
        let mut rng = Pcg32::new(99, 3);
        let mut samples: Vec<u64> = (0..50_000).map(|_| d.sample(&mut rng).as_nanos()).collect();
        samples.sort_unstable();

        let median = samples[samples.len() / 2] as f64;
        let want = d.quantile(0.5).as_nanos() as f64;
        assert!(
            (median / want - 1.0).abs() < 0.02,
            "empirischer Median {median} vs {want}"
        );

        let p99 = samples[samples.len() * 99 / 100] as f64;
        let want99 = d.quantile(0.99).as_nanos() as f64;
        assert!(
            (p99 / want99 - 1.0).abs() < 0.05,
            "empirisches p99 {p99} vs {want99}"
        );
    }

    #[test]
    fn impossible_profiles_are_rejected() {
        assert!(
            RuntimeDistribution::from_percentiles(ms(20), ms(10)).is_none(),
            "p99 < p50"
        );
        assert!(RuntimeDistribution::from_percentiles(Duration::ZERO, ms(10)).is_none());
    }

    #[test]
    fn constant_distribution_never_varies() {
        let d = RuntimeDistribution::constant(ms(7));
        let mut rng = Pcg32::new(1, 1);
        for _ in 0..1_000 {
            assert_eq!(d.sample(&mut rng).as_millis(), 7);
        }
    }

    #[test]
    fn periodic_stream_separates_capture_from_arrival() {
        let s = PeriodicStream {
            period: ms(33),
            jitter: Duration::ZERO,
            transport: ms(5),
            phase: Duration::ZERO,
        };
        let mut rng = Pcg32::new(0, 0);
        let capture = s.capture_at(3, &mut rng);
        assert_eq!(capture.as_nanos(), 99_000_000);
        assert_eq!(s.arrival_at(capture).as_nanos(), 104_000_000);
    }

    #[test]
    fn jitter_stays_within_its_bound() {
        let s = PeriodicStream {
            period: ms(33),
            jitter: ms(3),
            transport: Duration::ZERO,
            phase: Duration::ZERO,
        };
        let mut rng = Pcg32::new(5, 5);
        for n in 1..2_000_u64 {
            let nominal = 33_000_000_u64 * n;
            let got = s.capture_at(n, &mut rng).as_nanos();
            assert!(
                got.abs_diff(nominal) <= 3_000_000,
                "Jitter ausserhalb der Schranke bei n={n}"
            );
        }
    }
}
