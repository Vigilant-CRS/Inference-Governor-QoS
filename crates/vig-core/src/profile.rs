//! Laufzeitprofile und konservative Laufzeitprognose (Spec 13).
//!
//! Der Scheduler darf nie mit einer Durchschnittslatenz planen. Er plant mit
//! einem hohen Quantil plus Sicherheitsmarge, denn eine zu optimistische
//! Prognose erzeugt genau die Deadline-Misses, die das Produkt verhindern soll
//! (Spec 13.1).
//!
//! ## Profile pro Belegungsgrad
//!
//! ADR-0006 verschiebt den Offline-Interferenzprofiler hinter Gate M3 und
//! ersetzt ihn im MVP durch Laufzeitprofile **pro Slot-Belegungsgrad**: wie
//! lange braucht diese Variante, wenn ausser ihr null, ein, zwei ... weitere
//! Slots belegt sind. Damit wird der Interferenzeffekt datengetrieben erfasst,
//! ohne ihn paarweise offline vermessen zu muessen — dieselbe Argumentation,
//! die Spec 13.2 fuer Taktabsenkung und Temperatur fuehrt: auf den Effekt
//! reagieren, nicht auf die Ursache.

use crate::arrayvec::ArrayVec;
use crate::ids::MAX_SLOTS;
use crate::time::Duration;

/// Sicherheitsmarge als exakter Bruch.
///
/// Bewusst kein `f64`: der Hot Path bleibt gleitkommafrei und damit ueber
/// Plattformen und Laeufe hinweg bitgenau reproduzierbar (Spec 18, 30.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetyMargin {
    num: u32,
    den: u32,
}

impl SafetyMargin {
    /// Marge ohne Aufschlag.
    ///
    /// Bewusst als `100/100` und nicht als `1/1` dargestellt: beide rechnen
    /// gleich, aber nur bei `100/100` liefert [`SafetyMargin::as_percent`] den
    /// erwarteten Wert. Die kuerzere Form lieferte 1 Prozent, was jeder
    /// Leser dieses Wertes als „unzulaessig" interpretieren muss — und dann
    /// still auf einen anderen Wert ausweicht.
    pub const NONE: Self = Self { num: 100, den: 100 };

    /// Die Startmarge aus Spec 13.2: Faktor 1,10.
    pub const DEFAULT: Self = Self { num: 110, den: 100 };

    /// Harte Untergrenze: keine Marge unter 1,0 — der Scheduler darf nie
    /// optimistischer planen als das gemessene Profil.
    pub const MIN_PERCENT: u32 = 100;

    /// Harte Obergrenze: Faktor 3,0. Daruber ist das Profil kaputt und der
    /// Circuit Breaker aus Spec 30.3 zustaendig, nicht eine immer groessere Marge.
    pub const MAX_PERCENT: u32 = 300;

    /// Erzeugt eine Marge aus einem Prozentwert.
    ///
    /// Gibt `None` ausserhalb von `[MIN_PERCENT, MAX_PERCENT]` zurueck.
    #[must_use]
    pub const fn from_percent(percent: u32) -> Option<Self> {
        if percent < Self::MIN_PERCENT || percent > Self::MAX_PERCENT {
            return None;
        }
        Some(Self {
            num: percent,
            den: 100,
        })
    }

    /// Der Prozentwert dieser Marge.
    #[must_use]
    pub const fn as_percent(self) -> u32 {
        self.num
    }

    /// Wendet die Marge auf eine Zeitspanne an.
    #[must_use]
    pub fn apply(self, d: Duration) -> Option<Duration> {
        d.checked_scale(self.num, self.den)
    }
}

impl Default for SafetyMargin {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Gemessene Laufzeitquantile einer Variante bei einem festen Belegungsgrad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeProfile {
    /// Median.
    pub p50: Duration,
    /// 95-%-Quantil.
    pub p95: Duration,
    /// 99-%-Quantil. Planungsgrundlage.
    pub p99: Duration,
    /// Anzahl der Messungen, aus denen die Quantile stammen.
    pub samples: u32,
}

/// Warum ein Profil unbrauchbar ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileError {
    /// Die Quantile sind nicht monoton (`p50 <= p95 <= p99`).
    NotMonotonic,
    /// Zu wenige Messungen fuer ein belastbares p99.
    TooFewSamples {
        /// Die vorhandene Anzahl.
        samples: u32,
        /// Die geforderte Mindestanzahl.
        required: u32,
    },
    /// Es existiert kein Profil fuer diesen Belegungsgrad.
    NoProfileForOccupancy {
        /// Der angefragte Belegungsgrad.
        occupancy: usize,
    },
    /// Die Laufzeitprognose ueberlaeuft.
    Unrepresentable,
}

