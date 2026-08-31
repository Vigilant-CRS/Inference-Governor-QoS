//! Die lokale monotone Zeitbasis des Gateways.
//!
//! Spec L-019 verbietet die Wall-Clock als Scheduling-Grundlage. Der
//! Scheduling-Kern kennt deshalb ueberhaupt keine Uhr; er bekommt `now` an
//! jedem Eintrittspunkt uebergeben. Dieses Modul ist die einzige Stelle, an der
//! eine echte Uhr abgelesen wird.

use onetimer_core::Instant;

/// Eine monotone Uhr mit einem festen Nullpunkt beim Prozessstart.
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    origin: std::time::Instant,
}

impl MonotonicClock {
    /// Startet die Uhr.
    #[must_use]
    pub fn start() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }

    /// Die aktuelle Zeit in der Zeitbasis des Schedulers.
    ///
    /// `u64`-Nanosekunden reichen fuer 584 Jahre Laufzeit; der Ueberlauf wird
    /// trotzdem saettigend behandelt, statt zu wrappen — eine rueckwaerts
    /// laufende Uhr waere fuer den Scheduler schlimmer als eine stehende.
    #[must_use]
    pub fn now(&self) -> Instant {
        let nanos = self.origin.elapsed().as_nanos();
        Instant::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::start()
    }
}
