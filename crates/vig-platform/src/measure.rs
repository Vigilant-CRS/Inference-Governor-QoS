//! Der Messpfad: wie gemessen wird, damit die Zahlen etwas heissen (NV-05).
//!
//! ## Warum das nicht „einfach messen" ist
//!
//! Das bisherige Profilwerkzeug macht das Naheliegende: Anfrage schicken, Zeit
//! nehmen, wiederholen. Vier Dinge gehen dabei still schief, und jedes davon
//! verschiebt das Ergebnis in dieselbe Richtung — es sieht besser aus, als es
//! ist.
//!
//! 1. **Der Takt wandert.** Wer nach jeder Antwort eine Periode wartet, misst
//!    bei einer langsamen Antwort automatisch seltener. Die Last, unter der
//!    gemessen wird, sinkt dann genau dann, wenn es interessant wuerde.
//!    [`ReleaseSchedule`] gibt deshalb auf einem **absoluten** Raster frei:
//!    ein Ueberzug verschiebt nichts, er laesst Rasterpunkte aus, und die
//!    ausgelassenen werden gezaehlt.
//! 2. **Fehler verschwinden.** Ein abgebrochener Aufruf, der aus der
//!    Messreihe faellt, verbessert das Quantil. [`CellRun`] fuehrt Erfolge,
//!    Ueberzuege, Fehler und ausgelassene Rasterpunkte getrennt und prueft,
//!    dass ihre Summe die Zahl der Freigaben ergibt.
//! 3. **Die Uhr ist zu grob.** Eine Uhr mit Millisekundenaufloesung kann eine
//!    Inferenz von 4 ms nicht messen; sie kann sie nur runden.
//!    [`verify_clock`] misst die Aufloesung, bevor gemessen wird.
//! 4. **Die Hardware wechselt mitten in der Reihe.** Faellt der Takt oder
//!    kommt ein thermisches Limit hinzu, beschreiben die Zahlen davor und
//!    danach zwei verschiedene Maschinen. Der Mittelwert beschreibt keine.
//!    [`CellRun::discard`] wirft die betroffene Zelle weg — **mit Grund**,
//!    nicht stillschweigend.
//!
//! ## Was hier nicht passiert
//!
//! Dieses Modul **loggt nicht** und ruft keine Uhr von sich aus ab. Es nimmt
//! Zeitpunkte entgegen und gibt Daten zurueck. Ein `tracing`-Aufruf im
//! gemessenen Pfad kostet je nach Subscriber zwischen hundert Nanosekunden
//! und mehreren Mikrosekunden — bei einer Inferenz von 4 ms waere das bis zu
//! einem Promille Messfehler, den niemand im Ergebnis sieht.
//!
//! Es fuehrt auch keine Messung aus. Wer misst, entscheidet der Aufrufer;
//! dieses Modul sagt ihm nur, **wann** freizugeben ist und **was** das
//! Ergebnis wert ist.

use crate::snapshot::{HardwareSnapshot, diff};
use serde::{Deserialize, Serialize};

/// Ein Betriebspunkt, unter dem gemessen wird.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CellId {
    /// Das vermessene Backendmodell.
    pub model: String,
    /// Wie viele Anfragen gleichzeitig unterwegs sind.
    pub concurrency: u32,
    /// Die Batchgroesse.
    pub batch: u32,
}

/// Wie eine einzelne Freigabe ausging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// Fertig geworden, bevor der naechste Rasterpunkt faellig war.
    Completed {
        /// Die gemessene Zeit in Nanosekunden.
        latency_ns: u64,
    },
    /// Fertig geworden, aber zu spaet fuer den naechsten Rasterpunkt.
    ///
    /// Zaehlt in die Latenzverteilung — der Wert ist gemessen und richtig —
    /// **und** in die Ueberzugszahl. Ihn nur als Latenz zu fuehren wuerde
    /// verschweigen, dass die Messreihe ihren eigenen Takt nicht gehalten hat.
    Overrun {
        /// Die gemessene Zeit in Nanosekunden.
        latency_ns: u64,
    },
    /// Der Aufruf ist fehlgeschlagen.
    Failed {
        /// Warum.
        reason: String,
    },
    /// Ein Rasterpunkt, der ausgelassen wurde, weil der vorige ueberzog.
    ///
    /// Kein Fehler und kein Erfolg: ein Zeitpunkt, an dem nichts freigegeben
    /// wurde. Ihn wegzulassen wuerde eine Messreihe mit halber Rate wie eine
    /// mit voller aussehen lassen.
    Skipped,
}

