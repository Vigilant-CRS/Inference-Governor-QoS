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

/// Die Zwischenergebnisse der Verbrauchersicht.
#[derive(Debug, Default, Clone, Copy)]
struct ConsumerView {
    covered: u64,
    no_result: u64,
    longest_miss_run: u64,
    longest_gap_ns: u64,
    mean_aoi_ns: u64,
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

    // ---------------------------------------------------------------------
    // Verbrauchersicht (NV-01)
    //
    // Die Groessen darueber messen **Auslieferungen**: ein Fenster gilt als
    // abgedeckt, wenn in ihm etwas Frisches ankam. Die Groessen hier messen,
    // was der Regler zum Abtastzeitpunkt tatsaechlich **vorliegen** hat —
    // auch ein Ergebnis aus dem vorigen Fenster, wenn es noch frisch genug
    // ist. Beide stehen nebeneinander, damit vorhandene Vergleichszahlen
    // reproduzierbar bleiben und nicht stillschweigend umgedeutet werden.
    // ---------------------------------------------------------------------
    /// Abtastzeitpunkte, an denen ein hinreichend frisches Ergebnis vorlag.
    ///
    /// Auch ein Ergebnis aus einem frueheren Fenster zaehlt, solange sein
    /// Alter unter `max_age` liegt. Genau das ist die Frage des Reglers: nicht
    /// „kam gerade etwas an", sondern „habe ich etwas Brauchbares".
    pub consumer_covered: u64,
    /// Abtastzeitpunkte, an denen ueberhaupt kein Ergebnis vorlag.
    ///
    /// Getrennt von „vorhanden, aber zu alt": das eine ist ein Anlaufzustand
    /// oder ein Totalausfall, das andere ein Frischeproblem. Sie zusammen zu
    /// zaehlen verwischt zwei verschiedene Fehlerbilder.
    pub no_result: u64,
    /// Die laengste Folge aufeinanderfolgender unabgedeckter Abtastzeitpunkte.
    ///
    /// Der Grund, warum eine Miss-Rate allein nichts taugt: zehn verstreute
    /// Ausfaelle und ein Block von zehn haben dieselbe Rate und voellig
    /// verschiedene Folgen fuer einen Regler.
    pub longest_miss_run: u64,
    /// Die laengste zusammenhaengende Zeit ohne brauchbares Ergebnis.
    ///
    /// Exakt aus dem Lieferprotokoll berechnet, nicht aus Fenstern gerundet:
    /// eine Versorgungsluecke haelt sich nicht an Periodengrenzen.
    pub longest_gap_ns: u64,
    /// Zeitgewichtetes mittleres Informationsalter ueber das Messfenster.
    ///
    /// Das Integral des Alters ueber die Zeit, geteilt durch die Dauer — die
    /// Groesse, die die Fachliteratur „Age of Information" nennt. Die
    /// Perzentile oben sind Antwortalter und etwas anderes.
    pub mean_aoi_ns: u64,
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

