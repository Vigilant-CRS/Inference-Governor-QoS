//! Versionierte Vertragszusaetze und der Weakly-hard-Monitor (NV-02).
//!
//! ## Warum ein Zusatzobjekt und kein zweites Auftragsmodell
//!
//! Der bestehende [`ModelContract`](crate::model::ModelContract) sagt, was ein
//! Strom braucht: Periode, Deadline, Hoechstalter, Kritikalitaet,
//! Mindestqualitaet. Diese Felder bleiben die Ausgangswerte. Was hier
//! hinzukommt, beantwortet Fragen, die der alte Vertrag nicht stellen konnte:
//!
//! * Wann tastet der **Verbraucher** ab — und nicht, wann ein Request eintrifft?
//! * Wie viele Fehlversorgungen sind in einem Fenster zulaessig, und wie viele
//!   hintereinander?
//! * Welche Varianten sind fachlich **freigegeben** — nicht nur qualitativ
//!   ausreichend?
//! * Wie stark ist die Zusage, die der Betreiber verlangt: beobachtet,
//!   qualifiziert oder bewiesen?
//!
//! Ein zweites, paralleles Auftragsmodell einzufuehren waere die teure
//! Variante gewesen: zwei Wahrheiten ueber denselben Strom, die
//! auseinanderlaufen, sobald jemand nur eine pflegt. Der Zusatz ist deshalb
//! **optional** und **versioniert**. Fehlt er, gilt der Vertrag wie bisher.
//!
//! ## Die Trennlinie, die dieses Modul zieht
//!
//! **Anforderung und Beobachtung sind verschiedene Felder.**
//! [`ContractExtension::evidence_required`] sagt, welche Nachweisstufe der
//! Betreiber verlangt. [`MissWindow`] sagt, was tatsaechlich passiert ist.
//! Das eine aus dem anderen abzuleiten — die Deadline zu verlaengern, bis die
//! Messung passt — ist genau der Fehler, den Dokument 04 als Risiko benennt.
//! Es gibt hier deshalb keine Funktion, die eine Anforderung aus Messwerten
//! erzeugt.
//!
//! ## Was der Monitor nicht ist
//!
//! Eine begrenzte Ringstruktur genuegt, um eine Weakly-hard-Bedingung zu
//! **beobachten**. Sie durchzusetzen braucht kuenftige Kapazitaet und
//! beherrschte Stoerungen. Ein `MissWindow` ist ein Messgeraet, keine Zusage —
//! und die Kritikalitaetsklasse `Protected` ist noch kein Weakly-hard-Vertrag.

use crate::ids::{MAX_VARIANTS, VariantIdx};
use crate::time::{Duration, Instant};

/// Die Version dieses Zusatzformats.
///
/// Eine unbekannte Version wird abgelehnt und nicht ignoriert: ein Feld, das
/// der Governor nicht versteht, koennte genau die Einschraenkung enthalten,
/// auf die sich der Betreiber verlaesst.
pub const CONTRACT_EXTENSION_VERSION: u32 = 1;

/// Die groesste beobachtbare Fenstergroesse in Verbraucherzyklen.
///
/// Der Ring ist ein Bitfeld fester Groesse; ohne Obergrenze waere er ein
/// unbeschraenkter Puffer (Spec L-003). 1024 Zyklen sind bei 30 Hz gut eine
/// halbe Minute — laenger, als eine Weakly-hard-Aussage sinnvoll traegt.
pub const MAX_WINDOW_CYCLES: u32 = 1024;

/// Wie viele `u64`-Woerter der Ring braucht.
///
/// Muss zu [`RING_CAPACITY`] passen; ein Test haelt beide zusammen.
const RING_WORDS: usize = 16;

/// Wie viele Zyklen der Ring fasst.
///
/// Zweierpotenz, damit Index und Bit ohne Division bestimmt werden koennen.
const RING_CAPACITY: u32 = 1024;

/// Die Maske fuer [`RING_CAPACITY`].
const RING_MASK: u64 = 1023;

/// Was der Verbraucher aus einer Lieferung macht.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliverySemantics {
    /// Nur der juengste Zustand zaehlt; aeltere Ergebnisse duerfen entfallen.
    #[default]
    LatestState,
    /// Jedes Ereignis muss ankommen; Supersession ist unzulaessig.
    EveryEvent,
    /// Die Reihenfolge traegt Bedeutung; Luecken brechen die Sequenz.
    StatefulSequence,
}

/// Bis wohin die Zusage reicht.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliveryBoundary {
    /// Bis der Governor die Antwort hat.
    ///
    /// Voreinstellung, weil sie die einzige Grenze ist, die der Governor
    /// selbst messen kann. Alles danach — Transport, Deserialisierung,
    /// Regeltakt des Verbrauchers — liegt ausserhalb seiner Beobachtung.
    #[default]
    Governor,
    /// Bis das Ergebnis beim vereinbarten Verbraucher liegt.
    ///
    /// Verlangt, dass der Verbraucher seine Abtastzeitpunkte meldet. Ohne
    /// diese Meldung ist die Grenze eine Behauptung, keine Messung.
    Consumer,
}

/// Wie stark die Zusage ist, die der Betreiber verlangt.
///
/// Drei verschiedene Aussagen, die im Sprachgebrauch gern zusammenfallen:
/// „lief im Test durch", „unter benannten Annahmen qualifiziert" und „fuer
/// alle zulaessigen Faelle garantiert".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum EvidenceLevel {
    /// Beobachtet: der Monitor zaehlt mit, mehr wird nicht behauptet.
    #[default]
    Observed,
    /// Qualifiziert: unter benannten Annahmen und in einer Messkampagne belegt.
    QualifiedSlo,
    /// Bewiesen: analytisch abgesichert. Heute von nichts in diesem Projekt
    /// erfuellt; das Feld existiert, damit ein Betreiber die Forderung
    /// aufschreiben kann und eine Ablehnung bekommt statt eines Achselzuckens.
    Proven,
}

