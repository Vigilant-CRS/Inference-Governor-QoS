//! Zustandsabhaengige Laufzeitprognose als begrenzte Nachschlagepolicy
//! (NV-06).
//!
//! ## Was der bisherige Schaetzer nicht kann
//!
//! [`RuntimeEstimator`](crate::estimator::RuntimeEstimator) fuehrt je Modell,
//! Variante und Slot-Belegungsgrad ein gleitendes Fenster und plant mit
//! `max(offline_p99, online_p95)`. Er kann die Planung damit nur
//! **verschaerfen** — was sicher ist, aber eine Sorte Wissen verschenkt:
//!
//! Die Zahlen dieses Projekts sind auf einer Karte entstanden, die unter
//! einem Leistungslimit lief (1830 statt 2100 MHz). Laeuft dieselbe Karte
//! spaeter ohne Limit, ist das Profil-p99 dauerhaft zu pessimistisch — und
//! der Governor lehnt Arbeit ab, die gepasst haette. Umgekehrt gilt ein unter
//! vollem Takt gemessenes Profil nicht mehr, sobald gedrosselt wird.
//!
//! ## Die Regel dieses Moduls
//!
//! **Zellen werden nie ueber Zustandsgrenzen gemischt.** Jede Beobachtung
//! traegt die [`StateEpoch`], unter der sie entstand: Profilidentitaet
//! (NV-03) plus Zustandsklasse (NV-04). Wechselt die Epoche, wird die Zelle
//! **geleert**, nicht fortgeschrieben. Ein Mittelwert ueber zwei Maschinen
//! beschreibt keine.
//!
//! **Es wird nichts interpoliert und nichts verteilt.** Keine Zelle bedeutet
//! keine Aussage, nicht eine geschaetzte. Zwischen zwei Zellen liegt kein
//! Wert, sondern eine Wissensluecke, und die wird als solche behandelt.
//!
//! **Erst Schatten, dann scharf.** Im [`Mode::Shadow`] entscheidet weiter der
//! alte Schaetzer; die Prognose v2 wird nur mitgeschrieben. Der Betreiber
//! sieht damit vor der Umstellung, ob v2 tatsaechlich besser plant — oder ob
//! es nur mehr ablehnt, was auch jede kaputte Policy schafft.
//!
//! **Der Rueckfall ist der alte Schaetzer, nicht ein Ratewert.** Fehlt die
//! Telemetrie, ist die Zelle zu duenn oder wechselt der Zustand zwischen
//! Planung und Dispatch, gilt `max(offline_p99, online_p95)` wie bisher.

use crate::ids::{ModelIdx, VariantIdx};
use crate::time::Duration;

/// Wie stark die Karte gerade gebremst wird.
///
/// Bewusst zwei Stufen und nicht fuenf: jede zusaetzliche Stufe halbiert die
/// Messwerte je Zelle. Ein Zustandsraum, der feiner ist als die Datenlage,
/// erzeugt viele Zellen, die alle nichts aussagen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThrottleClass {
    /// Kein laufzeitwirksamer Drosselgrund.
    #[default]
    Nominal,
    /// Mindestens ein laufzeitwirksamer Drosselgrund liegt an.
    Limited,
}

/// Wie nah die Karte an ihrem Maximaltakt laeuft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClockClass {
    /// Innerhalb von zehn Prozent des Maximaltakts.
    Full,
    /// Deutlich darunter.
    Reduced,
    /// Nicht beobachtbar.
    ///
    /// Ein eigener Wert und nicht `Full`, und die **Voreinstellung**:
    /// „unbekannt" ist keine Aussage ueber den Takt, und sie als die
    /// guenstigste zu lesen waere die gefaehrlichste Auslegung. Ein Governor
    /// ohne Hardwarebeobachtung soll deshalb gar keine Prognose bekommen und
    /// nicht die beste.
    #[default]
    Unknown,
}

impl ClockClass {
    /// Die Klasse aus aktuellem und maximalem Takt.
    ///
    /// `None` fuer eine der beiden Groessen heisst [`ClockClass::Unknown`] —
    /// aus einer fehlenden Zahl wird keine Klasse geraten.
    #[must_use]
    pub fn from_mhz(current: Option<u32>, max: Option<u32>) -> Self {
        let (Some(current), Some(max)) = (current, max) else {
            return Self::Unknown;
        };
        if max == 0 {
            return Self::Unknown;
        }
        // Zehn Prozent unter dem Maximum ist die Grenze: darunter aendert
        // sich die Laufzeit messbar, darueber liegt es im Rauschen der
        // Boost-Regelung.
        let threshold = max.saturating_mul(90).checked_div(100).unwrap_or(max);
        if current >= threshold {
            Self::Full
        } else {
            Self::Reduced
        }
    }
}

