//! Mindestlaufzeit fuer nachrangige Arbeit (ADR-0046).
//!
//! ## Wofuer
//!
//! Die lexikographische Ordnung (Spec 10.6) laesst `best_effort` erst laufen,
//! wenn keine `normal`-Arbeit mehr wartet. Unter Dauerlast wartet immer
//! welche: vier Kameras bei 30 Bildern/s und ein Sprachbildmodell daneben
//! ergaben zwei Antworten in 40 Sekunden. Ein Budget sagt: „dieses Modell
//! bekommt mindestens `budget` Ausfuehrungszeit je `window`, bezahlt von den
//! nachrangigen Klassen — nie von bewachter Arbeit".
//!
//! ## Was gebucht wird
//!
//! Beim Dispatch die **geplante** Laufzeit, bei der Fertigstellung ersetzt
//! durch die **gemessene**. Die Planung muss sofort sichtbar sein: ohne sie
//! gaelte ein gerade gestarteter 150-ms-Auftrag bis zu seinem Ende als nicht
//! verbraucht, und ein zweiter Slot naehme denselben Vorrang noch einmal.
//!
//! Gebucht wird am **Dispatchzeitpunkt**. Ein Auftrag zaehlt damit ganz in das
//! Fenster, in dem er begann, auch wenn er darueber hinaus rechnet — ein nicht
//! unterbrechbarer Block laesst sich nicht anteilig starten.
//!
//! ## Das Fenster
//!
//! Gleitend, in [`LEDGER_BUCKETS`] Abschnitten fester Laenge. Eine Buchung
//! faellt fruehestens nach `15/16 window` und spaetestens nach `window` aus dem
//! Fenster; genauer braucht es eine Zusage nicht, die ohnehin um bis zu einen
//! Auftrag ueberzogen wird. Der Speicher ist fest, der Aufwand je Abfrage
//! konstant (Spec L-003).

use crate::ids::MAX_SLOTS;
use crate::request::Criticality;
use crate::time::{Duration, Instant};

/// Wie viele Abschnitte ein Fenster hat. Zweierpotenz: Index ohne Division.
pub const LEDGER_BUCKETS: u64 = 16;

/// Die Maske fuer [`LEDGER_BUCKETS`].
const BUCKET_MASK: u64 = 15;

/// Das laengste zulaessige Fenster: eine Minute.
///
/// Laenger traegt die Aussage „mindestens so viel je Fenster" nicht mehr: ein
/// Budget, das eine Minute lang aufgespart und dann am Stueck abgerufen wird,
/// ist fuer den Verbraucher kein Fortschritt, sondern eine Pause.
pub const MAX_RUNTIME_WINDOW: Duration = Duration::from_nanos_unbounded(60_000_000_000);

/// Das vereinbarte Mindestlaufzeitbudget eines Modells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeBudget {
    /// So viel Ausfuehrungszeit steht dem Modell je Fenster mindestens zu.
    pub budget: Duration,
    /// Die Laenge des gleitenden Fensters.
    pub window: Duration,
}

/// Warum ein Budget unzulaessig ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBudgetError {
    /// Budget oder Fenster ist null.
    Zero,
    /// Das Fenster ist laenger als [`MAX_RUNTIME_WINDOW`].
    WindowTooLong,
    /// Das Modell ist bewacht (`protected` oder `high`).
    ///
    /// Diese Klassen stehen bereits ueber jedem Budget; eines zu vereinbaren
    /// hiesse, eine Zusage aufzuschreiben, die nichts bewirkt.
    GuardedClass,
    /// Das Budget uebersteigt, was die Slots in einem Fenster rechnen koennen.
    BeyondSlots {
        /// Die Zahl der regulaeren Slots.
        slots: usize,
    },
}

impl core::fmt::Display for RuntimeBudgetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Zero => write!(
                f,
                "min_runtime: budget und window muessen groesser als null sein"
            ),
            Self::WindowTooLong => write!(
                f,
                "min_runtime: window hoechstens {} ms",
                MAX_RUNTIME_WINDOW.as_millis()
            ),
            Self::GuardedClass => write!(
                f,
                "min_runtime nur fuer normal und best_effort; protected und high stehen \
                 bereits ueber jedem Budget (ADR-0046)"
            ),
            Self::BeyondSlots { slots } => write!(
                f,
                "min_runtime: budget ueber window mal {slots} Slot(s); so viel kann in \
                 einem Fenster nicht rechnen"
            ),
        }
    }
}

impl core::error::Error for RuntimeBudgetError {}