impl core::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotMonotonic => write!(f, "Laufzeitquantile sind nicht monoton"),
            Self::TooFewSamples { samples, required } => {
                write!(f, "{samples} Messungen, mindestens {required} noetig")
            }
            Self::NoProfileForOccupancy { occupancy } => {
                write!(f, "kein Profil fuer Belegungsgrad {occupancy}")
            }
            Self::Unrepresentable => write!(f, "Laufzeitprognose nicht darstellbar"),
        }
    }
}

impl core::error::Error for ProfileError {}

impl RuntimeProfile {
    /// Mindestanzahl Messungen fuer ein Profil, das als p99 gelten darf.
    ///
    /// Unter 100 Messungen ist ein 99-%-Quantil kein Quantil, sondern das
    /// Maximum. Spec 13.5 verlangt, dass ein unzureichendes Profil als solches
    /// erkennbar ist, statt still als exakt zu gelten.
    pub const MIN_SAMPLES: u32 = 100;

    /// Erzeugt ein geprueftes Profil.
    ///
    /// # Errors
    ///
    /// [`ProfileError::NotMonotonic`] oder [`ProfileError::TooFewSamples`].
    pub const fn new(
        p50: Duration,
        p95: Duration,
        p99: Duration,
        samples: u32,
    ) -> Result<Self, ProfileError> {
        if p50.as_nanos() > p95.as_nanos() || p95.as_nanos() > p99.as_nanos() {
            return Err(ProfileError::NotMonotonic);
        }
        if samples < Self::MIN_SAMPLES {
            return Err(ProfileError::TooFewSamples {
                samples,
                required: Self::MIN_SAMPLES,
            });
        }
        Ok(Self {
            p50,
            p95,
            p99,
            samples,
        })
    }

    /// Ein synthetisches Profil ohne Streuung.
    ///
    /// Nur fuer Golden Tests, in denen die Laufzeit exakt bekannt sein muss.
    #[must_use]
    pub const fn exact(d: Duration) -> Self {
        Self {
            p50: d,
            p95: d,
            p99: d,
            samples: Self::MIN_SAMPLES,
        }
    }

    /// Die konservative Planungslaufzeit: `p99 * margin`.
    ///
    /// # Errors
    ///
    /// [`ProfileError::Unrepresentable`] bei Ueberlauf.
    pub fn conservative(&self, margin: SafetyMargin) -> Result<Duration, ProfileError> {
        margin.apply(self.p99).ok_or(ProfileError::Unrepresentable)
    }
}

/// Die Laufzeitprofile einer Variante ueber alle Belegungsgrade.
///
/// Index 0 bedeutet „laeuft allein"; Index `n` bedeutet „`n` weitere Slots sind
/// gleichzeitig belegt" (ADR-0006).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantProfile {
    by_occupancy: ArrayVec<RuntimeProfile, MAX_SLOTS>,
}

impl VariantProfile {
    /// Ein Profil, das nur den Alleinbetrieb kennt.
    ///
    /// Zulaessiger Startzustand: solange kein Profil unter Nebenlast vorliegt,
    /// wird das Alleinprofil verwendet — sichtbar optimistisch, weshalb der
    /// Online Estimator (WP11) es als Erstes korrigiert.
    #[must_use]
    pub fn solo(profile: RuntimeProfile) -> Self {
        let mut by_occupancy = ArrayVec::new();
        let _ = by_occupancy.push(profile);
        Self { by_occupancy }
    }

    /// Erzeugt ein Profil aus Messungen je Belegungsgrad, aufsteigend.
    ///
    /// # Errors
    ///
    /// [`ProfileError::NoProfileForOccupancy`], wenn die Liste leer ist.
    pub fn from_levels(levels: ArrayVec<RuntimeProfile, MAX_SLOTS>) -> Result<Self, ProfileError> {
        if levels.is_empty() {
            return Err(ProfileError::NoProfileForOccupancy { occupancy: 0 });
        }
        Ok(Self {
            by_occupancy: levels,
        })
    }

    /// Das Profil fuer einen Belegungsgrad.
    ///
    /// Liegt fuer den angefragten Grad keine Messung vor, wird der hoechste
    /// gemessene Grad verwendet — nach oben extrapolieren waere Raten, und der
    /// hoechste gemessene Grad ist die konservativste vorhandene Information.
    #[must_use]
    pub fn at_occupancy(&self, occupancy: usize) -> Option<&RuntimeProfile> {
        if self.by_occupancy.is_empty() {
            return None;
        }
        let last = self.by_occupancy.len().saturating_sub(1);
        self.by_occupancy.get(occupancy.min(last))
    }