/// Wie viele Fehlversorgungen zulaessig sind.
///
/// `M` Misses in jedem vollstaendigen Fenster aus `K` Zyklen, und hoechstens
/// `L` hintereinander. „Hoechstens zwei Misses in 100 Zyklen und niemals zwei
/// hintereinander" ist `M=2, K=100, L=1` — nicht `L=2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissBudget {
    /// M — hoechstens so viele Misses je Fenster.
    pub max_misses: u32,
    /// K — die Fenstergroesse in Verbraucherzyklen.
    pub window_cycles: u32,
    /// L — hoechstens so viele Misses hintereinander.
    ///
    /// `None` heisst „keine Aussage ueber Folgen", nicht „beliebig viele in
    /// einem Fenster": M begrenzt weiterhin.
    pub max_consecutive: Option<u32>,
}

/// Der Betriebsbereich, fuer den der Vertrag beansprucht wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ValidityEnvelope {
    /// Bis zu welcher Eingabegroesse in KiB.
    pub max_input_kib: Option<u64>,
    /// Bis zu welcher serialisierten Auslastung in Prozent.
    pub max_occupancy_pct: Option<u32>,
}

/// Die freigegebenen Varianten eines Vertrags.
///
/// Bewusst eine Maske und keine Qualitaetsschwelle: „gut genug" und
/// „freigegeben" sind verschiedene Aussagen. Eine Variante kann qualitativ
/// ueber der Mindestschwelle liegen und trotzdem nie zertifiziert worden
/// sein — etwa weil sie mit einem anderen Datensatz trainiert wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovedVariants(u16);

impl Default for ApprovedVariants {
    /// Alle Varianten freigegeben.
    ///
    /// Der Zusatz ist optional; wer ihn nicht schreibt, soll nicht plotzlich
    /// keine Variante mehr verwenden duerfen.
    fn default() -> Self {
        Self::all()
    }
}

impl ApprovedVariants {
    /// Alle Varianten freigegeben.
    #[must_use]
    pub const fn all() -> Self {
        Self(u16::MAX)
    }

    /// Keine Variante freigegeben.
    ///
    /// Als Vertrag unzulaessig — [`ContractExtension::validate`] lehnt das ab.
    /// Der Konstruktor existiert, um genau das testen zu koennen.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Aus einer Liste von Variantenindizes.
    #[must_use]
    pub fn from_indices(indices: &[VariantIdx]) -> Self {
        let mut mask = 0_u16;
        for idx in indices {
            let bit = idx.get();
            if bit < MAX_VARIANTS {
                mask |= 1_u16 << bit;
            }
        }
        Self(mask)
    }

    /// Ob diese Variante verwendet werden darf.
    #[must_use]
    pub const fn contains(self, idx: VariantIdx) -> bool {
        let bit = idx.get();
        if bit >= MAX_VARIANTS {
            return false;
        }
        self.0 & (1_u16 << bit) != 0
    }

    /// Ob ueberhaupt eine Variante freigegeben ist, gegeben deren Anzahl.
    #[must_use]
    pub fn any_within(self, variants: usize) -> bool {
        (0..variants.min(MAX_VARIANTS)).any(|bit| self.0 & (1_u16 << bit) != 0)
    }
}

/// Der versionierte Vertragszusatz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractExtension {
    /// Die Formatversion dieses Zusatzes.
    pub version: u32,
    /// Der Abtasttakt des Verbrauchers.
    ///
    /// **Nicht** die Ankunftsperiode der Requests. Der Vertragstakt kommt aus
    /// dem Vertrag; wuerde er aus den tatsaechlich angenommenen Requests
    /// abgeleitet, koennte man ihn durch Ablehnen aller Requests einhalten.
    pub consumer_period: Option<Duration>,
    /// Der Versatz des ersten Abtastzeitpunkts gegenueber dem Vertragsbeginn.
    pub phase: Option<Duration>,
    /// Wie weit ein Abtastzeitpunkt schwanken darf.
    pub release_jitter_envelope: Option<Duration>,
    /// Bis wohin die Zusage reicht.
    pub delivery_boundary: DeliveryBoundary,
    /// Was der Verbraucher aus einer Lieferung macht.
    pub delivery_semantics: DeliverySemantics,
    /// Ob jeder Zyklus einen **neuen** Messwert braucht.
    ///
    /// `false` heisst: ein noch gueltiges Bestandsresultat versorgt mehrere
    /// Zyklen. `true` heisst: es tut es nicht — jeder Zyklus verlangt eine
    /// eigene Messung.
    pub require_new_sample_each_cycle: bool,
    /// Ueber wie viele Zyklen beobachtet wird.
    pub observation_window: Option<u32>,
    /// Die Weakly-hard-Bedingung, falls eine vereinbart ist.
    pub miss_budget: Option<MissBudget>,
    /// Der Mindestfortschritt fuer Hintergrundlast, in Prozent der Zyklen.
    ///
    /// Verhindert, dass ein perfekt versorgter Vordergrund einen Strom
    /// vollstaendig aushungert, ohne dass es jemandem auffaellt.
    pub minimum_background_progress_pct: Option<u32>,
    /// Welche Varianten freigegeben sind.
    pub approved_variants: ApprovedVariants,
    /// Der Betriebsbereich, fuer den der Vertrag beansprucht wird.
    pub validity_envelope: ValidityEnvelope,
    /// Welche Nachweisstufe der Betreiber verlangt.
    pub evidence_required: EvidenceLevel,
    /// Die Revision des Vertrags selbst, vom Betreiber vergeben.
    ///
    /// Traegt zusammen mit der Aktivierungszeit die Antwort auf „welcher
    /// Vertrag galt, als dieser Zaehler lief".
    pub contract_version: u32,
}

