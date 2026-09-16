//! Modellvertraege und Varianten (Spec 12, ADR-0007).

use crate::arrayvec::ArrayVec;
use crate::contract_ext::ContractExtension;
use crate::ids::{MAX_VARIANTS, VariantIdx};
use crate::objective::{Objective, ObjectiveError};
use crate::profile::VariantProfile;
use crate::queue::{QueueConfig, QueueConfigError};
use crate::request::Criticality;
use crate::runtime_budget::{RuntimeBudget, RuntimeBudgetError};
use crate::semantics::SemanticConflict;
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
    /// Was diese Variante fachlich liefert und verlangt (NV-10).
    ///
    /// Leer heisst „nicht beschrieben". Nicht beschrieben ist kein Beleg fuer
    /// Austauschbarkeit: der Governor waehlt dann nicht automatisch, sondern
    /// bleibt bei der freigegebenen festen Variante.
    pub semantics: crate::semantics::VariantSemantics,
    /// Die Zeit fuer Vor- und Nachverarbeitung dieser Variante.
    ///
    /// Eine Variante, die ein anderes Resize braucht, ist nicht nur eine
    /// andere Bedeutung, sondern auch eine andere Rechnung. Diese Zeit
    /// entsteht **ausserhalb** des Backends und faellt deshalb aus jedem
    /// Backendprofil heraus — sie gehoert trotzdem in die Planung.
    pub preprocess: Duration,
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
    /// Der Vertragszusatz ist unzulaessig (NV-02).
    Extension(crate::contract_ext::ExtensionError),
    /// Das Mindestlaufzeitbudget ist unzulaessig (ADR-0046).
    RuntimeBudget(RuntimeBudgetError),
    /// Die Zusage ist unzulaessig (ADR-0047).
    Objective(ObjectiveError),
}

impl From<crate::contract_ext::ExtensionError> for ContractError {
    fn from(e: crate::contract_ext::ExtensionError) -> Self {
        Self::Extension(e)
    }
}

impl From<RuntimeBudgetError> for ContractError {
    fn from(e: RuntimeBudgetError) -> Self {
        Self::RuntimeBudget(e)
    }
}

impl From<ObjectiveError> for ContractError {
    fn from(e: ObjectiveError) -> Self {
        Self::Objective(e)
    }
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
                    "min_acceptable_quality wird von keiner freigegebenen Variante erreicht"
                )
            }
            Self::ZeroDeadline => write!(f, "die relative Deadline muss groesser als null sein"),
            Self::Queue(e) => write!(f, "Queue-Konfiguration: {e}"),
            Self::Extension(e) => write!(f, "Vertragszusatz: {e}"),
            Self::RuntimeBudget(e) => write!(f, "{e}"),
            Self::Objective(e) => write!(f, "{e}"),
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
    /// Aufwand je Token **bereits vorhandenen Kontexts**, bei jeder
    /// Fortsetzung erneut (NV-16).
    ///
    /// Null heisst: nicht gemessen oder wirksamer Prefix-Cache — dann
    /// verhaelt sich die Zuschneidung wie vor NV-16, und alte Konfigurationen
    /// bleiben unveraendert gueltig.
    ///
    /// Ist der Wert gesetzt, wird der Sockel oben **kontextabhaengig**: ein
    /// spaetes Quantum kostet mehr als ein frueheres, weil sein Prompt
    /// laenger ist. Der Kommentar am Sockel nennt genau diese Ursache — nur
    /// behandelte das Modell sie bis NV-16 als Konstante.
    pub prefill_per_token: Duration,
    /// Wie viel Mehrarbeit die Zerlegung hoechstens kosten darf, in Promille
    /// (NV-16).
    ///
    /// `None` heisst: keine Grenze — es wird zerlegt, was zerlegbar ist, wie
    /// vor NV-16. Eine Grenze, die niemand gesetzt hat, darf keine bestehende
    /// Konfiguration stillschweigend umstellen.
    ///
    /// Ist ein Wert gesetzt und der vorausgerechnete Aufschlag ueberschreitet
    /// ihn, laeuft der Auftrag **ungeteilt** — der Rueckfall aus ADR-0014. Er
    /// blockiert dann seine Zeit am Stueck, und das ist eine Entscheidung mit
    /// Preis: sie tauscht Blockadezeit gegen Gesamtarbeit.
    ///
    /// 0 heisst „jeder Aufschlag ist zu viel", 1000 heisst „hoechstens
    /// doppelt so viel Arbeit wie ungeteilt".
    pub max_overhead_permille: Option<u32>,
}

