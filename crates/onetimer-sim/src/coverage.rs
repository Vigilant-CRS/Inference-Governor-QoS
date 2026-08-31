//! Die Erfolgsmetriken des Gate-S-Vergleichs (ADR-0005, Spec 18).
//!
//! ## Warum nicht „Deadline Misses pro Request"
//!
//! Bei `LATEST` ist diese Groesse nicht wohldefiniert und im Vergleich
//! systematisch verzerrt. Zaehlt man supersedierte Requests als Miss, verliert
//! OneTimer per Konstruktion; zaehlt man sie nicht, sinkt die Rate trivial,
//! weil OneTimer den Nenner verkleinert — eine Politik, die 90 % aller Frames
//! verwirft, erreicht eine nahezu perfekte Miss-Rate, ohne dem Roboter zu
//! nuetzen.
//!
//! Deshalb misst Gate S periodenbezogen: **hatte das System in diesem
//! Regelzyklus eine hinreichend aktuelle Wahrnehmung?** Diese Frage ist gegen
//! beide Manipulationen immun und ist zugleich die Frage, die der Roboter
//! tatsaechlich stellt.

use onetimer_core::{Duration, Instant};

/// Zaehlt abgedeckte und unabgedeckte Perioden eines Streams.
#[derive(Debug, Clone)]
pub struct CoverageTracker {
    period: Duration,
    max_age: Duration,
    start: Instant,
    covered: Vec<bool>,
    ages_ns: Vec<u64>,
}

/// Das Ergebnis der Coverage-Messung eines Streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    /// Perioden, in denen ein hinreichend frisches Ergebnis vorlag.
    pub covered: u64,
    /// Perioden insgesamt.
    pub total: u64,
    /// Age of Information, 50. Perzentil, in Nanosekunden.
    pub aoi_p50_ns: u64,
    /// Age of Information, 95. Perzentil.
    pub aoi_p95_ns: u64,
    /// Age of Information, 99. Perzentil.
    pub aoi_p99_ns: u64,
    /// Ausgelieferte Ergebnisse insgesamt.
    pub delivered: u64,
}

impl Coverage {
    /// Der Anteil abgedeckter Perioden in Promille.
    #[must_use]
    pub fn covered_permille(&self) -> u64 {
        if self.total == 0 {
            return 0;
        }
        self.covered.saturating_mul(1_000) / self.total
    }

    /// Der Anteil **un**abgedeckter Perioden in Promille.
    ///
    /// Die eigentliche Zielgroesse: Gate S verlangt mindestens 2x weniger
    /// unabgedeckte Perioden als die Baseline.
    #[must_use]
    pub fn uncovered_permille(&self) -> u64 {
        1_000_u64.saturating_sub(self.covered_permille())
    }
}

impl CoverageTracker {
    /// Legt einen Tracker fuer einen Stream an.
    ///
    /// `period` bestimmt die Fensterbreite, `max_age` das Frischekriterium.
    /// Ohne beide Angaben ist die Metrik bedeutungslos — deshalb sind sie
    /// Pflichtparameter und werden im Report ausgewiesen.
    #[must_use]
    pub fn new(period: Duration, max_age: Duration, start: Instant, duration: Duration) -> Self {
        let windows = (duration.as_nanos() / period.as_nanos().max(1)).max(1);
        let windows = usize::try_from(windows).unwrap_or(usize::MAX);
        Self {
            period,
            max_age,
            start,
            covered: vec![false; windows],
            ages_ns: Vec::new(),
        }
    }

