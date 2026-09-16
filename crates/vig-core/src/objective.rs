//! Ziele: eine Zusage aus zwei Haelften (ADR-0047).
//!
//! ## Wofuer
//!
//! Die vier Klassen sind grobkoernig: unter Last blieb am 16.09.2026 die
//! geschuetzte Kamera bei 13 ‰ unabgedeckt, die Klasse darunter bei 465 ‰.
//! Ein Betreiber will Zwischenstufen aussprechen koennen — „Front 98 %, Heck
//! 20 %, aber mindestens einmal je Sekunde eines".
//!
//! ## Zwei Haelften
//!
//! Ein Anteil allein sagt nichts ueber die Verteilung: 20 % in zehn Sekunden
//! koennten acht Sekunden Stille sein. Deshalb hat ein Ziel einen **Anteil**
//! (wie viel im Fenster) und eine **Luecke** (wie lange nie nichts). Beide
//! sind einzeln optional, mindestens eine muss vereinbart sein.
//!
//! ## Eine Waehrung
//!
//! Beide Haelften werden in **Restzeit bis zum Bruch** umgerechnet, damit sie
//! vergleichbar sind und mit Fristen in einem Schluessel stehen koennen. Der
//! Slack eines Stroms ist der kleinere der beiden Werte; er ist negativ,
//! sobald die Zusage verletzt ist.
//!
//! ## Das Fenster
//!
//! Gleitend, in [`COVERAGE_BUCKETS`] Abschnitten fester Laenge — wie der
//! `RuntimeLedger` aus ADR-0046. Fester Speicher, konstanter Aufwand je
//! Abfrage (Spec L-003).

use crate::time::{Duration, Instant, Slack};

/// Wie viele Abschnitte ein Fenster hat. Zweierpotenz: Index ohne Division.
pub const COVERAGE_BUCKETS: u64 = 16;

/// Dieselbe Zahl als Laenge des Ringpuffers.
///
/// Zwei Konstanten statt einer Umwandlung: ein Cast zwischen `u64` und
/// `usize` waere auf fremden Zielen nicht verlustfrei, und die Zahl steht
/// hier ohnehin fest.
const BUCKETS_LEN: usize = 16;

/// Die Maske fuer [`COVERAGE_BUCKETS`].
const BUCKET_MASK: u64 = 15;

/// Das laengste zulaessige Fenster: eine Minute.
///
/// Laenger traegt die Aussage „so viel Anteil je Fenster" nicht mehr: ein
/// Ziel, das eine Minute lang gerissen und dann am Stueck nachgeholt wird,
/// ist fuer den Verbraucher kein Dienst, sondern eine Pause mit Nachschlag.
pub const MAX_OBJECTIVE_WINDOW: Duration = Duration::from_nanos_unbounded(60_000_000_000);

/// Die vereinbarte Zusage eines Modells (ADR-0047).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Objective {
    /// Anteil frischer Zyklen im Fenster, in Promille. Null heisst: keine
    /// Anteilszusage, nur die Luecke zaehlt.
    pub coverage_permille: u16,
    /// Die Laenge des gleitenden Fensters.
    pub window: Duration,
    /// Die laengste erlaubte Zeit ohne Ergebnis, falls vereinbart.
    pub max_gap: Option<Duration>,
}

/// Warum ein Ziel unzulaessig ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectiveError {
    /// Weder Anteil noch Luecke ist vereinbart.
    ///
    /// Ein Ziel ohne beide Haelften sagt nichts; es waere eine Zeile, die
    /// aussieht wie eine Zusage und keine ist.
    Empty,
    /// Das Fenster ist null.
    ZeroWindow,
    /// Das Fenster ist laenger als [`MAX_OBJECTIVE_WINDOW`].
    WindowTooLong,
    /// Der Anteil liegt ueber 1000 Promille.
    CoverageAboveAll,
    /// Eine Luecke von null.
    ZeroGap,
    /// Ein Anteil ohne Periode ist nicht ausrechenbar.
    ///
    /// Der Anteil zaehlt Zyklen; ohne `period_ms` gibt es keine Zyklen.
    CoverageNeedsPeriod,
    /// Die Luecke liegt unter der Periode.
    ///
    /// Haeufiger als jeden Zyklus kann kein Ergebnis entstehen; die Zusage
    /// waere von der ersten Sekunde an gebrochen.
    GapBelowPeriod,
    /// Das Fenster ist kuerzer als die Periode.
    WindowBelowPeriod,
}

