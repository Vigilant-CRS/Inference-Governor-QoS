//! Modellvertraege und Varianten (Spec 12, ADR-0007).

use crate::arrayvec::ArrayVec;
use crate::ids::{MAX_VARIANTS, VariantIdx};
use crate::profile::VariantProfile;
use crate::queue::{QueueConfig, QueueConfigError};
use crate::request::Criticality;
use crate::time::Duration;

/// Relative Qualitaet einer Variante in Tausendsteln.
///
/// Ganzzahlig statt `f64`: Qualitaetsvergleiche sind Sortier- und
/// Schwellenentscheidungen; ein Gleitkommavergleich waere hier eine
/// Fehlerquelle ohne Gegenwert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Quality(u16);

impl Quality {
    /// Volle Qualitaet (1,000).
    pub const FULL: Self = Self(1_000);

    /// Erzeugt einen Qualitaetswert aus Tausendsteln, `None` ueber 1000.
    #[must_use]
    pub const fn from_milli(v: u16) -> Option<Self> {
        if v > 1_000 { None } else { Some(Self(v)) }
    }

    /// Der Wert in Tausendsteln.
    #[must_use]
    pub const fn as_milli(self) -> u16 {
        self.0
    }
}

impl core::fmt::Display for Quality {
    // Division und Rest durch die Konstante 1000, um Tausendstel als
    // Dezimalzahl darzustellen. Kein Praezisionsverlust: der Wert ist per
    // Konstruktion ganzzahlig und <= 1000.
    #[allow(clippy::integer_division)]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{:03}", self.0 / 1_000, self.0 % 1_000)
    }
}

/// Woher der Qualitaetswert stammt (ADR-0007).
///
/// Vigilant darf Qualitaet nicht erfinden (Spec 12.2). Wer den Wert gesetzt hat
/// und worauf er beruht, entscheidet darueber, ob eine automatische
/// Variantenwahl ueberhaupt zulaessig ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QualitySource {
    /// Vom Nutzer deklariert, nicht gemessen.
    UserDeclared,
    /// Auf einem benannten Datensatz gemessen.
    Measured,
    /// Unbekannt — deaktiviert die automatische Variantenwahl.
    #[default]
    Unknown,
}

/// Ein Qualitaetswert samt Herkunft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QualityValue {
    /// Der relative Wert.
    pub value: Quality,
    /// Die Herkunft.
    pub source: QualitySource,
}

impl QualityValue {
    /// Ein deklarierter Wert.
    #[must_use]
    pub const fn declared(value: Quality) -> Self {
        Self {
            value,
            source: QualitySource::UserDeclared,
        }
    }

    /// Ein gemessener Wert.
    #[must_use]
    pub const fn measured(value: Quality) -> Self {
        Self {
            value,
            source: QualitySource::Measured,
        }
    }
}

/// Eine physische Modellvariante.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    /// Relative Qualitaet samt Herkunft.
    pub quality: QualityValue,
    /// Laufzeitprofile je Belegungsgrad.
    pub profile: VariantProfile,
}

/// Warum ein Modellvertrag unzulaessig ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractError {
    /// Kein einziges physisches Modell hinterlegt.
    NoVariants,
    /// Mehr Varianten als [`MAX_VARIANTS`].
    TooManyVariants {
        /// Die geforderte Anzahl.
        requested: usize,
    },
    /// Die Varianten sind nicht absteigend nach Qualitaet sortiert.
    ///
    /// Spec 12.3 waehlt „die erste feasible Variante" aus einer absteigend
    /// sortierten Liste. Eine unsortierte Liste wuerde diese Regel still
    /// verfaelschen, statt sie zu verletzen — deshalb wird sie abgelehnt.
    VariantsNotSortedByQuality {
        /// Der Index, an dem die Sortierung bricht.
        at: usize,
    },
    /// `min_acceptable_quality` ist von keiner Variante erreichbar.
    MinQualityUnreachable,
    /// Die Deadline ist null.
    ZeroDeadline,
    /// Die kooperativen Grenzen sind unzulaessig.
    ///
    /// `min_tokens > max_total_tokens` beschreibt kein Intervall. Erkennt man
    /// das erst beim Dispatch, panickt die Zuschneidung der Quantengroesse —
    /// und mit `panic = "abort"` im Releaseprofil beendet das den Prozess.
    /// Eine Konfiguration, die den Governor abschiessen kann, darf nicht
    /// angenommen werden.
    CooperativeRangeEmpty {
        /// Die geforderte Untergrenze.
        min_tokens: u32,
        /// Die geforderte Obergrenze.
        max_total_tokens: u32,
    },
    /// Die kooperative Erzeugungsrate ist null.
    ///
    /// Aus einem Zeitbudget liesse sich dann keine Tokenzahl ableiten; jede
    /// Zerlegung waere geraten.
    CooperativeZeroRate,
    /// Die Queue-Konfiguration ist unzulaessig.
    Queue(QueueConfigError),
}