/// Die Zustandsklasse, unter der gemessen oder geplant wird.
///
/// Diskret, klein und vollstaendig aufzaehlbar. Der Slot-Belegungsgrad
/// gehoert dazu, weil er der Ersatz fuer die Interferenzmatrix ist
/// (ADR-0006); Drossel- und Taktklasse kommen aus der Hardwarebeobachtung
/// (NV-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StateClass {
    /// Wie viele Slots belegt sind.
    pub occupancy: u8,
    /// Ob laufzeitwirksam gedrosselt wird.
    pub throttle: ThrottleClass,
    /// Wie nah am Maximaltakt.
    pub clock: ClockClass,
}

impl StateClass {
    /// Wie viele unterscheidbare Klassen es je Belegungsgrad gibt.
    ///
    /// Zwei Drossel- mal drei Taktklassen. Die Zahl steht hier, damit das
    /// Speicherbudget nachrechenbar ist und nicht geschaetzt werden muss.
    pub const VARIANTS_PER_OCCUPANCY: usize = 6;

    /// Eine kompakte, stabile Kennzahl dieser Klasse.
    ///
    /// Geht in die [`StateEpoch`] ein. Stabil ueber Prozessstarts, weil sie
    /// aus den Diskriminanten und nicht aus Zeigern gebildet wird.
    #[must_use]
    pub const fn code(self) -> u32 {
        let throttle = match self.throttle {
            ThrottleClass::Nominal => 0_u32,
            ThrottleClass::Limited => 1,
        };
        let clock = match self.clock {
            ClockClass::Full => 0_u32,
            ClockClass::Reduced => 1,
            ClockClass::Unknown => 2,
        };
        // occupancy < 256, throttle < 2, clock < 3 — passt ohne Ueberlauf.
        (self.occupancy as u32)
            .wrapping_mul(6)
            .wrapping_add(throttle.wrapping_mul(3))
            .wrapping_add(clock)
    }

    /// Ob dieser Zustand ueberhaupt eine belastbare Aussage zulaesst.
    ///
    /// Ein unbekannter Takt macht die Klasse nicht wertlos — Messungen unter
    /// „Takt unbekannt" sind untereinander vergleichbar — aber er verbietet,
    /// aus ihnen eine **mutigere** Planung abzuleiten. Genau das prueft
    /// [`Predictor::predict`].
    #[must_use]
    pub const fn is_fully_observed(self) -> bool {
        !matches!(self.clock, ClockClass::Unknown)
    }
}

/// Die Epoche, unter der eine Beobachtung entstand.
///
/// Zwei Beobachtungen gehoeren nur dann in dieselbe Zelle, wenn beide
/// Bestandteile uebereinstimmen: dasselbe Profil (dieselbe Runtime, dasselbe
/// Artefakt, dasselbe Geraet — NV-03) **und** dieselbe Zustandsklasse
/// (NV-04). Weicht eines ab, ist es eine andere Maschine oder ein anderer
/// Betriebspunkt, und die alten Werte gelten nicht mehr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StateEpoch {
    /// Die Revision der Profilidentitaet.
    pub profile_revision: u32,
    /// Die Kennzahl der Zustandsklasse.
    pub state_code: u32,
}

impl StateEpoch {
    /// Die Epoche zu Profilrevision und Zustand.
    #[must_use]
    pub const fn new(profile_revision: u32, state: StateClass) -> Self {
        Self {
            profile_revision,
            state_code: state.code(),
        }
    }
}

/// Wie die Prognose v2 wirkt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Sie wird nur mitgeschrieben; entschieden wird wie bisher.
    #[default]
    Shadow,
    /// Sie entscheidet mit.
    Active,
}

/// Warum eine Prognose nicht galt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Es gibt keine Zelle zu diesem Zustand.
    NoCell,
    /// Die Zelle hat zu wenige Messwerte.
    TooFewObservations {
        /// Wie viele es sind.
        got: usize,
        /// Wie viele noetig waeren.
        needed: usize,
    },
    /// Die Zelle stammt aus einer anderen Epoche.
    WrongEpoch,
    /// Der Zustand ist nicht vollstaendig beobachtet.
    StateNotObserved,
    /// Der Zustand hat sich zwischen Planung und Dispatch geaendert.
    StateChanged,
}

