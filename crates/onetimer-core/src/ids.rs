//! Skalare Bezeichner fuer den Hot Path.
//!
//! Alle Bezeichner sind `Copy`-Skalare. Namen (Modellnamen, Stream-Namen,
//! Variantennamen) existieren nur in der Konfiguration und werden am Ingress
//! einmal in Indizes aufgeloest. Damit enthaelt der Scheduling-Entscheidungspfad
//! keine Stringvergleiche und keine Allokation (Spec 8.1).

/// Obergrenze aktiver logischer Queues.
///
/// Spec 8.1 nennt `N <= 32` aktive logische Queues als MVP-Zielgroesse. Die
/// Grenze ist hier hart, damit alle Scheduler-Datenstrukturen statisch
/// dimensioniert werden koennen und kein unbeschraenktes Wachstum aus
/// Konfiguration entsteht.
pub const MAX_MODELS: usize = 32;

/// Obergrenze der Backend-Execution-Slots.
///
/// Siehe ADR-0004. Die Look-ahead-Simulation ist linear in dieser Groesse;
/// die Grenze haelt die Entscheidungslatenz unter dem 100-us-p99-Ziel.
pub const MAX_SLOTS: usize = 8;

/// Obergrenze der Varianten je logischem Modell.
pub const MAX_VARIANTS: usize = 8;

/// Eindeutige, monoton vergebene Kennung eines Requests innerhalb eines Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

impl core::fmt::Display for RequestId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// Index eines logischen Modells (z. B. `detector`) in der Modelltabelle.
///
/// Logisch, nicht physisch: die physische Backendvariante waehlt der Variant
/// Resolver (Spec 12.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelIdx(pub u16);

impl ModelIdx {
    /// Der Index als `usize` fuer Tabellenzugriffe.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

impl core::fmt::Display for ModelIdx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "m{}", self.0)
    }
}

/// Index einer physischen Variante innerhalb ihres logischen Modells.
///
/// Index 0 ist per Konvention die Variante hoechster Qualitaet; der Resolver
/// haelt die Liste absteigend nach Qualitaet sortiert (Spec 12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VariantIdx(pub u16);

impl VariantIdx {
    /// Die Variante hoechster Qualitaet.
    pub const BEST: Self = Self(0);

    /// Der Index als `usize` fuer Tabellenzugriffe.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

impl core::fmt::Display for VariantIdx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// Index eines Backend-Execution-Slots (ADR-0004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotIdx(pub u16);

impl SlotIdx {
    /// Der Index als `usize` fuer Tabellenzugriffe.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

impl core::fmt::Display for SlotIdx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "s{}", self.0)
    }
}

/// Der Freshness-Scope, innerhalb dessen ein Request einen anderen ueberholt.
///
/// Bei `LATEST` ist der Scope das logische Modell; bei `LATEST_PER_KEY` das
/// Paar aus Modell und Key (Spec 11.1, 11.2). Der Key ist bereits am Ingress
/// zu einem Skalar aufgeloest — der Scheduler vergleicht nie Strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SupersessionKey(pub u64);

impl SupersessionKey {
    /// Der Key, den ein Request ohne eigenen Schluessel erhaelt.
    ///
    /// Alle solchen Requests eines Modells liegen damit im selben Scope, was
    /// fuer `LATEST` genau das gewuenschte Verhalten ist.
    pub const DEFAULT: Self = Self(0);
}

impl core::fmt::Display for SupersessionKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "k{}", self.0)
    }
}

/// Undurchsichtiger Verweis auf die Tensor-Payload.
///
/// Der Scheduling-Core sieht die Payload nie. Er transportiert nur diesen
/// Handle; ob dahinter ein gRPC-Puffer oder eine Shared-Memory-Referenz steht,
/// entscheidet das Gateway (ADR-0003, Spec 9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PayloadRef(pub u64);
