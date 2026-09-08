//! Verwaltung der Shared-Memory-Regionen (ADR-0003).
//!
//! ## Warum Vigilant hier ueberhaupt etwas tut
//!
//! Auf dem Shm-Pfad traegt ein Inferenzrequest keine Tensordaten, sondern nur
//! eine Referenz: Regionsname, Offset, Groesse. Vigilant reicht diese Angaben
//! unveraendert weiter und **beruehrt die Nutzlast nie** — der Aufwand des
//! Governors wird damit unabhaengig von der Tensorgroesse. Gemessen: 212 us
//! statt 9375 us bei einem 6,2-MB-Frame.
//!
//! Damit das funktioniert, muessen die Registrierungsaufrufe ebenfalls
//! durchgereicht werden. Genau daraus entsteht eine Verantwortung: eine Region,
//! die ein Client registriert und nie wieder abmeldet, bleibt im Backend
//! liegen. Ohne Buchfuehrung waere Vigilant die Stelle, an der diese Information
//! verloren geht.
//!
//! ## Was hier bewusst nicht passiert
//!
//! Regionen werden **nicht** automatisch beim Verbindungsabbruch eines Clients
//! freigegeben. gRPC kennt keine Sitzung, und eine Zuordnung ueber die
//! Gegenstelle waere bei mehreren Clients hinter einem Proxy falsch — im
//! Zweifel wuerde Vigilant eine Region abmelden, die ein anderer noch benutzt.
//! Das waere schlimmer als ein Leck. Stattdessen wird der Bestand gefuehrt und
//! sichtbar gemacht; `vig serve` meldet beim Herunterfahren ab, was es
//! selbst kennt.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Eine registrierte Region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Groesse der Region in Bytes.
    pub byte_size: u64,
    /// Offset innerhalb des zugrunde liegenden Objekts.
    pub offset: u64,
    /// Wahr, wenn es sich um CUDA- statt System-Shared-Memory handelt.
    pub cuda: bool,
}

/// Der Bestand registrierter Regionen.
///
/// `BTreeMap` und nicht `HashMap`: der Bestand wird ausgegeben, und eine
/// stabile Reihenfolge macht Diagnose und Tests reproduzierbar.
#[derive(Debug, Default)]
pub struct ShmRegistry {
    regions: Mutex<BTreeMap<String, Region>>,
}

impl ShmRegistry {
    /// Ein leerer Bestand.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Vermerkt eine Registrierung.
    ///
    /// Wird erst aufgerufen, **nachdem** das Backend die Registrierung
    /// bestaetigt hat. Andernfalls fuehrte Vigilant Regionen, die es nicht gibt.
    pub fn record(&self, name: String, region: Region) {
        if let Ok(mut regions) = self.regions.lock() {
            regions.insert(name, region);
        }
    }

    /// Vermerkt eine Abmeldung.
    ///
    /// Ein leerer Name bedeutet im Protokoll „alle Regionen".
    pub fn forget(&self, name: &str) {
        let Ok(mut regions) = self.regions.lock() else {
            return;
        };
        if name.is_empty() {
            regions.clear();
        } else {
            regions.remove(name);
        }
    }

    /// Die Anzahl registrierter Regionen.
    #[must_use]
    pub fn len(&self) -> usize {
        self.regions.lock().map_or(0, |regions| regions.len())
    }

    /// Wahr, wenn keine Region registriert ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Die insgesamt registrierte Bytemenge.
    ///
    /// Als Metrik nuetzlich: wachsende Bytemengen ohne Abmeldungen sind das
    /// sichtbare Symptom eines Clients, der seine Regionen nicht aufraeumt.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.regions.lock().map_or(0, |regions| {
            regions
                .values()
                .map(|r| r.byte_size)
                .fold(0_u64, u64::saturating_add)
        })
    }

    /// Die Namen aller registrierten Regionen.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.regions
            .lock()
            .map(|regions| regions.keys().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(bytes: u64) -> Region {
        Region {
            byte_size: bytes,
            offset: 0,
            cuda: false,
        }
    }

    #[test]
    fn registrations_are_tracked_and_summed() {
        let registry = ShmRegistry::new();
        assert!(registry.is_empty());

        registry.record("frames".to_owned(), region(6_220_800));
        registry.record("depth".to_owned(), region(2_073_600));
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.total_bytes(), 8_294_400);
        assert_eq!(
            registry.names(),
            vec!["depth".to_owned(), "frames".to_owned()]
        );

        registry.forget("frames");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_bytes(), 2_073_600);
    }

    /// Im Protokoll bedeutet ein leerer Name „alle Regionen".
    #[test]
    fn an_empty_name_unregisters_everything() {
        let registry = ShmRegistry::new();
        registry.record("a".to_owned(), region(1));
        registry.record("b".to_owned(), region(2));
        registry.forget("");
        assert!(registry.is_empty());
    }

    /// Eine erneute Registrierung desselben Namens ersetzt den Eintrag, statt
    /// ihn zu verdoppeln — sonst waere die Bytemenge nach einem Reconnect falsch.
    #[test]
    fn re_registering_replaces_instead_of_accumulating() {
        let registry = ShmRegistry::new();
        registry.record("frames".to_owned(), region(100));
        registry.record("frames".to_owned(), region(200));
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_bytes(), 200);
    }
}
