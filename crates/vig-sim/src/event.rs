//! Ereigniswarteschlange und virtuelle Uhr der Simulation.
//!
//! Der Simulator ist ereignisgesteuert: die Zeit springt von Ereignis zu
//! Ereignis, statt in festen Schritten zu laufen. Damit ist ein Lauf ueber
//! Stunden simulierter Zeit in Sekunden Rechenzeit moeglich und — wichtiger —
//! exakt reproduzierbar.
//!
//! ## Determinismus
//!
//! Zwei gleichzeitige Ereignisse muessen eine definierte Reihenfolge haben,
//! sonst haengt das Ergebnis von der Heap-Implementierung ab. Jedes Ereignis
//! traegt deshalb eine monoton vergebene Einfuegenummer, die als
//! Zweitschluessel dient. Gleicher Seed erzeugt damit garantiert identische
//! Reihenfolge (Spec WP1, DoD).

use core::cmp::Ordering;
use std::collections::BinaryHeap;
use vig_core::{Instant, ModelIdx, RequestId, SlotIdx, SupersessionKey};

/// Ein Ereignis im Simulationslauf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimEvent {
    /// Ein neuer Request trifft am Gateway ein.
    Arrival {
        /// Das angefragte logische Modell.
        model: ModelIdx,
        /// Der Freshness-Scope.
        key: SupersessionKey,
        /// Die Capture-Zeit des zugrunde liegenden Sensordatums.
        ///
        /// Getrennt von der Ankunftszeit, damit der Simulator Transport- und
        /// Vorverarbeitungsverzoegerung abbilden kann (Spec 10.2).
        generation: Instant,
    },
    /// Das Backend meldet die Fertigstellung einer Inferenz.
    Completion {
        /// Der fertiggestellte Request.
        request: RequestId,
        /// Der Slot, der dadurch frei wird.
        slot: SlotIdx,
    },
    /// Das Backend meldet einen Fehler.
    BackendFailure {
        /// Der betroffene Request.
        request: RequestId,
        /// Der Slot, der dadurch frei wird.
        slot: SlotIdx,
    },
    /// Periodischer Weckruf des Schedulers.
    ///
    /// Notwendig, weil non-work-conserving Entscheidungen (Spec 10.7) eine
    /// Aktion zu einem Zeitpunkt ausloesen, an dem sonst kein Ereignis liegt:
    /// „jetzt ist der reservierte Zeitpunkt, jetzt darf gestartet werden."
    Tick,
    /// Ende des Messfensters.
    EndOfRun,
}

/// Ein Ereignis mit Ausfuehrungszeitpunkt und Einfuegenummer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scheduled {
    at: Instant,
    seq: u64,
    event: SimEvent,
}

impl Ord for Scheduled {
    fn cmp(&self, other: &Self) -> Ordering {
        // Umgekehrt, weil BinaryHeap ein Max-Heap ist: kleinste Zeit zuerst.
        other
            .at
            .cmp(&self.at)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Scheduled {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Die virtuelle Uhr mit ihrer Ereigniswarteschlange.
///
/// Die Uhr laeuft ausschliesslich vorwaerts. Ein Versuch, ein Ereignis in die
/// Vergangenheit einzuplanen, ist ein Programmierfehler im Simulationsmodell
/// und wird als solcher gemeldet, statt still korrigiert zu werden.
#[derive(Debug)]
pub struct SimClock {
    now: Instant,
    seq: u64,
    queue: BinaryHeap<Scheduled>,
    processed: u64,
}

/// Fehler beim Einplanen eines Ereignisses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    /// Der Zeitpunkt liegt vor der aktuellen Simulationszeit.
    InThePast {
        /// Der angeforderte Zeitpunkt.
        requested: Instant,
        /// Die aktuelle Simulationszeit.
        now: Instant,
    },
    /// Der Zeitpunkt ist durch Ueberlauf nicht darstellbar.
    Unrepresentable,
}

impl core::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InThePast { requested, now } => {
                write!(
                    f,
                    "Ereignis fuer {requested} liegt vor der Simulationszeit {now}"
                )
            }
            Self::Unrepresentable => write!(f, "Zeitpunkt nicht darstellbar"),
        }
    }
}

