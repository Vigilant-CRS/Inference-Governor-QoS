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
//! ## Besitz (Security-Review H2, M4, N7)
//!
//! Das Backend laeuft mit Zugriff auf das `/dev/shm` des Hosts. Eine
//! Registrierung ohne Pruefung waere damit eine Lese- und Schreibprimitive
//! fuer fremden Speicher: als Eingaberegion liest ein Aufrufer die Frames
//! eines anderen, als Ausgaberegion schreibt das Backend in dessen Segment.
//! Deshalb:
//!
//! * **Ein Schluessel muss den Praefix des Betreibers tragen**
//!   (`backend.security.shm_key_prefix`, Voreinstellung `/vig_`). Segmente
//!   anderer Prozesse sind ueber den Governor nicht registrierbar.
//! * **Eine Region gehoert, wer sie registriert hat.** Abmelden darf nur der
//!   Besitzer; „alle abmelden" nur ein Administrationstoken.
//! * **Unter `trust: strict` darf eine Inferenz nur Regionen nennen, die ihr
//!   Aufrufer ueber diesen Governor registriert hat.** Unter `trust: open`
//!   bleibt es beim bisherigen Verhalten — dort gilt ohnehin „ein Betreiber,
//!   ein abgeschlossenes Netz", und Clients registrieren oft direkt beim
//!   Backend.
//! * **Der Bestand ist begrenzt** (`backend.security.max_shm_regions`).
//!
//! ## Was hier bewusst nicht passiert
//!
//! Regionen werden **nicht** automatisch beim Verbindungsabbruch eines Clients
//! freigegeben. gRPC kennt keine Sitzung, und eine Zuordnung ueber die
//! Gegenstelle waere bei mehreren Clients hinter einem Proxy falsch — im
//! Zweifel wuerde Vigilant eine Region abmelden, die ein anderer noch benutzt.
//! Das waere schlimmer als ein Leck. Stattdessen wird der Bestand gefuehrt,
//! begrenzt und sichtbar gemacht.

use crate::auth::Identity;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Die Voreinstellung fuer die Hoechstzahl registrierter Regionen.
pub const DEFAULT_MAX_REGIONS: usize = 256;

/// Die laengste zulaessige Schluessellaenge (POSIX `NAME_MAX`).
pub const MAX_KEY_LEN: usize = 255;

/// Eine registrierte Region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Groesse der Region in Bytes.
    pub byte_size: u64,
    /// Offset innerhalb des zugrunde liegenden Objekts.
    pub offset: u64,
    /// Wahr, wenn es sich um CUDA- statt System-Shared-Memory handelt.
    pub cuda: bool,
    /// Wer sie ueber diesen Governor registriert hat.
    pub owner: Identity,
}

/// Warum eine Registrierung nicht angenommen wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Der Name gehoert einer anderen Identitaet.
    Foreign,
    /// Der Bestand ist voll.
    Full {
        /// Die Obergrenze.
        limit: usize,
    },
}

/// Der Bestand registrierter Regionen.
///
/// `BTreeMap` und nicht `HashMap`: der Bestand wird ausgegeben, und eine
/// stabile Reihenfolge macht Diagnose und Tests reproduzierbar.
#[derive(Debug)]
pub struct ShmRegistry {
    regions: Mutex<BTreeMap<String, Region>>,
    limit: usize,
}

impl Default for ShmRegistry {
    fn default() -> Self {
        Self::with_limit(DEFAULT_MAX_REGIONS)
    }
}

impl ShmRegistry {
    /// Ein leerer Bestand mit der Voreinstellung als Obergrenze.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ein leerer Bestand mit einer eigenen Obergrenze.
    #[must_use]
    pub fn with_limit(limit: usize) -> Self {
        Self {
            regions: Mutex::new(BTreeMap::new()),
            limit,
        }
    }