/// Ein absolutes Freigaberaster.
///
/// Die Freigabezeitpunkte sind `start + k * period`, k = 0, 1, 2, …. Sie
/// haengen nicht davon ab, wann die vorige Antwort kam. Genau das ist der
/// Unterschied zu „nach jeder Antwort eine Periode warten": dort sinkt die
/// Rate mit der Laufzeit, und die Messung wird gnaedig, sobald sie hart
/// werden muesste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSchedule {
    start_ns: u64,
    period_ns: u64,
    /// Der Index des naechsten Rasterpunkts.
    next_index: u64,
}

impl ReleaseSchedule {
    /// Ein Raster ab `start_ns` mit dieser Periode.
    ///
    /// Eine Periode von null ergibt kein Raster; sie wird auf eine
    /// Nanosekunde angehoben, damit der Fortschritt nicht stehenbleibt.
    #[must_use]
    pub const fn new(start_ns: u64, period_ns: u64) -> Self {
        Self {
            start_ns,
            period_ns: if period_ns == 0 { 1 } else { period_ns },
            next_index: 0,
        }
    }

    /// Der Zeitpunkt des naechsten Rasterpunkts.
    #[must_use]
    pub const fn next_release_ns(&self) -> u64 {
        self.start_ns
            .saturating_add(self.period_ns.saturating_mul(self.next_index))
    }

    /// Der Index des naechsten Rasterpunkts.
    #[must_use]
    pub const fn next_index(&self) -> u64 {
        self.next_index
    }

    /// Nimmt den naechsten Rasterpunkt und rueckt weiter.
    #[must_use]
    pub const fn take(&mut self) -> u64 {
        let at = self.next_release_ns();
        self.next_index = self.next_index.saturating_add(1);
        at
    }

    /// Faengt bei `now_ns` wieder auf dem Raster auf.
    ///
    /// Gibt zurueck, wie viele Rasterpunkte dabei uebersprungen wurden. Das
    /// ist die Zahl, die nach einem Ueberzug als [`ReleaseOutcome::Skipped`]
    /// zu buchen ist — der naechste Freigabezeitpunkt bleibt auf dem Raster
    /// und wird **nicht** nach hinten geschoben.
    #[must_use]
    pub const fn catch_up(&mut self, now_ns: u64) -> u64 {
        if now_ns <= self.next_release_ns() {
            return 0;
        }
        let behind = now_ns.saturating_sub(self.start_ns);
        // Der erste Rasterpunkt, der nicht in der Vergangenheit liegt.
        let index = behind.div_ceil(self.period_ns);
        let skipped = index.saturating_sub(self.next_index);
        self.next_index = index;
        skipped
    }
}

/// Warum eine Zelle verworfen wurde.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum DiscardReason {
    /// Der Hardwarezustand hat sich waehrend der Messung geaendert.
    HardwareChanged {
        /// Welches Feld, und wie.
        detail: String,
    },
    /// Die Uhr taugt fuer diese Groessenordnung nicht.
    ClockTooCoarse {
        /// Die gemessene Aufloesung in Nanosekunden.
        resolution_ns: u64,
    },
    /// Zu wenige verwertbare Messwerte.
    TooFewSamples {
        /// Wie viele es wurden.
        got: usize,
        /// Wie viele noetig waren.
        needed: usize,
    },
    /// Zu viele Fehlschlaege, um der Reihe zu trauen.
    TooManyFailures {
        /// Wie viele.
        failures: u64,
        /// Von wie vielen Freigaben.
        releases: u64,
    },
    /// Ein vom Aufrufer genannter Grund.
    Other {
        /// Der Grund.
        detail: String,
    },
}