/// Das Ergebnis einer Nachschlage-Prognose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prediction {
    /// Eine belegte Aussage aus einer gueltigen Zelle.
    FromCell {
        /// Der Wert.
        runtime: Duration,
        /// Die Epoche, unter der er gilt.
        epoch: StateEpoch,
        /// Wie viele Messwerte ihn stuetzen.
        observations: usize,
    },
    /// Keine Aussage; der Aufrufer nimmt den bisherigen Weg.
    Fallback {
        /// Warum.
        reason: Rejection,
    },
}

impl Prediction {
    /// Der Wert, falls es einen gibt.
    #[must_use]
    pub const fn runtime(&self) -> Option<Duration> {
        match self {
            Self::FromCell { runtime, .. } => Some(*runtime),
            Self::Fallback { .. } => None,
        }
    }

    /// Ob die Prognose in dieser Epoche noch gilt.
    ///
    /// Zwischen Planung und Dispatch kann die Karte in eine Drosselung
    /// gelaufen sein. Eine Prognose aus der Zeit davor ist dann nicht falsch,
    /// sondern unzustaendig.
    ///
    /// # Errors
    ///
    /// [`Rejection::StateChanged`], wenn die Epoche gewechselt hat; sonst der
    /// Grund, aus dem es schon bei der Planung keine Aussage gab.
    pub fn still_valid_in(&self, now: StateEpoch) -> Result<Duration, Rejection> {
        match self {
            Self::FromCell { runtime, epoch, .. } if *epoch == now => Ok(*runtime),
            Self::FromCell { .. } => Err(Rejection::StateChanged),
            Self::Fallback { reason } => Err(*reason),
        }
    }
}

/// Wie oft die Prognose v2 mit dem alten Weg uebereinstimmte.
///
/// Der Zweck ist die Frage, die vor jeder Umstellung steht: **schlaegt v2 den
/// alten Weg, oder lehnt es nur mehr ab?** Eine Policy, die alles ablehnt,
/// haelt jede Zusage ein und ist trotzdem wertlos. Deshalb wird getrennt
/// gezaehlt, wie oft v2 mutiger und wie oft es vorsichtiger war.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShadowLedger {
    /// Wie oft ueberhaupt verglichen wurde.
    pub comparisons: u64,
    /// Wie oft v2 keine Aussage hatte.
    pub fallbacks: u64,
    /// Wie oft v2 mehr Zeit veranschlagte als der alte Weg.
    pub more_conservative: u64,
    /// Wie oft v2 weniger Zeit veranschlagte.
    pub more_optimistic: u64,
    /// Die groesste Abweichung nach oben, in Mikrosekunden.
    pub largest_increase_us: u64,
    /// Die groesste Abweichung nach unten, in Mikrosekunden.
    pub largest_decrease_us: u64,
}

impl ShadowLedger {
    /// Vergleicht eine Prognose mit dem bisherigen Wert.
    pub fn compare(&mut self, legacy: Duration, prediction: &Prediction) {
        self.comparisons = self.comparisons.saturating_add(1);
        let Some(v2) = prediction.runtime() else {
            self.fallbacks = self.fallbacks.saturating_add(1);
            return;
        };
        let (a, b) = (legacy.as_micros(), v2.as_micros());
        if b > a {
            self.more_conservative = self.more_conservative.saturating_add(1);
            self.largest_increase_us = self.largest_increase_us.max(b.saturating_sub(a));
        } else if b < a {
            self.more_optimistic = self.more_optimistic.saturating_add(1);
            self.largest_decrease_us = self.largest_decrease_us.max(a.saturating_sub(b));
        }
    }

    /// Wie oft v2 ueberhaupt etwas zu sagen hatte.
    #[must_use]
    pub const fn decisive(&self) -> u64 {
        self.comparisons.saturating_sub(self.fallbacks)
    }
}

/// Die Obergrenze fuer die Zellenzahl.
///
/// Modelle mal Varianten mal Belegungsgrade mal Zustandsklassen — der Wert,
/// den die Tabelle **nie** ueberschreitet. Belegt wird sie nach der
/// tatsaechlichen Form der Konfiguration ([`Predictor::with_shape`]): ein
/// Aufbau mit vier Modellen und einer Variante braucht 24 Zellen und nicht
/// zwoelftausend. Ein Zustandsraum, der mit jeder neuen Groesse waechst, ist
/// die haeufigste Art, sich eine unbeschraenkte Datenstruktur einzuhandeln
/// (Spec L-003).
pub const MAX_CELLS: usize = crate::ids::MAX_MODELS
    * crate::ids::MAX_VARIANTS
    * crate::ids::MAX_SLOTS
    * StateClass::VARIANTS_PER_OCCUPANCY;