impl Cooperative {
    /// Das Kostenmodell dieser Zerlegung, fuer Vorausrechnungen (NV-16).
    ///
    /// **Nicht** fuer die Zuschneidung eines einzelnen Quantums: hier wird die
    /// Rate auf eine Dauer je Token gerundet, und erst dividieren, dann
    /// multiplizieren verliert gegenueber der exakten Ratenrechnung. Fuer ein
    /// Verhaeltnis wie den Zerlegungsaufschlag faellt das nicht ins Gewicht;
    /// fuer eine Dauer, gegen die der Look-ahead prueft, schon.
    /// [`Self::cost_of_with_context`] rechnet deshalb exakt.
    #[must_use]
    pub fn cost_model(&self) -> crate::generative::ContextCost {
        crate::generative::ContextCost {
            fixed: self.base_cost,
            prefill_per_token: self.prefill_per_token,
            decode_per_token: if self.tokens_per_second == 0 {
                Duration::from_nanos_unbounded(0)
            } else {
                Duration::from_nanos_unbounded(
                    1_000_000_000_u64
                        .checked_div(u64::from(self.tokens_per_second))
                        .unwrap_or(0),
                )
            },
        }
    }

    /// Wie viele Token in `budget` erzeugt werden koennen.
    ///
    /// Der feste Sockel geht zuerst ab: er faellt je Quantum an, gleich wie
    /// klein es ist. Reicht das Budget nicht einmal fuer ihn, ist die Antwort
    /// null — dann gibt es kein Quantum, das in diese Luecke passt.
    #[must_use]
    pub fn tokens_in(&self, budget: Duration) -> u32 {
        self.tokens_in_with_context(budget, 0)
    }

    /// Die erwartete Dauer eines Quantums dieser Groesse.
    ///
    /// Affin, nicht proportional: Sockel plus Erzeugungszeit.
    ///
    /// Ohne Kontext, also fuer das **erste** Quantum. Fuer eine Fortsetzung
    /// ist [`Cooperative::cost_of_with_context`] die richtige Frage: ihr
    /// Prompt ist laenger, und das kostet (NV-16).
    #[must_use]
    pub fn cost_of(&self, tokens: u32) -> Duration {
        self.cost_of_with_context(tokens, 0)
    }

    /// Die erwartete Dauer eines Quantums bei diesem Kontext (NV-16).
    ///
    /// `context` ist Prompt plus alles bisher Erzeugte. Ohne gemessenen
    /// Prefill-Anteil ist das Ergebnis dasselbe wie bei [`Self::cost_of`].
    #[must_use]
    pub fn cost_of_with_context(&self, tokens: u32, context: u32) -> Duration {
        // Exakt: erst multiplizieren, dann dividieren. Andersherum ginge je
        // Token eine Nanosekunde verloren, und der Fehler waechst mit der
        // Tokenzahl.
        let generating = u64::from(tokens)
            .saturating_mul(1_000_000_000)
            .checked_div(u64::from(self.tokens_per_second).max(1))
            .unwrap_or(0);
        let prefill = self
            .prefill_per_token
            .as_nanos()
            .saturating_mul(u64::from(context));
        Duration::from_nanos_unbounded(
            self.base_cost
                .as_nanos()
                .saturating_add(prefill)
                .saturating_add(generating),
        )
    }