impl core::fmt::Display for ContractError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoVariants => write!(f, "der Vertrag nennt keine physische Variante"),
            Self::TooManyVariants { requested } => {
                write!(f, "{requested} Varianten, Maximum {MAX_VARIANTS}")
            }
            Self::VariantsNotSortedByQuality { at } => {
                write!(
                    f,
                    "Varianten nicht absteigend nach Qualitaet sortiert, Bruch bei Index {at}"
                )
            }
            Self::CooperativeRangeEmpty {
                min_tokens,
                max_total_tokens,
            } => {
                write!(
                    f,
                    "cooperative.min_tokens {min_tokens} ueberschreitet max_total_tokens {max_total_tokens}"
                )
            }
            Self::CooperativeZeroRate => {
                write!(f, "cooperative.tokens_per_second ist null")
            }
            Self::MinQualityUnreachable => {
                write!(
                    f,
                    "min_acceptable_quality wird von keiner Variante erreicht"
                )
            }
            Self::ZeroDeadline => write!(f, "die relative Deadline muss groesser als null sein"),
            Self::Queue(e) => write!(f, "Queue-Konfiguration: {e}"),
        }
    }
}

impl core::error::Error for ContractError {}

impl From<QueueConfigError> for ContractError {
    fn from(e: QueueConfigError) -> Self {
        Self::Queue(e)
    }
}

/// Die Angaben, die ein zerlegbares Modell mitbringen muss (ADR-0014).
///
/// Nur generative Modelle haben natuerliche Unterbrechungspunkte. Ein
/// Detektor hat keine — sein Vorwaertslauf ist unteilbar. Deshalb steht das
/// hier explizit in der Konfiguration und wird nicht geraten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cooperative {
    /// Erzeugungsrate in Token je Sekunde, gemessen.
    ///
    /// Ohne diesen Wert laesst sich aus einem Zeitbudget keine Tokenzahl
    /// ableiten. Ein geratener Wert waere hier besonders teuer: zu hoch
    /// geschaetzt entstehen Quanten, die laenger dauern als der Slack, und die
    /// Zerlegung erreicht genau nichts.
    pub tokens_per_second: u32,
    /// Kleinste sinnvolle Quantengroesse.
    ///
    /// Unterhalb davon ueberwiegt der Aufwand je Auftrag den Nutzen: jeder
    /// Auftrag kostet einen Round-Trip und, ohne Prefix-Caching, eine erneute
    /// Prefill-Berechnung.
    pub min_tokens: u32,
    /// Obergrenze der insgesamt erzeugten Token je Auftrag.
    ///
    /// Spec 8.3: keine unbeschraenkte Arbeit aus fremd kontrollierter Eingabe.
    pub max_total_tokens: u32,
    /// Feste Kosten je Quantum, unabhaengig von seiner Tokenzahl.
    ///
    /// Round-Trip, Scheduling im Backend und — ohne wirksames Prefix-Caching —
    /// die erneute Prefill-Berechnung des gewachsenen Prompts. Auf der
    /// Messmaschine sind das 18 ms je Auftrag, bei rund 4 ms je Token.
    ///
    /// Ohne diesen Term rechnet die Zuschneidung die Dauer eines Quantums rein
    /// proportional zur Tokenzahl. Das ist genau dann falsch, wenn es darauf
    /// ankommt: ist der Sockel so gross wie der Slack bis zur naechsten
    /// geschuetzten Ankunft, passt **kein** Quantum, gleichgueltig wie klein
    /// man es waehlt. Der Scheduler muss das sehen koennen, statt ein Quantum
    /// zu starten, das seine Luecke sicher ueberzieht.
    ///
    /// Gemessen, nicht geraten — wie `tokens_per_second`.
    pub base_cost: Duration,
}

impl Cooperative {
    /// Wie viele Token in `budget` erzeugt werden koennen.
    ///
    /// Der feste Sockel geht zuerst ab: er faellt je Quantum an, gleich wie
    /// klein es ist. Reicht das Budget nicht einmal fuer ihn, ist die Antwort
    /// null — dann gibt es kein Quantum, das in diese Luecke passt.
    #[must_use]
    pub fn tokens_in(&self, budget: Duration) -> u32 {
        let generating = budget.as_nanos().saturating_sub(self.base_cost.as_nanos());
        let tokens = generating
            .saturating_mul(u64::from(self.tokens_per_second))
            .checked_div(1_000_000_000)
            .unwrap_or(0);
        u32::try_from(tokens).unwrap_or(u32::MAX)
    }

    /// Die erwartete Dauer eines Quantums dieser Groesse.
    ///
    /// Affin, nicht proportional: Sockel plus Erzeugungszeit.
    #[must_use]
    pub fn cost_of(&self, tokens: u32) -> Duration {
        let generating = u64::from(tokens)
            .saturating_mul(1_000_000_000)
            .checked_div(u64::from(self.tokens_per_second).max(1))
            .unwrap_or(0);
        Duration::from_nanos_unbounded(self.base_cost.as_nanos().saturating_add(generating))
    }
}

