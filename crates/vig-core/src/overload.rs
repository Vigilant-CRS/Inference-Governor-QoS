//! Ueberlastzustaende und kontrollierte Degradation (Spec 14, WP5).
//!
//! Ohne Ueberlaststeuerung wird unter Last **alles** gleichzeitig langsamer und
//! unvorhersehbarer. Ziel ist das Gegenteil: zuerst veraltete und weniger
//! wichtige Arbeit entfernen, dann Varianten verkleinern, dann Best-Effort
//! abweisen — und erst zuletzt nur noch geschuetzte Arbeit bedienen.
//!
//! ## Warum ein Fenster und nicht ein Messwert
//!
//! Ein Zustandswechsel auf Basis einer Momentaufnahme erzeugt Pendeln: ein
//! einzelner verspaeteter Frame wuerde das System degradieren, der naechste
//! puenktliche es sofort zurueckholen. Der Druck wird deshalb ueber ein
//! gleitendes Fenster gemessen, und Ein- und Ausstiegsschwellen sind getrennt
//! (Spec 14.3). Zusaetzlich gilt eine Mindestverweildauer je Zustand.

use crate::request::Criticality;
use crate::time::{Duration, Instant};

/// Anzahl der Buckets des gleitenden Fensters.
const BUCKETS: usize = 16;

/// Die Ueberlastzustaende (Spec 14.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum OverloadState {
    /// Hoechste machbare Qualitaet, Best-Effort erlaubt.
    #[default]
    Normal,
    /// Aggressiveres Superseding, konservativere Variantenaufwertung.
    FreshnessPressure,
    /// Kleinere Varianten zulaessig bzw. erzwungen.
    Degraded,
    /// Keine neue Best-Effort-Arbeit annehmen.
    RejectBestEffort,
    /// Nur noch Protected und High.
    ProtectedOnly,
}

impl OverloadState {
    /// Die Stufe, null fuer [`OverloadState::Normal`].
    #[must_use]
    pub const fn level(self) -> usize {
        match self {
            Self::Normal => 0,
            Self::FreshnessPressure => 1,
            Self::Degraded => 2,
            Self::RejectBestEffort => 3,
            Self::ProtectedOnly => 4,
        }
    }

    /// Der Zustand einer Stufe.
    #[must_use]
    pub const fn from_level(level: usize) -> Self {
        match level {
            0 => Self::Normal,
            1 => Self::FreshnessPressure,
            2 => Self::Degraded,
            3 => Self::RejectBestEffort,
            _ => Self::ProtectedOnly,
        }
    }

    /// Wahr, wenn in diesem Zustand neue Arbeit dieser Klasse angenommen wird.
    #[must_use]
    pub const fn admits(self, criticality: Criticality) -> bool {
        match self {
            Self::Normal | Self::FreshnessPressure | Self::Degraded => true,
            Self::RejectBestEffort => !matches!(criticality, Criticality::BestEffort),
            Self::ProtectedOnly => criticality.is_guarded(),
        }
    }

    /// Wahr, wenn Varianten aktiv abgewertet werden sollen (Spec 14.2).
    #[must_use]
    pub const fn forces_degradation(self) -> bool {
        self.level() >= Self::Degraded.level()
    }

    /// Wahr, wenn wartende Arbeit aggressiver supersediert werden soll.
    #[must_use]
    pub const fn aggressive_supersession(self) -> bool {
        self.level() >= Self::FreshnessPressure.level()
    }
}

impl core::fmt::Display for OverloadState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::Normal => "NORMAL",
            Self::FreshnessPressure => "FRESHNESS_PRESSURE",
            Self::Degraded => "DEGRADED",
            Self::RejectBestEffort => "REJECT_BEST_EFFORT",
            Self::ProtectedOnly => "PROTECTED_ONLY",
        };
        f.write_str(s)
    }
}

/// Eine Beobachtung, die in die Druckmessung eingeht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PressureSample {
    /// Wahr, wenn die Beobachtung eine geschuetzte Klasse betrifft.
    ///
    /// Nur geschuetzte Arbeit erzeugt Ueberlastdruck. Ein abgewiesener
    /// Best-Effort-Job ist die **beabsichtigte** Wirkung des Systems und darf
    /// es nicht weiter eskalieren lassen.
    pub guarded: bool,
    /// Wahr, wenn der Vertrag verletzt wurde: Deadline verfehlt, als
    /// unmachbar abgelehnt oder wegen Ueberalterung verworfen.
    pub violated: bool,
}

