//! Deterministischer Zufallszahlengenerator fuer die Simulation.
//!
//! Bewusst selbst implementiert statt aus einem Crate bezogen: Gate S (ADR-0001)
//! verlangt, dass ein Simulationsergebnis **dauerhaft** reproduzierbar ist. Ein
//! externes RNG-Crate darf seinen Algorithmus ueber Versionsgrenzen hinweg
//! aendern; damit waere ein sechs Monate alter Gate-S-Report nicht mehr
//! nachrechenbar. PCG32 ist hier festgeschrieben und aendert sich nie.
//!
//! Referenz: O'Neill, "PCG: A Family of Simple Fast Space-Efficient
//! Statistically Good Algorithms for Random Number Generation" (2014).

/// PCG-XSH-RR mit 64 Bit Zustand und 32 Bit Ausgabe.
#[derive(Debug, Clone)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

const PCG_MULTIPLIER: u64 = 6_364_136_223_846_793_005;

impl Pcg32 {
    /// Erzeugt einen Generator aus Seed und Stream-Kennung.
    ///
    /// Verschiedene `stream`-Werte liefern unabhaengige Folgen aus demselben
    /// Seed. Die Simulation nutzt das, um Ankunftsprozess und Laufzeitziehung
    /// zu entkoppeln: eine Aenderung an der Lastdefinition verschiebt dann
    /// nicht die Laufzeitfolge und umgekehrt.
    #[must_use]
    pub fn new(seed: u64, stream: u64) -> Self {
        let inc = (stream << 1) | 1;
        let mut rng = Self { state: 0, inc };
        let _ = rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        let _ = rng.next_u32();
        rng
    }

    /// Die naechste 32-Bit-Zufallszahl.
    // Die Verkuerzung auf 32 Bit ist der Ausgabeschritt von PCG-XSH-RR, kein
    // Praezisionsverlust: `xorshifted` ist die definierte 32-Bit-Ausgabe,
    // `rot` liegt konstruktionsbedingt in [0, 31].
    #[allow(clippy::cast_possible_truncation)]
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(PCG_MULTIPLIER).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Die naechste 64-Bit-Zufallszahl.
    pub fn next_u64(&mut self) -> u64 {
        let hi = u64::from(self.next_u32());
        let lo = u64::from(self.next_u32());
        (hi << 32) | lo
    }

    /// Eine gleichverteilte Zahl in `[0, bound)`.
    ///
    /// Verwirft verzerrende Ziehungen (Lemire-Schwelle), damit die Verteilung
    /// exakt gleichfoermig bleibt. Gibt fuer `bound == 0` null zurueck.
    pub fn next_bounded(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let r = self.next_u32();
            if r >= threshold {
                return r % bound;
            }
        }
    }

    /// Eine gleichverteilte Zahl in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        // 53 signifikante Bits, exakt darstellbar.
        let bits = self.next_u64() >> 11;
        bits as f64 * (1.0 / 9_007_199_254_740_992.0)
    }

    /// Eine standardnormalverteilte Zahl (Box-Muller, polare Form).
    pub fn next_standard_normal(&mut self) -> f64 {
        loop {
            let u = self.next_f64() * 2.0 - 1.0;
            let v = self.next_f64() * 2.0 - 1.0;
            let s = u * u + v * v;
            if s > 0.0 && s < 1.0 {
                return u * (-2.0 * s.ln() / s).sqrt();
            }
        }
    }

    /// Eine exponentialverteilte Zahl mit Erwartungswert `mean`.
    pub fn next_exponential(&mut self, mean: f64) -> f64 {
        // 1 - U statt U, damit ln(0) ausgeschlossen ist.
        -(1.0 - self.next_f64()).ln() * mean
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn same_seed_yields_identical_sequence() {
        let mut a = Pcg32::new(42, 0);
        let mut b = Pcg32::new(42, 0);
        for _ in 0..10_000 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn different_streams_are_independent() {
        let mut a = Pcg32::new(42, 0);
        let mut b = Pcg32::new(42, 1);
        let mut equal = 0_u32;
        for _ in 0..1_000 {
            if a.next_u32() == b.next_u32() {
                equal += 1;
            }
        }
        assert!(
            equal < 5,
            "Streams duerfen nicht korrelieren, {equal} Kollisionen"
        );
    }

    #[test]
    fn bounded_is_within_range_and_covers_it() {
        let mut rng = Pcg32::new(7, 0);
        let mut seen = [false; 6];
        for _ in 0..10_000 {
            let v = rng.next_bounded(6);
            assert!(v < 6);
            seen[v as usize] = true;
        }
        assert!(seen.iter().all(|s| *s), "alle Werte muessen vorkommen");
    }

    #[test]
    fn normal_has_expected_moments() {
        let mut rng = Pcg32::new(1234, 0);
        let n = 100_000;
        let mut sum = 0.0;
        let mut sumsq = 0.0;
        for _ in 0..n {
            let x = rng.next_standard_normal();
            sum += x;
            sumsq += x * x;
        }
        let mean = sum / f64::from(n);
        let var = sumsq / f64::from(n) - mean * mean;
        assert!(mean.abs() < 0.02, "Mittelwert {mean} sollte nahe 0 liegen");
        assert!(
            (var - 1.0).abs() < 0.03,
            "Varianz {var} sollte nahe 1 liegen"
        );
    }
}