/// Der Vertrag eines logischen Modells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelContract {
    /// Ob die Varianten dieselbe Schnittstelle bedienen.
    ///
    /// Der Governor waehlt die Variante je Request und sagt es dem Client
    /// nicht. Diese Freiheit setzt voraus, dass alle Varianten dieselben
    /// Eingaben nehmen und dieselben Ausgaben liefern. Ist das nachweislich
    /// nicht so, wird die automatische Wahl abgeschaltet, statt dem Client
    /// nach einem Wechsel einen Backendfehler oder — schlimmer — einen Tensor
    /// mit anderer Bedeutung bei gleicher Form zu liefern.
    ///
    /// Wird beim Start aus den Backendmetadaten gesetzt (siehe
    /// `vig verify`/`doctor`). Voreinstellung ist `true`: solange nichts
    /// dagegen spricht, gilt die Konfiguration.
    pub variants_interchangeable: bool,
    /// Die Wichtigkeitsklasse.
    pub criticality: Criticality,
    /// Das Queue-Verhalten.
    pub queue: QueueConfig,
    /// Die erwartete Periode, falls das Modell periodisch bedient wird.
    ///
    /// Grundlage des Look-ahead auf erwartbare zukuenftige Protected-Arbeit
    /// (Spec 10.8). Keine Garantie, dass ein Frame exakt dann eintrifft.
    pub period: Option<Duration>,
    /// Die relative Deadline ab Generation Time.
    pub deadline: Duration,
    /// Das fachliche Hoechstalter.
    pub max_age: Option<Duration>,
    /// Wahr fuer sequenzbasierte Modelle.
    pub stateful: bool,
    /// Die niedrigste noch akzeptable Variantenqualitaet.
    pub min_quality: Option<Quality>,
    /// Mindestverweildauer auf einer Variante vor einer Aufwertung (Spec 12.4).
    pub variant_dwell: Duration,
    /// Die physischen Varianten, absteigend nach Qualitaet.
    pub variants: ArrayVec<Variant, MAX_VARIANTS>,
    /// Zerlegbarkeit in kooperative Quanten (ADR-0014), falls zutreffend.
    pub cooperative: Option<Cooperative>,
}

impl ModelContract {
    /// Prueft den Vertrag.
    ///
    /// # Errors
    ///
    /// Siehe [`ContractError`]. Spec L-020: eine ungueltige Konfiguration wird
    /// abgelehnt, nicht repariert.
    pub fn validate(&self) -> Result<(), ContractError> {
        self.queue.validate(self.stateful)?;

        if self.deadline.as_nanos() == 0 {
            return Err(ContractError::ZeroDeadline);
        }
        if self.variants.is_empty() {
            return Err(ContractError::NoVariants);
        }
        if self.variants.len() > MAX_VARIANTS {
            return Err(ContractError::TooManyVariants {
                requested: self.variants.len(),
            });
        }

        let mut previous: Option<Quality> = None;
        for (i, v) in self.variants.iter().enumerate() {
            if let Some(prev) = previous
                && v.quality.value > prev
            {
                return Err(ContractError::VariantsNotSortedByQuality { at: i });
            }
            previous = Some(v.quality.value);
        }

        if let Some(min) = self.min_quality
            && !self.variants.iter().any(|v| v.quality.value >= min)
        {
            return Err(ContractError::MinQualityUnreachable);
        }

        if let Some(cooperative) = self.cooperative {
            if cooperative.tokens_per_second == 0 {
                return Err(ContractError::CooperativeZeroRate);
            }
            if cooperative.min_tokens > cooperative.max_total_tokens {
                return Err(ContractError::CooperativeRangeEmpty {
                    min_tokens: cooperative.min_tokens,
                    max_total_tokens: cooperative.max_total_tokens,
                });
            }
        }
        Ok(())
    }

    /// Wahr, wenn Vigilant die Variante automatisch waehlen darf.
    ///
    /// ADR-0007: eine Variante mit unbekannter Qualitaetsherkunft deaktiviert
    /// die automatische Wahl fuer das gesamte Modell. Die Alternative waere,
    /// auf einer geratenen Zahl zu degradieren — und damit die Wahrnehmung zu
    /// verschlechtern, waehrend die eigenen Metriken gruen bleiben.
    #[must_use]
    pub fn auto_variant_selection(&self) -> bool {
        self.variants.len() > 1
            && !self.stateful
            && self.variants_interchangeable
            && self
                .variants
                .iter()
                .all(|v| v.quality.source != QualitySource::Unknown)
    }

    /// Die Variante an einem Index.
    #[must_use]
    pub fn variant(&self, idx: VariantIdx) -> Option<&Variant> {
        self.variants.get(idx.get())
    }

    /// Wahr, wenn die Variante die Mindestqualitaet erfuellt.
    #[must_use]
    pub fn meets_min_quality(&self, idx: VariantIdx) -> bool {
        match (self.min_quality, self.variant(idx)) {
            (Some(min), Some(v)) => v.quality.value >= min,
            (None, Some(_)) => true,
            (_, None) => false,
        }
    }
}
