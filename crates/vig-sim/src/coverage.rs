//! Die Erfolgsmetriken des Gate-S-Vergleichs (ADR-0005, Spec 18).
//!
//! ## Warum nicht „Deadline Misses pro Request"
//!
//! Bei `LATEST` ist diese Groesse nicht wohldefiniert und im Vergleich
//! systematisch verzerrt. Zaehlt man supersedierte Requests als Miss, verliert
//! Vigilant per Konstruktion; zaehlt man sie nicht, sinkt die Rate trivial,
//! weil Vigilant den Nenner verkleinert — eine Politik, die 90 % aller Frames
//! verwirft, erreicht eine nahezu perfekte Miss-Rate, ohne dem Roboter zu
//! nuetzen.
//!
//! Deshalb misst Gate S periodenbezogen: **hatte das System in diesem
//! Regelzyklus eine hinreichend aktuelle Wahrnehmung?** Diese Frage ist gegen
//! beide Manipulationen immun und ist zugleich die Frage, die der Roboter
//! tatsaechlich stellt.

use vig_core::{Duration, Instant};

/// Zaehlt abgedeckte und unabgedeckte Perioden eines Streams.
#[derive(Debug, Clone)]
pub struct CoverageTracker {
    period: Duration,
    max_age: Duration,
    start: Instant,
    end: Instant,
    covered: Vec<bool>,
    ages_ns: Vec<u64>,
    /// Auslieferungen als (Auslieferungszeit, Capture-Zeit).
    ///
    /// Fuer die zeitbezogene Peak-AoI: sie braucht die Luecken *zwischen* den
    /// Auslieferungen, nicht nur deren Alter.
    ///
    /// Waechst mit der Zahl der Auslieferungen, wie `ages_ns` auch. Ein
    /// Tracker lebt genau ein Messfenster lang; im Dauerlauf sind das rund
    /// 3.000 Auslieferungen je Fenster und Strom, also einige Dutzend
    /// Kilobyte. Das ist gegenueber dem gemessenen Speicherverlauf des
    /// Prozesses nicht sichtbar — waere der Tracker dagegen ueber die volle
    /// Laufzeit offen, verfaelschte er genau die Leckmessung, fuer die der
    /// Dauerlauf existiert.
    deliveries: Vec<(u64, u64)>,
}