    /// Der Anteil der Abtastzeitpunkte ohne brauchbares Ergebnis, in Promille
    /// (Verbrauchersicht, NV-01).
    ///
    /// Anders als [`Coverage::uncovered_permille`] fragt diese Groesse nicht,
    /// ob **in** einem Fenster etwas ankam, sondern ob am Ende des Fensters
    /// etwas Frisches **vorliegt**. Die Fensterfrage kippt, wenn Laufzeit und
    /// Periode fast gleich lang sind: eine Lieferung knapp nach der
    /// Fenstergrenze laesst ein Fenster leer und fuellt das naechste doppelt,
    /// ohne dass dem Verbraucher je etwas fehlte — und unter Saettigung zaehlt
    /// sie ein Fenster als abgedeckt, dessen Ergebnis beim Abtasten schon zu
    /// alt ist (docs/analysis/bursts-and-frontier.md).
    #[must_use]
    pub fn consumer_uncovered_permille(&self) -> u64 {
        if self.total == 0 {
            return 0;
        }
        self.total
            .saturating_sub(self.consumer_covered)
            .saturating_mul(1_000)
            / self.total
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
        // Ausserhalb des Messfensters gibt es nichts zu messen. Eine Lieferung
        // nach Fensterende in Alter und Peak-AoI aufzunehmen erfindet einen
        // Spitzenwert aus einer Zeit, ueber die die Messung nichts aussagt —
        // und ein Nachzuegler nach dem Ende eines Lastfensters ist der
        // Normalfall, nicht die Ausnahme.
        if completion < self.start || completion > self.end {
            return;
        }
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
        let consumer = self.consumer_view();
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
            consumer_covered: consumer.covered,
            no_result: consumer.no_result,
            longest_miss_run: consumer.longest_miss_run,
            longest_gap_ns: consumer.longest_gap_ns,
            mean_aoi_ns: consumer.mean_aoi_ns,
        }
    }

    /// Der Verlauf des Informationsstands ueber das Messfenster.
    ///
    /// Ein Paar je Ereignis: ab welchem Zeitpunkt welche Capture-Zeit die
    /// neueste vorliegende ist. Eine spaet gelieferte, aber aeltere Aufnahme
    /// verjuengt nichts — deshalb steigt die zweite Komponente monoton.
    fn timeline(&self) -> Vec<(u64, u64)> {
        let mut sorted = self.deliveries.clone();
        sorted.sort_unstable();
        let mut newest = 0_u64;
        let mut out = Vec::with_capacity(sorted.len());
        for (completion, generation) in sorted {
            newest = newest.max(generation);
            out.push((completion, newest));
        }
        out
    }

    /// Die Verbrauchersicht: was liegt zum Abtastzeitpunkt vor?
    ///
    /// Abgetastet wird am **Ende** jeder Periode — das ist der Zeitpunkt, an
    /// dem ein Regelzyklus sein Eingangsdatum braucht. Ein Ergebnis aus einem
    /// frueheren Fenster zaehlt mit, solange es frisch genug ist.
    fn consumer_view(&self) -> ConsumerView {
        let timeline = self.timeline();
        let start = self.start.as_nanos();
        let end = self.end.as_nanos();
        let period = self.period.as_nanos().max(1);
        let max_age = self.max_age.as_nanos();

        let mut view = ConsumerView::default();
        let mut run = 0_u64;
        let mut cursor = 0_usize;
        let mut newest: Option<u64> = None;

        for window in 0..self.covered.len() {
            let sample =
                start.saturating_add((window as u64).saturating_add(1).saturating_mul(period));
            while let Some((completion, generation)) = timeline.get(cursor).copied() {
                if completion > sample {
                    break;
                }
                newest = Some(newest.map_or(generation, |n: u64| n.max(generation)));
                cursor = cursor.saturating_add(1);
            }

            let covered = match newest {
                None => {
                    view.no_result = view.no_result.saturating_add(1);
                    false
                }
                Some(generation) => sample.saturating_sub(generation) <= max_age,
            };

            if covered {
                view.covered = view.covered.saturating_add(1);
                run = 0;
            } else {
                run = run.saturating_add(1);
                view.longest_miss_run = view.longest_miss_run.max(run);
            }
        }

        view.longest_gap_ns = Self::longest_gap(&timeline, start, end, max_age);
        view.mean_aoi_ns = Self::mean_aoi(&timeline, start, end);
        view
    }

    /// Die laengste zusammenhaengende Zeit ohne brauchbares Ergebnis.
    ///
    /// Aus dem Lieferprotokoll, nicht aus Fenstern: eine Versorgungsluecke
    /// haelt sich nicht an Periodengrenzen, und auf ein Vielfaches der Periode
    /// gerundet verschwindet gerade der Fall, der weh tut.
    fn longest_gap(timeline: &[(u64, u64)], start: u64, end: u64, max_age: u64) -> u64 {
        // Vor der ersten Lieferung liegt nichts vor.
        let mut usable_until = start;
        let mut longest = 0_u64;

        for (completion, generation) in timeline.iter().copied() {
            // Brauchbar ist ein Ergebnis von seiner **Auslieferung** bis zum
            // Ablauf seines Hoechstalters, gerechnet ab der Aufnahme. Faellt
            // der Ablauf vor die Auslieferung, war es nie brauchbar: es kam
            // bereits zu alt an und hat keinen einzigen Moment versorgt
            // (Review R03).
            //
            // Ohne diese Pruefung schob eine veraltete Lieferung `usable_until`
            // trotzdem nach vorn und verkuerzte die gemeldete Luecke — 150 ms
            // ohne ein einziges brauchbares Ergebnis erschienen als 90 ms.
            // Genau die Zahl, mit der dieses Projekt argumentiert, fiel damit
            // zu gut aus.
            let expires = generation.saturating_add(max_age);
            if expires <= completion {
                continue;
            }
            if completion > usable_until {
                longest = longest.max(completion.saturating_sub(usable_until));
            }
            usable_until = usable_until.max(expires);
        }
        if end > usable_until {
            longest = longest.max(end.saturating_sub(usable_until));
        }
        longest
    }

    /// Das zeitgewichtete mittlere Informationsalter.
    ///
    /// Das Integral des Alters ueber die Zeit, geteilt durch die Dauer.
    /// Zwischen zwei Ereignissen waechst das Alter linear; das Integral ueber
    /// ein solches Stueck ist deshalb geschlossen berechenbar.
    fn mean_aoi(timeline: &[(u64, u64)], start: u64, end: u64) -> u64 {
        let duration = end.saturating_sub(start);
        if duration == 0 {
            return 0;
        }
        // Vor der ersten Lieferung wird das Alter ab Messbeginn gezaehlt.
        let mut reference = start;
        let mut at = start;
        let mut integral = 0_u128;

        let segment = |from: u64, to: u64, reference: u64| -> u128 {
            let a = u128::from(from.saturating_sub(reference));
            let b = u128::from(to.saturating_sub(reference));
            b.saturating_mul(b).saturating_sub(a.saturating_mul(a)) / 2
        };

        for (completion, generation) in timeline.iter().copied() {
            let until = completion.min(end);
            if until > at {
                integral = integral.saturating_add(segment(at, until, reference));
                at = until;
            }
            reference = reference.max(generation);
            if at >= end {
                break;
            }
        }
        if end > at {
            integral = integral.saturating_add(segment(at, end, reference));
        }
        u64::try_from(integral / u128::from(duration)).unwrap_or(u64::MAX)
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
    /// Eine Lieferung nach Fensterende gehoert nicht in die Messung.
    ///
    /// Sonst meldet ein 100-ms-Fenster eine Spitze von 1000 ms, die aus einer
    /// Zeit stammt, ueber die es nichts aussagt.
    #[test]
    fn a_delivery_outside_the_window_is_not_measured() {
        let mut t = CoverageTracker::new(ms(10), ms(1_000), Instant::ZERO, ms(100));
        t.record_delivery(at(1_000), at(0));
        let c = t.finish();
        assert_eq!(c.delivered, 0, "sie zaehlt nicht als Lieferung");
        assert_eq!(
            c.peak_aoi_ns, 100_000_000,
            "und die Spitze bleibt die Fensterlaenge"
        );
    }

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

    // -----------------------------------------------------------------
    // NV-01: Verbrauchersicht
    // -----------------------------------------------------------------

    /// Gleiche Miss-Rate, verschiedene Burststruktur — und das muss man sehen.
    ///
    /// Zehn verstreute Ausfaelle und ein Block von zehn haben dieselbe Rate.
    /// Fuer einen Regler sind sie voellig verschieden: das eine faengt die
    /// Regelung ab, das andere ist ein Blindflug ueber eine Drittelsekunde.
    /// Eine Kennzahl, die beides gleich bewertet, verschweigt genau das.
    #[test]
    fn the_same_miss_rate_with_different_bursts_is_distinguished() {
        // 20 Fenster a 10 ms. Ergebnis ist 5 ms nach der Aufnahme da und
        // hoechstens 15 ms brauchbar.
        let build = |deliver: &[u64]| {
            let mut t = CoverageTracker::new(ms(10), ms(15), Instant::ZERO, ms(200));
            for w in deliver {
                // Aufnahme zu Fensterbeginn, Auslieferung 5 ms spaeter.
                t.record_delivery(at(w * 10 + 5), at(w * 10));
            }
            t.finish()
        };

        // Verstreut: jedes zweite Fenster faellt aus.
        let scattered = build(&[0, 2, 4, 6, 8, 10, 12, 14, 16, 18]);
        // Am Stueck: die ersten zehn Fenster geliefert, dann Schweigen.
        let bursty = build(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

        assert_eq!(
            scattered.delivered, bursty.delivered,
            "gleich viele Auslieferungen"
        );
        assert!(
            scattered.longest_miss_run < bursty.longest_miss_run,
            "verstreut {} gegen am Stueck {}",
            scattered.longest_miss_run,
            bursty.longest_miss_run
        );
        assert!(
            scattered.longest_gap_ns < bursty.longest_gap_ns,
            "und die laengste Versorgungsluecke ebenso"
        );
    }

    /// Ein noch frisches Ergebnis aus dem vorigen Fenster zaehlt weiter.
    ///
    /// Die Lieferzaehlung markiert nur das Fenster, in dem etwas ankam. Der
    /// Regler fragt anders: nicht „kam gerade etwas an", sondern „habe ich
    /// etwas Brauchbares". Bei einem Modell, das langsamer liefert als der
    /// Regeltakt, sind das zwei sehr verschiedene Zahlen.
    #[test]
    fn a_still_fresh_earlier_result_counts_in_the_next_window_too() {
        // Regeltakt 10 ms, brauchbar bis 25 ms Alter, geliefert alle 20 ms.
        let mut t = CoverageTracker::new(ms(10), ms(25), Instant::ZERO, ms(100));
        for k in 0..5_u64 {
            t.record_delivery(at(k * 20 + 5), at(k * 20));
        }
        let c = t.finish();

        assert_eq!(c.total, 10);
        assert!(
            c.consumer_covered > c.covered,
            "Verbrauchersicht {} gegen Lieferfenster {}",
            c.consumer_covered,
            c.covered
        );
        assert_eq!(c.no_result, 0, "es lag immer etwas vor");
    }

    /// „Nichts vorhanden" und „vorhanden, aber zu alt" sind zwei Fehlerbilder.
    #[test]
    fn nothing_available_is_counted_apart_from_too_old() {
        // Erste Lieferung erst nach 50 ms: davor liegt gar nichts vor.
        let mut t = CoverageTracker::new(ms(10), ms(15), Instant::ZERO, ms(100));
        t.record_delivery(at(55), at(50));
        let c = t.finish();

        assert_eq!(c.no_result, 5, "die ersten fuenf Abtastungen ohne Ergebnis");
        assert!(c.consumer_covered >= 1);
    }

    /// Schweigen nach der letzten Lieferung faellt in die Luecke.
    #[test]
    fn silence_after_the_last_delivery_counts_towards_the_gap() {
        let mut t = CoverageTracker::new(ms(10), ms(20), Instant::ZERO, ms(500));
        t.record_delivery(at(5), at(0));
        let c = t.finish();
        // Brauchbar bis 20 ms, danach 480 ms Schweigen.
        assert_eq!(c.longest_gap_ns, 480_000_000);
        assert!(c.mean_aoi_ns > 200_000_000, "das Alter waechst weiter");
    }

    /// Die Legacy-Zahlen bleiben unveraendert.
    ///
    /// Neue Metriken kommen **zusaetzlich**. Bestehende Vergleichszahlen still
    /// umzudeuten waere schlimmer, als sie gar nicht zu verbessern.
    #[test]
    fn the_legacy_numbers_are_untouched() {
        let mut t = CoverageTracker::new(ms(33), ms(66), Instant::ZERO, ms(99));
        t.record_delivery(at(10), at(0));
        let c = t.finish();
        assert_eq!(c.total, 3);
        assert_eq!(c.covered, 1, "unveraendert: ein Lieferfenster");
        assert_eq!(c.delivered, 1);
    }

    #[test]
    fn aoi_percentiles_are_reported() {
        // Fenster gross genug fuer alle 100 Lieferungen: die letzte liegt bei
        // 99*33+99 = 3366 ms. Vorher endete das Fenster bei 3300 ms, und der
        // Test mass unbemerkt eine Lieferung ausserhalb mit.
        let mut t = CoverageTracker::new(ms(33), ms(200), Instant::ZERO, ms(3_400));
        for n in 0..100_u64 {
            t.record_delivery(at(n * 33 + n), at(n * 33));
        }
        let c = t.finish();
        assert!(c.response_age_p50_ns < c.response_age_p99_ns);
        assert_eq!(c.response_age_p99_ns, 99_000_000);
    }

    /// Eine veraltete Lieferung verkuerzt keine Versorgungsluecke (Review R03).
    ///
    /// Beide Lieferungen kommen 50 ms nach ihrer Aufnahme an, bei einem
    /// Hoechstalter von 10 ms. Keine von beiden war je brauchbar — in den
    /// ganzen 150 ms lag zu keinem Zeitpunkt ein verwertbares Ergebnis vor.
    ///
    /// Gemeldet wurden 90 ms. Der Grund: die Lieferung schob die
    /// Brauchbarkeitsgrenze nach vorn, ohne dass geprueft wurde, ob sie
    /// ueberhaupt jemals gegolten hat. Genau die Zahl, mit der dieses Projekt
    /// ueber Zuverlaessigkeit argumentiert, fiel damit zu gut aus.
    #[test]
    fn a_stale_delivery_does_not_shorten_a_continuous_supply_gap() {
        let mut tracker = CoverageTracker::new(ms(10), ms(10), at(0), ms(150));
        tracker.record_delivery(at(50), at(0));
        tracker.record_delivery(at(100), at(50));
        let coverage = tracker.finish();

        assert_eq!(coverage.consumer_covered, 0, "kein Zyklus war versorgt");
        assert_eq!(
            coverage.longest_gap_ns,
            ms(150).as_nanos(),
            "und die Luecke ist das ganze Messfenster"
        );
    }

    /// Die Gegenprobe: eine rechtzeitige Lieferung schliesst die Luecke sehr
    /// wohl.
    ///
    /// Sonst pruefte der Test darueber nur, dass die Rechnung ueberall
    /// dieselbe Zahl liefert.
    #[test]
    fn a_timely_delivery_still_closes_the_gap() {
        let mut tracker = CoverageTracker::new(ms(10), ms(10), at(0), ms(150));
        tracker.record_delivery(at(50), at(45));
        let coverage = tracker.finish();
        assert!(
            coverage.longest_gap_ns < ms(150).as_nanos(),
            "ein frisches Ergebnis versorgt seinen Zeitraum: {} ns",
            coverage.longest_gap_ns
        );
        assert!(coverage.consumer_covered > 0);
    }
}