impl std::error::Error for ScheduleError {}

impl SimClock {
    /// Eine neue Uhr, die bei [`Instant::ZERO`] startet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            now: Instant::ZERO,
            seq: 0,
            queue: BinaryHeap::new(),
            processed: 0,
        }
    }

    /// Die aktuelle Simulationszeit.
    #[must_use]
    pub const fn now(&self) -> Instant {
        self.now
    }

    /// Die Anzahl bereits abgearbeiteter Ereignisse.
    #[must_use]
    pub const fn processed(&self) -> u64 {
        self.processed
    }

    /// Die Anzahl noch anstehender Ereignisse.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Plant ein Ereignis fuer den absoluten Zeitpunkt `at` ein.
    ///
    /// # Errors
    ///
    /// [`ScheduleError::InThePast`], wenn `at` vor der aktuellen Simulationszeit
    /// liegt. Die Uhr darf nicht rueckwaerts laufen.
    pub fn schedule(&mut self, at: Instant, event: SimEvent) -> Result<(), ScheduleError> {
        if at < self.now {
            return Err(ScheduleError::InThePast {
                requested: at,
                now: self.now,
            });
        }
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.queue.push(Scheduled { at, seq, event });
        Ok(())
    }

    /// Entnimmt das naechste Ereignis und setzt die Uhr auf dessen Zeitpunkt.
    ///
    /// Gibt `None` zurueck, wenn keine Ereignisse mehr anstehen.
    pub fn advance(&mut self) -> Option<(Instant, SimEvent)> {
        let next = self.queue.pop()?;
        self.now = next.at;
        self.processed = self.processed.wrapping_add(1);
        Some((next.at, next.event))
    }

    /// Verwirft alle noch anstehenden Ereignisse.
    pub fn drain(&mut self) {
        self.queue.clear();
    }
}

impl Default for SimClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;
    use vig_core::Duration;

    fn tick_at(clock: &mut SimClock, ms: u64) {
        let at = Instant::ZERO
            .checked_add(Duration::from_millis(ms).unwrap())
            .unwrap();
        clock.schedule(at, SimEvent::Tick).unwrap();
    }

    #[test]
    fn events_come_out_in_time_order() {
        let mut clock = SimClock::new();
        tick_at(&mut clock, 30);
        tick_at(&mut clock, 10);
        tick_at(&mut clock, 20);

        let mut order = Vec::new();
        while let Some((at, _)) = clock.advance() {
            order.push(at.as_nanos());
        }
        assert_eq!(order, vec![10_000_000, 20_000_000, 30_000_000]);
    }

    #[test]
    fn simultaneous_events_keep_insertion_order() {
        let mut clock = SimClock::new();
        let at = Instant::from_nanos(5_000);
        clock.schedule(at, SimEvent::Tick).unwrap();
        clock
            .schedule(
                at,
                SimEvent::Completion {
                    request: RequestId(1),
                    slot: SlotIdx(0),
                },
            )
            .unwrap();
        clock.schedule(at, SimEvent::EndOfRun).unwrap();

        assert_eq!(clock.advance().map(|(_, e)| e), Some(SimEvent::Tick));
        assert!(matches!(
            clock.advance().map(|(_, e)| e),
            Some(SimEvent::Completion { .. })
        ));
        assert_eq!(clock.advance().map(|(_, e)| e), Some(SimEvent::EndOfRun));
    }

    #[test]
    fn clock_refuses_to_run_backwards() {
        let mut clock = SimClock::new();
        tick_at(&mut clock, 10);
        let _ = clock.advance();
        let err = clock.schedule(Instant::ZERO, SimEvent::Tick).unwrap_err();
        assert!(matches!(err, ScheduleError::InThePast { .. }));
    }
}