impl RuntimeBudget {
    /// Prueft das Budget gegen die Klasse seines Modells.
    ///
    /// # Errors
    ///
    /// Siehe [`RuntimeBudgetError`]; die Slotzahl prueft [`Self::fits_slots`].
    pub const fn validate(&self, criticality: Criticality) -> Result<(), RuntimeBudgetError> {
        if self.budget.as_nanos() == 0 || self.window.as_nanos() == 0 {
            return Err(RuntimeBudgetError::Zero);
        }
        if self.window.as_nanos() > MAX_RUNTIME_WINDOW.as_nanos() {
            return Err(RuntimeBudgetError::WindowTooLong);
        }
        if criticality.is_guarded() {
            return Err(RuntimeBudgetError::GuardedClass);
        }
        if !self.fits_slots(MAX_SLOTS) {
            return Err(RuntimeBudgetError::BeyondSlots { slots: MAX_SLOTS });
        }
        Ok(())
    }

    /// Ob `slots` Slots das Budget in einem Fenster ueberhaupt rechnen koennen.
    #[must_use]
    pub const fn fits_slots(&self, slots: usize) -> bool {
        let slots = slots as u64;
        self.budget.as_nanos() <= self.window.as_nanos().saturating_mul(slots)
    }

    /// Der Anteil eines Slots, den das Budget belegt, in Promille.
    ///
    /// `budget / window`: 300 ms je Sekunde sind 300 ‰ eines Slots.
    #[must_use]
    pub fn slot_share_permille(&self) -> u64 {
        self.budget
            .as_nanos()
            .saturating_mul(1_000)
            .checked_div(self.window.as_nanos().max(1))
            .unwrap_or(0)
    }
}

/// Die Buchfuehrung eines Budgets ueber das gleitende Fenster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeLedger {
    budget: RuntimeBudget,
    /// Die Laenge eines Abschnitts in Nanosekunden, mindestens 1.
    bucket: u64,
    /// Die laufende Nummer des juengsten beschriebenen Abschnitts.
    ///
    /// `None`, solange nichts gebucht wurde.
    head: Option<u64>,
    /// Gebuchte Nanosekunden je Abschnitt; Index `Nummer & 15`.
    buckets: [u64; 16],
}

impl RuntimeLedger {
    /// Eine leere Buchfuehrung fuer dieses Budget.
    #[must_use]
    pub fn new(budget: RuntimeBudget) -> Self {
        Self {
            budget,
            bucket: budget
                .window
                .as_nanos()
                .checked_div(LEDGER_BUCKETS)
                .unwrap_or(1)
                .max(1),
            head: None,
            buckets: [0; 16],
        }
    }

    /// Das vereinbarte Budget.
    #[must_use]
    pub const fn budget(&self) -> RuntimeBudget {
        self.budget
    }

    /// Bucht die geplante Laufzeit eines Dispatches.
    pub fn charge(&mut self, at: Instant, runtime: Duration) {
        let epoch = self.epoch(at);
        self.advance(epoch);
        if let Some(cell) = self.cell_mut(epoch) {
            *cell = cell.saturating_add(runtime.as_nanos());
        }
    }

    /// Ersetzt die geplante durch die gemessene Laufzeit.
    ///
    /// Gebucht bleibt am Dispatchzeitpunkt. Ist dessen Abschnitt schon aus dem
    /// Fenster gefallen, gibt es nichts mehr zu berichtigen.
    pub fn settle(
        &mut self,
        dispatched: Instant,
        planned: Duration,
        measured: Duration,
        now: Instant,
    ) {
        self.advance(self.epoch(now));
        let epoch = self.epoch(dispatched);
        if let Some(cell) = self.cell_mut(epoch) {
            *cell = cell
                .saturating_sub(planned.as_nanos())
                .saturating_add(measured.as_nanos());
        }
    }

    /// Die im Fenster bis `now` gebuchte Ausfuehrungszeit.
    #[must_use]
    pub fn used(&self, now: Instant) -> Duration {
        let Some(head) = self.head else {
            return Duration::ZERO;
        };
        let epoch = self.epoch(now);
        let span = BUCKET_MASK;
        let lowest = epoch.saturating_sub(span).max(head.saturating_sub(span));
        let highest = head.min(epoch);
        let mut total = 0_u64;
        let mut current = lowest;
        while current <= highest {
            if let Some(cell) = self.buckets.get(index(current)) {
                total = total.saturating_add(*cell);
            }
            current = current.saturating_add(1);
            if current == 0 {
                break;
            }
        }
        Duration::from_nanos_unbounded(total)
    }

    /// Ob im Fenster noch Budget bleibt.
    ///
    /// Echt kleiner: ein Budget, das genau aufgebraucht ist, gibt keinen
    /// Vorrang mehr.
    #[must_use]
    pub fn remaining(&self, now: Instant) -> bool {
        self.used(now).as_nanos() < self.budget.budget.as_nanos()
    }