/// Mindestzahl Messwerte, bevor eine Zelle eine mutigere Planung stuetzt.
///
/// Deutlich hoeher als [`crate::estimator::MIN_OBSERVATIONS`]: eine Zelle zu
/// **verschaerfen** darf auf duennem Wissen geschehen, eine Zusage zu
/// **lockern** nicht. Die Asymmetrie ist dieselbe wie bei der Marge.
pub const MIN_OBSERVATIONS_TO_RELAX: usize = 48;

/// Eine Zelle des Vorhersagers.
#[derive(Debug, Clone, Copy)]
struct Cell {
    epoch: StateEpoch,
    /// Ob diese Zelle je beschrieben wurde.
    used: bool,
    samples: [u32; WINDOW],
    len: usize,
    next: usize,
    p95_micros: u32,
}

/// Groesse des Fensters je Zelle.
///
/// Muss mindestens [`MIN_OBSERVATIONS_TO_RELAX`] fassen — ein Fenster, das
/// die geforderte Belegzahl gar nicht erreichen kann, wuerde jede Zelle
/// dauerhaft unbrauchbar machen.
const WINDOW: usize = 64;

impl Default for Cell {
    fn default() -> Self {
        Self {
            epoch: StateEpoch::default(),
            used: false,
            samples: [0; WINDOW],
            len: 0,
            next: 0,
            p95_micros: 0,
        }
    }
}

impl Cell {
    /// Nimmt einen Messwert auf; leert die Zelle bei Epochenwechsel.
    ///
    /// Geleert und nicht fortgeschrieben: die alten Werte stammen aus einem
    /// anderen Zustand, und ein gleitendes Fenster wuerde sie noch dutzende
    /// Messungen lang mitschleppen.
    fn record(&mut self, epoch: StateEpoch, micros: u32) {
        if !self.used || self.epoch != epoch {
            self.epoch = epoch;
            self.used = true;
            self.len = 0;
            self.next = 0;
            self.p95_micros = 0;
        }
        if let Some(slot) = self.samples.get_mut(self.next) {
            *slot = micros;
        }
        self.next = self.next.saturating_add(1) % WINDOW;
        self.len = self.len.saturating_add(1).min(WINDOW);
        self.recompute();
    }

    fn recompute(&mut self) {
        let mut sorted = [0_u32; WINDOW];
        let len = self.len.min(WINDOW);
        for i in 0..len {
            if let (Some(dst), Some(src)) = (sorted.get_mut(i), self.samples.get(i)) {
                *dst = *src;
            }
        }
        let Some(slice) = sorted.get_mut(..len) else {
            return;
        };
        slice.sort_unstable();
        let index = len
            .saturating_mul(95)
            .checked_div(100)
            .unwrap_or(0)
            .min(len.saturating_sub(1));
        self.p95_micros = slice.get(index).copied().unwrap_or(0);
    }
}

/// Die zustandsabhaengige Nachschlagepolicy.
#[derive(Debug)]
pub struct Predictor {
    models: usize,
    variants: usize,
    slots: usize,
    cells: Vec<Cell>,
    mode: Mode,
    ledger: ShadowLedger,
}

impl Default for Predictor {
    fn default() -> Self {
        Self::new()
    }
}

impl Predictor {
    /// Ein Vorhersager fuer die volle Ausbaustufe.
    ///
    /// Fuer Tests und fuer den Fall, dass die Form nicht bekannt ist. Im
    /// Betrieb ist [`Predictor::with_shape`] die richtige Wahl: die Tabelle
    /// soll so gross sein wie die Konfiguration, nicht wie ihr Maximum.
    #[must_use]
    pub fn new() -> Self {
        Self::with_shape(
            crate::ids::MAX_MODELS,
            crate::ids::MAX_VARIANTS,
            crate::ids::MAX_SLOTS,
        )
    }

    /// Ein leerer Vorhersager im Schattenbetrieb, passend zur Konfiguration.
    ///
    /// Die Tabelle wird einmal beim Start belegt und danach nie wieder. Die
    /// Werte werden auf die Maxima des Kerns gedeckelt.
    #[must_use]
    pub fn with_shape(models: usize, variants: usize, slots: usize) -> Self {
        let models = models.clamp(1, crate::ids::MAX_MODELS);
        let variants = variants.clamp(1, crate::ids::MAX_VARIANTS);
        let slots = slots.clamp(1, crate::ids::MAX_SLOTS);
        let cells = models
            .saturating_mul(variants)
            .saturating_mul(slots)
            .saturating_mul(StateClass::VARIANTS_PER_OCCUPANCY)
            .min(MAX_CELLS);
        Self {
            models,
            variants,
            slots,
            cells: vec![Cell::default(); cells],
            mode: Mode::Shadow,
            ledger: ShadowLedger::default(),
        }
    }