impl core::fmt::Display for ObjectiveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => write!(
                f,
                "objective: mindestens coverage_permille oder max_gap_ms muss dastehen \
                 (ADR-0047)"
            ),
            Self::ZeroWindow => write!(f, "objective: window_ms muss groesser als null sein"),
            Self::WindowTooLong => write!(
                f,
                "objective: window_ms hoechstens {} ms",
                MAX_OBJECTIVE_WINDOW.as_millis()
            ),
            Self::CoverageAboveAll => {
                write!(f, "objective: coverage_permille hoechstens 1000")
            }
            Self::ZeroGap => write!(f, "objective: max_gap_ms muss groesser als null sein"),
            Self::CoverageNeedsPeriod => write!(
                f,
                "objective: coverage_permille zaehlt Zyklen und braucht period_ms"
            ),
            Self::GapBelowPeriod => write!(
                f,
                "objective: max_gap_ms unter period_ms — haeufiger als jeden Zyklus kann \
                 kein Ergebnis entstehen"
            ),
            Self::WindowBelowPeriod => write!(
                f,
                "objective: window_ms unter period_ms — in ein Fenster passt dann kein \
                 ganzer Zyklus"
            ),
        }
    }
}

impl core::error::Error for ObjectiveError {}

impl Objective {
    /// Prueft das Ziel gegen die Periode seines Vertrags.
    ///
    /// # Errors
    ///
    /// Siehe [`ObjectiveError`].
    pub const fn validate(&self, period: Option<Duration>) -> Result<(), ObjectiveError> {
        if self.coverage_permille == 0 && self.max_gap.is_none() {
            return Err(ObjectiveError::Empty);
        }
        if self.window.as_nanos() == 0 {
            return Err(ObjectiveError::ZeroWindow);
        }
        if self.window.as_nanos() > MAX_OBJECTIVE_WINDOW.as_nanos() {
            return Err(ObjectiveError::WindowTooLong);
        }
        if self.coverage_permille > 1_000 {
            return Err(ObjectiveError::CoverageAboveAll);
        }
        if let Some(gap) = self.max_gap
            && gap.as_nanos() == 0
        {
            return Err(ObjectiveError::ZeroGap);
        }
        match period {
            None => {
                if self.coverage_permille > 0 {
                    return Err(ObjectiveError::CoverageNeedsPeriod);
                }
            }
            Some(period) => {
                if self.window.as_nanos() < period.as_nanos() {
                    return Err(ObjectiveError::WindowBelowPeriod);
                }
                if let Some(gap) = self.max_gap
                    && gap.as_nanos() < period.as_nanos()
                {
                    return Err(ObjectiveError::GapBelowPeriod);
                }
            }
        }
        Ok(())
    }

    /// Die Zyklen, die in ein Fenster passen — mindestens einer.
    #[must_use]
    pub fn cycles_in_window(&self, period: Duration) -> u64 {
        self.window
            .as_nanos()
            .checked_div(period.as_nanos().max(1))
            .unwrap_or(1)
            .max(1)
    }

    /// Wie viele Zyklen eines Fensters die Zusage mindestens erfuellen muessen.
    ///
    /// Aufgerundet: „98 %" von 300 Zyklen sind 294, und 293 waeren zu wenig.
    #[must_use]
    pub fn required_in_window(&self, period: Duration) -> u64 {
        let cycles = self.cycles_in_window(period);
        let product = cycles.saturating_mul(u64::from(self.coverage_permille));
        // Aufrunden ohne Division durch null und ohne `/`.
        product
            .saturating_add(999)
            .checked_div(1_000)
            .unwrap_or(0)
            .min(cycles)
    }

    /// Der Anteil, den die Zusage **wirklich** verlangt (ADR-0047, Randfall 5).
    ///
    /// Eine Luecke von einer Sekunde erzwingt bei 33 ms Takt mindestens
    /// 33 ‰ — auch wenn `coverage_permille` darunter steht. Fuer die Zulassung
    /// zaehlt die strengere der beiden Haelften.
    #[must_use]
    pub fn effective_permille(&self, period: Duration) -> u16 {
        let from_gap = match self.max_gap {
            None => 0,
            Some(gap) => {
                let per = period.as_nanos().max(1);
                let share = per
                    .saturating_mul(1_000)
                    .checked_div(gap.as_nanos().max(1))
                    .unwrap_or(0);
                u16::try_from(share.min(1_000)).unwrap_or(1_000)
            }
        };
        self.coverage_permille.max(from_gap)
    }
}

