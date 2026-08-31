//! Ereignis-Digest fuer Reproduzierbarkeitsnachweise.
//!
//! Spec 30.2 verlangt, dass ein Scheduler-Trace offline im Simulator
//! reproduzierbar ist; Spec WP1 verlangt, dass derselbe Seed dieselbe
//! Ereignisreihenfolge **und** dieselben Kennzahlen erzeugt.
//!
//! Beides laesst sich nicht sinnvoll durch Vergleich vollstaendiger Traces
//! pruefen — 100 000 Ereignisse als Testfixture waeren unlesbar und wuerden bei
//! jeder Modellaenderung neu geschrieben, was den Test wertlos macht. Statt
//! dessen wird ein Digest ueber den Ereignisstrom gebildet: ein einziger Wert,
//! der sich aendert, sobald sich Reihenfolge, Zeitpunkt oder Inhalt eines
//! Ereignisses aendert.

use crate::event::SimEvent;
use onetimer_core::Instant;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Ein laufender Digest ueber einen Ereignisstrom.
///
/// FNV-1a: klein, dependencyfrei, und dauerhaft stabil — anders als ein
/// Hash aus einem Crate, dessen Implementierung sich zwischen Versionen
/// aendern darf. Kryptographische Staerke ist hier nicht gefordert; gefordert
/// ist, dass derselbe Lauf in zwei Jahren denselben Wert liefert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceDigest {
    hash: u64,
    count: u64,
}

impl TraceDigest {
    /// Ein leerer Digest.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hash: FNV_OFFSET,
            count: 0,
        }
    }

    /// Die Anzahl eingespeister Ereignisse.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Der aktuelle Digestwert.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.hash
    }

    fn absorb(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.hash ^= u64::from(b);
            self.hash = self.hash.wrapping_mul(FNV_PRIME);
        }
    }

    /// Nimmt ein Ereignis samt Zeitpunkt auf.
    pub fn record(&mut self, at: Instant, event: &SimEvent) {
        self.absorb(at.as_nanos());
        match *event {
            SimEvent::Arrival {
                model,
                key,
                generation,
            } => {
                self.absorb(1);
                self.absorb(u64::from(model.0));
                self.absorb(key.0);
                self.absorb(generation.as_nanos());
            }
            SimEvent::Completion { request, slot } => {
                self.absorb(2);
                self.absorb(request.0);
                self.absorb(u64::from(slot.0));
            }
            SimEvent::BackendFailure { request, slot } => {
                self.absorb(3);
                self.absorb(request.0);
                self.absorb(u64::from(slot.0));
            }
            SimEvent::Tick => self.absorb(4),
            SimEvent::EndOfRun => self.absorb(5),
        }
        self.count = self.count.wrapping_add(1);
    }

    /// Nimmt eine Kennzahl auf, damit auch Metriken vom Digest gedeckt sind.
    pub fn record_metric(&mut self, label: &str, value: u64) {
        for b in label.as_bytes() {
            self.hash ^= u64::from(*b);
            self.hash = self.hash.wrapping_mul(FNV_PRIME);
        }
        self.absorb(value);
    }
}

impl Default for TraceDigest {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Display for TraceDigest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:016x} ({} Ereignisse)", self.hash, self.count)
    }
}