    /// Ob diese Identitaet diesen Namen registrieren darf.
    ///
    /// Vor der Weitergabe an das Backend: eine Ablehnung soll das Backend
    /// nicht erreichen. Eine erneute Registrierung desselben Namens durch
    /// denselben Besitzer ist erlaubt und zaehlt nicht doppelt.
    ///
    /// # Errors
    ///
    /// [`Refusal::Foreign`], wenn der Name einer anderen Identitaet gehoert;
    /// [`Refusal::Full`], wenn der Bestand voll ist.
    pub fn admit(&self, name: &str, owner: Identity) -> Result<(), Refusal> {
        let Ok(regions) = self.regions.lock() else {
            return Err(Refusal::Full { limit: self.limit });
        };
        match regions.get(name) {
            Some(existing) if existing.owner != owner => Err(Refusal::Foreign),
            None if regions.len() >= self.limit => Err(Refusal::Full { limit: self.limit }),
            // Die eigene Region erneut, oder Platz fuer eine neue.
            Some(_) | None => Ok(()),
        }
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

    /// Wem ein Name gehoert, falls er ueber diesen Governor registriert wurde.
    #[must_use]
    pub fn owner_of(&self, name: &str) -> Option<Identity> {
        self.regions
            .lock()
            .ok()
            .and_then(|regions| regions.get(name).map(|r| r.owner))
    }

    /// Vermerkt eine Abmeldung.
    ///
    /// Ein leerer Name bedeutet im Protokoll „alle Regionen". Wer das darf,
    /// entscheidet der Dienst — hier wird nur gebucht.
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

/// Prueft einen Shared-Memory-Schluessel gegen den Praefix des Betreibers.
///
/// Ein POSIX-Schluessel ist `/name` ohne weiteren Schraegstrich. Was nicht mit
/// dem Praefix beginnt, gehoert nicht zu den Clients dieses Governors — es
/// kann das Segment eines beliebigen anderen Prozesses auf dem Host sein.
///
/// # Errors
///
/// Ein Text, der den Grund nennt.
pub fn check_key(prefix: &str, key: &str) -> Result<(), String> {
    if key.len() > MAX_KEY_LEN {
        return Err(format!("Schluessel laenger als {MAX_KEY_LEN} Zeichen"));
    }
    if key.contains('\0') {
        return Err("Schluessel mit NUL-Zeichen".to_owned());
    }
    if !key.starts_with(prefix) {
        return Err(format!(
            "Schluessel {key:?} traegt nicht den Praefix {prefix:?} \
             (backend.security.shm_key_prefix); fremde Segmente sind ueber den \
             Governor nicht registrierbar"
        ));
    }
    if key.get(1..).is_some_and(|rest| rest.contains('/')) {
        return Err(format!(
            "Schluessel {key:?} enthaelt einen weiteren Schraegstrich"
        ));
    }
    Ok(())
}

/// Prueft Offset und Groesse einer Region auf Darstellbarkeit.
///
/// `offset + byte_size` muss in `i64` passen: das Backend bildet damit einen
/// POSIX-Offset, und ein Ueberlauf dort ist kein Fehler dieses Prozesses mehr,
/// sondern einer im fremden.
///
/// # Errors
///
/// Ein Text, der den Grund nennt.
pub fn check_extent(offset: u64, byte_size: u64) -> Result<(), String> {
    if byte_size == 0 {
        return Err("byte_size ist null".to_owned());
    }
    let end = offset
        .checked_add(byte_size)
        .ok_or_else(|| "offset + byte_size laeuft ueber".to_owned())?;
    if end > i64::MAX as u64 {
        return Err("offset + byte_size ist als Dateioffset nicht darstellbar".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Identity = Identity(7);
    const B: Identity = Identity(9);

    fn region(bytes: u64, owner: Identity) -> Region {
        Region {
            byte_size: bytes,
            offset: 0,
            cuda: false,
            owner,
        }
    }

    #[test]
    fn registrations_are_tracked_and_summed() {
        let registry = ShmRegistry::new();
        assert!(registry.is_empty());

        registry.record("frames".to_owned(), region(6_220_800, A));
        registry.record("depth".to_owned(), region(2_073_600, A));
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
        registry.record("a".to_owned(), region(1, A));
        registry.record("b".to_owned(), region(2, B));
        registry.forget("");
        assert!(registry.is_empty());
    }

    /// Eine erneute Registrierung desselben Namens ersetzt den Eintrag, statt
    /// ihn zu verdoppeln — sonst waere die Bytemenge nach einem Reconnect falsch.
    #[test]
    fn re_registering_replaces_instead_of_accumulating() {
        let registry = ShmRegistry::new();
        registry.record("frames".to_owned(), region(100, A));
        registry.record("frames".to_owned(), region(200, A));
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_bytes(), 200);
    }

    /// H2: ein Name gehoert, wer ihn registriert hat.
    #[test]
    fn a_name_belongs_to_its_registrant() {
        let registry = ShmRegistry::new();
        registry.record("frames".to_owned(), region(100, A));
        assert_eq!(registry.admit("frames", A), Ok(()));
        assert_eq!(registry.admit("frames", B), Err(Refusal::Foreign));
        assert_eq!(registry.owner_of("frames"), Some(A));
    }

    /// N7: der Bestand ist begrenzt; ein bekannter Name zaehlt nicht doppelt.
    #[test]
    fn the_registry_is_bounded() {
        let registry = ShmRegistry::with_limit(1);
        registry.record("a".to_owned(), region(1, A));
        assert_eq!(registry.admit("b", A), Err(Refusal::Full { limit: 1 }));
        assert_eq!(registry.admit("a", A), Ok(()));
    }

    #[test]
    fn keys_must_carry_the_prefix_and_nothing_else() {
        assert!(check_key("/vig_", "/vig_frames").is_ok());
        assert!(check_key("/vig_", "/other_frames").is_err());
        assert!(check_key("/vig_", "/vig_a/b").is_err());
        assert!(check_key("/vig_", "/vig_\0x").is_err());
        assert!(check_key("/vig_", &format!("/vig_{}", "x".repeat(300))).is_err());
    }

    #[test]
    fn extents_must_not_overflow() {
        assert!(check_extent(0, 4096).is_ok());
        assert!(check_extent(u64::MAX, 1).is_err());
        assert!(check_extent(i64::MAX as u64, 1).is_err());
        assert!(check_extent(0, 0).is_err());
    }
}
