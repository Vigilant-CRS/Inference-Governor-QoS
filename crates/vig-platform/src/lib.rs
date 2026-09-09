//! Lesende Hardwarebeobachtung (NV-04).
//!
//! ## Was dieses Crate tut — und was ausdruecklich nicht
//!
//! Es **liest**. Geraetename, Treiber, Compute Capability, Speicher, Takt,
//! Temperatur, Leistungsaufnahme und die Gruende, aus denen die Karte gerade
//! gedrosselt wird. Es **stellt nichts**: keine Taktvorgabe, kein
//! Persistence-Mode, kein Power-Limit, kein `nvpmodel`. Ein Governor, der die
//! Hardware verstellt, braucht Rechte, die ein Governor nicht haben sollte —
//! und macht jede Messung des Betreibers zu einer Messung des Governors.
//!
//! Es braucht auch kein Root. Alles hier laeuft mit den Rechten eines
//! gewoehnlichen Benutzers; wo das nicht reicht, ist der Wert
//! [`Observation::Unavailable`] und nicht geraten.
//!
//! ## Drei Zustaende, die nicht dasselbe sind
//!
//! Der haeufigste Fehler in Telemetrie ist, „geht hier nicht", „weiss ich
//! gerade nicht" und „ist alt" in einen Nullwert zu falten. Dieses Crate
//! trennt sie:
//!
//! * [`Observation::Observed`] — ein Wert mit Quelle und Messzeitpunkt.
//! * [`Observation::Unsupported`] — diese Plattform kennt die Groesse nicht.
//!   Ein Laptop-Ampere meldet kein `power.limit`; das ist kein Ausfall.
//! * [`Observation::Unavailable`] — die Groesse gaebe es, sie war aber nicht
//!   zu holen. Collector weg, Rechte fehlen, Antwort unlesbar.
//!
//! Und quer dazu die [`Freshness`]: ein beobachteter Wert von vor zwei
//! Minuten ist etwas anderes als einer von vor zwei Sekunden. Wie alt zu alt
//! ist, entscheidet der Aufrufer — hier wird es nur ausgerechnet und
//! ausgewiesen.
//!
//! ## Warum ein Snapshot und kein Dauerstrom
//!
//! Ein Zustand, der zwischen zwei Messungen wechselt, ist nicht beobachtet,
//! sondern erschlossen. Deshalb gibt es hier vollstaendige Momentaufnahmen mit
//! einem Zeitstempel, die sich aufzeichnen und wieder abspielen lassen
//! ([`snapshot::HardwareSnapshot`]). Erst der Vergleich zweier Aufnahmen
//! ergibt eine Aenderung — und die traegt dann beide Zeitpunkte.

pub mod collector;
pub mod gpu;
pub mod measure;
pub mod snapshot;

pub use collector::{Collector, CollectorHealth, Fallback, NvidiaSmi, Recorded};
pub use gpu::{GpuState, ThrottleReason, parse_query_line};
pub use measure::{
    CellId, CellRun, CellSummary, ClockVerdict, DiscardReason, ReleaseOutcome, ReleaseSchedule,
    RunManifest, verify_clock,
};
pub use snapshot::{Change, HardwareSnapshot, diff};

use serde::{Deserialize, Serialize};

/// Woher ein Wert stammt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Aus `nvidia-smi`.
    NvidiaSmi,
    /// Aus dem sysfs des Kernels.
    Sysfs,
    /// Aus `/proc`.
    Proc,
    /// Aus einer aufgezeichneten Datei, also ein Replay.
    Recorded,
}

/// Ein gemessener Wert samt Herkunft und Zeitpunkt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Sample<T> {
    /// Der Wert.
    pub value: T,
    /// Woher er stammt.
    pub source: Source,
    /// Wann er gemessen wurde, als Unix-Zeit in Millisekunden.
    ///
    /// Bewusst eine Zahl und kein `SystemTime`: ein Snapshot soll sich
    /// schreiben, wieder einlesen und mit einem anderen vergleichen lassen,
    /// ohne dass die Serialisierung Genauigkeit erfindet oder verliert.
    pub observed_at_ms: u64,
}