    /// Wie viele Zellen die Tabelle fasst.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cells.len()
    }

    /// Schaltet die Betriebsart.
    pub const fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Die Betriebsart.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Der Schattenvergleich.
    #[must_use]
    pub const fn ledger(&self) -> &ShadowLedger {
        &self.ledger
    }

    /// Der Schattenvergleich, veraenderlich.
    ///
    /// Fuer Aufrufer, die den Vergleich auf der Schreibseite fuehren wollen,
    /// statt ihn an [`Predictor::effective`] zu koppeln.
    pub const fn ledger_mut(&mut self) -> &mut ShadowLedger {
        &mut self.ledger
    }

    /// Wie viele Zellen belegt sind.
    ///
    /// Die Zahl ist durch [`MAX_CELLS`] gedeckelt; sie waechst nicht mit der
    /// Laufzeit.
    #[must_use]
    pub fn occupied_cells(&self) -> usize {
        self.cells.iter().filter(|c| c.used).count()
    }

    fn index(&self, model: ModelIdx, variant: VariantIdx, state: StateClass) -> Option<usize> {
        let m = model.get();
        let v = variant.get();
        let o = usize::from(state.occupancy);
        if m >= self.models || v >= self.variants || o >= self.slots {
            return None;
        }
        let within = match (state.throttle, state.clock) {
            (ThrottleClass::Nominal, ClockClass::Full) => 0_usize,
            (ThrottleClass::Nominal, ClockClass::Reduced) => 1,
            (ThrottleClass::Nominal, ClockClass::Unknown) => 2,
            (ThrottleClass::Limited, ClockClass::Full) => 3,
            (ThrottleClass::Limited, ClockClass::Reduced) => 4,
            (ThrottleClass::Limited, ClockClass::Unknown) => 5,
        };
        m.checked_mul(self.variants)?
            .checked_add(v)?
            .checked_mul(self.slots)?
            .checked_add(o)?
            .checked_mul(StateClass::VARIANTS_PER_OCCUPANCY)?
            .checked_add(within)
    }

    /// Nimmt eine beobachtete Laufzeit unter dieser Epoche auf.
    pub fn record(
        &mut self,
        model: ModelIdx,
        variant: VariantIdx,
        state: StateClass,
        profile_revision: u32,
        observed: Duration,
    ) {
        let Some(index) = self.index(model, variant, state) else {
            return;
        };
        let epoch = StateEpoch::new(profile_revision, state);
        let micros = u32::try_from(observed.as_micros()).unwrap_or(u32::MAX);
        if let Some(cell) = self.cells.get_mut(index) {
            cell.record(epoch, micros);
        }
    }

    /// Wie viele Messwerte diese Zelle in dieser Epoche traegt.
    #[must_use]
    pub fn observations(
        &self,
        model: ModelIdx,
        variant: VariantIdx,
        state: StateClass,
        profile_revision: u32,
    ) -> usize {
        let epoch = StateEpoch::new(profile_revision, state);
        self.index(model, variant, state)
            .and_then(|i| self.cells.get(i))
            .filter(|c| c.used && c.epoch == epoch)
            .map_or(0, |c| c.len)
    }

    /// Die Prognose fuer diesen Betriebspunkt.
    ///
    /// Sie darf nur dann eine **mutigere** Planung stuetzen, wenn drei Dinge
    /// zusammenkommen: die Zelle gehoert zur aktuellen Epoche, sie traegt
    /// genug Messwerte, und der Zustand ist vollstaendig beobachtet. Fehlt
    /// eines davon, gibt es keine Aussage — und der Aufrufer bleibt beim
    /// bisherigen Weg.
    #[must_use]
    pub fn predict(
        &self,
        model: ModelIdx,
        variant: VariantIdx,
        state: StateClass,
        profile_revision: u32,
    ) -> Prediction {
        if !state.is_fully_observed() {
            return Prediction::Fallback {
                reason: Rejection::StateNotObserved,
            };
        }
        let epoch = StateEpoch::new(profile_revision, state);
        let Some(cell) = self
            .index(model, variant, state)
            .and_then(|i| self.cells.get(i))
        else {
            return Prediction::Fallback {
                reason: Rejection::NoCell,
            };
        };
        if !cell.used {
            return Prediction::Fallback {
                reason: Rejection::NoCell,
            };
        }
        if cell.epoch != epoch {
            return Prediction::Fallback {
                reason: Rejection::WrongEpoch,
            };
        }
        if cell.len < MIN_OBSERVATIONS_TO_RELAX {
            return Prediction::Fallback {
                reason: Rejection::TooFewObservations {
                    got: cell.len,
                    needed: MIN_OBSERVATIONS_TO_RELAX,
                },
            };
        }
        Prediction::FromCell {
            runtime: Duration::from_nanos_unbounded(
                u64::from(cell.p95_micros).saturating_mul(1_000),
            ),
            epoch,
            observations: cell.len,
        }
    }

    /// Die wirksame Planungslaufzeit.
    ///
    /// Im Schattenbetrieb immer `legacy`; die Prognose wird nur verglichen und
    /// mitgeschrieben. Im scharfen Betrieb gilt die Prognose, wenn es eine
    /// gibt — auch wenn sie kuerzer ist als `legacy`. Genau das ist der
    /// Gewinn, und genau deshalb muss die Zelle vorher belegt sein.
    pub fn effective(
        &mut self,
        legacy: Duration,
        model: ModelIdx,
        variant: VariantIdx,
        state: StateClass,
        profile_revision: u32,
    ) -> Duration {
        let prediction = self.predict(model, variant, state, profile_revision);
        self.ledger.compare(legacy, &prediction);
        match self.mode {
            Mode::Shadow => legacy,
            Mode::Active => prediction.runtime().unwrap_or(legacy),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    fn nominal(occupancy: u8) -> StateClass {
        StateClass {
            occupancy,
            throttle: ThrottleClass::Nominal,
            clock: ClockClass::Full,
        }
    }

    #[test]
    fn an_unobserved_state_is_the_default_not_the_best_case() {
        assert_eq!(StateClass::default().clock, ClockClass::Unknown);
        assert!(!StateClass::default().is_fully_observed());
    }

    fn throttled(occupancy: u8) -> StateClass {
        StateClass {
            occupancy,
            throttle: ThrottleClass::Limited,
            clock: ClockClass::Reduced,
        }
    }

    fn fill(p: &mut Predictor, state: StateClass, revision: u32, value_ms: u64, count: usize) {
        for _ in 0..count {
            p.record(ModelIdx(0), VariantIdx(0), state, revision, ms(value_ms));
        }
    }

    // -- Zustandsklassen ---------------------------------------------------

    #[test]
    fn an_unobservable_clock_is_its_own_class() {
        assert_eq!(ClockClass::from_mhz(None, Some(2100)), ClockClass::Unknown);
        assert_eq!(ClockClass::from_mhz(Some(1740), None), ClockClass::Unknown);
        assert_eq!(
            ClockClass::from_mhz(Some(1740), Some(0)),
            ClockClass::Unknown,
            "ein Maximaltakt von null ist keine Bezugsgroesse"
        );
    }

    #[test]
    fn the_clock_class_follows_the_ten_percent_line() {
        // Diese Karte laeuft im Dauerlauf bei 1830 von 2100 MHz — das sind
        // 87 Prozent und damit `Reduced`.
        assert_eq!(
            ClockClass::from_mhz(Some(1830), Some(2100)),
            ClockClass::Reduced
        );
        assert_eq!(
            ClockClass::from_mhz(Some(1890), Some(2100)),
            ClockClass::Full,
            "genau 90 Prozent zaehlt noch als voll"
        );
        assert_eq!(
            ClockClass::from_mhz(Some(2100), Some(2100)),
            ClockClass::Full
        );
    }

    #[test]
    fn every_state_class_has_its_own_code() {
        let mut codes = Vec::new();
        for occupancy in 0..3_u8 {
            for throttle in [ThrottleClass::Nominal, ThrottleClass::Limited] {
                for clock in [ClockClass::Full, ClockClass::Reduced, ClockClass::Unknown] {
                    codes.push(
                        StateClass {
                            occupancy,
                            throttle,
                            clock,
                        }
                        .code(),
                    );
                }
            }
        }
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "Kollision in den Zustandscodes");
    }

    // -- Keine Vermischung -------------------------------------------------

    #[test]
    fn a_profile_change_empties_the_cell_instead_of_blending_it() {
        // Der Kern von NV-06: nach einem Profilwechsel gelten die alten
        // Messwerte nicht mehr. Sie in einem gleitenden Fenster
        // mitzuschleppen hiesse, dutzende Messungen lang eine Mischung aus
        // zwei Maschinen zu planen.
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 10, 100);
        assert_eq!(
            p.observations(ModelIdx(0), VariantIdx(0), nominal(1), 1),
            WINDOW
        );

        p.record(ModelIdx(0), VariantIdx(0), nominal(1), 2, ms(50));
        assert_eq!(
            p.observations(ModelIdx(0), VariantIdx(0), nominal(1), 2),
            1,
            "die neue Epoche faengt bei eins an"
        );
        assert_eq!(
            p.observations(ModelIdx(0), VariantIdx(0), nominal(1), 1),
            0,
            "und die alte ist weg, nicht halb da"
        );
    }

    #[test]
    fn a_throttled_state_does_not_read_the_nominal_cell() {
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 10, 64);
        assert!(matches!(
            p.predict(ModelIdx(0), VariantIdx(0), throttled(1), 1),
            Prediction::FromCell { .. } | Prediction::Fallback { .. }
        ));
        assert_eq!(
            p.predict(ModelIdx(0), VariantIdx(0), throttled(1), 1),
            Prediction::Fallback {
                reason: Rejection::NoCell
            }
        );
    }

    #[test]
    fn two_states_keep_two_answers() {
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 10, 64);
        fill(&mut p, throttled(1), 1, 25, 64);
        let fast = p
            .predict(ModelIdx(0), VariantIdx(0), nominal(1), 1)
            .runtime()
            .unwrap();
        let slow = p
            .predict(ModelIdx(0), VariantIdx(0), throttled(1), 1)
            .runtime()
            .unwrap();
        assert!(fast < slow, "{fast:?} < {slow:?}");
    }

    #[test]
    fn a_different_occupancy_is_a_different_cell() {
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 10, 64);
        assert_eq!(
            p.predict(ModelIdx(0), VariantIdx(0), nominal(2), 1),
            Prediction::Fallback {
                reason: Rejection::NoCell
            }
        );
    }

    // -- Nichts erfinden ---------------------------------------------------

    #[test]
    fn a_thin_cell_yields_no_relaxation() {
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 10, 20);
        assert_eq!(
            p.predict(ModelIdx(0), VariantIdx(0), nominal(1), 1),
            Prediction::Fallback {
                reason: Rejection::TooFewObservations {
                    got: 20,
                    needed: MIN_OBSERVATIONS_TO_RELAX
                }
            }
        );
    }

    #[test]
    fn an_unobserved_state_yields_no_prediction() {
        let mut p = Predictor::new();
        let unknown = StateClass {
            occupancy: 1,
            throttle: ThrottleClass::Nominal,
            clock: ClockClass::Unknown,
        };
        fill(&mut p, unknown, 1, 10, 64);
        assert_eq!(
            p.predict(ModelIdx(0), VariantIdx(0), unknown, 1),
            Prediction::Fallback {
                reason: Rejection::StateNotObserved
            },
            "unter unbekanntem Takt gemessene Werte duerfen nicht mutiger machen"
        );
    }

    #[test]
    fn nothing_is_interpolated_between_cells() {
        // Zwischen Belegungsgrad 1 und 3 liegt kein Wert, sondern eine
        // Wissensluecke.
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 10, 64);
        fill(&mut p, nominal(3), 1, 30, 64);
        assert_eq!(
            p.predict(ModelIdx(0), VariantIdx(0), nominal(2), 1),
            Prediction::Fallback {
                reason: Rejection::NoCell
            }
        );
    }

    // -- Zustandswechsel zwischen Planung und Dispatch ---------------------

    #[test]
    fn a_state_change_between_planning_and_dispatch_invalidates_the_prediction() {
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 10, 64);
        let planned = p.predict(ModelIdx(0), VariantIdx(0), nominal(1), 1);
        assert!(
            planned
                .still_valid_in(StateEpoch::new(1, nominal(1)))
                .is_ok()
        );
        assert_eq!(
            planned.still_valid_in(StateEpoch::new(1, throttled(1))),
            Err(Rejection::StateChanged),
            "zwischen Planung und Dispatch ist die Karte in eine Drosselung gelaufen"
        );
        assert_eq!(
            planned.still_valid_in(StateEpoch::new(2, nominal(1))),
            Err(Rejection::StateChanged),
            "und ein Profilwechsel zaehlt genauso"
        );
    }

    #[test]
    fn a_fallback_carries_its_reason_through() {
        let p = Predictor::new();
        let planned = p.predict(ModelIdx(0), VariantIdx(0), nominal(1), 1);
        assert_eq!(
            planned.still_valid_in(StateEpoch::new(1, nominal(1))),
            Err(Rejection::NoCell)
        );
    }

    // -- Schatten, dann scharf ---------------------------------------------

    #[test]
    fn shadow_mode_changes_no_decision() {
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 5, 64);
        let legacy = ms(20);
        let effective = p.effective(legacy, ModelIdx(0), VariantIdx(0), nominal(1), 1);
        assert_eq!(
            effective, legacy,
            "im Schatten entscheidet weiter der alte Weg"
        );
        assert_eq!(p.ledger().comparisons, 1);
        assert_eq!(p.ledger().more_optimistic, 1);
        assert_eq!(p.ledger().largest_decrease_us, 15_000);
    }

    #[test]
    fn active_mode_uses_the_cell_even_when_it_is_shorter() {
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 5, 64);
        p.set_mode(Mode::Active);
        let effective = p.effective(ms(20), ModelIdx(0), VariantIdx(0), nominal(1), 1);
        assert_eq!(effective.as_millis(), 5);
    }

    #[test]
    fn active_mode_falls_back_when_there_is_no_cell() {
        let mut p = Predictor::new();
        p.set_mode(Mode::Active);
        let legacy = ms(20);
        assert_eq!(
            p.effective(legacy, ModelIdx(0), VariantIdx(0), nominal(1), 1),
            legacy
        );
        assert_eq!(p.ledger().fallbacks, 1);
        assert_eq!(p.ledger().decisive(), 0);
    }

    #[test]
    fn the_ledger_separates_bolder_from_more_cautious() {
        // Die Frage vor jeder Umstellung: schlaegt v2 den alten Weg, oder
        // lehnt es nur mehr ab? Eine Policy, die alles ablehnt, haelt jede
        // Zusage ein und ist trotzdem wertlos.
        let mut p = Predictor::new();
        fill(&mut p, nominal(1), 1, 30, 64);
        let _ = p.effective(ms(20), ModelIdx(0), VariantIdx(0), nominal(1), 1);
        assert_eq!(p.ledger().more_conservative, 1);
        assert_eq!(p.ledger().more_optimistic, 0);
        assert_eq!(p.ledger().largest_increase_us, 10_000);

        fill(&mut p, nominal(2), 1, 5, 64);
        let _ = p.effective(ms(20), ModelIdx(0), VariantIdx(0), nominal(2), 1);
        assert_eq!(p.ledger().more_optimistic, 1);
        assert_eq!(p.ledger().decisive(), 2);
    }

    // -- Budget ------------------------------------------------------------

    #[test]
    fn the_cell_table_is_bounded_and_allocated_once() {
        let p = Predictor::new();
        assert_eq!(p.capacity(), MAX_CELLS);
        assert_eq!(p.occupied_cells(), 0);
        assert_eq!(
            MAX_CELLS,
            32 * 8 * 8 * 6,
            "Modelle x Varianten x Belegungsgrade x Zustandsklassen"
        );
    }

    #[test]
    fn the_table_follows_the_configuration_not_the_maximum() {
        // Der Messaufbau dieses Projekts: vier Modelle, eine Variante, ein
        // Slot. Das sind 24 Zellen und nicht zwoelftausend.
        let p = Predictor::with_shape(4, 1, 1);
        assert_eq!(p.capacity(), 24);
        assert!(p.capacity() < MAX_CELLS);
    }

    #[test]
    fn a_shape_beyond_the_maximum_is_clamped() {
        let p = Predictor::with_shape(1_000, 1_000, 1_000);
        assert_eq!(p.capacity(), MAX_CELLS);
    }

    #[test]
    fn a_window_that_cannot_reach_the_threshold_would_be_useless() {
        // Ein Fenster, das die geforderte Belegzahl nie erreicht, macht jede
        // Zelle dauerhaft unbrauchbar.
        const { assert!(WINDOW >= MIN_OBSERVATIONS_TO_RELAX) };
    }

    #[test]
    fn the_state_space_does_not_grow_with_runtime() {
        let mut p = Predictor::new();
        for round in 0..1_000_u32 {
            let state = StateClass {
                occupancy: u8::try_from(round % 8).unwrap_or(0),
                throttle: if round % 2 == 0 {
                    ThrottleClass::Nominal
                } else {
                    ThrottleClass::Limited
                },
                clock: ClockClass::Full,
            };
            p.record(ModelIdx(0), VariantIdx(0), state, round % 3, ms(10));
        }
        assert!(
            p.occupied_cells() <= 16,
            "acht Belegungsgrade mal zwei Drosselklassen: {}",
            p.occupied_cells()
        );
    }

    #[test]
    fn an_index_outside_the_table_is_refused_not_wrapped() {
        let mut p = Predictor::new();
        let too_far = StateClass {
            occupancy: 200,
            ..nominal(0)
        };
        p.record(ModelIdx(0), VariantIdx(0), too_far, 1, ms(10));
        assert_eq!(p.occupied_cells(), 0);
        assert_eq!(
            p.predict(ModelIdx(0), VariantIdx(0), too_far, 1),
            Prediction::Fallback {
                reason: Rejection::NoCell
            }
        );
    }
}