    fn epoch(&self, at: Instant) -> u64 {
        at.as_nanos().checked_div(self.bucket).unwrap_or(0)
    }

    /// Schiebt den juengsten Abschnitt auf `epoch` und leert, was dazwischen
    /// neu beginnt.
    fn advance(&mut self, epoch: u64) {
        let Some(head) = self.head else {
            self.head = Some(epoch);
            return;
        };
        if epoch <= head {
            return;
        }
        let steps = epoch.saturating_sub(head).min(LEDGER_BUCKETS);
        for step in 1..=steps {
            if let Some(cell) = self.buckets.get_mut(index(head.saturating_add(step))) {
                *cell = 0;
            }
        }
        self.head = Some(epoch);
    }

    /// Der Abschnitt einer Nummer, solange er noch im Ring liegt.
    fn cell_mut(&mut self, epoch: u64) -> Option<&mut u64> {
        let head = self.head?;
        if epoch > head || head.saturating_sub(epoch) > BUCKET_MASK {
            return None;
        }
        self.buckets.get_mut(index(epoch))
    }
}

/// Der Ringindex einer Abschnittsnummer.
fn index(epoch: u64) -> usize {
    usize::try_from(epoch & BUCKET_MASK).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_nanos_unbounded(v.saturating_mul(1_000_000))
    }

    fn at(v: u64) -> Instant {
        Instant::from_nanos(v.saturating_mul(1_000_000))
    }

    fn per_second(budget_ms: u64) -> RuntimeLedger {
        RuntimeLedger::new(RuntimeBudget {
            budget: ms(budget_ms),
            window: ms(1_600),
        })
    }

    #[test]
    fn a_budget_is_checked_against_its_class_and_the_slots() {
        let ok = RuntimeBudget {
            budget: ms(300),
            window: ms(1_000),
        };
        assert_eq!(ok.validate(Criticality::BestEffort), Ok(()));
        assert_eq!(ok.validate(Criticality::Normal), Ok(()));
        assert_eq!(
            ok.validate(Criticality::High),
            Err(RuntimeBudgetError::GuardedClass)
        );
        assert_eq!(
            ok.validate(Criticality::Protected),
            Err(RuntimeBudgetError::GuardedClass)
        );
        let zero = RuntimeBudget {
            window: Duration::ZERO,
            ..ok
        };
        assert_eq!(
            zero.validate(Criticality::BestEffort),
            Err(RuntimeBudgetError::Zero)
        );
        let two_slots = RuntimeBudget {
            budget: ms(2_000),
            ..ok
        };
        assert!(two_slots.fits_slots(2));
        assert!(!two_slots.fits_slots(1));
        assert_eq!(ok.slot_share_permille(), 300);
    }

    #[test]
    fn a_charge_counts_until_the_window_has_passed() {
        // 1600 ms in 16 Abschnitten zu je 100 ms.
        let mut ledger = per_second(300);
        ledger.charge(at(50), ms(150));
        assert_eq!(ledger.used(at(50)), ms(150));
        assert!(ledger.remaining(at(50)));
        ledger.charge(at(250), ms(150));
        assert_eq!(ledger.used(at(260)), ms(300));
        assert!(
            !ledger.remaining(at(260)),
            "genau aufgebraucht ist aufgebraucht"
        );
        // Der Abschnitt 0 (0..100 ms) faellt mit Abschnitt 16 heraus.
        assert_eq!(ledger.used(at(1_599)), ms(300));
        assert_eq!(ledger.used(at(1_600)), ms(150));
        assert!(ledger.remaining(at(1_600)));
        assert_eq!(ledger.used(at(1_800)), ms(0));
    }

    #[test]
    fn the_measured_runtime_replaces_the_planned_one() {
        let mut ledger = per_second(300);
        ledger.charge(at(0), ms(187));
        ledger.settle(at(0), ms(187), ms(150), at(150));
        assert_eq!(ledger.used(at(150)), ms(150));
        // Laenger als geplant zaehlt ebenso.
        ledger.charge(at(160), ms(100));
        ledger.settle(at(160), ms(100), ms(130), at(290));
        assert_eq!(ledger.used(at(290)), ms(280));
    }

    #[test]
    fn a_settlement_after_the_window_changes_nothing() {
        let mut ledger = per_second(300);
        ledger.charge(at(0), ms(100));
        ledger.settle(at(0), ms(100), ms(5_000), at(2_000));
        assert_eq!(ledger.used(at(2_000)), ms(0));
    }

    #[test]
    fn a_long_silence_forgets_everything_without_cost() {
        let mut ledger = per_second(300);
        ledger.charge(at(10), ms(250));
        ledger.charge(at(3_600_000), ms(20));
        assert_eq!(ledger.used(at(3_600_000)), ms(20));
    }
}