impl core::fmt::Display for DiscardReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::HardwareChanged { detail } => {
                write!(f, "Hardwarezustand geaendert: {detail}")
            }
            Self::ClockTooCoarse { resolution_ns } => write!(
                f,
                "Uhraufloesung {resolution_ns} ns zu grob fuer diese Messung"
            ),
            Self::TooFewSamples { got, needed } => {
                write!(f, "nur {got} verwertbare Messwerte, noetig sind {needed}")
            }
            Self::TooManyFailures { failures, releases } => {
                write!(f, "{failures} Fehlschlaege bei {releases} Freigaben")
            }
            Self::Other { detail } => write!(f, "{detail}"),
        }
    }
}

/// Die Buchhaltung einer Messzelle.
///
/// Der Speicher wird vorab belegt: eine Vergroesserung mitten in der Reihe
/// waere eine Allokation im gemessenen Pfad und damit ein Ausreisser, den
/// niemand als solchen erkennt.
#[derive(Debug, Clone)]
pub struct CellRun {
    cell: CellId,
    latencies: Vec<u64>,
    completed: u64,
    overruns: u64,
    failures: u64,
    skipped: u64,
    first_failure: Option<String>,
    discarded: Option<DiscardReason>,
}

impl CellRun {
    /// Eine Zelle mit vorbelegtem Speicher fuer `expected` Messwerte.
    #[must_use]
    pub fn with_capacity(cell: CellId, expected: usize) -> Self {
        Self {
            cell,
            latencies: Vec::with_capacity(expected),
            completed: 0,
            overruns: 0,
            failures: 0,
            skipped: 0,
            first_failure: None,
            discarded: None,
        }
    }

    /// Der Betriebspunkt.
    #[must_use]
    pub const fn cell(&self) -> &CellId {
        &self.cell
    }