/// Der gleitende Zaehler eines Stroms: wie viele **Verbraucherzyklen**
/// versorgt waren und wann zuletzt eines ankam.
///
/// Gezaehlt werden Zyklen, nicht Lieferungen — das ist der Unterschied
/// zwischen der Zusage und ihrer Vortaeuschung. Ein Ergebnis, das ueber drei
/// Zyklen frisch bleibt, versorgt drei; fuenf Ergebnisse, die vor dem
/// naechsten Verbraucherzyklus veralten, versorgen diesen nicht. Wer Lieferungen zaehlt,
/// meldet im ersten Fall ein Drittel und im zweiten das Zweieinhalbfache —
/// beides ist falsch, und beides stand hier, bis eine Pruefung es aufdeckte.
///
/// Beobachtet wird deshalb im Takt: bei jedem Zyklus fragt der Scheduler, ob
/// gerade ein gueltiges Ergebnis vorliegt, das noch nicht ueber dem
/// Hoechstalter ist (`last_valid` und `valid_since`). Nichts wirkt
/// rueckwirkend — eine spaete Fertigstellung versorgt die Zyklen nicht, die
/// vor ihr lagen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageLedger {
    /// Versorgte Zyklen je Abschnitt des Fensters.
    supplied: [u32; BUCKETS_LEN],
    /// **Beobachtete** Zyklen je Abschnitt — der Nenner.
    ///
    /// Zaehler und Nenner muessen aus derselben Koernung kommen. Wird der
    /// Nenner stattdessen aus Fenster und Takt gerechnet, erfindet die
    /// Abschnittsgrenze Fehlzyklen: der aelteste Teilabschnitt faellt bereits
    /// vor dem exakten Fensterrand heraus, im Nenner aber nicht.
    observed: [u32; BUCKETS_LEN],
    /// Der Abschnittsindex, auf dem der Zaehler gerade steht.
    cursor: u64,
    /// Beginn der Messung; davor gibt es keine Aussage.
    start: Instant,
    /// Wann zuletzt ein frisches Ergebnis geliefert wurde.
    last_ok: Instant,
    /// Bis wann die Zyklen bereits beobachtet sind.
    ///
    /// Ohne diese Marke zaehlte jeder Aufruf dieselben Zyklen erneut — der
    /// Scheduler ruft im Takt, aber auch bei jedem anderen Ereignis.
    observed_until: Instant,
}