/// Das Ergebnis der Coverage-Messung eines Streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    /// Perioden, in denen ein hinreichend frisches Ergebnis vorlag.
    pub covered: u64,
    /// Perioden insgesamt.
    pub total: u64,
    /// Alter der **ausgelieferten** Ergebnisse, 50. Perzentil, in Nanosekunden.
    ///
    /// Bewusst nicht „AoI" genannt: die Stichprobe besteht aus Auslieferungen,
    /// nicht aus Zeitpunkten. Wer nur einmal liefert und danach schweigt, hat
    /// hier weiterhin einen guten Wert — der Konsument nicht. Fuer die
    /// zeitbezogene Groesse siehe [`Coverage::peak_aoi_ns`].
    pub response_age_p50_ns: u64,
    /// Alter der ausgelieferten Ergebnisse, 95. Perzentil.
    pub response_age_p95_ns: u64,
    /// Alter der ausgelieferten Ergebnisse, 99. Perzentil.
    pub response_age_p99_ns: u64,
    /// Peak Age of Information ueber das gesamte Messfenster, in Nanosekunden.
    ///
    /// Das hoechste Alter, das die beim Konsumenten vorliegende Information im
    /// Messzeitraum je erreicht hat. Sie altert auch dann weiter, wenn nichts
    /// geliefert wird — deshalb ist dies die Groesse, die eine Versorgungs-
    /// luecke sichtbar macht. Ein Strom ohne jede Auslieferung erreicht hier
    /// die volle Fensterlaenge, waehrend die Perzentile ueber Auslieferungen
    /// mangels Stichprobe null blieben.
    pub peak_aoi_ns: u64,
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
            end: start.checked_add(duration).unwrap_or(start),
            covered: vec![false; windows],
            ages_ns: Vec::new(),
            deliveries: Vec::new(),
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
        self.deliveries
            .push((completion.as_nanos(), generation.as_nanos()));
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
            response_age_p50_ns: pick(50),
            response_age_p95_ns: pick(95),
            response_age_p99_ns: pick(99),
            peak_aoi_ns: self.peak_aoi_ns(),
            delivered: self.ages_ns.len() as u64,
        }
    }

    /// Die Peak Age of Information ueber das Messfenster.
    ///
    /// Zwischen zwei Auslieferungen altert die vorliegende Information linear.
    /// Ihr Hoechstwert liegt deshalb immer unmittelbar **vor** einer
    /// Auslieferung — dort ist sie so alt wie die Zeit seit der Capture-Zeit
    /// der vorigen — und am Ende des Fensters. Vor der ersten Auslieferung
    /// liegt ueberhaupt nichts vor; gemessen wird dann ab Messbeginn.
    fn peak_aoi_ns(&self) -> u64 {
        let mut sorted = self.deliveries.clone();
        sorted.sort_unstable();

        let start = self.start.as_nanos();
        let end = self.end.as_nanos();
        // Ohne jede Auslieferung ist die Information ueber das ganze Fenster
        // nicht vorhanden — das ist der schlechteste Fall, nicht der beste.
        let mut newest_generation = start;
        let mut peak = 0_u64;

        for (completion, generation) in &sorted {
            // Unmittelbar vor dieser Auslieferung galt noch die vorige.
            peak = peak.max(completion.saturating_sub(newest_generation));
            // Eine spaet gelieferte, aber aeltere Aufnahme verjuengt nichts.
            newest_generation = newest_generation.max(*generation);
        }
        peak.max(end.saturating_sub(newest_generation))
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

    /// Der Punkt, an dem sich Antwortalter und AoI trennen.
    ///
    /// Eine einzige schnelle Antwort und danach fast eine Sekunde Funkstille
    /// sieht ueber die Auslieferungen gemessen tadellos aus. Beim Konsumenten
    /// altert die Information in dieser Zeit trotzdem weiter — genau das
    /// misst die Peak-AoI.
    #[test]
    fn peak_aoi_counts_the_time_without_deliveries() {
        let mut t = CoverageTracker::new(ms(10), ms(1_000), Instant::ZERO, ms(1_000));
        t.record_delivery(at(1), at(0));
        let c = t.finish();
        assert_eq!(
            c.response_age_p95_ns, 1_000_000,
            "die eine Antwort war 1 ms alt"
        );
        assert_eq!(
            c.peak_aoi_ns, 1_000_000_000,
            "am Fensterende ist die Aufnahme von t=0 genau 1000 ms alt"
        );
    }

    /// Ein Strom ohne jede Auslieferung ist der schlechteste Fall, nicht der
    /// beste. Ueber Auslieferungen gemittelt bliebe er mangels Stichprobe
    /// null — die Peak-AoI zeigt die volle Fensterlaenge.
    #[test]
    fn a_stream_without_any_delivery_has_maximal_peak_aoi() {
        let t = CoverageTracker::new(ms(10), ms(1_000), Instant::ZERO, ms(500));
        let c = t.finish();
        assert_eq!(c.delivered, 0);
        assert_eq!(c.response_age_p95_ns, 0);
        assert_eq!(c.peak_aoi_ns, 500_000_000);
    }

    /// Regelmaessige Lieferung haelt die Peak-AoI bei rund einer Periode plus
    /// Laufzeit — der Fall, in dem beide Groessen dasselbe sagen.
    #[test]
    fn steady_delivery_keeps_peak_aoi_near_one_period() {
        let mut t = CoverageTracker::new(ms(10), ms(100), Instant::ZERO, ms(100));
        for n in 0..10_u64 {
            t.record_delivery(at(n * 10 + 2), at(n * 10));
        }
        let c = t.finish();
        // Letzte Aufnahme bei t=90, Fensterende bei t=100.
        assert_eq!(c.peak_aoi_ns, 12_000_000);
    }

    /// Eine spaet eingetroffene, aber aeltere Aufnahme verjuengt die
    /// vorliegende Information nicht.
    #[test]
    fn a_late_older_frame_does_not_lower_the_age() {
        let mut t = CoverageTracker::new(ms(10), ms(1_000), Instant::ZERO, ms(100));
        t.record_delivery(at(50), at(40));
        t.record_delivery(at(60), at(10));
        let c = t.finish();
        // Neueste vorliegende Aufnahme bleibt die von t=40.
        assert_eq!(c.peak_aoi_ns, 60_000_000);
    }

    #[test]
    fn aoi_percentiles_are_reported() {
        let mut t = CoverageTracker::new(ms(33), ms(200), Instant::ZERO, ms(3_300));
        for n in 0..100_u64 {
            t.record_delivery(at(n * 33 + n), at(n * 33));
        }
        let c = t.finish();
        assert!(c.response_age_p50_ns < c.response_age_p99_ns);
        assert_eq!(c.response_age_p99_ns, 99_000_000);
    }
}
