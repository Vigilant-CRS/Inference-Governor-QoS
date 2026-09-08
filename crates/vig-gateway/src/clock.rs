//! Die lokale monotone Zeitbasis des Gateways.
//!
//! Spec L-019 verbietet die Wall-Clock als Scheduling-Grundlage. Der
//! Scheduling-Kern kennt deshalb ueberhaupt keine Uhr; er bekommt `now` an
//! jedem Eintrittspunkt uebergeben. Dieses Modul ist die einzige Stelle, an der
//! eine echte Uhr abgelesen wird.

use vig_core::Instant;

/// Vorlauf des Nullpunkts gegenueber dem Prozessstart, in Nanosekunden.
///
/// Ohne Vorlauf beginnt die Zeitbasis bei null, und vor diesem Nullpunkt gibt
/// es keine darstellbare Zeit. `arrival - age` schneidet dann in den ersten
/// Sekunden nach dem Start jede Altersangabe ab, die aelter ist als der
/// Prozess selbst: ein Frame, der bei Sekunde 0,3 mit zwei Sekunden Alter
/// eintrifft, waere anschliessend 0,3 Sekunden alt. Genau die Verjuengung, die
/// die Alterspruefung verhindern soll — und ausgerechnet direkt nach einem
/// Neustart, wenn die Clients noch mit alten Frames in der Leitung stehen.
///
/// Eine Stunde Vorlauf kostet nichts: `u64`-Nanosekunden reichen fuer 584
/// Jahre. Sie deckt jede plausible Altersangabe ab.
pub const EPOCH_HEADROOM_NANOS: u64 = 3_600 * 1_000_000_000;

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
        let since_start = u64::try_from(nanos).unwrap_or(u64::MAX);
        Instant::from_nanos(since_start.saturating_add(EPOCH_HEADROOM_NANOS))
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::start()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{EPOCH_HEADROOM_NANOS, MonotonicClock};
    use vig_core::Duration;
    use vig_protocol_oip::params::{GenerationHint, VigParams, resolve_generation};

    /// Ein zwei Sekunden altes Datum bleibt zwei Sekunden alt — auch, wenn der
    /// Prozess gerade erst gestartet ist.
    ///
    /// Ohne Vorlauf des Nullpunkts waere das Alter hier auf die wenigen
    /// Mikrosekunden seit dem Start gekuerzt worden. Der Frame saehe taufrisch
    /// aus und bekaeme die volle Deadline, statt verworfen zu werden.
    #[test]
    fn an_old_frame_right_after_startup_keeps_its_age() {
        let clock = MonotonicClock::start();
        let arrival = clock.now();
        assert!(
            arrival.as_nanos() >= EPOCH_HEADROOM_NANOS,
            "die Zeitbasis hat Vorlauf"
        );

        let params = VigParams {
            generation: Some(GenerationHint::AgeMicros(2_000_000)),
            ..VigParams::default()
        };
        let plausible = Duration::from_millis(5_000).unwrap();
        let (generation, _) = resolve_generation(&params, arrival, plausible);
        assert_eq!(
            arrival.saturating_since(generation),
            Duration::from_millis(2_000).unwrap()
        );
    }
}
