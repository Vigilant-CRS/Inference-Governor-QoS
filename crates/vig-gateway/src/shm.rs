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
//! ## Besitz (Security-Review H2, M4, N7; Review 15.09.2026 R02)
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
//! * **Unter `trust: strict` gehoert auch das Segment dahinter dem, der es
//!   zuerst registriert hat.** Der Regionsname ist nur ein frei gewaehlter
//!   Alias; wer denselben Schluessel unter einem anderen Namen registrieren
//!   will, wird abgewiesen, solange eine Region eines anderen Aufrufers darauf
//!   zeigt — auch bei einem Bereich, der sich mit keinem registrierten
//!   ueberschneidet: der Rest des Objekts ist ebenso fremder Speicher.
//! * **Unter `trust: strict` darf eine Inferenz nur Regionen nennen, die ihr
//!   Aufrufer ueber diesen Governor registriert hat.** Unter `trust: open`
//!   bleibt es beim bisherigen Verhalten — dort gilt ohnehin „ein Betreiber,
//!   ein abgeschlossenes Netz", und Clients registrieren oft direkt beim
//!   Backend.
//! * **Der Bestand ist begrenzt** (`backend.security.max_shm_regions`).
//!
//! ## Reservierung statt Pruefen-dann-Buchen (Review 15.09.2026 R07)
//!
//! Zwischen Pruefung und Bestaetigung liegt ein Backendaufruf. Waeren beide
//! getrennte Sperrabschnitte, saehen gleichzeitige Registrierungen denselben
//! freien Platz, denselben freien Schluessel. Deshalb reserviert
//! [`ShmRegistry::reserve_registration`] Name, Platz und Segment in **einem**
//! Sperrabschnitt; die [`Reservation`] wird nach der Bestaetigung des Backends
//! gebucht und sonst beim Fallenlassen zurueckgegeben. Fuer einen Namen laeuft
//! hoechstens ein Backendaufruf zugleich, und „alle abmelden" laeuft allein:
//! sonst haenge der gebuchte Stand von der Reihenfolge ab, in der das Backend
//! zwei Aufrufe zufaellig bearbeitet hat.
//!
//! ## Was hier bewusst nicht passiert
//!
//! Regionen werden **nicht** automatisch beim Verbindungsabbruch eines Clients
//! freigegeben. gRPC kennt keine Sitzung, und eine Zuordnung ueber die
//! Gegenstelle waere bei mehreren Clients hinter einem Proxy falsch — im
//! Zweifel wuerde Vigilant eine Region abmelden, die ein anderer noch benutzt.
//! Das waere schlimmer als ein Leck. Stattdessen wird der Bestand gefuehrt,
//! begrenzt und sichtbar gemacht.
//!
//! ## Was offen bleibt
//!
//! Der Governor sieht Schluessel, nicht, wer ein Segment angelegt hat.
//! **Registriert ein Angreifer einen fremden Schluessel zuerst** — nachdem
//! der Besitzer das Segment angelegt, aber bevor er es registriert hat —,
//! gehoert das Segment hier dem Angreifer. Der Besitzer wird dann abgewiesen;
//! die Uebernahme ist also nicht still, aber sie ist nicht verhindert. Das
//! loest erst eine vom Betreiber vergebene Zuordnung von Schluesseln zu
//! Identitaeten (etwa ein Praefix je Token). Ebenso wenig erkennt der Vergleich
//! ueber Namen einen harten Link in `/dev/shm`; wer den anlegen kann, liest das
//! Segment allerdings auch ohne den Governor.

use crate::auth::Identity;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Die Voreinstellung fuer die Hoechstzahl registrierter Regionen.
pub const DEFAULT_MAX_REGIONS: usize = 256;

/// Die laengste zulaessige Schluessellaenge (POSIX `NAME_MAX`).
pub const MAX_KEY_LEN: usize = 255;

/// Eine registrierte Region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Der Schluessel des zugrunde liegenden Objekts, wie registriert.
    ///
    /// Er bezeichnet das physische Segment; der Regionsname ist nur ein
    /// Alias darauf.
    pub key: String,
    /// Groesse der Region in Bytes.
    pub byte_size: u64,
    /// Offset innerhalb des zugrunde liegenden Objekts.
    pub offset: u64,
    /// Wahr, wenn es sich um CUDA- statt System-Shared-Memory handelt.
    pub cuda: bool,
    /// Wer sie ueber diesen Governor registriert hat.
    pub owner: Identity,
}