    /// Die optimistische Laufzeitschaetzung bei einem Belegungsgrad: der Median,
    /// ohne Sicherheitsmarge.
    ///
    /// Ausschliesslich fuer die Verwerfensentscheidung (ADR-0010). Wer mit
    /// diesem Wert **plant**, verspricht, was er nicht halten kann; wer mit dem
    /// konservativen Wert **verwirft**, vernichtet Arbeit, die ueberwiegend
    /// noch gut gewesen waere.
    ///
    /// # Errors
    ///
    /// [`ProfileError::NoProfileForOccupancy`].
    pub fn optimistic_at(&self, occupancy: usize) -> Result<Duration, ProfileError> {
        Ok(self
            .at_occupancy(occupancy)
            .ok_or(ProfileError::NoProfileForOccupancy { occupancy })?
            .p50)
    }

    /// Die konservative Planungslaufzeit bei einem Belegungsgrad.
    ///
    /// # Errors
    ///
    /// [`ProfileError::NoProfileForOccupancy`] oder [`ProfileError::Unrepresentable`].
    pub fn conservative_at(
        &self,
        occupancy: usize,
        margin: SafetyMargin,
    ) -> Result<Duration, ProfileError> {
        self.at_occupancy(occupancy)
            .ok_or(ProfileError::NoProfileForOccupancy { occupancy })?
            .conservative(margin)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    /// Alle Margen muessen dieselbe Darstellung benutzen, sonst liest ein
    /// Verbraucher aus `as_percent()` einen Wert, den `from_percent` ablehnt.
    #[test]
    fn every_margin_round_trips_through_its_percent_value() {
        for margin in [SafetyMargin::NONE, SafetyMargin::DEFAULT] {
            let percent = margin.as_percent();
            assert!(
                SafetyMargin::from_percent(percent).is_some(),
                "as_percent() lieferte {percent}, was from_percent ablehnt"
            );
            assert_eq!(SafetyMargin::from_percent(percent), Some(margin));
        }
        assert_eq!(SafetyMargin::NONE.as_percent(), 100);
        assert_eq!(SafetyMargin::DEFAULT.as_percent(), 110);
    }

    #[test]
    fn margin_is_bounded_in_both_directions() {
        assert!(
            SafetyMargin::from_percent(99).is_none(),
            "nie optimistischer als das Profil"
        );
        assert!(
            SafetyMargin::from_percent(301).is_none(),
            "eine 3x-Marge ist ein Profilfehler"
        );
        assert_eq!(
            SafetyMargin::from_percent(110).unwrap(),
            SafetyMargin::DEFAULT
        );
    }

    #[test]
    fn conservative_runtime_applies_the_margin_to_p99() {
        let p = RuntimeProfile::new(ms(8), ms(9), ms(10), 1_000).unwrap();
        assert_eq!(
            p.conservative(SafetyMargin::DEFAULT).unwrap().as_millis(),
            11
        );
        assert_eq!(p.conservative(SafetyMargin::NONE).unwrap().as_millis(), 10);
    }

    #[test]
    fn non_monotonic_quantiles_are_rejected() {
        assert_eq!(
            RuntimeProfile::new(ms(10), ms(9), ms(20), 1_000).unwrap_err(),
            ProfileError::NotMonotonic
        );
    }

    /// Spec 13.5: ein unzureichendes Profil darf nicht still als exakt gelten.
    #[test]
    fn a_p99_from_too_few_samples_is_refused() {
        let err = RuntimeProfile::new(ms(8), ms(9), ms(10), 12).unwrap_err();
        assert_eq!(
            err,
            ProfileError::TooFewSamples {
                samples: 12,
                required: 100
            }
        );
    }

    #[test]
    fn occupancy_profile_uses_the_highest_measured_level_when_extrapolating() {
        let mut levels = ArrayVec::new();
        levels.push(RuntimeProfile::exact(ms(10))).unwrap(); // allein
        levels.push(RuntimeProfile::exact(ms(14))).unwrap(); // ein Nachbar
        let vp = VariantProfile::from_levels(levels).unwrap();

        assert_eq!(vp.at_occupancy(0).unwrap().p99.as_millis(), 10);
        assert_eq!(vp.at_occupancy(1).unwrap().p99.as_millis(), 14);
        assert_eq!(
            vp.at_occupancy(7).unwrap().p99.as_millis(),
            14,
            "nicht nach oben raten, sondern den konservativsten Messwert nehmen"
        );
    }

    #[test]
    fn solo_profile_is_a_valid_starting_point() {
        let vp = VariantProfile::solo(RuntimeProfile::exact(ms(10)));
        assert_eq!(
            vp.conservative_at(0, SafetyMargin::DEFAULT)
                .unwrap()
                .as_millis(),
            11
        );
        assert_eq!(
            vp.conservative_at(5, SafetyMargin::DEFAULT)
                .unwrap()
                .as_millis(),
            11
        );
    }
}