impl Default for ContractExtension {
    fn default() -> Self {
        Self {
            version: CONTRACT_EXTENSION_VERSION,
            consumer_period: None,
            phase: None,
            release_jitter_envelope: None,
            delivery_boundary: DeliveryBoundary::Governor,
            delivery_semantics: DeliverySemantics::LatestState,
            require_new_sample_each_cycle: false,
            observation_window: None,
            miss_budget: None,
            minimum_background_progress_pct: None,
            approved_variants: ApprovedVariants::all(),
            validity_envelope: ValidityEnvelope::default(),
            evidence_required: EvidenceLevel::Observed,
            contract_version: 1,
        }
    }
}

/// Warum ein Vertragszusatz abgelehnt wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionError {
    /// Die Formatversion ist unbekannt.
    UnsupportedVersion {
        /// Die gefundene Version.
        found: u32,
        /// Die unterstuetzte Version.
        supported: u32,
    },
    /// Die Fenstergroesse ist null.
    ZeroWindow,
    /// Die Fenstergroesse uebersteigt [`MAX_WINDOW_CYCLES`].
    WindowTooLarge {
        /// Die geforderte Groesse.
        requested: u32,
    },
    /// `max_misses` ist nicht kleiner als `window_cycles`.
    ///
    /// `M >= K` erlaubt jeden Miss in jedem Fenster. Das ist kein Vertrag,
    /// sondern seine Abwesenheit — und als Konfiguration fast immer ein
    /// Vertipper.
    MissBudgetNotBinding {
        /// M.
        max_misses: u32,
        /// K.
        window_cycles: u32,
    },
    /// `max_consecutive` ist groesser als `max_misses`.
    ///
    /// L Misses hintereinander sind auch L Misses im Fenster. Ein L ueber M
    /// waere durch M bereits ausgeschlossen und beschreibt damit eine Regel,
    /// die nie greift.
    ConsecutiveExceedsBudget {
        /// L.
        max_consecutive: u32,
        /// M.
        max_misses: u32,
    },
    /// Eine Weakly-hard-Bedingung ohne Verbrauchertakt.
    ///
    /// Ohne `consumer_period` gibt es keinen Zyklus, ueber den gezaehlt werden
    /// koennte. Der Takt aus den Ankuenften abzuleiten waere der Fehler, den
    /// Dokument 04 ausdruecklich ausschliesst.
    MissBudgetWithoutPeriod,
    /// Kein einziger Variantenindex ist freigegeben.
    NoApprovedVariant,
    /// Das Beobachtungsfenster ist null oder zu gross.
    ObservationWindowOutOfRange {
        /// Der geforderte Wert.
        requested: u32,
    },
    /// Der Mindestfortschritt liegt ueber 100 Prozent.
    BackgroundProgressOutOfRange {
        /// Der geforderte Wert.
        requested: u32,
    },
}

impl core::fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "Vertragszusatz der Version {found}; unterstuetzt wird {supported}"
            ),
            Self::ZeroWindow => write!(
                f,
                "window_cycles ist 0; ein Fenster ohne Zyklen zaehlt nichts"
            ),
            Self::WindowTooLarge { requested } => write!(
                f,
                "window_cycles {requested} ueberschreitet das Maximum {MAX_WINDOW_CYCLES}"
            ),
            Self::MissBudgetNotBinding {
                max_misses,
                window_cycles,
            } => write!(
                f,
                "max_misses {max_misses} bei window_cycles {window_cycles} erlaubt jeden Zyklus \
                 als Miss; verlangt ist 0 <= M < K"
            ),
            Self::ConsecutiveExceedsBudget {
                max_consecutive,
                max_misses,
            } => write!(
                f,
                "max_consecutive {max_consecutive} ueber max_misses {max_misses}; \
                 L Misses hintereinander sind auch L Misses im Fenster"
            ),
            Self::MissBudgetWithoutPeriod => write!(
                f,
                "ein Missbudget ohne consumer_period; ohne Verbrauchertakt gibt es keinen Zyklus"
            ),
            Self::NoApprovedVariant => {
                write!(f, "keine einzige Variante ist freigegeben")
            }
            Self::ObservationWindowOutOfRange { requested } => write!(
                f,
                "observation_window {requested}; zulaessig ist 1 bis {MAX_WINDOW_CYCLES}"
            ),
            Self::BackgroundProgressOutOfRange { requested } => write!(
                f,
                "minimum_background_progress_pct {requested}; zulaessig ist 0 bis 100"
            ),
        }
    }
}

impl core::error::Error for ExtensionError {}

impl ContractExtension {
    /// Prueft den Zusatz.
    ///
    /// # Errors
    ///
    /// Siehe [`ExtensionError`]. Wie ueberall in diesem Projekt gilt Spec
    /// L-020: eine ungueltige Konfiguration wird abgelehnt, nicht repariert.
    pub fn validate(&self, variants: usize) -> Result<(), ExtensionError> {
        if self.version != CONTRACT_EXTENSION_VERSION {
            return Err(ExtensionError::UnsupportedVersion {
                found: self.version,
                supported: CONTRACT_EXTENSION_VERSION,
            });
        }
        if !self.approved_variants.any_within(variants) {
            return Err(ExtensionError::NoApprovedVariant);
        }
        if let Some(window) = self.observation_window
            && (window == 0 || window > MAX_WINDOW_CYCLES)
        {
            return Err(ExtensionError::ObservationWindowOutOfRange { requested: window });
        }
        if let Some(pct) = self.minimum_background_progress_pct
            && pct > 100
        {
            return Err(ExtensionError::BackgroundProgressOutOfRange { requested: pct });
        }
        if let Some(budget) = self.miss_budget {
            if self.consumer_period.is_none() {
                return Err(ExtensionError::MissBudgetWithoutPeriod);
            }
            budget.validate()?;
        }
        Ok(())
    }