    /// Wie viele Token in `budget` passen, wenn `context` schon dasteht.
    ///
    /// Der Prefill geht zusammen mit dem Sockel ab: beide fallen an, gleich
    /// wie klein das Quantum ist.
    #[must_use]
    pub fn tokens_in_with_context(&self, budget: Duration, context: u32) -> u32 {
        let prefill = self
            .prefill_per_token
            .as_nanos()
            .saturating_mul(u64::from(context));
        let overhead = self.base_cost.as_nanos().saturating_add(prefill);
        let generating = budget.as_nanos().saturating_sub(overhead);
        let tokens = generating
            .saturating_mul(u64::from(self.tokens_per_second))
            .checked_div(1_000_000_000)
            .unwrap_or(0);
        u32::try_from(tokens).unwrap_or(u32::MAX)
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
    /// Der versionierte Vertragszusatz (NV-02), falls einer vereinbart ist.
    ///
    /// Optional und additiv: die Felder oben bleiben die Ausgangswerte. Fehlt
    /// der Zusatz, verhaelt sich der Vertrag wie vor NV-02 — es gibt kein
    /// zweites, paralleles Auftragsmodell.
    pub extension: Option<ContractExtension>,
    /// Das Mindestlaufzeitbudget, falls eines vereinbart ist (ADR-0046).
    ///
    /// Solange es im Fenster nicht aufgebraucht ist, steht wartende Arbeit
    /// dieses Modells ueber `normal` und unter `high`. `None` ist die
    /// bisherige Ordnung, bitgleich.
    pub min_runtime: Option<RuntimeBudget>,
    /// Die Zusage dieses Stroms, falls eine vereinbart ist (ADR-0047).
    ///
    /// Anteil und Luecke — „98 % der Zyklen frisch" und „nie laenger als eine
    /// Sekunde nichts". Sie ordnet **innerhalb** der Klasse um, nie darueber
    /// (NV-24). `None` ist die bisherige Ordnung, bitgleich.
    pub objective: Option<Objective>,
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

        if let Some(extension) = self.extension.as_ref() {
            extension.validate(self.variants.len())?;
            // Eine Mindestqualitaet, die nur ausserhalb der Freigabe
            // erreichbar waere, ist kein erfuellbarer Vertrag. Sie erst beim
            // Dispatch scheitern zu lassen hiesse, im Feld zu entdecken, was
            // beim Start feststeht.
            if let Some(min) = self.min_quality
                && !self.variants.iter().enumerate().any(|(i, v)| {
                    v.quality.value >= min
                        && u16::try_from(i)
                            .is_ok_and(|idx| extension.approved_variants.contains(VariantIdx(idx)))
                })
            {
                return Err(ContractError::MinQualityUnreachable);
            }
        }

        if let Some(budget) = self.min_runtime {
            budget.validate(self.criticality)?;
        }

        // Gegen die Periode, denn der Anteil zaehlt Zyklen: ohne Takt gibt es
        // keine, und eine Luecke unter einem Takt ist nicht erfuellbar.
        if let Some(objective) = self.objective {
            objective.validate(self.period)?;
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
            && self.semantic_conflict().is_none()
    }

    /// Der erste fachliche Widerspruch zwischen zwei freigegebenen Varianten
    /// (NV-10).
    ///
    /// `None` heisst: entweder ist keine Semantik hinterlegt — dann
    /// entscheidet allein die I/O-Signatur wie vor NV-10 — oder alle
    /// beschriebenen Varianten bedeuten dasselbe.
    ///
    /// Geprueft werden nur **freigegebene** Varianten: eine gesperrte Variante
    /// mit abweichender Bedeutung ist kein Grund, die automatische Wahl
    /// abzuschalten, weil sie ohnehin nie laeuft.
    #[must_use]
    pub fn semantic_conflict(&self) -> Option<(VariantIdx, VariantIdx, SemanticConflict)> {
        let approved: ArrayVec<(VariantIdx, &Variant), MAX_VARIANTS> = self
            .variants
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let idx = VariantIdx(u16::try_from(i).ok()?);
                self.variant_approved(idx).then_some((idx, v))
            })
            .fold(ArrayVec::new(), |mut acc, entry| {
                let _ = acc.push(entry);
                acc
            });

        // Beschreibt keine Variante ihre Bedeutung, gilt der Zustand vor
        // NV-10: die Signaturpruefung entscheidet. Beschreibt sie **eine**,
        // muessen es alle tun — eine halb beschriebene Variantenreihe ist
        // gefaehrlicher als eine gar nicht beschriebene, weil sie nach
        // Sorgfalt aussieht.
        if approved.iter().all(|(_, v)| !v.semantics.is_specified()) {
            return None;
        }

        let mut first: Option<(VariantIdx, &Variant)> = None;
        for (idx, variant) in approved.iter() {
            match first {
                None => first = Some((*idx, variant)),
                Some((reference_idx, reference)) => {
                    if let Err(conflict) =
                        crate::semantics::interchangeable(&reference.semantics, &variant.semantics)
                    {
                        return Some((reference_idx, *idx, conflict));
                    }
                }
            }
        }
        None
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

    /// Wahr, wenn diese Variante fachlich freigegeben ist (NV-02).
    ///
    /// „Gut genug" und „freigegeben" sind verschiedene Aussagen. Eine
    /// Variante kann ueber der Mindestqualitaet liegen und trotzdem nie
    /// zertifiziert worden sein. Ohne Vertragszusatz ist alles freigegeben —
    /// wer nichts einschraenkt, soll nicht ploetzlich eingeschraenkt sein.
    #[must_use]
    pub fn variant_approved(&self, idx: VariantIdx) -> bool {
        if self.variant(idx).is_none() {
            return false;
        }
        self.extension
            .as_ref()
            .is_none_or(|e| e.approved_variants.contains(idx))
    }

    /// Wahr, wenn die Variante verwendet werden darf.
    ///
    /// Qualitaet **und** Freigabe. Der Scheduler fragt diese Funktion, nicht
    /// die beiden einzeln — die Reihenfolge zweier Bedingungen zu vergessen
    /// ist der billigste Weg zu einer unautorisierten Lockerung.
    #[must_use]
    pub fn variant_usable(&self, idx: VariantIdx) -> bool {
        self.meets_min_quality(idx) && self.variant_approved(idx)
    }
}