impl CoverageLedger {
    /// Ein leerer Zaehler, der jetzt beginnt.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            supplied: [0; BUCKETS_LEN],
            observed: [0; BUCKETS_LEN],
            cursor: 0,
            start: now,
            last_ok: now,
            observed_until: now,
        }
    }

    /// Beobachtet die Verbraucherzyklen, die seit dem letzten Aufruf
    /// verstrichen sind.
    ///
    /// Die Bewertung ist dieselbe wie fuer den Weakly-hard-Monitor
    /// (`scheduler::observe_cycles`, ADR-0005), und das ist Absicht: zwei
    /// Kriterien fuer „vertragsgemaess versorgt" im selben Kern liefen
    /// irgendwann auseinander. Ein Zyklus gilt als versorgt, wenn zu seinem
    /// Zeitpunkt ein gueltiges Ergebnis vorlag, das noch nicht ueber dem
    /// Hoechstalter war.
    ///
    /// `last_valid` ist die Aufnahmezeit des juengsten brauchbaren
    /// Ergebnisses, `valid_since` der Zeitpunkt, ab dem es **vorlag** — seine
    /// Fertigstellung. Beide werden gebraucht: Ein Ergebnis, das erst spaeter
    /// fertig wurde, versorgt die Zyklen davor nicht, auch wenn seine
    /// Aufnahmezeit vor ihnen liegt. Nichts wirkt rueckwirkend.
    ///
    /// Aufgerufen wird das bei jedem Ereignis; `observed_until` sorgt dafuer,
    /// dass kein Zyklus doppelt zaehlt.
    pub fn observe(
        &mut self,
        now: Instant,
        last_valid: Option<Instant>,
        valid_since: Option<Instant>,
        max_age: Duration,
        objective: &Objective,
        period: Duration,
    ) {
        let step = period.as_nanos().max(1);
        let start = self.start.as_nanos();
        let previous = self
            .observed_until
            .as_nanos()
            .saturating_sub(start)
            .checked_div(step)
            .unwrap_or(0);
        let last = now
            .as_nanos()
            .saturating_sub(start)
            .checked_div(step)
            .unwrap_or(0);
        if last <= previous {
            return;
        }
        let first = previous.saturating_add(1);
        self.advance(now, objective);
        let width = objective
            .window
            .as_nanos()
            .checked_div(COVERAGE_BUCKETS)
            .unwrap_or(1)
            .max(1);
        let oldest = self
            .cursor
            .saturating_sub(COVERAGE_BUCKETS.saturating_sub(1));
        let valid_ticks = last_valid.and_then(|capture| {
            let available = capture.max(valid_since.unwrap_or(capture));
            let expires = capture.as_nanos().saturating_add(max_age.as_nanos());
            if expires < start {
                return None;
            }
            Some((
                available.as_nanos().saturating_sub(start).div_ceil(step),
                expires.saturating_sub(start).checked_div(step).unwrap_or(0),
            ))
        });
        // Count ticks analytically in each retained bucket. Iterating over
        // every missed period could do 120,000 iterations per model after a
        // pause (60-s window, 1-ms period), or never terminate at u64::MAX.
        // The tick phase remains anchored at `start`, including after pauses.
        for offset in 0..COVERAGE_BUCKETS {
            let section = oldest.saturating_add(offset);
            if section > self.cursor {
                break;
            }
            let from = u128::from(section).saturating_mul(u128::from(width));
            let to = u128::from(section)
                .saturating_add(1)
                .saturating_mul(u128::from(width))
                .saturating_sub(1);
            let lo = first.max(u64::try_from(from.div_ceil(u128::from(step))).unwrap_or(u64::MAX));
            let hi = last.min(
                u64::try_from(to.checked_div(u128::from(step)).unwrap_or(0)).unwrap_or(u64::MAX),
            );
            if hi < lo {
                continue;
            }
            let count = hi.saturating_sub(lo).saturating_add(1);
            let index = section.bitand_mask();
            if let Some(cell) = self.observed.get_mut(index) {
                *cell = cell.saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
            }
            if let Some((valid_lo, valid_hi)) = valid_ticks {
                let supplied_lo = lo.max(valid_lo);
                let supplied_hi = hi.min(valid_hi);
                if supplied_hi >= supplied_lo
                    && let Some(cell) = self.supplied.get_mut(index)
                {
                    let count = supplied_hi.saturating_sub(supplied_lo).saturating_add(1);
                    *cell = cell.saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
                }
            }
        }
        self.observed_until = Instant::from_nanos(start.saturating_add(last.saturating_mul(step)));
    }

    /// Records a usable delivery for the gap promise, independently of ticks.
    /// A retained result may supply several cycles but is still one delivery.
    pub fn delivered(&mut self, now: Instant) {
        self.last_ok = self.last_ok.max(now);
    }

    /// Der Abschnitt, in den `now` faellt.
    fn bucket_of(&self, now: Instant, objective: &Objective) -> u64 {
        let width = objective
            .window
            .as_nanos()
            .checked_div(COVERAGE_BUCKETS)
            .unwrap_or(1)
            .max(1);
        let since = now.as_nanos().saturating_sub(self.start.as_nanos());
        since.checked_div(width).unwrap_or(0)
    }

    /// Schiebt das Fenster bis `now` vor und loescht, was herausgefallen ist.
    fn advance(&mut self, now: Instant, objective: &Objective) {
        let target = self.bucket_of(now, objective);
        if target <= self.cursor {
            return;
        }
        let steps = target.saturating_sub(self.cursor).min(COVERAGE_BUCKETS);
        let mut step = 0;
        while step < steps {
            let index = self
                .cursor
                .saturating_add(step)
                .saturating_add(1)
                .bitand_mask();
            // Beide Reihen: bliebe der Nenner eines abgelaufenen Abschnitts
            // stehen, saenke die Abdeckung ohne einen einzigen Fehlzyklus.
            if let Some(cell) = self.supplied.get_mut(index) {
                *cell = 0;
            }
            if let Some(cell) = self.observed.get_mut(index) {
                *cell = 0;
            }
            step = step.saturating_add(1);
        }
        self.cursor = target;
    }

    /// Die versorgten Zyklen im Fenster, von `now` aus gesehen.
    ///
    /// Die Abfrage muss dasselbe Fenster sehen wie eine Buchung: sonst zaehlte
    /// ein Strom, der seit zwei Sekunden nichts geliefert hat, weiterhin seine
    /// alten Erfolge — und stuende faelschlich im Plan. Gezaehlt werden nur
    /// Abschnitte, die noch im Fenster liegen und bereits befuellt sein
    /// konnten; geaendert wird dabei nichts.
    #[must_use]
    pub fn fulfilled(&self, now: Instant, objective: &Objective) -> u64 {
        self.sum_over_window(now, objective, &self.supplied)
    }

    /// Die **beobachteten** Zyklen im Fenster — der Nenner zu
    /// [`Self::fulfilled`].
    ///
    /// Aus derselben Quelle und ueber dieselben Abschnitte: eine zweite
    /// Rechnung aus Fenster und Takt weicht um die Koernung ab und erfindet
    /// damit Fehlzyklen, wo keine sind.
    #[must_use]
    pub fn observed_cycles(&self, now: Instant, objective: &Objective) -> u64 {
        self.sum_over_window(now, objective, &self.observed)
    }

    /// Summiert eine Reihe ueber die Abschnitte, die noch im Fenster liegen
    /// und bereits befuellt sein konnten. Geaendert wird dabei nichts.
    fn sum_over_window(
        &self,
        now: Instant,
        objective: &Objective,
        row: &[u32; BUCKETS_LEN],
    ) -> u64 {
        let target = self.bucket_of(now, objective);
        let oldest = target.saturating_sub(COVERAGE_BUCKETS.saturating_sub(1));
        let newest = self.cursor.min(target);
        if newest < oldest {
            return 0;
        }
        let mut sum = 0_u64;
        for offset in 0..COVERAGE_BUCKETS {
            let section = oldest.saturating_add(offset);
            if section > newest {
                break;
            }
            if let Some(cell) = row.get(section.bitand_mask()) {
                sum = sum.saturating_add(u64::from(*cell));
            }
        }
        sum
    }

    /// Die Zyklen, gegen die die Zusage gemessen wird: die **beobachteten**.
    ///
    /// Hier stand eine eigene Rechnung aus verstrichener Zeit und Takt. Sie
    /// wich um die Koernung des Fensters ab und meldete Fehlzyklen, die es nicht gab: bei
    /// lueckenloser Versorgung 966 statt 1000 Promille. Zaehler und Nenner
    /// kommen jetzt aus derselben Quelle.
    ///
    /// Randfall 2 aus ADR-0047 bleibt erfuellt: im ersten Moment ist noch kein
    /// Zyklus beobachtet, und ein Strom, der noch gar nicht dran war, ist
    /// nicht im Rueckstand.
    #[must_use]
    pub fn expected(&self, now: Instant, objective: &Objective, _period: Duration) -> u64 {
        self.observed_cycles(now, objective)
    }

    /// Restzeit, bis die Anteilszusage bricht — negativ, wenn sie es schon ist.
    ///
    /// Der Puffer sind die Zyklen ueber der Pflicht; jeder davon darf
    /// ausfallen, und jeder kostet eine Periode Zeit.
    #[must_use]
    pub fn coverage_slack(&self, now: Instant, objective: &Objective, period: Duration) -> Slack {
        if objective.coverage_permille == 0 {
            return Slack::from_nanos(i64::MAX);
        }
        let expected = self.expected(now, objective, period);
        let required = expected
            .saturating_mul(u64::from(objective.coverage_permille))
            .saturating_add(999)
            .checked_div(1_000)
            .unwrap_or(0);
        let have = self.fulfilled(now, objective);
        let spare = i128::from(have).saturating_sub(i128::from(required));
        let per = i128::from(period.as_nanos().max(1));
        saturate(spare.saturating_mul(per), objective.window)
    }

    /// Restzeit, bis die Lueckenzusage bricht — negativ, wenn sie es schon ist.
    #[must_use]
    pub fn gap_slack(&self, now: Instant, objective: &Objective) -> Slack {
        let Some(gap) = objective.max_gap else {
            return Slack::from_nanos(i64::MAX);
        };
        let deadline =
            i128::from(self.last_ok.as_nanos()).saturating_add(i128::from(gap.as_nanos()));
        let left = deadline.saturating_sub(i128::from(now.as_nanos()));
        saturate(left, objective.window)
    }

    /// Der Slack des Stroms: die knappere der beiden Haelften.
    #[must_use]
    pub fn slack(&self, now: Instant, objective: &Objective, period: Duration) -> Slack {
        self.coverage_slack(now, objective, period)
            .min(self.gap_slack(now, objective))
    }

    /// Wann zuletzt ein frisches Ergebnis ankam.
    #[must_use]
    pub const fn last_ok(&self) -> Instant {
        self.last_ok
    }
}