    /// Der Verbrauchertakt, falls einer vereinbart ist.
    #[must_use]
    pub const fn tick(&self) -> Option<Duration> {
        self.consumer_period
    }
}

impl MissBudget {
    /// Prueft die Weakly-hard-Bedingung auf Wohlgeformtheit.
    ///
    /// # Errors
    ///
    /// Siehe [`ExtensionError`].
    pub const fn validate(&self) -> Result<(), ExtensionError> {
        if self.window_cycles == 0 {
            return Err(ExtensionError::ZeroWindow);
        }
        if self.window_cycles > MAX_WINDOW_CYCLES {
            return Err(ExtensionError::WindowTooLarge {
                requested: self.window_cycles,
            });
        }
        if self.max_misses >= self.window_cycles {
            return Err(ExtensionError::MissBudgetNotBinding {
                max_misses: self.max_misses,
                window_cycles: self.window_cycles,
            });
        }
        if let Some(l) = self.max_consecutive
            && l > self.max_misses
        {
            return Err(ExtensionError::ConsecutiveExceedsBudget {
                max_consecutive: l,
                max_misses: self.max_misses,
            });
        }
        Ok(())
    }
}

/// Wie ein Verbraucherzyklus ausgegangen ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleOutcome {
    /// Vertragsgemaess versorgt.
    Supplied,
    /// Nicht versorgt.
    Missed,
}

/// Was ein Fenster ueber die Weakly-hard-Bedingung sagt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeaklyHardStatus {
    /// Noch kein vollstaendiges Fenster beobachtet.
    Warmup {
        /// Wie viele Zyklen noch fehlen.
        remaining: u32,
    },
    /// Die Bedingung ist im letzten vollstaendigen Fenster eingehalten.
    Holding,
    /// Die Bedingung ist verletzt.
    Violated {
        /// Misses im Fenster.
        misses: u32,
        /// Die laengste Folge im Fenster.
        longest_run: u32,
    },
}

/// Ein begrenzter Ring ueber die letzten Verbraucherzyklen.
///
/// Beobachtet, ob eine Weakly-hard-Bedingung eingehalten wird. Der Ring hat
/// feste Groesse; die Anzahl beobachteter Zyklen waechst nicht mit der
/// Laufzeit (Spec L-003).
///
/// ## Der Takt kommt aus dem Vertrag
///
/// [`MissWindow::advance_to`] rechnet aus, wie viele Verbraucherzyklen seit
/// dem letzten Abtastzeitpunkt vergangen sind, und traegt fuer jeden davon
/// ein Ergebnis ein. Uebersprungene Zyklen sind Misses. Genau das macht den
/// Unterschied zu einer Zaehlung ueber angenommene Requests: ein Governor,
/// der alles ablehnt, erzeugt keine Zyklen und faellt in einer solchen
/// Zaehlung nicht auf. Hier faellt er auf.
#[derive(Debug, Clone)]
pub struct MissWindow {
    /// Bitfeld: 1 = Miss. Bit `n % (RING_WORDS * 64)` gehoert zu Zyklus n.
    ring: [u64; RING_WORDS],
    /// Die Fenstergroesse K.
    window: u32,
    /// L, falls vereinbart.
    max_consecutive: Option<u32>,
    /// M.
    max_misses: u32,
    /// Wie viele Zyklen insgesamt eingetragen wurden.
    cycles: u64,
    /// Der Zeitpunkt des zuletzt eingetragenen Zyklus.
    last_sample: Option<Instant>,
    /// Der Verbrauchertakt.
    period: Duration,
    /// Die Vertragsrevision, unter der dieser Zaehler laeuft.
    contract_version: u32,
}

impl MissWindow {
    /// Legt einen Monitor fuer diese Bedingung an.
    ///
    /// Gibt `None`, wenn kein Verbrauchertakt vereinbart ist oder die
    /// Bedingung ungueltig ist — ein Monitor ohne Takt koennte nur raten.
    #[must_use]
    pub fn new(extension: &ContractExtension) -> Option<Self> {
        let budget = extension.miss_budget?;
        let period = extension.consumer_period?;
        if budget.validate().is_err() || period.as_nanos() == 0 {
            return None;
        }
        Some(Self {
            ring: [0; RING_WORDS],
            window: budget.window_cycles.min(RING_CAPACITY),
            max_consecutive: budget.max_consecutive,
            max_misses: budget.max_misses,
            cycles: 0,
            last_sample: None,
            period,
            contract_version: extension.contract_version,
        })
    }

    /// Die Vertragsrevision, unter der dieser Zaehler laeuft.
    ///
    /// Ein Vertragswechsel legt einen neuen Monitor an, statt den alten
    /// weiterlaufen zu lassen: Zahlen aus zwei Vertraegen in einem Fenster
    /// beschreiben keinen von beiden.
    #[must_use]
    pub const fn contract_version(&self) -> u32 {
        self.contract_version
    }