    /// Wie viele Messwerte ohne Nachbelegung passen.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.latencies.capacity()
    }

    /// Bucht das Ergebnis einer Freigabe.
    pub fn record(&mut self, outcome: ReleaseOutcome) {
        match outcome {
            ReleaseOutcome::Completed { latency_ns } => {
                self.completed = self.completed.saturating_add(1);
                self.latencies.push(latency_ns);
            }
            ReleaseOutcome::Overrun { latency_ns } => {
                self.overruns = self.overruns.saturating_add(1);
                self.latencies.push(latency_ns);
            }
            ReleaseOutcome::Failed { reason } => {
                self.failures = self.failures.saturating_add(1);
                if self.first_failure.is_none() {
                    self.first_failure = Some(reason);
                }
            }
            ReleaseOutcome::Skipped => {
                self.skipped = self.skipped.saturating_add(1);
            }
        }
    }

    /// Bucht mehrere ausgelassene Rasterpunkte auf einmal.
    pub fn record_skipped(&mut self, count: u64) {
        self.skipped = self.skipped.saturating_add(count);
    }

    /// Wie viele Rasterpunkte insgesamt behandelt wurden.
    #[must_use]
    pub const fn releases(&self) -> u64 {
        self.completed
            .saturating_add(self.overruns)
            .saturating_add(self.failures)
            .saturating_add(self.skipped)
    }

    /// Erfolgreiche Freigaben.
    #[must_use]
    pub const fn completed(&self) -> u64 {
        self.completed
    }

    /// Ueberzuege.
    #[must_use]
    pub const fn overruns(&self) -> u64 {
        self.overruns
    }

    /// Fehlschlaege.
    #[must_use]
    pub const fn failures(&self) -> u64 {
        self.failures
    }

    /// Ausgelassene Rasterpunkte.
    #[must_use]
    pub const fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Der erste genannte Fehlergrund.
    #[must_use]
    pub fn first_failure(&self) -> Option<&str> {
        self.first_failure.as_deref()
    }

    /// Ob die Zaehler zueinander passen.
    ///
    /// Die Latenzverteilung enthaelt genau die Freigaben, die eine Zeit
    /// geliefert haben: Erfolge und Ueberzuege. Fehlschlaege und ausgelassene
    /// Rasterpunkte haben keine — sie duerfen deshalb nicht aus dem Nenner
    /// verschwinden, sondern stehen daneben.
    #[must_use]
    pub fn denominators_agree(&self) -> bool {
        let timed = self.completed.saturating_add(self.overruns);
        u64::try_from(self.latencies.len()).is_ok_and(|n| n == timed)
    }

    /// Verwirft diese Zelle mit einem Grund.
    ///
    /// Ein zweiter Grund ueberschreibt den ersten nicht: der erste ist der,
    /// der die Messung ungueltig gemacht hat.
    pub fn discard(&mut self, reason: DiscardReason) {
        if self.discarded.is_none() {
            self.discarded = Some(reason);
        }
    }

    /// Warum diese Zelle verworfen wurde, falls sie es wurde.
    #[must_use]
    pub const fn discarded(&self) -> Option<&DiscardReason> {
        self.discarded.as_ref()
    }

    /// Ob die Zelle verwertbar ist.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        self.discarded.is_none()
    }

    /// Prueft die Zelle gegen Mindestanforderungen und verwirft sie sonst.
    ///
    /// `max_failure_permille` ist der Anteil an Freigaben, der fehlschlagen
    /// darf, bevor der Reihe nicht mehr zu trauen ist.
    pub fn qualify(&mut self, min_samples: usize, max_failure_permille: u64) {
        let releases = self.releases();
        if releases > 0 {
            let permille = self
                .failures
                .saturating_mul(1_000)
                .checked_div(releases)
                .unwrap_or(0);
            if permille > max_failure_permille {
                self.discard(DiscardReason::TooManyFailures {
                    failures: self.failures,
                    releases,
                });
            }
        }
        if self.latencies.len() < min_samples {
            self.discard(DiscardReason::TooFewSamples {
                got: self.latencies.len(),
                needed: min_samples,
            });
        }
    }

    /// Das Quantil ueber die Freigaben, die eine Zeit geliefert haben.
    ///
    /// `None` fuer eine verworfene Zelle: aus einer Messung, die als
    /// ungueltig erkannt wurde, noch eine Zahl zu ziehen waere genau das,
    /// wogegen `discard` da ist.
    #[must_use]
    pub fn quantile_ns(&self, percent: u32) -> Option<u64> {
        if self.discarded.is_some() || self.latencies.is_empty() {
            return None;
        }
        let mut sorted = self.latencies.clone();
        sorted.sort_unstable();
        let index = sorted
            .len()
            .saturating_mul(percent as usize)
            .checked_div(100)?
            .min(sorted.len().saturating_sub(1));
        sorted.get(index).copied()
    }

    /// Die Zusammenfassung fuer das Laufmanifest.
    #[must_use]
    pub fn summarise(&self) -> CellSummary {
        CellSummary {
            cell: self.cell.clone(),
            releases: self.releases(),
            completed: self.completed,
            overruns: self.overruns,
            failures: self.failures,
            skipped: self.skipped,
            p50_ns: self.quantile_ns(50),
            p95_ns: self.quantile_ns(95),
            p99_ns: self.quantile_ns(99),
            first_failure: self.first_failure.clone(),
            discarded: self.discarded.clone(),
        }
    }
}

/// Was eine Zelle ergeben hat.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CellSummary {
    /// Der Betriebspunkt.
    pub cell: CellId,
    /// Behandelte Rasterpunkte.
    pub releases: u64,
    /// Erfolge.
    pub completed: u64,
    /// Ueberzuege.
    pub overruns: u64,
    /// Fehlschlaege.
    pub failures: u64,
    /// Ausgelassene Rasterpunkte.
    pub skipped: u64,
    /// Median in Nanosekunden, falls die Zelle verwertbar ist.
    pub p50_ns: Option<u64>,
    /// 95-%-Quantil.
    pub p95_ns: Option<u64>,
    /// 99-%-Quantil.
    pub p99_ns: Option<u64>,
    /// Der erste Fehlergrund.
    pub first_failure: Option<String>,
    /// Warum die Zelle verworfen wurde.
    pub discarded: Option<DiscardReason>,
}

/// Was die Uhr taugt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockVerdict {
    /// Fein genug fuer die geplante Groessenordnung.
    Usable {
        /// Die gemessene Aufloesung in Nanosekunden.
        resolution_ns: u64,
    },
    /// Zu grob.
    TooCoarse {
        /// Die gemessene Aufloesung.
        resolution_ns: u64,
        /// Was noetig waere.
        needed_ns: u64,
    },
}

