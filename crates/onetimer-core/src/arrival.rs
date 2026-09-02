//! Wie schnell treffen Requests tatsaechlich ein?
//!
//! Der Vertrag eines Modells nennt eine Periode. Ob der Sensor sich daran
//! haelt, stand bisher nirgends — und das ist eine Luecke, die der Dauerlauf
//! sichtbar gemacht hat (`docs/benchmark/soak.md`).
//!
//! Liefert eine Kamera dauerhaft schneller als vereinbart, verwirft der
//! Governor mehr Frames, die Abdeckung faellt, und nichts sagt, warum. Der
//! Betreiber sieht schlechtere Zahlen und hat keinen Hinweis darauf, dass
//! seine Konfiguration nicht mehr zur Wirklichkeit passt. Das ist genau die
//! stille Fehlfunktion, die dieses System sonst konsequent sichtbar macht —
//! Aushungerung ist ein Befund (ADR-0012), ein veraltetes Profil ist ein
//! Befund (ADR-0016), und eine Last, die den Vertrag sprengt, gehoert
//! ebenfalls dazu.
//!
//! Gemessen wird der gleitende Mittelwert des Abstands zwischen zwei
//! Ankuenften. Er ist bewusst traege: eine einzelne dichte Folge ist ein
//! Burst und kein Konfigurationsfehler, und eine Warnung, die bei jedem Burst
//! kommt, wird ignoriert.

use crate::time::{Duration, Instant};

/// Der beobachtete Ankunftsabstand eines Modells.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArrivalTracker {
    last: Option<Instant>,
    /// Gleitender Mittelwert in Nanosekunden.
    ewma_nanos: u64,
    /// Wie viele Abstaende eingeflossen sind.
    samples: u32,
}

impl ArrivalTracker {
    /// Gewicht der neuen Messung als Kehrwert: 1/32.
    ///
    /// Ueber eine Schiebeoperation statt einer Division, damit der Pfad ohne
    /// Gleitkomma und ohne Division auskommt.
    ///
    /// Der Wert wurde von 1/8 heraufgesetzt, weil ein Mittelwert mit 1/8
    /// bereits nach fuenf dichten Ankuenften umschlaegt — ein Burst haette
    /// dann als Konfigurationsfehler gemeldet. Bei 1/32 liegt die Zeitkonstante
    /// bei rund 32 Abstaenden, also etwa einer Sekunde bei 30 ms Periode:
    /// traege genug fuer Bursts, schnell genug fuer eine echte Ratenaenderung.
    const SHIFT: u32 = 5;

    /// So viele Abstaende, bevor der Wert als belastbar gilt.
    ///
    /// Doppelt so viele wie die Zeitkonstante des Mittelwerts: vorher hat er
    /// sich noch nicht eingeschwungen, und eine Aussage waere eine Aussage
    /// ueber den Anlauf des Stroms, nicht ueber seinen Dauerbetrieb.
    pub const MIN_SAMPLES: u32 = 64;

    /// Toleranz, bevor eine Abweichung als Befund gilt, in Prozent.
    ///
    /// Ein Sensor, der fuenf Prozent schneller laeuft als der Vertrag sagt,
    /// ist normal — Taktungenauigkeit, Jitter, Rundung. Zwanzig Prozent
    /// dauerhaft sind es nicht.
    pub const TOLERANCE_PERCENT: u64 = 120;

    /// Eine Ankunft eintragen.
    pub fn record(&mut self, now: Instant) {
        if let Some(previous) = self.last {
            // `saturating_since` liefert bei Rueckwaertssprung null; ein
            // Abstand von null wuerde den Mittelwert verfaelschen und wird
            // deshalb uebergangen.
            let nanos = now.saturating_since(previous).as_nanos();
            if nanos == 0 {
                self.last = Some(now);
                return;
            }
            self.ewma_nanos = if self.samples == 0 {
                nanos
            } else {
                // ewma += (neu - ewma) / 8, ohne vorzeichenbehaftete
                // Zwischenwerte und ohne Ueberlauf.
                let old = self.ewma_nanos;
                if nanos >= old {
                    old.saturating_add((nanos.saturating_sub(old)) >> Self::SHIFT)
                } else {
                    old.saturating_sub((old.saturating_sub(nanos)) >> Self::SHIFT)
                }
            };
            self.samples = self.samples.saturating_add(1);
        }
        self.last = Some(now);
    }

    /// Der beobachtete Abstand, sobald genug Messungen vorliegen.
    #[must_use]
    pub fn observed(&self) -> Option<Duration> {
        if self.samples < Self::MIN_SAMPLES {
            return None;
        }
        Some(Duration::from_nanos_unbounded(self.ewma_nanos))
    }

