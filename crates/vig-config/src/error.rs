//! Konfigurationsfehler mit Fundstelle.

use vig_core::model::ContractError;
use vig_core::queue::QueueConfigError;
use vig_core::slots::SlotError;

/// Ein Fehler samt der Stelle, an der er auftrat.
///
/// Die Fundstelle ist ein Pfad wie `models.detector.queue.capacity`. Ohne sie
/// muesste der Nutzer in einer laengeren YAML-Datei raten, welches Modell
/// gemeint ist — und `vig doctor` soll genau das ersparen (Spec 23).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// Der Pfad in der Konfiguration.
    pub path: String,
    /// Der Fehler.
    pub error: ConfigError,
}

impl core::fmt::Display for Located {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.path, self.error)
    }
}

impl core::error::Error for Located {}

/// Warum eine Konfiguration abgelehnt wird.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// Die Schemaversion ist unbekannt.
    UnsupportedVersion {
        /// Die gefundene Version.
        found: u32,
        /// Die unterstuetzte Version.
        supported: u32,
    },
    /// Es ist kein Modell konfiguriert.
    NoModels,
    /// Mehr Modelle als der Kern verwalten kann.
    TooManyModels {
        /// Die gefundene Anzahl.
        found: usize,
        /// Das Maximum.
        maximum: usize,
    },
    /// Ein Pflichtwert fehlt.
    ///
    /// Nur fuer Werte, deren Default **still gefaehrlich** waere.
    Missing {
        /// Was fehlt und warum es nicht geraten werden kann.
        what: &'static str,
    },
    /// Ein Wert liegt ausserhalb des Zulaessigen.
    OutOfRange {
        /// Beschreibung des zulaessigen Bereichs.
        expected: &'static str,
    },
    /// Ein Aufzaehlungswert ist unbekannt.
    UnknownValue {
        /// Der gefundene Wert.
        found: String,
        /// Die zulaessigen Werte.
        allowed: &'static str,
    },
    /// Ein Modellname in `no_corun` existiert nicht.
    UnknownModelReference {
        /// Der referenzierte Name.
        name: String,
    },
    /// Der Kern hat den Modellvertrag abgelehnt.
    Contract(ContractError),
    /// Der Kern hat die Queue-Konfiguration abgelehnt.
    Queue(QueueConfigError),
    /// Der Kern hat die Slot-Konfiguration abgelehnt.
    Slots(SlotError),
    /// Die Datei liess sich nicht als YAML lesen.
    Syntax {
        /// Die Meldung des Parsers.
        message: String,
    },
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "Schemaversion {found} wird nicht unterstuetzt; erwartet wird {supported}"
            ),
            Self::NoModels => write!(f, "die Konfiguration nennt kein Modell"),
            Self::TooManyModels { found, maximum } => {
                write!(
                    f,
                    "{found} Modelle konfiguriert, hoechstens {maximum} moeglich"
                )
            }
            Self::Missing { what } => write!(f, "{what}"),
            Self::OutOfRange { expected } => write!(f, "Wert ausserhalb des Bereichs: {expected}"),
            Self::UnknownValue { found, allowed } => {
                write!(f, "unbekannter Wert {found:?}; zulaessig sind {allowed}")
            }
            Self::UnknownModelReference { name } => {
                write!(f, "das Modell {name:?} ist nicht konfiguriert")
            }
            Self::Contract(e) => write!(f, "{e}"),
            Self::Queue(e) => write!(f, "{e}"),
            Self::Slots(e) => write!(f, "{e}"),
            Self::Syntax { message } => write!(f, "YAML konnte nicht gelesen werden: {message}"),
        }
    }
}

impl core::error::Error for ConfigError {}

impl ConfigError {
    /// Verortet diesen Fehler.
    #[must_use]
    pub fn at(self, path: impl Into<String>) -> Located {
        Located {
            path: path.into(),
            error: self,
        }
    }
}