/// Was ueber eine Groesse bekannt ist.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Observation<T> {
    /// Gemessen.
    Observed(Sample<T>),
    /// Diese Plattform kennt die Groesse nicht.
    ///
    /// Kein Ausfall: ein Laptop-Ampere meldet kein `power.limit`, und das
    /// bleibt auch nach dem naechsten Versuch so.
    Unsupported {
        /// Warum, in einem Satz fuer den Betreiber.
        reason: String,
    },
    /// Die Groesse gaebe es, sie war aber nicht zu holen.
    Unavailable {
        /// Was schiefging.
        reason: String,
    },
}

/// Wie frisch ein Wert ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Innerhalb der zugestandenen Frist gemessen.
    Fresh {
        /// Wie alt, in Millisekunden.
        age_ms: u64,
    },
    /// Aelter als die zugestandene Frist.
    Stale {
        /// Wie alt, in Millisekunden.
        age_ms: u64,
    },
    /// Kein Wert, also auch kein Alter.
    NoValue,
}

impl<T> Observation<T> {
    /// Der Wert, falls einer beobachtet wurde.
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Observed(sample) => Some(&sample.value),
            Self::Unsupported { .. } | Self::Unavailable { .. } => None,
        }
    }

    /// Ob ueberhaupt etwas gemessen wurde.
    pub const fn is_observed(&self) -> bool {
        matches!(self, Self::Observed(_))
    }

    /// Wie frisch der Wert zum Zeitpunkt `now_ms` ist.
    ///
    /// Eine Messung aus der Zukunft — Uhrensprung, verstellte Systemzeit —
    /// gilt als Alter null und nicht als negatives Alter: der Wert ist dann
    /// nicht besonders gut, sondern nur nicht datierbar.
    pub fn freshness(&self, now_ms: u64, max_age_ms: u64) -> Freshness {
        let Self::Observed(sample) = self else {
            return Freshness::NoValue;
        };
        let age_ms = now_ms.saturating_sub(sample.observed_at_ms);
        if age_ms > max_age_ms {
            Freshness::Stale { age_ms }
        } else {
            Freshness::Fresh { age_ms }
        }
    }
}

/// Die Unix-Zeit in Millisekunden, falls die Systemuhr sie hergibt.
///
/// # Errors
///
/// Gibt `None`, wenn die Systemuhr vor der Epoche steht. Ein erfundener
/// Zeitstempel waere schlimmer als keiner: er wuerde eine Messung datieren,
/// die nicht datiert ist.
#[must_use]
pub fn now_ms() -> Option<u64> {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis(),
    )
    .ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn observed(at: u64) -> Observation<u32> {
        Observation::Observed(Sample {
            value: 42,
            source: Source::NvidiaSmi,
            observed_at_ms: at,
        })
    }

    #[test]
    fn unsupported_and_unavailable_are_different_states() {
        let unsupported: Observation<u32> = Observation::Unsupported {
            reason: "power.limit auf dieser Karte nicht gemeldet".to_owned(),
        };
        let unavailable: Observation<u32> = Observation::Unavailable {
            reason: "nvidia-smi nicht ausfuehrbar".to_owned(),
        };
        assert_ne!(unsupported, unavailable);
        assert_eq!(unsupported.value(), None);
        assert_eq!(unavailable.value(), None);
        assert!(!unsupported.is_observed());
    }

    #[test]
    fn freshness_separates_old_from_absent() {
        assert_eq!(
            observed(1_000).freshness(1_500, 1_000),
            Freshness::Fresh { age_ms: 500 }
        );
        assert_eq!(
            observed(1_000).freshness(3_000, 1_000),
            Freshness::Stale { age_ms: 2_000 }
        );
        let missing: Observation<u32> = Observation::Unavailable {
            reason: "weg".to_owned(),
        };
        assert_eq!(missing.freshness(3_000, 1_000), Freshness::NoValue);
    }

    #[test]
    fn a_measurement_from_the_future_is_not_extra_fresh() {
        assert_eq!(
            observed(5_000).freshness(1_000, 100),
            Freshness::Fresh { age_ms: 0 },
            "ein Uhrensprung darf keinen negativen Alterswert erzeugen"
        );
    }

    #[test]
    fn exactly_at_the_limit_is_still_fresh() {
        assert_eq!(
            observed(1_000).freshness(2_000, 1_000),
            Freshness::Fresh { age_ms: 1_000 }
        );
        assert_eq!(
            observed(1_000).freshness(2_001, 1_000),
            Freshness::Stale { age_ms: 1_001 }
        );
    }
}