    /// Vermerkt ein ausgeliefertes Ergebnis.
    ///
    /// `completion` ist der Zeitpunkt der Auslieferung, `generation` die
    /// Capture-Zeit des zugrunde liegenden Sensordatums. Das Fenster wird nach
    /// dem **Auslieferungszeitpunkt** indiziert: gefragt ist, ob in diesem
    /// Regelzyklus etwas Frisches vorlag.
    pub fn record_delivery(&mut self, completion: Instant, generation: Instant) {
        let age = completion.saturating_since(generation);
        self.ages_ns.push(age.as_nanos());
        if age > self.max_age {
            return;
        }
        let offset = completion.saturating_since(self.start).as_nanos();
        let index = usize::try_from(offset / self.period.as_nanos().max(1)).unwrap_or(usize::MAX);
        if let Some(slot) = self.covered.get_mut(index) {
            *slot = true;
        }
    }

    /// Schliesst die Messung ab.
    #[must_use]
    pub fn finish(&self) -> Coverage {
        let mut sorted = self.ages_ns.clone();
        sorted.sort_unstable();
        let pick = |q: usize| -> u64 {
            if sorted.is_empty() {
                return 0;
            }
            let idx = (sorted.len().saturating_mul(q) / 100).min(sorted.len().saturating_sub(1));
            sorted.get(idx).copied().unwrap_or(0)
        };
        Coverage {
            covered: self.covered.iter().filter(|c| **c).count() as u64,
            total: self.covered.len() as u64,
            aoi_p50_ns: pick(50),
            aoi_p95_ns: pick(95),
            aoi_p99_ns: pick(99),
            delivered: self.ages_ns.len() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    fn at(v: u64) -> Instant {
        Instant::ZERO.checked_add(ms(v)).unwrap()
    }

    #[test]
    fn a_fresh_result_covers_its_window() {
        let mut t = CoverageTracker::new(ms(33), ms(66), Instant::ZERO, ms(99));
        t.record_delivery(at(10), at(0));
        let c = t.finish();
        assert_eq!(c.total, 3);
        assert_eq!(c.covered, 1);
    }

    /// Der Kernpunkt aus ADR-0005: ein Ergebnis, das zu alt ist, deckt sein
    /// Fenster nicht ab — auch wenn es puenktlich geliefert wurde.
    #[test]
    fn a_stale_result_does_not_cover_its_window() {
        let mut t = CoverageTracker::new(ms(33), ms(66), Instant::ZERO, ms(99));
        t.record_delivery(at(10), at(0));
        t.record_delivery(at(50), at(0)); // Alter 50ms, frisch genug
        t.record_delivery(at(90), at(0)); // Alter 90ms, zu alt
        let c = t.finish();
        assert_eq!(c.covered, 2, "das dritte Fenster bleibt unabgedeckt");
        assert_eq!(c.delivered, 3, "geliefert wurde trotzdem dreimal");
    }

    /// Ein leergeraeumter Stream faellt sofort auf: viele Supersessions ohne
    /// Auslieferungen erzeugen unabgedeckte Perioden.
    #[test]
    fn dropping_everything_scores_zero_coverage() {
        let t = CoverageTracker::new(ms(33), ms(66), Instant::ZERO, ms(330));
        let c = t.finish();
        assert_eq!(c.covered, 0);
        assert_eq!(c.total, 10);
        assert_eq!(c.uncovered_permille(), 1_000);
    }

    #[test]
    fn several_results_in_one_window_count_once() {
        let mut t = CoverageTracker::new(ms(33), ms(66), Instant::ZERO, ms(99));
        t.record_delivery(at(5), at(0));
        t.record_delivery(at(10), at(5));
        t.record_delivery(at(20), at(15));
        let c = t.finish();
        assert_eq!(c.covered, 1, "Abdeckung ist keine Durchsatzmetrik");
        assert_eq!(c.delivered, 3);
    }

    #[test]
    fn aoi_percentiles_are_reported() {
        let mut t = CoverageTracker::new(ms(33), ms(200), Instant::ZERO, ms(3_300));
        for n in 0..100_u64 {
            t.record_delivery(at(n * 33 + n), at(n * 33));
        }
        let c = t.finish();
        assert!(c.aoi_p50_ns < c.aoi_p99_ns);
        assert_eq!(c.aoi_p99_ns, 99_000_000);
    }
}