    /// Traegt das Ergebnis eines einzelnen Zyklus ein.
    pub fn record(&mut self, outcome: CycleOutcome) {
        let (word, bit) = locate(self.cycles);
        if let Some(cell) = self.ring.get_mut(word) {
            let mask = 1_u64 << bit;
            if outcome == CycleOutcome::Missed {
                *cell |= mask;
            } else {
                *cell &= !mask;
            }
        }
        self.cycles = self.cycles.saturating_add(1);
    }

    /// Traegt alle Zyklen bis `now` ein.
    ///
    /// Der Zyklus, der `now` enthaelt, bekommt `outcome`; jeder dazwischen
    /// uebersprungene Zyklus gilt als Miss. Beim ersten Aufruf wird nur der
    /// Startzeitpunkt gesetzt und ein Zyklus eingetragen — vor dem Vertrag
    /// gibt es keine Zyklen, die man nachtragen koennte.
    ///
    /// Gibt zurueck, wie viele Verbraucherzyklen dabei vergangen sind.
    pub fn advance_to(&mut self, now: Instant, outcome: CycleOutcome) -> u64 {
        let Some((last, ticks, step)) = self.elapsed_ticks(now) else {
            return self.start_at(now, outcome);
        };
        if ticks == 0 {
            return 0;
        }
        // Den letzten Rasterpunkt am Zeitpunkt erkennen und nicht an einem
        // Zaehler: bei einer Luecke, die laenger ist als der Ring, wird die
        // Bewertungsfunktion seltener aufgerufen, als Takte vergangen sind.
        let final_nanos = last
            .checked_add(Duration::from_nanos_unbounded(step.saturating_mul(ticks)))
            .map_or_else(|| now.as_nanos(), Instant::as_nanos);
        self.advance_with(now, |at| {
            if at.as_nanos() == final_nanos {
                outcome
            } else {
                CycleOutcome::Missed
            }
        })
    }

    /// Traegt alle Zyklen bis `now` ein und bewertet jeden **einzeln**.
    ///
    /// `outcome_at` bekommt den Zeitpunkt des jeweiligen Zyklus und
    /// entscheidet, ob er versorgt war. Das ist der Unterschied, auf den es
    /// bei `latest_state` ankommt: ein Ergebnis von vor 20 ms versorgt bei
    /// einem Hoechstalter von 66 ms auch den Zyklus, in dem nichts Neues
    /// ankam. [`MissWindow::advance_to`] wuerde ihn als Miss zaehlen, weil es
    /// nur den letzten Zeitpunkt kennt.
    ///
    /// Bei einer Luecke, die laenger ist als der Ring, werden nur die
    /// juengsten [`MAX_WINDOW_CYCLES`] Zyklen ausgewertet — die aelteren
    /// wuerden ohnehin sofort ueberschrieben. Gezaehlt werden sie trotzdem.
    ///
    /// Gibt zurueck, wie viele Verbraucherzyklen vergangen sind.
    pub fn advance_with(
        &mut self,
        now: Instant,
        mut outcome_at: impl FnMut(Instant) -> CycleOutcome,
    ) -> u64 {
        let Some((last, ticks, step)) = self.elapsed_ticks(now) else {
            let outcome = outcome_at(now);
            return self.start_at(now, outcome);
        };
        if ticks == 0 {
            // Noch im selben Zyklus: der zuletzt eingetragene Wert gilt
            // weiter. Ein zweiter Eintrag wuerde denselben Zyklus doppelt
            // zaehlen.
            return 0;
        }

        // Der Ring fasst begrenzt viele Zyklen. Was er nicht mehr traegt,
        // wird gezaehlt, aber nicht mehr bewertet — sonst waere die
        // Auswertung einer langen Stille unbeschraenkt teuer.
        let written = ticks.min(u64::from(RING_CAPACITY));
        self.cycles = self.cycles.saturating_add(ticks.saturating_sub(written));
        let first = ticks.saturating_sub(written).saturating_add(1);
        for k in first..=ticks {
            let offset = Duration::from_nanos_unbounded(step.saturating_mul(k));
            let at = last.checked_add(offset).unwrap_or(now);
            let outcome = outcome_at(at);
            self.record(outcome);
        }

        // Den Startpunkt auf das Takt-Raster setzen, nicht auf `now`: sonst
        // wandert der Vertragstakt mit den Ankuenften mit.
        let advanced = Duration::from_nanos_unbounded(step.saturating_mul(ticks));
        self.last_sample = last.checked_add(advanced).or(Some(now));
        ticks
    }

    /// Der erste Zyklus eines Vertrags.
    ///
    /// Vor dem Vertrag gibt es keine Zyklen, die man nachtragen koennte.
    fn start_at(&mut self, now: Instant, outcome: CycleOutcome) -> u64 {
        self.last_sample = Some(now);
        self.record(outcome);
        1
    }

    /// Letzter Rasterpunkt, Anzahl vergangener Takte und die Taktlaenge.
    ///
    /// `None`, solange noch kein Zyklus eingetragen wurde.
    fn elapsed_ticks(&self, now: Instant) -> Option<(Instant, u64, u64)> {
        let last = self.last_sample?;
        let step = self.period.as_nanos().max(1);
        let elapsed = now.saturating_since(last).as_nanos();
        Some((last, elapsed.checked_div(step).unwrap_or(0), step))
    }

    /// Wie viele Zyklen insgesamt beobachtet wurden.
    #[must_use]
    pub const fn observed_cycles(&self) -> u64 {
        self.cycles
    }

    /// Misses im letzten vollstaendigen Fenster.
    #[must_use]
    pub fn misses_in_window(&self) -> u32 {
        self.scan().0
    }

    /// Die laengste Missfolge im letzten vollstaendigen Fenster.
    #[must_use]
    pub fn longest_run(&self) -> u32 {
        self.scan().1
    }