/// Warum eine Ueberlastkonfiguration unzulaessig ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverloadConfigError {
    /// Eine Rueckkehrschwelle liegt nicht unter ihrer Eintrittsschwelle.
    ///
    /// Ohne echte Hysterese pendelt der Zustand (Spec 14.3). Gleiche Schwellen
    /// sind deshalb ebenfalls unzulaessig, nicht nur hoehere.
    NoHysteresis {
        /// Die betroffene Stufe.
        level: usize,
        /// Die Eintrittsschwelle in Promille.
        enter: u16,
        /// Die Rueckkehrschwelle in Promille.
        exit: u16,
    },
    /// Die Eintrittsschwellen steigen nicht mit der Stufe.
    ThresholdsNotMonotonic {
        /// Die betroffene Stufe.
        level: usize,
    },
    /// Fensterlaenge oder Mindestverweildauer ist null.
    ZeroDuration,
}

impl core::fmt::Display for OverloadConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoHysteresis { level, enter, exit } => write!(
                f,
                "Stufe {level}: Rueckkehrschwelle {exit} permille muss unter der \
                 Eintrittsschwelle {enter} permille liegen"
            ),
            Self::ThresholdsNotMonotonic { level } => {
                write!(
                    f,
                    "Eintrittsschwelle der Stufe {level} liegt nicht ueber der vorherigen"
                )
            }
            Self::ZeroDuration => write!(f, "Fenster und Mindestverweildauer muessen > 0 sein"),
        }
    }
}

impl core::error::Error for OverloadConfigError {}

/// Die Schwellen und Zeiten der Ueberlaststeuerung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverloadConfig {
    /// Laenge des gleitenden Messfensters.
    pub window: Duration,
    /// Mindestverweildauer in einem Zustand vor dem naechsten Wechsel.
    pub min_dwell: Duration,
    /// Eintrittsschwellen in Promille fuer die Stufen 1 bis 4.
    pub enter: [u16; 4],
    /// Rueckkehrschwellen in Promille fuer die Stufen 1 bis 4.
    pub exit: [u16; 4],
}

impl OverloadConfig {
    /// Prueft die Konfiguration.
    ///
    /// # Errors
    ///
    /// Siehe [`OverloadConfigError`].
    pub fn validate(&self) -> Result<(), OverloadConfigError> {
        if self.window.as_nanos() == 0 || self.min_dwell.as_nanos() == 0 {
            return Err(OverloadConfigError::ZeroDuration);
        }
        let mut previous: Option<u16> = None;
        for (i, (enter, exit)) in self.enter.iter().zip(self.exit.iter()).enumerate() {
            let level = i.saturating_add(1);
            if exit >= enter {
                return Err(OverloadConfigError::NoHysteresis {
                    level,
                    enter: *enter,
                    exit: *exit,
                });
            }
            if let Some(prev) = previous
                && *enter <= prev
            {
                return Err(OverloadConfigError::ThresholdsNotMonotonic { level });
            }
            previous = Some(*enter);
        }
        Ok(())
    }
}

impl Default for OverloadConfig {
    /// Ein Satz Defaults, der Spec 14.3 erfuellt: die Rueckkehrschwelle liegt
    /// jeweils deutlich unter der Eintrittsschwelle, nicht knapp darunter.
    fn default() -> Self {
        Self {
            window: Duration::from_nanos_unbounded(1_000_000_000),
            min_dwell: Duration::from_nanos_unbounded(200_000_000),
            enter: [50, 100, 200, 400],
            exit: [20, 50, 100, 200],
        }
    }
}

/// Gleitendes Fenster ueber Druckbeobachtungen.
#[derive(Debug, Clone)]
struct Window {
    bucket_span: Duration,
    bucket_start: Instant,
    index: usize,
    total: [u32; BUCKETS],
    violated: [u32; BUCKETS],
}

impl Window {
    fn new(span: Duration, start: Instant) -> Self {
        let bucket_span = Duration::from_nanos_unbounded(
            span.as_nanos()
                .div_ceil(u64::try_from(BUCKETS).unwrap_or(1))
                .max(1),
        );
        Self {
            bucket_span,
            bucket_start: start,
            index: 0,
            total: [0; BUCKETS],
            violated: [0; BUCKETS],
        }
    }