impl Region {
    /// Ob beide Regionen im selben Objekt liegen, gleich in welchem Bereich.
    #[must_use]
    pub fn same_segment(&self, other: &Self) -> bool {
        self.cuda == other.cuda && segment(&self.key) == segment(&other.key)
    }
}

/// Der Objektname hinter einem Schluessel.
///
/// glibc entfernt in `shm_open` fuehrende Schraegstriche: `/vig_x` und
/// `vig_x` oeffnen dasselbe Objekt. Mit einem Praefix ohne `/` waeren beide
/// Schreibweisen registrierbar.
fn segment(key: &str) -> &str {
    key.trim_start_matches('/')
}

/// Warum eine Registrierung oder Abmeldung nicht angenommen wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Der Name gehoert einer anderen Identitaet.
    Foreign,
    /// Das Segment hinter dem Schluessel hat eine andere Identitaet
    /// registriert (nur mit exklusiven Segmenten, also `trust: strict`).
    ForeignSegment,
    /// Der Name wurde nie ueber diesen Governor registriert, und das wird
    /// verlangt.
    Unknown,
    /// Fuer diesen Namen — oder fuer alle — laeuft gerade ein Backendaufruf.
    Busy,
    /// Der Bestand ist voll.
    Full {
        /// Die Obergrenze.
        limit: usize,
    },
}

/// Ein Backendaufruf, dessen Ergebnis noch aussteht.
#[derive(Debug)]
enum Pending {
    /// Eine Registrierung mit der Region, die sie buchen wird.
    Register(Region),
    /// Eine Abmeldung.
    Unregister,
}

/// Der gebuchte und der reservierte Stand, unter einer Sperre.
#[derive(Debug, Default)]
struct Book {
    /// Vom Backend bestaetigte Regionen.
    regions: BTreeMap<String, Region>,
    /// Laufende Backendaufrufe, hoechstens einer je Name.
    pending: BTreeMap<String, Pending>,
    /// Wahr, solange „alle abmelden" beim Backend laeuft.
    clearing: bool,
}

impl Book {
    /// Belegte Plaetze: bestaetigte Regionen und reservierte neue Namen.
    fn occupied(&self) -> usize {
        let reserved = self
            .pending
            .iter()
            .filter(|(name, pending)| {
                matches!(pending, Pending::Register(_)) && !self.regions.contains_key(*name)
            })
            .count();
        self.regions.len().saturating_add(reserved)
    }

    /// Bestaetigte und reservierte Registrierungen.
    fn registrations(&self) -> impl Iterator<Item = &Region> {
        self.regions
            .values()
            .chain(self.pending.values().filter_map(|pending| match pending {
                Pending::Register(region) => Some(region),
                Pending::Unregister => None,
            }))
    }
}

/// Die Sperre auf den Bestand.
///
/// Unter der Sperre laeuft nichts, was paniken kann, und das Releaseprofil
/// bricht bei einer Panik ohnehin ab. Eine vergiftete Sperre haelt deshalb
/// einen gueltigen Stand — und eine Reservierung muss auch dann
/// zurueckgegeben werden koennen.
fn lock(book: &Mutex<Book>) -> MutexGuard<'_, Book> {
    book.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Der Bestand registrierter Regionen.