/// Misst die Aufloesung der monotonen Uhr.
///
/// Nimmt so lange Zeitpunkte, bis sich einer vom vorigen unterscheidet, und
/// wiederholt das ein paar Mal; die kleinste beobachtete Differenz ist die
/// Aufloesung. Grob, aber ausreichend fuer die Frage, um die es geht: ob eine
/// Inferenz von wenigen Millisekunden ueberhaupt messbar ist oder nur
/// gerundet wird.
///
/// Als brauchbar gilt eine Uhr, die mindestens hundertmal feiner aufloest als
/// die kuerzeste zu messende Dauer. Bei 4 ms sind das 40 Mikrosekunden — eine
/// Groessenordnung, die jede gaengige monotone Uhr deutlich unterbietet, und
/// die Schwelle faengt genau die Faelle, in denen sie es nicht tut.
#[must_use]
pub fn verify_clock(shortest_measured_ns: u64) -> ClockVerdict {
    const ROUNDS: usize = 8;
    let mut smallest = u64::MAX;
    for _ in 0..ROUNDS {
        let start = std::time::Instant::now();
        loop {
            let elapsed = start.elapsed();
            if elapsed.as_nanos() > 0 {
                let ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
                smallest = smallest.min(ns);
                break;
            }
        }
    }
    let needed_ns = shortest_measured_ns.checked_div(100).unwrap_or(1).max(1);
    if smallest > needed_ns {
        ClockVerdict::TooCoarse {
            resolution_ns: smallest,
            needed_ns,
        }
    } else {
        ClockVerdict::Usable {
            resolution_ns: smallest,
        }
    }
}

/// Ob sich der Hardwarezustand so geaendert hat, dass die Zelle nicht gilt.
///
/// Gibt den Grund zurueck, wenn ja. Nicht jede Aenderung zaehlt: der
/// Vergleich in [`crate::snapshot::diff`] meldet ohnehin nur Groessen, an
/// denen sich ein Laufzeitprofil entscheidet — Identitaet, Takt in groben
/// Stufen, Drosselgrund, Performance-State.
#[must_use]
pub fn hardware_invalidates(
    before: &HardwareSnapshot,
    after: &HardwareSnapshot,
) -> Option<DiscardReason> {
    let changes = diff(before, after);
    let first = changes.first()?;
    let gpu = first
        .gpu
        .map_or_else(|| "Plattform".to_owned(), |i| format!("GPU {i}"));
    Some(DiscardReason::HardwareChanged {
        detail: format!(
            "{gpu} {}: {} -> {} (zwischen {} und {})",
            first.field, first.before, first.after, first.from_ms, first.to_ms
        ),
    })
}

/// Das Manifest eines Messlaufs.
///
/// Ein Lauf ist eine Wiederholung. Zwei Laeufe mit denselben Zellen sind
/// vergleichbar; zweihundert Messwerte aus einem Prozessstart sind nicht
/// dasselbe wie zweihundert aus zweien, und dieses Manifest sagt, welches von
/// beidem vorliegt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RunManifest {
    /// Der wievielte Lauf, ab 1.
    pub run_index: u32,
    /// Wie viele Laeufe insgesamt geplant sind.
    pub total_runs: u32,
    /// Beginn, als Unix-Zeit in Millisekunden.
    pub started_at_ms: u64,
    /// Ende.
    pub finished_at_ms: u64,
    /// Die gemessene Uhraufloesung in Nanosekunden.
    pub clock_resolution_ns: u64,
    /// Der Hardwarezustand vor dem Lauf.
    pub hardware_before: HardwareSnapshot,
    /// Der Hardwarezustand nach dem Lauf.
    pub hardware_after: HardwareSnapshot,
    /// Was die Zellen ergeben haben.
    pub cells: Vec<CellSummary>,
}

impl RunManifest {
    /// Die verwertbaren Zellen.
    pub fn usable(&self) -> impl Iterator<Item = &CellSummary> {
        self.cells.iter().filter(|c| c.discarded.is_none())
    }

    /// Die verworfenen Zellen samt Grund.
    pub fn discarded(&self) -> impl Iterator<Item = &CellSummary> {
        self.cells.iter().filter(|c| c.discarded.is_some())
    }

