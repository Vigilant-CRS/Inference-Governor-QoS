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

/// Der gleitende Zaehler eines Stroms: wie viele Zyklen erfuellt waren und
/// wann zuletzt eines ankam.
///
/// Gebucht wird, was der Verbraucher bekommen hat — ein frisches Ergebnis,
/// nicht ein abgeschickter Auftrag. Alles andere waere eine Zusage, die sich
/// selbst bestaetigt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageLedger {
    /// Erfuellte Zyklen je Abschnitt des Fensters.
    buckets: [u32; BUCKETS_LEN],
    /// Der Abschnittsindex, auf dem der Zaehler gerade steht.
    cursor: u64,
    /// Beginn der Messung; davor gibt es keine Aussage.
    start: Instant,
    /// Wann zuletzt ein frisches Ergebnis ankam.
    last_ok: Instant,
}

impl CoverageLedger {
    /// Ein leerer Zaehler, der jetzt beginnt.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            buckets: [0; BUCKETS_LEN],
            cursor: 0,
            start: now,
            last_ok: now,
        }
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
            if let Some(cell) = self.buckets.get_mut(index) {
                *cell = 0;
            }
            step = step.saturating_add(1);
        }
        self.cursor = target;
    }

    /// Bucht ein frisches Ergebnis.
    pub fn record(&mut self, now: Instant, objective: &Objective) {
        self.advance(now, objective);
        let index = self.cursor.bitand_mask();
        if let Some(cell) = self.buckets.get_mut(index) {
            *cell = cell.saturating_add(1);
        }
        self.last_ok = now;
    }

    /// Die erfuellten Zyklen im Fenster, von `now` aus gesehen.
    ///
    /// Die Abfrage muss dasselbe Fenster sehen wie eine Buchung: sonst zaehlte
    /// ein Strom, der seit zwei Sekunden nichts geliefert hat, weiterhin seine
    /// alten Erfolge — und stuende faelschlich im Plan. Gezaehlt werden nur
    /// Abschnitte, die noch im Fenster liegen und bereits befuellt sein
    /// konnten; geaendert wird dabei nichts.
    #[must_use]
    pub fn fulfilled(&self, now: Instant, objective: &Objective) -> u64 {
        let target = self.bucket_of(now, objective);
        let oldest = target.saturating_sub(COVERAGE_BUCKETS.saturating_sub(1));
        let newest = self.cursor.min(target);
        if newest < oldest {
            return 0;
        }
        let mut sum = 0_u64;
        let mut section = oldest;
        while section <= newest {
            if let Some(cell) = self.buckets.get(section.bitand_mask()) {
                sum = sum.saturating_add(u64::from(*cell));
            }
            section = section.saturating_add(1);
        }
        sum
    }

    /// Die Zyklen, die seit dem Beginn erwartet werden konnten — hoechstens
    /// ein volles Fenster.
    ///
    /// Randfall 2 aus ADR-0047: in der ersten Sekunde rechnet die Zusage gegen
    /// die verstrichene Zeit, nicht gegen das volle Fenster. Sonst waere jeder
    /// Strom beim Start maximal dringend.
    #[must_use]
    pub fn expected(&self, now: Instant, objective: &Objective, period: Duration) -> u64 {
        let elapsed = now.as_nanos().saturating_sub(self.start.as_nanos());
        let span = elapsed.min(objective.window.as_nanos());
        // Kein erzwungener Mindestzyklus: im ersten Moment ist noch keiner
        // faellig, und ein Strom, der noch gar nicht dran war, ist nicht im
        // Rueckstand.
        span.checked_div(period.as_nanos().max(1)).unwrap_or(0)
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

    #[test]
    fn meeting_the_promise_builds_slack_and_missing_it_spends_slack() {
        let g = goal(500, 1_000, None);
        let mut ledger = CoverageLedger::new(at(0));
        // Zehn Zyklen zu 100 ms, jeder erfuellt: deutlich ueber 50 %.
        let mut t = 0;
        while t < 1_000 {
            ledger.record(at(t), &g);
            t = t.saturating_add(100);
        }
        let ahead = ledger.coverage_slack(at(1_000), &g, ms(100));
        assert!(ahead > Slack::ZERO, "{ahead}");

        // Eine Sekunde ohne jedes Ergebnis: das Fenster ist leer, die Zusage
        // gerissen, der Slack negativ.
        let behind = ledger.coverage_slack(at(2_100), &g, ms(100));
        assert!(behind.is_infeasible(), "{behind}");
    }

    #[test]
    fn the_gap_half_counts_down_to_its_deadline() {
        let g = goal(0, 10_000, Some(1_000));
        let mut ledger = CoverageLedger::new(at(0));
        ledger.record(at(100), &g);
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
        // Fuenf Erfolge, verlangt ist bei neun Zyklen einer: der Anteil hat
        // reichlich Puffer, die Luecke laeuft ab.
        let mut t = 0;
        while t <= 400 {
            ledger.record(at(t), &g);
            t = t.saturating_add(100);
        }
        // Bei 1350 ms: der Anteil haette noch 300 ms Puffer (fuenf Erfolge,
        // verlangt sind zwei), die Luecke laeuft in 50 ms ab.
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
        let ledger = CoverageLedger::new(at(0));
        let very_late = ledger.slack(at(600_000), &g, ms(100));
        assert_eq!(very_late, Slack::from_nanos(-1_000_000_000));

        let mut full = CoverageLedger::new(at(0));
        let mut t = 0;
        while t < 1_000 {
            full.record(at(t), &g);
            t = t.saturating_add(10);
        }
        assert!(full.coverage_slack(at(1_000), &g, ms(100)) <= Slack::from_nanos(1_000_000_000));
    }

    #[test]
    fn old_results_fall_out_of_the_window() {
        let g = goal(500, 1_000, None);
        let mut ledger = CoverageLedger::new(at(0));
        let mut t = 0;
        while t < 1_000 {
            ledger.record(at(t), &g);
            t = t.saturating_add(100);
        }
        // Bei 900 ms sind alle zehn im Fenster. (Bei genau 1000 ms faellt die
        // aelteste knapp heraus: sechzehn Abschnitte zu 62 ms decken 992 ms
        // ab — dieselbe Koernung wie beim RuntimeLedger.)
        assert_eq!(ledger.fulfilled(at(900), &g), 10);
        assert_eq!(ledger.fulfilled(at(1_000), &g), 9);
        // Schon die blosse Abfrage weit spaeter sieht ein leeres Fenster —
        // ohne dass jemand etwas buchen muesste.
        assert_eq!(ledger.fulfilled(at(5_000), &g), 0);
        // Und eine Buchung danach steht allein darin.
        ledger.record(at(5_000), &g);
        assert_eq!(ledger.fulfilled(at(5_000), &g), 1);
    }
}