///
/// `BTreeMap` und nicht `HashMap`: der Bestand wird ausgegeben, und eine
/// stabile Reihenfolge macht Diagnose und Tests reproduzierbar.
#[derive(Debug)]
pub struct ShmRegistry {
    book: Arc<Mutex<Book>>,
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
            book: Arc::new(Mutex::new(Book::default())),
            limit,
        }
    }

    /// Reserviert eine Registrierung, bevor sie das Backend erreicht.
    ///
    /// Eine Ablehnung soll das Backend nicht erreichen. Eine erneute
    /// Registrierung desselben Namens durch denselben Besitzer ist erlaubt
    /// und zaehlt nicht doppelt. Mit `exclusive_segments` (`trust: strict`)
    /// wird auch ein Schluessel abgewiesen, auf den schon eine Region einer
    /// anderen Identitaet zeigt — bestaetigt oder selbst noch reserviert.
    ///
    /// # Errors
    ///
    /// [`Refusal::Foreign`], wenn der Name einer anderen Identitaet gehoert;
    /// [`Refusal::Busy`], wenn fuer den Namen schon ein Aufruf laeuft;
    /// [`Refusal::ForeignSegment`], wenn das Segment fremd ist;
    /// [`Refusal::Full`], wenn der Bestand voll ist.
    pub fn reserve_registration(
        &self,
        name: &str,
        region: Region,
        exclusive_segments: bool,
    ) -> Result<Reservation, Refusal> {
        let mut book = lock(&self.book);
        let holder =
            book.regions
                .get(name)
                .map(|r| r.owner)
                .or_else(|| match book.pending.get(name) {
                    Some(Pending::Register(r)) => Some(r.owner),
                    Some(Pending::Unregister) | None => None,
                });
        if holder.is_some_and(|owner| owner != region.owner) {
            return Err(Refusal::Foreign);
        }
        if book.clearing || book.pending.contains_key(name) {
            return Err(Refusal::Busy);
        }
        if exclusive_segments
            && book
                .registrations()
                .any(|r| r.owner != region.owner && r.same_segment(&region))
        {
            return Err(Refusal::ForeignSegment);
        }
        if !book.regions.contains_key(name) && book.occupied() >= self.limit {
            return Err(Refusal::Full { limit: self.limit });
        }
        book.pending
            .insert(name.to_owned(), Pending::Register(region));
        Ok(Reservation::new(&self.book, Scope::Name(name.to_owned())))
    }

    /// Reserviert eine Abmeldung, bevor sie das Backend erreicht.
    ///
    /// `claimant` ist der Aufrufer, dem der Name gehoeren muss; `None` fuer
    /// ein Administrationstoken. Ob ein leerer Name („alle") ueberhaupt
    /// erlaubt ist, entscheidet der Dienst — hier wird nur reserviert. Mit
    /// `only_known` (`trust: strict`) muss der Name ueber diesen Governor
    /// registriert sein. Bis zur Bestaetigung bleibt die Region gebucht: sie
    /// zaehlt weiter gegen die Obergrenze, und ihr Segment bleibt belegt.
    ///
    /// # Errors
    ///
    /// [`Refusal::Foreign`], [`Refusal::Unknown`] oder [`Refusal::Busy`].
    pub fn reserve_unregistration(
        &self,
        name: &str,
        claimant: Option<Identity>,
        only_known: bool,
    ) -> Result<Reservation, Refusal> {
        let mut book = lock(&self.book);
        let in_flight = book.pending.contains_key(name);
        if let Some(claimant) = claimant
            && !name.is_empty()
        {
            match book.regions.get(name).map(|r| r.owner) {
                Some(owner) if owner != claimant => return Err(Refusal::Foreign),
                None if only_known && !in_flight => return Err(Refusal::Unknown),
                Some(_) | None => {}
            }
        }
        if book.clearing || in_flight {
            return Err(Refusal::Busy);
        }
        if name.is_empty() {
            if !book.pending.is_empty() {
                return Err(Refusal::Busy);
            }
            book.clearing = true;
            return Ok(Reservation::new(&self.book, Scope::All));
        }
        book.pending.insert(name.to_owned(), Pending::Unregister);
        Ok(Reservation::new(&self.book, Scope::Name(name.to_owned())))
    }

    /// Wem ein Name gehoert, falls ihn das Backend ueber diesen Governor
    /// bestaetigt hat.
    #[must_use]
    pub fn owner_of(&self, name: &str) -> Option<Identity> {
        lock(&self.book).regions.get(name).map(|r| r.owner)
    }

    /// Die Anzahl bestaetigter Regionen.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.book).regions.len()
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
        lock(&self.book)
            .regions
            .values()
            .map(|r| r.byte_size)
            .fold(0_u64, u64::saturating_add)
    }

    /// Die Namen aller registrierten Regionen.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        lock(&self.book).regions.keys().cloned().collect()
    }
}

/// Was eine Reservierung belegt.
#[derive(Debug)]
enum Scope {
    /// Ein einzelner Name.
    Name(String),
    /// Der ganze Bestand („alle abmelden").
    All,
}

/// Ein reservierter Registrierungs- oder Abmeldeaufruf.
///
/// [`Reservation::confirm`] bucht ihn, nachdem das Backend bestaetigt hat.
/// Jeder andere Weg — Backendfehler, abgebrochene Aufgabe — gibt die
/// Reservierung beim Fallenlassen zurueck, und der gebuchte Stand bleibt, wie
/// er vorher war.
#[derive(Debug)]
#[must_use = "eine fallengelassene Reservierung ist sofort zurueckgegeben"]
pub struct Reservation {
    book: Arc<Mutex<Book>>,
    scope: Scope,
    settled: bool,
}

impl Reservation {
    fn new(book: &Arc<Mutex<Book>>, scope: Scope) -> Self {
        Self {
            book: Arc::clone(book),
            scope,
            settled: false,
        }
    }