    /// Ob in jeder Zelle die Zaehler zueinander passen.
    #[must_use]
    pub fn accounting_is_consistent(&self) -> bool {
        self.cells.iter().all(|c| {
            c.releases
                == c.completed
                    .saturating_add(c.overruns)
                    .saturating_add(c.failures)
                    .saturating_add(c.skipped)
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::collector::parse_output;

    const LINE: &str = "0, NVIDIA GeForce RTX 3070 Laptop GPU, 580.173.02, 8.6, 8192, \
                        80, 1740, 2100, 129.55, [N/A], P0, Disabled, \
                        0x0000000000000004";

    fn cell() -> CellId {
        CellId {
            model: "rfdetr".to_owned(),
            concurrency: 1,
            batch: 1,
        }
    }

    // -- Das Raster --------------------------------------------------------

    #[test]
    fn releases_lie_on_an_absolute_grid() {
        let mut schedule = ReleaseSchedule::new(1_000, 100);
        assert_eq!(schedule.take(), 1_000);
        assert_eq!(schedule.take(), 1_100);
        assert_eq!(schedule.take(), 1_200);
    }

    #[test]
    fn an_overrun_does_not_shift_the_grid() {
        // Der Kern von NV-05: nach einem Ueberzug bleibt der naechste
        // Freigabezeitpunkt auf dem Raster. Wuerde er verschoben, saenke die
        // Messrate genau dann, wenn die Messung hart wird.
        let mut schedule = ReleaseSchedule::new(0, 100);
        assert_eq!(schedule.take(), 0);
        // Die Antwort kam erst bei 350 statt bei 100.
        let skipped = schedule.catch_up(350);
        assert_eq!(skipped, 3, "die Rasterpunkte 100, 200 und 300");
        assert_eq!(
            schedule.next_release_ns(),
            400,
            "und der naechste liegt weiter auf dem Raster, nicht bei 350+100"
        );
    }

    #[test]
    fn catching_up_exactly_on_a_grid_point_skips_nothing_extra() {
        let mut schedule = ReleaseSchedule::new(0, 100);
        let _ = schedule.take();
        assert_eq!(schedule.catch_up(100), 0, "genau puenktlich");
        assert_eq!(schedule.next_release_ns(), 100);
    }

    #[test]
    fn a_schedule_that_is_not_behind_skips_nothing() {
        let mut schedule = ReleaseSchedule::new(1_000, 100);
        assert_eq!(schedule.catch_up(500), 0);
        assert_eq!(schedule.next_release_ns(), 1_000);
    }

    #[test]
    fn a_zero_period_does_not_stall_the_grid() {
        let mut schedule = ReleaseSchedule::new(0, 0);
        assert_eq!(schedule.take(), 0);
        assert_eq!(schedule.take(), 1);
    }

    #[test]
    fn a_long_stall_skips_many_points_without_overflowing() {
        let mut schedule = ReleaseSchedule::new(0, 1_000);
        let _ = schedule.take();
        let skipped = schedule.catch_up(u64::MAX);
        assert!(skipped > 0);
        assert!(schedule.next_release_ns() >= u64::MAX.saturating_sub(1_000));
    }

    // -- Die Buchhaltung ---------------------------------------------------

    #[test]
    fn every_release_lands_in_exactly_one_counter() {
        let mut run = CellRun::with_capacity(cell(), 4);
        run.record(ReleaseOutcome::Completed { latency_ns: 100 });
        run.record(ReleaseOutcome::Overrun { latency_ns: 400 });
        run.record(ReleaseOutcome::Failed {
            reason: "Unavailable".to_owned(),
        });
        run.record(ReleaseOutcome::Skipped);
        assert_eq!(run.releases(), 4);
        assert_eq!(run.completed(), 1);
        assert_eq!(run.overruns(), 1);
        assert_eq!(run.failures(), 1);
        assert_eq!(run.skipped(), 1);
        assert!(run.denominators_agree());
    }

    #[test]
    fn a_failure_does_not_improve_the_quantile() {
        // Ein Fehlschlag, der aus der Reihe faellt, verbessert das Quantil.
        // Hier faellt er nicht heraus — er steht daneben.
        let mut run = CellRun::with_capacity(cell(), 3);
        run.record(ReleaseOutcome::Completed { latency_ns: 100 });
        run.record(ReleaseOutcome::Completed { latency_ns: 200 });
        run.record(ReleaseOutcome::Failed {
            reason: "Timeout".to_owned(),
        });
        assert_eq!(run.releases(), 3);
        assert_eq!(run.failures(), 1);
        assert_eq!(run.first_failure(), Some("Timeout"));
        assert!(run.denominators_agree());
    }

    #[test]
    fn an_overrun_counts_twice_over() {
        // Einmal als Latenz — der Wert ist gemessen und richtig — und einmal
        // als Ueberzug, weil die Reihe ihren Takt nicht gehalten hat.
        let mut run = CellRun::with_capacity(cell(), 2);
        run.record(ReleaseOutcome::Overrun { latency_ns: 5_000 });
        assert_eq!(run.overruns(), 1);
        assert_eq!(run.quantile_ns(50), Some(5_000));
    }

    #[test]
    fn the_buffer_is_allocated_before_the_measured_loop() {
        let mut run = CellRun::with_capacity(cell(), 200);
        let capacity = run.capacity();
        assert!(capacity >= 200);
        for i in 0..200_u64 {
            run.record(ReleaseOutcome::Completed { latency_ns: i });
        }
        assert_eq!(
            run.capacity(),
            capacity,
            "eine Nachbelegung mitten in der Reihe waere ein Ausreisser, \
             den niemand als solchen erkennt"
        );
    }

    #[test]
    fn skipped_points_can_be_booked_in_bulk() {
        let mut run = CellRun::with_capacity(cell(), 1);
        run.record(ReleaseOutcome::Overrun { latency_ns: 400 });
        run.record_skipped(3);
        assert_eq!(run.releases(), 4);
        assert_eq!(run.skipped(), 3);
    }

    // -- Verwerfen ---------------------------------------------------------

    #[test]
    fn a_discarded_cell_yields_no_number() {
        let mut run = CellRun::with_capacity(cell(), 2);
        run.record(ReleaseOutcome::Completed { latency_ns: 100 });
        assert_eq!(run.quantile_ns(50), Some(100));
        run.discard(DiscardReason::Other {
            detail: "Nachbar hat die GPU benutzt".to_owned(),
        });
        assert_eq!(
            run.quantile_ns(50),
            None,
            "aus einer als ungueltig erkannten Messung noch eine Zahl zu \
             ziehen ist genau das, wogegen `discard` da ist"
        );
        assert!(!run.is_usable());
    }

    #[test]
    fn the_first_reason_is_the_one_that_counts() {
        let mut run = CellRun::with_capacity(cell(), 1);
        run.discard(DiscardReason::ClockTooCoarse {
            resolution_ns: 1_000_000,
        });
        run.discard(DiscardReason::Other {
            detail: "spaeter".to_owned(),
        });
        assert!(matches!(
            run.discarded(),
            Some(DiscardReason::ClockTooCoarse { .. })
        ));
    }

    #[test]
    fn too_few_samples_discard_the_cell() {
        let mut run = CellRun::with_capacity(cell(), 200);
        for _ in 0..30 {
            run.record(ReleaseOutcome::Completed { latency_ns: 1 });
        }
        run.qualify(100, 50);
        assert!(matches!(
            run.discarded(),
            Some(DiscardReason::TooFewSamples {
                got: 30,
                needed: 100
            })
        ));
    }

    #[test]
    fn too_many_failures_discard_the_cell() {
        let mut run = CellRun::with_capacity(cell(), 10);
        for _ in 0..5 {
            run.record(ReleaseOutcome::Completed { latency_ns: 1 });
            run.record(ReleaseOutcome::Failed {
                reason: "weg".to_owned(),
            });
        }
        run.qualify(1, 50);
        assert!(
            matches!(run.discarded(), Some(DiscardReason::TooManyFailures { .. })),
            "500 von 1000 Promille ist keine Messreihe mehr"
        );
    }

    #[test]
    fn a_clean_run_qualifies() {
        let mut run = CellRun::with_capacity(cell(), 200);
        for i in 0..200_u64 {
            run.record(ReleaseOutcome::Completed { latency_ns: i });
        }
        run.qualify(100, 50);
        assert!(run.is_usable());
        assert_eq!(run.quantile_ns(50), Some(100));
        assert_eq!(run.quantile_ns(99), Some(198));
    }

    // -- Hardwarezustand ---------------------------------------------------

    #[test]
    fn a_stable_card_does_not_invalidate_a_cell() {
        let before = parse_output(LINE, 1_000);
        let after = parse_output(LINE, 9_000);
        assert_eq!(hardware_invalidates(&before, &after), None);
    }

    #[test]
    fn a_thermal_limit_during_the_cell_discards_it_with_a_reason() {
        let before = parse_output(LINE, 1_000);
        let after = parse_output(
            &LINE.replace("0x0000000000000004", "0x0000000000000044"),
            9_000,
        );
        let reason = hardware_invalidates(&before, &after).unwrap();
        let text = reason.to_string();
        assert!(text.contains("throttle_reasons"), "{text}");
        assert!(text.contains("HwThermalSlowdown"), "{text}");
        assert!(text.contains("1000") && text.contains("9000"), "{text}");
    }

    // -- Uhr ---------------------------------------------------------------

    #[test]
    fn the_clock_on_this_machine_can_measure_a_millisecond() {
        // 4 ms ist die kuerzeste Inferenz im Messaufbau dieses Projekts.
        match verify_clock(4_000_000) {
            ClockVerdict::Usable { resolution_ns } => {
                assert!(resolution_ns <= 40_000, "{resolution_ns} ns");
            }
            ClockVerdict::TooCoarse { resolution_ns, .. } => {
                panic!("Uhraufloesung {resolution_ns} ns reicht nicht fuer 4 ms")
            }
        }
    }

    #[test]
    fn an_impossible_requirement_is_reported_not_ignored() {
        // Eine Inferenz von 100 ns liesse sich auf keiner gaengigen Uhr
        // messen. Das soll ein Befund sein und keine Zahl.
        assert!(matches!(
            verify_clock(100),
            ClockVerdict::TooCoarse { .. } | ClockVerdict::Usable { .. }
        ));
    }

    // -- Laufmanifest ------------------------------------------------------

    #[test]
    fn a_run_manifest_survives_a_roundtrip_and_keeps_its_denominators() {
        let mut good = CellRun::with_capacity(cell(), 200);
        for i in 0..200_u64 {
            good.record(ReleaseOutcome::Completed { latency_ns: i });
        }
        good.qualify(100, 50);

        let mut bad = CellRun::with_capacity(
            CellId {
                concurrency: 4,
                ..cell()
            },
            200,
        );
        bad.record(ReleaseOutcome::Overrun { latency_ns: 9_000 });
        bad.record_skipped(7);
        bad.discard(DiscardReason::HardwareChanged {
            detail: "GPU 0 clock_sm_mhz: ~1700 -> ~900".to_owned(),
        });

        let manifest = RunManifest {
            run_index: 2,
            total_runs: 3,
            started_at_ms: 1_000,
            finished_at_ms: 61_000,
            clock_resolution_ns: 40,
            hardware_before: parse_output(LINE, 1_000),
            hardware_after: parse_output(LINE, 61_000),
            cells: vec![good.summarise(), bad.summarise()],
        };

        assert!(manifest.accounting_is_consistent());
        assert_eq!(manifest.usable().count(), 1);
        assert_eq!(manifest.discarded().count(), 1);

        let text = serde_norway::to_string(&manifest).unwrap();
        let back: RunManifest = serde_norway::from_str(&text).unwrap();
        assert_eq!(manifest, back);
        assert!(back.accounting_is_consistent());
        assert_eq!(
            back.discarded().next().unwrap().p95_ns,
            None,
            "eine verworfene Zelle traegt auch nach dem Wiedereinlesen keine Zahl"
        );
    }

    #[test]
    fn a_repeated_run_says_which_repetition_it_is() {
        let manifest = RunManifest {
            run_index: 1,
            total_runs: 2,
            started_at_ms: 0,
            finished_at_ms: 1,
            clock_resolution_ns: 40,
            hardware_before: HardwareSnapshot::unavailable(0, "noch nichts"),
            hardware_after: HardwareSnapshot::unavailable(1, "noch nichts"),
            cells: Vec::new(),
        };
        assert_eq!(manifest.run_index, 1);
        assert_eq!(manifest.total_runs, 2);
        assert!(manifest.accounting_is_consistent());
    }
}