/// Begrenzt einen Slack auf ein Fenster in beide Richtungen.
///
/// Randfall 3 aus ADR-0047: ein Strom, dessen Fenster verloren ist, darf nicht
/// unendlich dringend werden und alles niederwalzen. Und ein Strom weit vor
/// seiner Zusage braucht keinen Vorsprung, den er nie einloest.
fn saturate(value: i128, window: Duration) -> Slack {
    let limit = i128::from(window.as_nanos());
    let clamped = value.max(limit.saturating_neg()).min(limit);
    Slack::from_nanos(i64::try_from(clamped).unwrap_or(i64::MAX))
}

/// Index in den Ringpuffer, ohne Division.
trait BucketIndex {
    fn bitand_mask(self) -> usize;
}

impl BucketIndex for u64 {
    fn bitand_mask(self) -> usize {
        usize::try_from(self & BUCKET_MASK).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value).unwrap()
    }

    fn at(millis: u64) -> Instant {
        Instant::from_nanos(millis.saturating_mul(1_000_000))
    }

    fn goal(permille: u16, window_ms: u64, gap_ms: Option<u64>) -> Objective {
        Objective {
            coverage_permille: permille,
            window: ms(window_ms),
            max_gap: gap_ms.map(ms),
        }
    }

    // --- Validierung ------------------------------------------------------

    #[test]
    fn a_goal_without_either_half_is_refused() {
        assert_eq!(
            goal(0, 10_000, None).validate(Some(ms(33))),
            Err(ObjectiveError::Empty)
        );
    }

    #[test]
    fn a_share_without_a_period_cannot_be_computed() {
        assert_eq!(
            goal(980, 10_000, None).validate(None),
            Err(ObjectiveError::CoverageNeedsPeriod)
        );
        // Nur eine Luecke geht auch ohne Periode.
        assert!(goal(0, 10_000, Some(1_000)).validate(None).is_ok());
    }

    #[test]
    fn a_gap_below_the_period_is_refused() {
        assert_eq!(
            goal(200, 10_000, Some(20)).validate(Some(ms(33))),
            Err(ObjectiveError::GapBelowPeriod)
        );
    }

    #[test]
    fn a_window_that_holds_no_cycle_is_refused() {
        assert_eq!(
            goal(500, 20, None).validate(Some(ms(33))),
            Err(ObjectiveError::WindowBelowPeriod)
        );
    }

    #[test]
    fn the_limits_of_window_and_share_are_refused() {
        assert_eq!(
            goal(1_001, 10_000, None).validate(Some(ms(33))),
            Err(ObjectiveError::CoverageAboveAll)
        );
        assert_eq!(
            Objective {
                coverage_permille: 500,
                window: Duration::ZERO,
                max_gap: None,
            }
            .validate(Some(ms(33))),
            Err(ObjectiveError::ZeroWindow)
        );
        assert_eq!(
            goal(500, 61_000, None).validate(Some(ms(33))),
            Err(ObjectiveError::WindowTooLong)
        );
    }

    // --- Die strengere Haelfte (Randfall 5) -------------------------------

    #[test]
    fn the_stricter_half_decides_what_is_promised() {
        // 20 % verlangt wenig, „einmal je Sekunde" bei 33 ms Takt mehr: 33 ‰.
        let g = goal(200, 10_000, Some(1_000));
        assert_eq!(g.effective_permille(ms(33)), 200);

        // Umgekehrt: 10 ‰ Anteil, aber jede Sekunde eines bei 200 ms Takt
        // sind 200 ‰ — dann zaehlt die Luecke.
        let g = goal(10, 10_000, Some(1_000));
        assert_eq!(g.effective_permille(ms(200)), 200);
    }

    #[test]
    fn a_share_rounds_up_because_almost_is_not_enough() {
        let g = goal(980, 10_000, None);
        // 10 s bei 33 ms sind 303 Zyklen, 98 % davon 296,94 -> 297.
        assert_eq!(g.cycles_in_window(ms(33)), 303);
        assert_eq!(g.required_in_window(ms(33)), 297);
    }

    // --- Der gleitende Zaehler --------------------------------------------

    #[test]
    fn a_fresh_ledger_is_not_already_behind() {
        // Randfall 2: beim Start gibt es keine Historie, und niemand darf
        // deshalb maximal dringend sein.
        let ledger = CoverageLedger::new(at(0));
        let g = goal(980, 10_000, Some(1_000));
        assert!(ledger.coverage_slack(at(0), &g, ms(33)) >= Slack::ZERO);
    }

    /// Beobachtet bis `until` mit einem Ergebnis, das seit `since` vorliegt
    /// und bei `capture` aufgenommen wurde.
    fn watch(
        ledger: &mut CoverageLedger,
        until: u64,
        capture: Option<u64>,
        since: Option<u64>,
        max_age_ms: u64,
        g: &Objective,
        period_ms: u64,
    ) {
        ledger.observe(
            at(until),
            capture.map(at),
            since.map(at),
            ms(max_age_ms),
            g,
            ms(period_ms),
        );
    }

    #[test]
    fn meeting_the_promise_builds_slack_and_missing_it_spends_slack() {
        let g = goal(500, 1_000, None);
        let mut ledger = CoverageLedger::new(at(0));
        // Zehn Zyklen zu 100 ms, durchgehend versorgt: deutlich ueber 50 %.
        watch(&mut ledger, 1_000, Some(0), Some(0), 10_000, &g, 100);
        let ahead = ledger.coverage_slack(at(1_000), &g, ms(100));
        assert!(ahead > Slack::ZERO, "{ahead}");

        // Eine weitere Sekunde ohne jedes Ergebnis: das Fenster ist leer, die
        // Zusage gerissen, der Slack negativ.
        watch(&mut ledger, 2_100, Some(0), Some(0), 10, &g, 100);
        let behind = ledger.coverage_slack(at(2_100), &g, ms(100));
        assert!(behind.is_infeasible(), "{behind}");
    }

    #[test]
    fn the_gap_half_counts_down_to_its_deadline() {
        let g = goal(0, 10_000, Some(1_000));
        let mut ledger = CoverageLedger::new(at(0));
        // Ein Ergebnis bei 100 ms, danach nichts mehr.
        watch(&mut ledger, 100, Some(100), Some(100), 10_000, &g, 100);
        ledger.delivered(at(100));
        // 400 ms spaeter sind noch 600 ms Luft.
        assert_eq!(
            ledger.gap_slack(at(500), &g),
            Slack::from_nanos(600_000_000)
        );
        // Nach 1,3 s ist die Zusage um 200 ms gerissen.
        assert_eq!(
            ledger.gap_slack(at(1_300), &g),
            Slack::from_nanos(-200_000_000)
        );
    }

    #[test]
    fn the_tighter_half_wins() {
        // Anteil ist bequem, die Luecke laeuft ab: der Slack folgt der Luecke.
        let g = goal(100, 10_000, Some(1_000));
        let mut ledger = CoverageLedger::new(at(0));
        // Bis 400 ms versorgt, danach nicht mehr.
        watch(&mut ledger, 400, Some(0), Some(0), 10_000, &g, 100);
        ledger.delivered(at(400));
        watch(&mut ledger, 1_350, Some(0), Some(0), 400, &g, 100);
        // Der Anteil hat Puffer, die Luecke laeuft in 50 ms ab.
        let slack = ledger.slack(at(1_350), &g, ms(100));
        assert_eq!(
            slack,
            Slack::from_nanos(50_000_000),
            "die Luecke ist die knappere Haelfte"
        );
    }

    #[test]
    fn slack_never_exceeds_a_window_in_either_direction() {
        // Randfall 3: ein verlorenes Fenster macht niemanden unendlich
        // dringend, und ein grosser Vorsprung ist kein Freifahrtschein.
        let g = goal(500, 1_000, Some(1_000));
        let mut empty = CoverageLedger::new(at(0));
        watch(&mut empty, 600_000, None, None, 100, &g, 100);
        assert_eq!(
            empty.slack(at(600_000), &g, ms(100)),
            Slack::from_nanos(-1_000_000_000)
        );

        let mut full = CoverageLedger::new(at(0));
        watch(&mut full, 1_000, Some(0), Some(0), 10_000, &g, 10);
        assert!(full.coverage_slack(at(1_000), &g, ms(10)) <= Slack::from_nanos(1_000_000_000));
    }

    #[test]
    fn a_stale_result_supplies_no_cycle() {
        // Der Kern der neuen Zaehlweise: gezaehlt werden Verbraucherzyklen.
        // Ein Ergebnis, das aelter ist als das Hoechstalter, versorgt keinen —
        // gleich wie oft es geliefert wurde.
        let g = goal(500, 1_000, None);
        let mut ledger = CoverageLedger::new(at(0));
        watch(&mut ledger, 500, Some(0), Some(0), 50, &g, 100);
        // Beobachtet werden die Zyklen 100 bis 500 — der Startzeitpunkt gilt
        // als bereits gesehen. Jeder davon ist aelter als 50 ms, also versorgt
        // das eine Ergebnis keinen einzigen.
        assert_eq!(ledger.fulfilled(at(500), &g), 0);
        assert_eq!(ledger.observed_cycles(at(500), &g), 5);
    }

    #[test]
    fn nothing_supplies_a_cycle_before_the_result_existed() {
        // Eine spaete Fertigstellung versorgt die Zyklen davor nicht, auch
        // wenn ihre Aufnahmezeit vor ihnen liegt.
        let g = goal(500, 1_000, None);
        let mut ledger = CoverageLedger::new(at(0));
        watch(&mut ledger, 300, Some(0), Some(250), 500, &g, 100);
        assert_eq!(
            ledger.fulfilled(at(300), &g),
            1,
            "nur der Zyklus bei 300, nicht die bei 100 und 200"
        );
        assert_eq!(ledger.observed_cycles(at(300), &g), 3);
    }

    #[test]
    fn old_cycles_fall_out_of_the_window() {
        let g = goal(500, 1_000, None);
        let mut ledger = CoverageLedger::new(at(0));
        watch(&mut ledger, 900, Some(0), Some(0), 10_000, &g, 100);
        assert_eq!(ledger.fulfilled(at(900), &g), 9);
        // Weit spaeter ist das Fenster leer — ohne dass jemand etwas
        // beobachten muesste.
        assert_eq!(ledger.fulfilled(at(5_000), &g), 0);
        assert_eq!(ledger.observed_cycles(at(5_000), &g), 0);
    }

    #[test]
    fn sparse_observation_keeps_the_original_tick_phase() {
        let g = goal(500, 1_000, None);
        let mut dense = CoverageLedger::new(at(0));
        let mut sparse = dense;
        for t in 0..=10_050 {
            watch(&mut dense, t, Some(0), Some(0), 10_010, &g, 100);
        }
        watch(&mut sparse, 10_050, Some(0), Some(0), 10_010, &g, 100);
        assert_eq!(
            sparse.fulfilled(at(10_050), &g),
            dense.fulfilled(at(10_050), &g)
        );
        assert_eq!(
            sparse.observed_cycles(at(10_050), &g),
            dense.observed_cycles(at(10_050), &g)
        );
    }

    #[test]
    fn observation_at_the_clock_limit_is_bounded_and_idempotent() {
        let nanosecond = Duration::from_nanos_unbounded(1);
        let g = Objective {
            coverage_permille: 500,
            window: Duration::from_nanos_unbounded(16),
            max_gap: None,
        };
        let mut ledger = CoverageLedger::new(Instant::ZERO);
        let end = Instant::from_nanos(u64::MAX);
        ledger.observe(end, None, None, nanosecond, &g, nanosecond);
        assert_eq!(ledger.observed_cycles(end, &g), 16);
        assert_eq!(ledger.fulfilled(end, &g), 0);
        let before = ledger;
        ledger.observe(end, None, None, nanosecond, &g, nanosecond);
        assert_eq!(ledger, before);
    }
}