    /// Das Backend hat bestaetigt: die Reservierung wird gebucht.
    pub fn confirm(mut self) {
        {
            let mut book = lock(&self.book);
            match &self.scope {
                Scope::All => {
                    book.regions.clear();
                    book.clearing = false;
                }
                Scope::Name(name) => match book.pending.remove(name) {
                    Some(Pending::Register(region)) => {
                        book.regions.insert(name.clone(), region);
                    }
                    Some(Pending::Unregister) => {
                        book.regions.remove(name);
                    }
                    None => {}
                },
            }
        }
        self.settled = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let mut book = lock(&self.book);
        match &self.scope {
            Scope::All => book.clearing = false,
            Scope::Name(name) => {
                book.pending.remove(name);
            }
        }
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
    #![allow(clippy::unwrap_used)]

    use super::*;

    const A: Identity = Identity(7);
    const B: Identity = Identity(9);

    fn region(key: &str, bytes: u64, owner: Identity) -> Region {
        Region {
            key: key.to_owned(),
            byte_size: bytes,
            offset: 0,
            cuda: false,
            owner,
        }
    }

    /// Reservieren und bestaetigen, wie nach einer Antwort des Backends.
    fn register(registry: &ShmRegistry, name: &str, region: Region) -> Result<(), Refusal> {
        registry
            .reserve_registration(name, region, true)
            .map(Reservation::confirm)
    }

    fn unregister(registry: &ShmRegistry, name: &str) -> Result<(), Refusal> {
        registry
            .reserve_unregistration(name, None, false)
            .map(Reservation::confirm)
    }

    #[test]
    fn registrations_are_tracked_and_summed() {
        let registry = ShmRegistry::new();
        assert!(registry.is_empty());

        register(&registry, "frames", region("/vig_f", 6_220_800, A)).unwrap();
        register(&registry, "depth", region("/vig_d", 2_073_600, A)).unwrap();
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.total_bytes(), 8_294_400);
        assert_eq!(
            registry.names(),
            vec!["depth".to_owned(), "frames".to_owned()]
        );

        unregister(&registry, "frames").unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_bytes(), 2_073_600);
    }

    /// Im Protokoll bedeutet ein leerer Name „alle Regionen".
    #[test]
    fn an_empty_name_unregisters_everything() {
        let registry = ShmRegistry::new();
        register(&registry, "a", region("/vig_a", 1, A)).unwrap();
        register(&registry, "b", region("/vig_b", 2, B)).unwrap();
        unregister(&registry, "").unwrap();
        assert!(registry.is_empty());
    }

    /// Eine erneute Registrierung desselben Namens ersetzt den Eintrag, statt
    /// ihn zu verdoppeln — sonst waere die Bytemenge nach einem Reconnect falsch.
    #[test]
    fn re_registering_replaces_instead_of_accumulating() {
        let registry = ShmRegistry::new();
        register(&registry, "frames", region("/vig_f", 100, A)).unwrap();
        register(&registry, "frames", region("/vig_f", 200, A)).unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_bytes(), 200);
    }

    /// H2: ein Name gehoert, wer ihn registriert hat.
    #[test]
    fn a_name_belongs_to_its_registrant() {
        let registry = ShmRegistry::new();
        register(&registry, "frames", region("/vig_f", 100, A)).unwrap();
        assert!(
            registry
                .reserve_registration("frames", region("/vig_f", 100, A), true)
                .is_ok()
        );
        assert_eq!(
            registry
                .reserve_registration("frames", region("/vig_other", 100, B), false)
                .err(),
            Some(Refusal::Foreign)
        );
        assert_eq!(registry.owner_of("frames"), Some(A));
        assert_eq!(
            registry
                .reserve_unregistration("frames", Some(B), false)
                .err(),
            Some(Refusal::Foreign)
        );
        assert_eq!(
            registry
                .reserve_unregistration("nobody", Some(A), true)
                .err(),
            Some(Refusal::Unknown)
        );
    }

    /// R02: mit exklusiven Segmenten gehoert ein Schluessel, wer ihn zuerst
    /// registriert hat — gleich unter welchem Namen, in welchem Bereich und
    /// mit wie vielen fuehrenden Schraegstrichen. Derselbe Besitzer darf ihn
    /// mehrfach nennen; ohne exklusive Segmente (`trust: open`) bleibt alles
    /// beim Alten.
    #[test]
    fn a_segment_belongs_to_its_registrant_when_exclusive() {
        let registry = ShmRegistry::new();
        register(&registry, "alpha", region("/vig_private", 1024, A)).unwrap();

        let mut elsewhere = region("/vig_private", 1024, B);
        elsewhere.offset = 1 << 20;
        for alias in [
            region("/vig_private", 1024, B),
            elsewhere,
            region("vig_private", 1024, B),
        ] {
            assert_eq!(
                registry.reserve_registration("beta", alias, true).err(),
                Some(Refusal::ForeignSegment)
            );
        }
        register(&registry, "alpha_2", region("/vig_private", 1024, A)).unwrap();
        assert!(
            registry
                .reserve_registration("beta", region("/vig_private", 1024, B), false)
                .is_ok()
        );

        // Auch eine erst reservierte Registrierung belegt ihr Segment.
        let held = registry
            .reserve_registration("gamma", region("/vig_gamma", 1, B), true)
            .unwrap();
        assert_eq!(
            registry
                .reserve_registration("delta", region("/vig_gamma", 1, A), true)
                .err(),
            Some(Refusal::ForeignSegment)
        );
        drop(held);
        assert!(
            registry
                .reserve_registration("delta", region("/vig_gamma", 1, A), true)
                .is_ok()
        );
    }

    /// N7: der Bestand ist begrenzt; ein bekannter Name zaehlt nicht doppelt.
    #[test]
    fn the_registry_is_bounded() {
        let registry = ShmRegistry::with_limit(1);
        register(&registry, "a", region("/vig_a", 1, A)).unwrap();
        assert_eq!(
            registry
                .reserve_registration("b", region("/vig_b", 1, A), true)
                .err(),
            Some(Refusal::Full { limit: 1 })
        );
        assert!(
            registry
                .reserve_registration("a", region("/vig_a", 1, A), true)
                .is_ok()
        );
    }

    /// R07: eine Reservierung belegt ihren Platz, bevor das Backend antwortet,
    /// und gibt ihn zurueck, wenn sie nicht bestaetigt wird. Sichtbar — fuer
    /// Referenzen in Inferenzen und fuer den Bestand — wird sie erst mit der
    /// Bestaetigung.
    #[test]
    fn a_reservation_holds_its_place_until_it_is_settled() {
        let registry = ShmRegistry::with_limit(1);
        let first = registry
            .reserve_registration("a", region("/vig_a", 1, A), true)
            .unwrap();
        assert_eq!(registry.owner_of("a"), None);
        assert!(registry.is_empty());
        assert_eq!(
            registry
                .reserve_registration("b", region("/vig_b", 1, B), true)
                .err(),
            Some(Refusal::Full { limit: 1 })
        );

        drop(first);
        let second = registry
            .reserve_registration("b", region("/vig_b", 1, B), true)
            .unwrap();
        second.confirm();
        assert_eq!(registry.owner_of("b"), Some(B));
        assert_eq!(registry.len(), 1);
    }

    /// R07: fuer einen Namen laeuft hoechstens ein Backendaufruf, und „alle
    /// abmelden" laeuft allein. Sonst entschiede die zufaellige Reihenfolge
    /// im Backend, ob der gebuchte Stand noch stimmt.
    #[test]
    fn calls_for_one_name_do_not_interleave() {
        let registry = ShmRegistry::new();
        let registering = registry
            .reserve_registration("a", region("/vig_a", 1, A), true)
            .unwrap();
        assert_eq!(
            registry.reserve_unregistration("a", Some(A), true).err(),
            Some(Refusal::Busy)
        );
        assert_eq!(
            registry
                .reserve_registration("a", region("/vig_a", 1, A), true)
                .err(),
            Some(Refusal::Busy)
        );
        assert_eq!(
            registry.reserve_unregistration("", None, false).err(),
            Some(Refusal::Busy)
        );
        registering.confirm();

        let unregistering = registry.reserve_unregistration("a", Some(A), true).unwrap();
        assert_eq!(
            registry
                .reserve_registration("a", region("/vig_a", 1, A), true)
                .err(),
            Some(Refusal::Busy)
        );
        // Bis zur Bestaetigung bleibt die Region gebucht.
        assert_eq!(registry.owner_of("a"), Some(A));
        drop(unregistering);
        assert_eq!(registry.owner_of("a"), Some(A));

        let clearing = registry.reserve_unregistration("", None, false).unwrap();
        assert_eq!(
            registry
                .reserve_registration("b", region("/vig_b", 1, B), true)
                .err(),
            Some(Refusal::Busy)
        );
        clearing.confirm();
        assert!(registry.is_empty());
        assert!(
            registry
                .reserve_registration("b", region("/vig_b", 1, B), true)
                .is_ok()
        );
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