    /// Kommt die Last dauerhaft schneller, als der Vertrag erlaubt?
    ///
    /// `None`, solange zu wenig gemessen wurde oder kein Vertrag mit Periode
    /// vorliegt — Schweigen ist hier richtiger als eine Vermutung.
    #[must_use]
    pub fn exceeds(&self, contract_period: Option<Duration>) -> Option<bool> {
        let observed = self.observed()?;
        let period = contract_period?;
        if period.as_nanos() == 0 {
            return None;
        }
        // Beobachtet * 120 % < Vertrag  <=>  Ankunftsrate mehr als 20 % zu hoch.
        let scaled = observed.as_nanos().saturating_mul(Self::TOLERANCE_PERCENT);
        Some(scaled < period.as_nanos().saturating_mul(100))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::integer_division
    )]

    use super::*;

    fn at(ms: u64) -> Instant {
        Instant::ZERO
            .checked_add(Duration::from_millis(ms).unwrap())
            .unwrap()
    }

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    /// Vor genug Messungen wird nichts behauptet.
    #[test]
    fn a_young_stream_makes_no_claim() {
        let mut tracker = ArrivalTracker::default();
        for i in 0..10 {
            tracker.record(at(i * 30));
        }
        assert_eq!(tracker.observed(), None);
        assert_eq!(tracker.exceeds(Some(ms(30))), None);
    }

    /// Ein Strom, der seinen Vertrag einhaelt, loest nichts aus.
    #[test]
    fn a_stream_at_its_contracted_rate_is_silent() {
        let mut tracker = ArrivalTracker::default();
        for i in 0..200 {
            tracker.record(at(i * 30));
        }
        assert_eq!(tracker.exceeds(Some(ms(30))), Some(false));
    }

    /// Fuenf Prozent schneller ist Jitter, kein Befund.
    #[test]
    fn slight_overspeed_stays_within_tolerance() {
        let mut tracker = ArrivalTracker::default();
        // 28,5 ms statt 30 ms — rund fuenf Prozent zu schnell.
        for i in 0..400 {
            tracker.record(at(i * 285 / 10));
        }
        assert_eq!(tracker.exceeds(Some(ms(30))), Some(false));
    }

    /// Die Haelfte der Periode ist doppelte Last und muss auffallen.
    #[test]
    fn a_stream_at_double_its_contracted_rate_is_reported() {
        let mut tracker = ArrivalTracker::default();
        for i in 0..200 {
            tracker.record(at(i * 15));
        }
        assert_eq!(tracker.exceeds(Some(ms(30))), Some(true));
    }

    /// Ein Strom, der langsamer liefert als vereinbart, ist kein Befund:
    /// er belastet niemanden.
    #[test]
    fn a_slow_stream_is_not_a_finding() {
        let mut tracker = ArrivalTracker::default();
        for i in 0..200 {
            tracker.record(at(i * 60));
        }
        assert_eq!(tracker.exceeds(Some(ms(30))), Some(false));
    }

    /// Ohne Vertragsperiode gibt es nichts zu vergleichen.
    #[test]
    fn without_a_contracted_period_nothing_is_claimed() {
        let mut tracker = ArrivalTracker::default();
        for i in 0..200 {
            tracker.record(at(i * 15));
        }
        assert_eq!(tracker.exceeds(None), None);
    }

    /// Der Mittelwert ist traege: ein einzelner Burst kippt ihn nicht.
    #[test]
    fn a_single_burst_does_not_flip_the_verdict() {
        let mut tracker = ArrivalTracker::default();
        for i in 0..200 {
            tracker.record(at(i * 30));
        }
        // Fuenf sehr dichte Ankuenfte hintereinander.
        let mut t = 200 * 30;
        for _ in 0..5 {
            t += 2;
            tracker.record(at(t));
        }
        assert_eq!(tracker.exceeds(Some(ms(30))), Some(false));
    }

    /// Die Gegenprobe: eine dauerhafte Ratenaenderung muss ankommen, sonst
    /// waere die Traegheit nur Blindheit.
    #[test]
    fn a_sustained_rate_change_is_detected() {
        let mut tracker = ArrivalTracker::default();
        for i in 0..200 {
            tracker.record(at(i * 30));
        }
        assert_eq!(tracker.exceeds(Some(ms(30))), Some(false));

        // Ab hier liefert der Sensor doppelt so schnell.
        let mut t = 200 * 30;
        for _ in 0..200 {
            t += 15;
            tracker.record(at(t));
        }
        assert_eq!(tracker.exceeds(Some(ms(30))), Some(true));
    }

    /// Und sie muss in vertretbarer Zeit ankommen, nicht irgendwann.
    #[test]
    fn a_sustained_rate_change_is_detected_within_seconds() {
        let mut tracker = ArrivalTracker::default();
        for i in 0..200 {
            tracker.record(at(i * 30));
        }
        let mut t = 200 * 30;
        let mut needed = None;
        for n in 1..=300_u64 {
            t += 15;
            tracker.record(at(t));
            if tracker.exceeds(Some(ms(30))) == Some(true) {
                needed = Some(n);
                break;
            }
        }
        let n = needed.expect("Ratenaenderung wird erkannt");
        // 15 ms je Ankunft: 100 Ankuenfte sind 1,5 Sekunden.
        assert!(n <= 100, "erst nach {n} Ankuenften erkannt");
    }
}