    /// Der Befund.
    #[must_use]
    pub fn status(&self) -> WeaklyHardStatus {
        if self.cycles < u64::from(self.window) {
            return WeaklyHardStatus::Warmup {
                remaining: u32::try_from(u64::from(self.window).saturating_sub(self.cycles))
                    .unwrap_or(self.window),
            };
        }
        let (misses, longest_run) = self.scan();
        let over_budget = misses > self.max_misses;
        let over_run = self.max_consecutive.is_some_and(|l| longest_run > l);
        if over_budget || over_run {
            return WeaklyHardStatus::Violated {
                misses,
                longest_run,
            };
        }
        WeaklyHardStatus::Holding
    }

    /// Zaehlt Misses und die laengste Folge im letzten Fenster.
    fn scan(&self) -> (u32, u32) {
        let span = u32::try_from(self.cycles.min(u64::from(self.window))).unwrap_or(self.window);
        let mut misses = 0_u32;
        let mut run = 0_u32;
        let mut longest = 0_u32;
        for back in (0..span).rev() {
            // `back` Zyklen vor dem zuletzt eingetragenen.
            let index = self
                .cycles
                .saturating_sub(u64::from(back))
                .saturating_sub(1);
            if self.is_miss(index) {
                misses = misses.saturating_add(1);
                run = run.saturating_add(1);
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        (misses, longest)
    }

    /// Ob der Zyklus mit dieser laufenden Nummer ein Miss war.
    fn is_miss(&self, cycle: u64) -> bool {
        let (word, bit) = locate(cycle);
        self.ring
            .get(word)
            .is_some_and(|cell| cell & (1_u64 << bit) != 0)
    }
}

/// Wort und Bit eines Zyklus im Ring.
///
/// Nur Bitoperationen: `RING_CAPACITY` ist eine Zweierpotenz, und eine
/// Restrechnung mit potenziellem Ueberlauf hat in einem Kern nichts zu suchen,
/// der `arithmetic_side_effects` verbietet.
const fn locate(cycle: u64) -> (usize, u64) {
    let slot = cycle & RING_MASK;
    ((slot >> 6) as usize, slot & 63)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn period(ms: u64) -> Duration {
        Duration::from_millis(ms).unwrap()
    }

    fn at(ms: u64) -> Instant {
        Instant::from_nanos(ms.saturating_mul(MS))
    }

    fn budget(m: u32, k: u32, l: Option<u32>) -> ContractExtension {
        ContractExtension {
            consumer_period: Some(period(10)),
            miss_budget: Some(MissBudget {
                max_misses: m,
                window_cycles: k,
                max_consecutive: l,
            }),
            ..ContractExtension::default()
        }
    }

    #[test]
    fn the_ring_constants_agree() {
        assert_eq!(RING_WORDS.saturating_mul(64), RING_CAPACITY as usize);
        assert_eq!(RING_MASK, u64::from(RING_CAPACITY).saturating_sub(1));
        const { assert!(RING_CAPACITY >= MAX_WINDOW_CYCLES) };
    }

    // -- Gueltigkeit -------------------------------------------------------

    #[test]
    fn a_contract_without_an_extension_is_the_old_contract() {
        let default = ContractExtension::default();
        assert_eq!(default.miss_budget, None);
        assert_eq!(default.evidence_required, EvidenceLevel::Observed);
        assert_eq!(default.delivery_boundary, DeliveryBoundary::Governor);
        assert!(default.approved_variants.contains(VariantIdx(0)));
        assert!(default.validate(2).is_ok());
    }

    #[test]
    fn an_unknown_extension_version_is_rejected_not_ignored() {
        let ext = ContractExtension {
            version: 99,
            ..ContractExtension::default()
        };
        assert_eq!(
            ext.validate(1),
            Err(ExtensionError::UnsupportedVersion {
                found: 99,
                supported: CONTRACT_EXTENSION_VERSION,
            })
        );
    }

    #[test]
    fn a_window_of_zero_is_rejected() {
        assert_eq!(
            budget(0, 0, None).validate(1),
            Err(ExtensionError::ZeroWindow)
        );
    }

    #[test]
    fn a_window_beyond_the_ring_is_rejected() {
        let too_big = MAX_WINDOW_CYCLES.saturating_add(1);
        assert_eq!(
            budget(1, too_big, None).validate(1),
            Err(ExtensionError::WindowTooLarge { requested: too_big })
        );
    }

    #[test]
    fn a_budget_that_permits_every_cycle_is_rejected() {
        // M >= K heisst: jeder Zyklus darf ein Miss sein. Das ist kein
        // Vertrag, sondern seine Abwesenheit.
        assert_eq!(
            budget(100, 100, None).validate(1),
            Err(ExtensionError::MissBudgetNotBinding {
                max_misses: 100,
                window_cycles: 100,
            })
        );
        assert!(budget(99, 100, None).validate(1).is_ok());
    }

    #[test]
    fn a_consecutive_limit_above_the_budget_is_rejected() {
        assert_eq!(
            budget(2, 100, Some(3)).validate(1),
            Err(ExtensionError::ConsecutiveExceedsBudget {
                max_consecutive: 3,
                max_misses: 2,
            })
        );
        assert!(budget(2, 100, Some(2)).validate(1).is_ok());
        assert!(budget(2, 100, Some(1)).validate(1).is_ok());
    }

    #[test]
    fn a_miss_budget_without_a_consumer_tick_is_rejected() {
        let ext = ContractExtension {
            consumer_period: None,
            ..budget(2, 100, Some(1))
        };
        assert_eq!(
            ext.validate(1),
            Err(ExtensionError::MissBudgetWithoutPeriod),
            "der Vertragstakt darf nicht aus den Ankuenften kommen"
        );
    }

    #[test]
    fn a_contract_that_approves_no_variant_is_rejected() {
        let ext = ContractExtension {
            approved_variants: ApprovedVariants::none(),
            ..ContractExtension::default()
        };
        assert_eq!(ext.validate(3), Err(ExtensionError::NoApprovedVariant));
    }

    #[test]
    fn an_approval_outside_the_variant_range_does_not_count() {
        // Variante 5 freigegeben, aber es gibt nur zwei: das ist kein
        // gueltiger Vertrag, sondern ein Tippfehler mit Folgen.
        let ext = ContractExtension {
            approved_variants: ApprovedVariants::from_indices(&[VariantIdx(5)]),
            ..ContractExtension::default()
        };
        assert_eq!(ext.validate(2), Err(ExtensionError::NoApprovedVariant));
        assert!(ext.validate(6).is_ok());
    }

    #[test]
    fn an_out_of_range_background_progress_is_rejected() {
        let ext = ContractExtension {
            minimum_background_progress_pct: Some(101),
            ..ContractExtension::default()
        };
        assert_eq!(
            ext.validate(1),
            Err(ExtensionError::BackgroundProgressOutOfRange { requested: 101 })
        );
    }

    #[test]
    fn an_out_of_range_observation_window_is_rejected() {
        for value in [0, MAX_WINDOW_CYCLES.saturating_add(1)] {
            let ext = ContractExtension {
                observation_window: Some(value),
                ..ContractExtension::default()
            };
            assert_eq!(
                ext.validate(1),
                Err(ExtensionError::ObservationWindowOutOfRange { requested: value })
            );
        }
    }

    // -- Freigabe ----------------------------------------------------------

    #[test]
    fn approval_and_quality_are_different_statements() {
        // Nur Variante 1 freigegeben. Variante 0 ist qualitativ besser und
        // trotzdem unzulaessig — Freigabe ist keine Schwelle.
        let approved = ApprovedVariants::from_indices(&[VariantIdx(1)]);
        assert!(!approved.contains(VariantIdx(0)));
        assert!(approved.contains(VariantIdx(1)));
        assert!(!approved.contains(VariantIdx(2)));
    }

    #[test]
    fn an_index_beyond_the_maximum_is_never_approved() {
        let all = ApprovedVariants::all();
        assert!(all.contains(VariantIdx(
            u16::try_from(MAX_VARIANTS.saturating_sub(1)).unwrap_or(0)
        )));
        assert!(!all.contains(VariantIdx(u16::try_from(MAX_VARIANTS).unwrap_or(0))));
    }

    // -- Monitor -----------------------------------------------------------

    #[test]
    fn a_monitor_needs_a_tick_and_a_budget() {
        assert!(MissWindow::new(&ContractExtension::default()).is_none());
        let no_period = ContractExtension {
            consumer_period: None,
            ..budget(2, 10, None)
        };
        assert!(MissWindow::new(&no_period).is_none());
        assert!(MissWindow::new(&budget(2, 10, None)).is_some());
    }

    #[test]
    fn an_incomplete_window_says_warmup_not_holding() {
        let mut w = MissWindow::new(&budget(2, 10, None)).unwrap();
        for _ in 0..9 {
            w.record(CycleOutcome::Supplied);
        }
        assert_eq!(w.status(), WeaklyHardStatus::Warmup { remaining: 1 });
        w.record(CycleOutcome::Supplied);
        assert_eq!(w.status(), WeaklyHardStatus::Holding);
    }

    #[test]
    fn a_budget_is_held_at_exactly_the_limit() {
        let mut w = MissWindow::new(&budget(2, 10, None)).unwrap();
        w.record(CycleOutcome::Missed);
        for _ in 0..7 {
            w.record(CycleOutcome::Supplied);
        }
        w.record(CycleOutcome::Missed);
        w.record(CycleOutcome::Supplied);
        assert_eq!(w.misses_in_window(), 2);
        assert_eq!(w.status(), WeaklyHardStatus::Holding);
    }

    #[test]
    fn one_miss_too_many_violates() {
        let mut w = MissWindow::new(&budget(2, 10, None)).unwrap();
        for i in 0..10 {
            w.record(if i % 4 == 0 {
                CycleOutcome::Missed
            } else {
                CycleOutcome::Supplied
            });
        }
        assert_eq!(w.misses_in_window(), 3);
        assert_eq!(
            w.status(),
            WeaklyHardStatus::Violated {
                misses: 3,
                longest_run: 1,
            }
        );
    }

    #[test]
    fn the_same_miss_rate_with_a_different_burst_shape_is_distinguished() {
        // Genau der Fall, den eine reine Missrate nicht sieht: zwei Misses
        // verteilt sind fuer einen Regler etwas anderes als zwei am Stueck.
        let spread = {
            let mut w = MissWindow::new(&budget(2, 10, Some(1))).unwrap();
            for i in 0..10 {
                w.record(if i == 0 || i == 5 {
                    CycleOutcome::Missed
                } else {
                    CycleOutcome::Supplied
                });
            }
            w
        };
        let burst = {
            let mut w = MissWindow::new(&budget(2, 10, Some(1))).unwrap();
            for i in 0..10 {
                w.record(if i == 4 || i == 5 {
                    CycleOutcome::Missed
                } else {
                    CycleOutcome::Supplied
                });
            }
            w
        };
        assert_eq!(spread.misses_in_window(), burst.misses_in_window());
        assert_eq!(spread.status(), WeaklyHardStatus::Holding);
        assert_eq!(
            burst.status(),
            WeaklyHardStatus::Violated {
                misses: 2,
                longest_run: 2,
            },
            "L=1 verbietet zwei hintereinander, auch wenn M=2 sie erlaubt"
        );
    }

    #[test]
    fn the_window_slides_and_forgets() {
        let mut w = MissWindow::new(&budget(1, 5, None)).unwrap();
        for _ in 0..3 {
            w.record(CycleOutcome::Missed);
        }
        for _ in 0..2 {
            w.record(CycleOutcome::Supplied);
        }
        assert_eq!(w.misses_in_window(), 3);
        // Fuenf gute Zyklen spaeter ist das Fenster sauber.
        for _ in 0..5 {
            w.record(CycleOutcome::Supplied);
        }
        assert_eq!(w.misses_in_window(), 0);
        assert_eq!(w.status(), WeaklyHardStatus::Holding);
    }

    // -- Der Takt kommt aus dem Vertrag ------------------------------------

    #[test]
    fn silence_fills_the_window_with_misses() {
        // Der Kern der Abnahme: ein Governor, der nichts mehr liefert, darf
        // nicht dadurch gut dastehen, dass keine Zyklen entstehen.
        let mut w = MissWindow::new(&budget(2, 10, None)).unwrap();
        assert_eq!(w.advance_to(at(0), CycleOutcome::Supplied), 1);
        // 100 ms Schweigen bei 10 ms Takt: neun uebersprungene Zyklen plus
        // der versorgte, in dem wieder etwas ankam.
        assert_eq!(w.advance_to(at(100), CycleOutcome::Supplied), 10);
        assert_eq!(w.observed_cycles(), 11);
        assert_eq!(w.misses_in_window(), 9);
        assert_eq!(
            w.status(),
            WeaklyHardStatus::Violated {
                misses: 9,
                longest_run: 9,
            }
        );
    }

    #[test]
    fn rejecting_everything_does_not_satisfy_the_contract() {
        let mut w = MissWindow::new(&budget(2, 10, None)).unwrap();
        for cycle in 0..20_u32 {
            w.advance_to(
                at(u64::from(cycle).saturating_mul(10)),
                CycleOutcome::Missed,
            );
        }
        assert_eq!(w.misses_in_window(), 10);
        assert!(matches!(w.status(), WeaklyHardStatus::Violated { .. }));
    }

    #[test]
    fn two_arrivals_within_one_cycle_count_once() {
        let mut w = MissWindow::new(&budget(2, 10, None)).unwrap();
        assert_eq!(w.advance_to(at(0), CycleOutcome::Supplied), 1);
        assert_eq!(
            w.advance_to(at(4), CycleOutcome::Supplied),
            0,
            "derselbe Zyklus wird nicht zweimal gezaehlt"
        );
        assert_eq!(w.observed_cycles(), 1);
    }

    #[test]
    fn the_tick_does_not_drift_with_the_arrivals() {
        // Ankuenfte kommen spaet, aber der Takt bleibt am Raster: nach zehn
        // Zyklen zu je 10 ms sind zehn Zyklen vergangen, nicht neun.
        let mut w = MissWindow::new(&budget(9, 10, None)).unwrap();
        w.advance_to(at(0), CycleOutcome::Supplied);
        for cycle in 1..10_u64 {
            // Jeweils 3 ms nach dem Rasterpunkt.
            w.advance_to(
                at(cycle.saturating_mul(10).saturating_add(3)),
                CycleOutcome::Supplied,
            );
        }
        assert_eq!(
            w.observed_cycles(),
            10,
            "ein mitwandernder Takt haette hier weniger Zyklen gezaehlt"
        );
    }

    #[test]
    fn a_gap_longer_than_the_ring_does_not_overflow() {
        let mut w = MissWindow::new(&budget(2, 10, None)).unwrap();
        w.advance_to(at(0), CycleOutcome::Supplied);
        // Zwei Stunden Schweigen bei 10 ms Takt: weit mehr Zyklen als der
        // Ring fasst. Das darf saettigen, nicht ueberlaufen.
        assert_eq!(w.advance_to(at(7_200_000), CycleOutcome::Supplied), 720_000);
        assert_eq!(
            w.observed_cycles(),
            720_001,
            "vergangene Zyklen werden gezaehlt, auch wenn der Ring sie nicht mehr traegt"
        );
        assert_eq!(
            w.misses_in_window(),
            9,
            "das Fenster ist zehn Zyklen lang; neun davon sind Misses, der letzte war versorgt"
        );
        assert!(matches!(w.status(), WeaklyHardStatus::Violated { .. }));
    }

    // -- Anforderung und Beobachtung -----------------------------------

    #[test]
    fn the_required_evidence_level_is_not_derived_from_observations() {
        let ext = ContractExtension {
            evidence_required: EvidenceLevel::QualifiedSlo,
            ..budget(2, 10, None)
        };
        let mut w = MissWindow::new(&ext).unwrap();
        for _ in 0..20 {
            w.record(CycleOutcome::Supplied);
        }
        assert_eq!(w.status(), WeaklyHardStatus::Holding);
        assert_eq!(
            ext.evidence_required,
            EvidenceLevel::QualifiedSlo,
            "ein guter Lauf hebt keine Nachweisstufe an"
        );
    }

    #[test]
    fn evidence_levels_are_ordered_from_weak_to_strong() {
        assert!(EvidenceLevel::Observed < EvidenceLevel::QualifiedSlo);
        assert!(EvidenceLevel::QualifiedSlo < EvidenceLevel::Proven);
    }

    #[test]
    fn a_monitor_carries_the_contract_revision_it_counts_under() {
        let ext = ContractExtension {
            contract_version: 7,
            ..budget(2, 10, None)
        };
        assert_eq!(MissWindow::new(&ext).unwrap().contract_version(), 7);
    }
}