    /// Rotiert das Fenster bis `now` und loescht dabei veraltete Buckets.
    fn advance(&mut self, now: Instant) {
        let mut guard = 0;
        while now.saturating_since(self.bucket_start) >= self.bucket_span && guard < BUCKETS {
            self.index = self.index.saturating_add(1) % BUCKETS;
            if let Some(t) = self.total.get_mut(self.index) {
                *t = 0;
            }
            if let Some(v) = self.violated.get_mut(self.index) {
                *v = 0;
            }
            self.bucket_start = self
                .bucket_start
                .checked_add(self.bucket_span)
                .unwrap_or(now);
            guard = guard.saturating_add(1);
        }
        // Eine lange Pause ohne Beobachtungen leert das Fenster vollstaendig,
        // statt alten Druck zu konservieren.
        if guard >= BUCKETS {
            self.total = [0; BUCKETS];
            self.violated = [0; BUCKETS];
            self.bucket_start = now;
        }
    }

    fn record(&mut self, violated: bool) {
        if let Some(t) = self.total.get_mut(self.index) {
            *t = t.saturating_add(1);
        }
        if violated && let Some(v) = self.violated.get_mut(self.index) {
            *v = v.saturating_add(1);
        }
    }

    fn pressure_permille(&self) -> u16 {
        let total: u64 = self.total.iter().map(|v| u64::from(*v)).sum();
        if total == 0 {
            return 0;
        }
        let bad: u64 = self.violated.iter().map(|v| u64::from(*v)).sum();
        let permille = bad.saturating_mul(1_000).checked_div(total).unwrap_or(0);
        u16::try_from(permille).unwrap_or(u16::MAX)
    }
}

/// Die Ueberlast-Zustandsmaschine.
#[derive(Debug, Clone)]
pub struct OverloadController {
    config: OverloadConfig,
    state: OverloadState,
    since: Instant,
    window: Window,
}

impl OverloadController {
    /// Erzeugt eine Zustandsmaschine im Zustand [`OverloadState::Normal`].
    ///
    /// # Errors
    ///
    /// Reicht [`OverloadConfig::validate`] durch.
    pub fn new(config: OverloadConfig, start: Instant) -> Result<Self, OverloadConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            state: OverloadState::Normal,
            since: start,
            window: Window::new(config.window, start),
        })
    }

    /// Der aktuelle Zustand.
    #[must_use]
    pub const fn state(&self) -> OverloadState {
        self.state
    }

    /// Setzt den Zustand von aussen.
    ///
    /// Nur fuer Tests und den Simulator. Der Regler bleibt sonst die einzige
    /// Quelle dieses Zustands: eine Stufe, die von zwei Stellen gesetzt wird,
    /// ist in einem Fehlerfall nicht mehr erklaerbar.
    pub const fn force(&mut self, state: OverloadState) {
        self.state = state;
    }

    /// Der gemessene Druck in Promille.
    #[must_use]
    pub fn pressure_permille(&self) -> u16 {
        self.window.pressure_permille()
    }

    /// Nimmt eine Beobachtung auf.
    ///
    /// Nur geschuetzte Beobachtungen gehen ein: abgewiesene Best-Effort-Arbeit
    /// ist die beabsichtigte Wirkung und darf den Zustand nicht weiter
    /// eskalieren lassen.
    pub fn observe(&mut self, now: Instant, sample: PressureSample) {
        self.window.advance(now);
        if sample.guarded {
            self.window.record(sample.violated);
        }
    }

    /// Wertet den Zustand neu aus und gibt ihn zurueck.
    ///
    /// Wechselt hoechstens eine Stufe je Aufruf und nur, wenn die
    /// Mindestverweildauer abgelaufen ist. Beides verhindert Spruenge und
    /// Pendeln (Spec 14.3).
    pub fn evaluate(&mut self, now: Instant) -> OverloadState {
        self.window.advance(now);
        if now.saturating_since(self.since) < self.config.min_dwell {
            return self.state;
        }
        let pressure = self.window.pressure_permille();
        let level = self.state.level();

        if level < 4 && self.config.enter.get(level).is_some_and(|t| pressure >= *t) {
            self.transition(OverloadState::from_level(level.saturating_add(1)), now);
        } else if level > 0
            && self
                .config
                .exit
                .get(level.saturating_sub(1))
                .is_some_and(|t| pressure <= *t)
        {
            self.transition(OverloadState::from_level(level.saturating_sub(1)), now);
        }
        self.state
    }

    fn transition(&mut self, next: OverloadState, now: Instant) {
        if next != self.state {
            self.state = next;
            self.since = now;
        }
    }
}
