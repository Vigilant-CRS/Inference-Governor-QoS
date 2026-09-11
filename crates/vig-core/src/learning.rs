//! Die Planung kalibriert sich an der Karte, auf der sie laeuft (ADR-0038).
//!
//! ## Was der Margenregler schon ist
//!
//! [`MarginController`](crate::estimator::MarginController) erhoeht die Marge
//! nach einer Ueberziehung um `G·(1−t)` und senkt sie nach einer eingehaltenen
//! Ausfuehrung um `G·t`. Ueberziehung heisst im Scheduler: die gemessene
//! Laufzeit liegt ueber dem Plan, und der Plan ist `Profil × Marge`. Das ist
//! ein Robbins-Monro-Schaetzer: er ruht genau dort, wo der Anteil `t` der
//! Ausfuehrungen seinen Plan ueberzieht — beim `(1−t)`-Quantil von
//! `tatsaechlich / Profil`. `Profil × Marge` verfolgt also das p99 der echten
//! Laufzeit. Einzig der Boden, die konfigurierte Marge, hindert ihn daran,
//! das auch dann zu tun, wenn das Profil zu pessimistisch ist.
//!
//! An der Kante kostet das Arbeit: bei echten 100 % Last plant, wer ein zu
//! pessimistisches Profil mit 110 % multipliziert, eine Ueberlast, die es
//! nicht gibt, und verwirft Frames, die gepasst haetten.
//!
//! ## Was dieses Modul anders macht
//!
//! * **Kein fester Boden.** Der Faktor darf unter 100 % fallen, wenn das
//!   Profil fuer diese Karte zu langsam ist — ein Profil von anderer Hardware
//!   oder unter anderer Last kalibriert. Nach unten begrenzen ihn ein Boden
//!   **aus Daten** (die Planung liegt nie unter dem beobachteten Median, siehe
//!   [`RuntimeEstimator`](crate::estimator::RuntimeEstimator)) und ein harter
//!   Konfigurationsboden, [`MarginLearning::min_factor_percent`].
//! * **Multiplikativ.** Die Schritte liegen im Log-Bereich. Ein Laptopprofil
//!   auf einem Geraet, das fuenfmal langsamer rechnet, braucht dann Dutzende
//!   Ueberziehungen und nicht Hunderte. Das Verhaeltnis der Schritte bleibt
//!   `(1−t) : t` — es ist das, was den Quantilschaetzer ausmacht.
//! * **Hierarchisch.** Der Faktor eines Modells ist ein **Geraetefaktor**,
//!   gelernt aus dem Verkehr aller Modelle, mal einem **Rest** je Modell. Ein
//!   neues Geraet ist so aus dem gesamten Verkehr schnell kalibriert, und ein
//!   selten laufendes Modell erbt, was die haeufigen gelernt haben.
//! * **Aufwaermen wie bisher.** Der Start ist die konfigurierte Marge, fuer
//!   ein nicht verifiziertes Profil die erhoehte (ADR-0016). Vorsichtiger wird
//!   der Faktor sofort, mutiger erst nach
//!   [`MarginLearning::min_observations`].
//!
//! ## Rechnen ohne Gleitkomma
//!
//! Der Entscheidungspfad bleibt bitgenau reproduzierbar (Spec 18): Faktoren in
//! Millionsteln, ein Schritt ist der Pade-Bruch `(2 + x) / (2 − x)` — die
//! rationale Naeherung von `e^x`, deren Kehrwert genau der Schritt um `−x`
//! ist. Hinauf und hinunter um dasselbe `x` heben sich exakt auf.
//!
//! ## Was ausdruecklich nicht dazugehoert
//!
//! Der gelernte Faktor ueberlebt keinen Neustart. Ihn zu speichern waere der
//! naechste Schritt (ADR-0038); dann muesste er an die Profilidentitaet
//! gebunden sein (NV-03), sonst erbte eine andere Karte ihn.

use crate::ids::{MAX_MODELS, ModelIdx};
use crate::profile::SafetyMargin;

/// Faktor eins in Millionsteln.
const ONE: u64 = 1_000_000;

/// Die Verstaerkung je Beobachtung und Ebene, in Millionsteln.
///
/// Geraetefaktor und Rest machen je einen halben Schritt; zusammen ist das
/// ein Zehntel im Log-Bereich, wenn das Ziel null waere — dieselbe
/// Groessenordnung wie die zehn Prozentpunkte des linearen Reglers
/// (ADR-0034). Ein Modell, das allein laeuft, sieht so dieselbe
/// Schrittweite wie dort.
const HALF_GAIN: u64 = 50_000;

/// Der Rest eines Modells bleibt innerhalb von einem Viertel bis zum
/// Vierfachen des Geraetefaktors.
///
/// Weiter auseinander liegen zwei Modelle auf derselben Karte nicht, ohne
/// dass ihr Profil falsch ist; dafuer ist [`crate::estimator::ProfileHealth`]
/// zustaendig, nicht ein immer groesserer Rest.
const RESIDUAL_MIN: u64 = 250_000;
/// Siehe [`RESIDUAL_MIN`].
const RESIDUAL_MAX: u64 = 4_000_000;

/// Wie weit sich die Planung an der Karte kalibrieren darf (ADR-0038).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarginLearning {
    min_factor_percent: u32,
    max_factor_percent: u32,
    min_observations: u32,
}

impl MarginLearning {
    /// Der niedrigste zulaessige harte Boden: ein Zehntel des Profils.
    ///
    /// Tiefer ist kein pessimistisches Profil mehr, sondern ein falsches.
    pub const LOWEST_MIN_FACTOR_PERCENT: u32 = 10;
    /// Die Voreinstellung des harten Bodens: die Haelfte des Profils.
    pub const DEFAULT_MIN_FACTOR_PERCENT: u32 = 50;
    /// Die Voreinstellung der Obergrenze: das Zehnfache des Profils.
    ///
    /// Hoeher als die 300 % einer konfigurierten Marge: ein Profil vom Laptop
    /// auf einem Geraet, das fuenfmal langsamer rechnet, soll konvergieren
    /// koennen. Laeuft die Karte dauerhaft ueber dem Doppelten des Profils,
    /// meldet ausserdem [`crate::estimator::ProfileHealth`] das Profil als
    /// verdorben.
    pub const DEFAULT_MAX_FACTOR_PERCENT: u32 = 1_000;
    /// Die hoechste zulaessige Obergrenze: das Hundertfache.
    pub const HIGHEST_MAX_FACTOR_PERCENT: u32 = 10_000;
    /// Die Voreinstellung der Aufwaermphase, dieselbe wie fuer eine mutigere
    /// Prognose (NV-06): eine Planung zu lockern braucht mehr Wissen als sie
    /// zu verschaerfen.
    pub const DEFAULT_OBSERVATIONS: u32 = 48;
    /// Die laengste zulaessige Aufwaermphase.
    pub const MAX_OBSERVATIONS: u32 = 100_000;

    /// Ein Lernbereich.
    ///
    /// `None`, wenn der Boden nicht in
    /// [`Self::LOWEST_MIN_FACTOR_PERCENT`]`..=100` liegt, die Obergrenze
    /// nicht in `100..=`[`Self::HIGHEST_MAX_FACTOR_PERCENT`] oder die
    /// Aufwaermphase nicht in `1..=`[`Self::MAX_OBSERVATIONS`].
    #[must_use]
    pub const fn new(
        min_factor_percent: u32,
        max_factor_percent: u32,
        min_observations: u32,
    ) -> Option<Self> {
        if min_factor_percent < Self::LOWEST_MIN_FACTOR_PERCENT
            || min_factor_percent > 100
            || max_factor_percent < 100
            || max_factor_percent > Self::HIGHEST_MAX_FACTOR_PERCENT
            || min_observations == 0
            || min_observations > Self::MAX_OBSERVATIONS
        {
            return None;
        }
        Some(Self {
            min_factor_percent,
            max_factor_percent,
            min_observations,
        })
    }

    /// Die Voreinstellung: 50 % bis 1000 %, mutiger nach 48 Ausfuehrungen.
    #[must_use]
    pub const fn default_range() -> Self {
        Self {
            min_factor_percent: Self::DEFAULT_MIN_FACTOR_PERCENT,
            max_factor_percent: Self::DEFAULT_MAX_FACTOR_PERCENT,
            min_observations: Self::DEFAULT_OBSERVATIONS,
        }
    }

    /// Der harte Boden, in Prozent des Profils.
    #[must_use]
    pub const fn min_factor_percent(&self) -> u32 {
        self.min_factor_percent
    }

    /// Die Obergrenze, in Prozent des Profils.
    #[must_use]
    pub const fn max_factor_percent(&self) -> u32 {
        self.max_factor_percent
    }

    /// Ab wie vielen Ausfuehrungen der Faktor mutiger werden darf.
    #[must_use]
    pub const fn min_observations(&self) -> u32 {
        self.min_observations
    }

    fn min_ppm(self) -> u64 {
        u64::from(self.min_factor_percent).saturating_mul(10_000)
    }

    fn max_ppm(self) -> u64 {
        u64::from(self.max_factor_percent).saturating_mul(10_000)
    }
}

/// Der gelernte Faktor zwischen Profil und Karte, je Modell (ADR-0038).
///
/// `Faktor(Modell) = Geraetefaktor × Rest(Modell)`, begrenzt auf den
/// Lernbereich. Beide Ebenen machen denselben Quantilschritt; der
/// Geraetefaktor sieht dabei den Verkehr aller Modelle.
#[derive(Debug, Clone)]
pub struct FactorLearner {
    params: MarginLearning,
    /// Der Geraetefaktor in Millionsteln.
    device: u64,
    /// Beobachtungen ueber alle Modelle.
    device_observations: u32,
    /// Der Rest je Modell in Millionsteln.
    residual: [u64; MAX_MODELS],
    /// Beobachtungen je Modell seit dem Start oder dem letzten Profilwechsel.
    observations: [u32; MAX_MODELS],
    /// Das Ziel je Modell: wie viele Ausfuehrungen je tausend ihren Plan
    /// ueberziehen duerfen (ADR-0034).
    target_permille: [u32; MAX_MODELS],
}

impl FactorLearner {
    /// Beginnt bei der konfigurierten Marge, je Modell mit seinem Ziel.
    #[must_use]
    pub fn new(
        params: MarginLearning,
        start: SafetyMargin,
        target_permille: &[u32; MAX_MODELS],
    ) -> Self {
        let device = u64::from(start.as_percent())
            .saturating_mul(10_000)
            .clamp(params.min_ppm(), params.max_ppm());
        Self {
            params,
            device,
            device_observations: 0,
            residual: [ONE; MAX_MODELS],
            observations: [0; MAX_MODELS],
            target_permille: *target_permille,
        }
    }

    /// Der Lernbereich.
    #[must_use]
    pub const fn params(&self) -> MarginLearning {
        self.params
    }

    /// Der Faktor eines Modells in Millionsteln.
    #[must_use]
    pub fn factor_ppm(&self, model: ModelIdx) -> u64 {
        let residual = self.residual.get(model.get()).copied().unwrap_or(ONE);
        self.device
            .saturating_mul(residual)
            .checked_div(ONE)
            .unwrap_or(self.device)
            .clamp(self.params.min_ppm(), self.params.max_ppm())
    }

    /// Der Faktor eines Modells als Marge, auf ganze Prozent aufgerundet.
    ///
    /// Aufgerundet wie beim linearen Regler: ein Rest von einem halben
    /// Prozentpunkt gehoert zur Vorsicht.
    #[must_use]
    pub fn margin(&self, model: ModelIdx) -> SafetyMargin {
        let percent = self.factor_ppm(model).div_ceil(10_000);
        SafetyMargin::learned(u32::try_from(percent).unwrap_or(u32::MAX))
    }

    /// Der Geraetefaktor in Prozent, aufgerundet.
    #[must_use]
    pub fn device_percent(&self) -> u32 {
        u32::try_from(self.device.div_ceil(10_000)).unwrap_or(u32::MAX)
    }

    /// Wie viele Ausfuehrungen dieses Modells der Lerner gesehen hat.
    #[must_use]
    pub fn observations(&self, model: ModelIdx) -> u32 {
        self.observations.get(model.get()).copied().unwrap_or(0)
    }

    /// Nimmt eine Ausfuehrung auf: hat sie ihren Plan ueberzogen?
    ///
    /// Beide Ebenen machen den Schritt `(1−t)` nach oben oder `t` nach unten.
    /// Nach unten erst nach der Aufwaermphase der jeweiligen Ebene, nach oben
    /// sofort. Liegt der Faktor schon an einer Grenze, entfaellt der Schritt
    /// in diese Richtung ganz: er aenderte nichts am Plan und saemmelte nur
    /// eine Schuld an, die spaeter jede Korrektur verzoegert.
    pub fn observe(&mut self, model: ModelIdx, overran: bool) {
        let index = model.get();
        let Some(target) = self.target_permille.get(index).copied() else {
            return;
        };
        let target = u64::from(target.min(1_000));
        let x = if overran {
            HALF_GAIN
                .saturating_mul(1_000_u64.saturating_sub(target))
                .checked_div(1_000)
                .unwrap_or(0)
        } else {
            HALF_GAIN
                .saturating_mul(target)
                .checked_div(1_000)
                .unwrap_or(0)
        };

        self.device_observations = self.device_observations.saturating_add(1);
        let own = match self.observations.get_mut(index) {
            Some(count) => {
                *count = count.saturating_add(1);
                *count
            }
            None => return,
        };

        let product = self.factor_ppm(model);
        let at_limit = if overran {
            product >= self.params.max_ppm()
        } else {
            product <= self.params.min_ppm()
        };
        if at_limit {
            return;
        }

        let warm = self.params.min_observations;
        if overran || self.device_observations >= warm {
            self.device =
                step(self.device, overran, x).clamp(self.params.min_ppm(), self.params.max_ppm());
        }
        if (overran || own >= warm)
            && let Some(residual) = self.residual.get_mut(index)
        {
            *residual = step(*residual, overran, x).clamp(RESIDUAL_MIN, RESIDUAL_MAX);
        }
    }

    /// Ein Profil, das nicht verifiziert werden konnte (G-010, ADR-0016).
    ///
    /// Der Rest dieses Modells beginnt um den Aufschlag erhoeht, und seine
    /// Aufwaermphase beginnt von vorn: mutiger wird es erst wieder mit eigenen
    /// Beobachtungen. Der Geraetefaktor bleibt — er beschreibt die Karte, nicht
    /// dieses Profil.
    pub fn mark_unverified(
        &mut self,
        model: ModelIdx,
        start: SafetyMargin,
        surcharge_percent: u32,
    ) {
        let base = u64::from(start.as_percent()).max(1);
        let raised = base.saturating_add(u64::from(surcharge_percent));
        let residual = raised
            .saturating_mul(ONE)
            .div_ceil(base)
            .clamp(RESIDUAL_MIN, RESIDUAL_MAX);
        if let Some(slot) = self.residual.get_mut(model.get()) {
            *slot = residual;
        }
        if let Some(count) = self.observations.get_mut(model.get()) {
            *count = 0;
        }
    }
}

/// Ein Schritt im Log-Bereich um `x` Millionstel: `f · (2 + x) / (2 − x)`
/// nach oben, `f · (2 − x) / (2 + x)` nach unten.
///
/// Nach oben aufgerundet, nach unten abgerundet — beides um hoechstens ein
/// Millionstel, und die Richtung der Rundung ist die der Vorsicht beim
/// Steigen und die des Schritts beim Sinken, damit ein kleiner Faktor nicht
/// an der Rundung haengen bleibt.
fn step(factor: u64, up: bool, x: u64) -> u64 {
    let two = ONE.saturating_mul(2);
    let x = x.min(ONE);
    let (numerator, denominator) = if up {
        (two.saturating_add(x), two.saturating_sub(x))
    } else {
        (two.saturating_sub(x), two.saturating_add(x))
    };
    let scaled = factor.saturating_mul(numerator);
    if up {
        scaled.div_ceil(denominator.max(1))
    } else {
        scaled.checked_div(denominator).unwrap_or(factor)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::integer_division,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use super::*;
    use crate::estimator::MarginController;

    const M: ModelIdx = ModelIdx(0);
    const N: ModelIdx = ModelIdx(1);

    fn learner(min_observations: u32) -> FactorLearner {
        FactorLearner::new(
            MarginLearning::new(20, 1_000, min_observations).unwrap(),
            SafetyMargin::DEFAULT,
            &[MarginController::DEFAULT_TARGET_PERMILLE; MAX_MODELS],
        )
    }

    /// Ein deterministischer Strom von Laufzeitverhaeltnissen
    /// `tatsaechlich / Profil-p99`, gleichverteilt in `[lo, hi]` Promille.
    ///
    /// Das 99-%-Quantil ist `lo + 0,99 · (hi − lo)`, und genau dorthin muss
    /// ein Faktor konvergieren, der auf ein Prozent Ueberziehung regelt.
    struct Ratios {
        state: u64,
        lo: u64,
        hi: u64,
    }

    impl Ratios {
        /// Das naechste Verhaeltnis in Millionsteln.
        fn next(&mut self) -> u64 {
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let unit = (self.state >> 33) % 1_000_000;
            self.lo * 1_000 + unit * (self.hi - self.lo) / 1_000
        }

        /// Das 99-%-Quantil in Millionsteln.
        fn p99(&self) -> u64 {
            self.lo * 1_000 + 990 * (self.hi - self.lo)
        }
    }

    /// Faehrt den Lerner gegen einen Verhaeltnisstrom, wie der Scheduler es
    /// tut: ueberzogen ist, was ueber `Profil × Faktor` liegt.
    ///
    /// Gibt den mittleren Faktor und den Anteil der Ueberziehungen in der
    /// zweiten Haelfte zurueck. Der Faktor eines Quantilschaetzers springt
    /// nach jeder Ueberziehung um einen Schritt nach oben und sinkt danach
    /// langsam; eine Momentaufnahme sagt deshalb wenig, der Anteil und das
    /// Mittel sagen, wohin er konvergiert.
    fn converge(l: &mut FactorLearner, ratios: &mut Ratios, steps: u64) -> (u64, u64) {
        let (mut sum, mut overruns, mut counted) = (0_u64, 0_u64, 0_u64);
        for i in 0..steps {
            let overran = ratios.next() > l.factor_ppm(M);
            l.observe(M, overran);
            if i >= steps / 2 {
                sum += l.factor_ppm(M);
                overruns += u64::from(overran);
                counted += 1;
            }
        }
        (sum / counted, overruns * 1_000_000 / counted)
    }

    /// Ein pessimistisches Profil — die Karte rechnet in rund der Haelfte der
    /// Zeit: der Faktor sinkt unter 100 %, und ein Prozent der Ausfuehrungen
    /// ueberzieht, wie vereinbart.
    #[test]
    fn a_pessimistic_profile_converges_below_one_to_the_p99() {
        let mut l = learner(48);
        let mut ratios = Ratios {
            state: 7,
            lo: 400,
            hi: 520,
        };
        let (factor, overrun_ppm) = converge(&mut l, &mut ratios, 40_000);
        let quantile = ratios.p99();
        assert!(
            factor < ONE,
            "unter 100 %: das Profil ist zu langsam, {factor}"
        );
        assert!(
            factor * 100 >= quantile * 97 && factor * 100 <= quantile * 110,
            "mittlerer Faktor {factor} ppm, p99 der Verhaeltnisse {quantile} ppm"
        );
        assert!(
            (5_000..=15_000).contains(&overrun_ppm),
            "{overrun_ppm} ppm Ueberziehungen statt rund ein Prozent"
        );
    }

    /// Ein optimistisches Profil — die Karte braucht das Fuenffache: der
    /// Faktor steigt in Dutzenden Ueberziehungen, nicht in Hunderten.
    #[test]
    fn an_optimistic_profile_is_corrected_within_dozens_of_overruns() {
        let mut l = learner(48);
        let mut overruns = 0;
        let target = 4_500_000; // 90 % des Fuenffachen
        for _ in 0..1_000 {
            if l.factor_ppm(M) >= target {
                break;
            }
            // Jede Ausfuehrung braucht das Fuenffache des Profils.
            let overran = 5 * ONE > l.factor_ppm(M);
            if overran {
                overruns += 1;
            }
            l.observe(M, overran);
        }
        assert!(l.factor_ppm(M) >= target, "nicht angekommen");
        assert!(
            overruns <= 40,
            "{overruns} Ueberziehungen bis 450 % — das waeren keine Dutzende"
        );

        // Danach haelt er das p99 eines streuenden Stroms um das Fuenffache.
        let mut ratios = Ratios {
            state: 11,
            lo: 4_500,
            hi: 5_500,
        };
        let (factor, overrun_ppm) = converge(&mut l, &mut ratios, 40_000);
        let quantile = ratios.p99();
        assert!(
            factor * 100 >= quantile * 97 && factor * 100 <= quantile * 110,
            "mittlerer Faktor {factor} ppm, p99 {quantile} ppm"
        );
        assert!(
            (5_000..=15_000).contains(&overrun_ppm),
            "{overrun_ppm} ppm Ueberziehungen statt rund ein Prozent"
        );
    }

    /// Das Verhaeltnis der Schritte macht den Quantilschaetzer: bei einem
    /// Prozent Ziel wiegt eine Ueberziehung 99 eingehaltene Plaene auf.
    #[test]
    fn one_overrun_weighs_as_much_as_ninety_nine_kept_plans() {
        let mut l = learner(1);
        // Aus der Aufwaermphase heraus und weg von den Grenzen.
        l.observe(M, false);
        let before = l.factor_ppm(M);
        l.observe(M, true);
        for _ in 0..99 {
            l.observe(M, false);
        }
        let after = l.factor_ppm(M);
        let drift = after.abs_diff(before);
        assert!(
            drift * 1_000 <= before,
            "nach 1 : 99 zurueck auf {after} statt {before} — Abweichung ueber ein Promille"
        );
        // Und ohne die Ueberziehung waere er deutlich gesunken.
        let mut quiet = learner(1);
        quiet.observe(M, false);
        for _ in 0..100 {
            quiet.observe(M, false);
        }
        assert!(quiet.factor_ppm(M) * 100 < before * 99);
    }

    /// Ein Schritt hinauf und einer hinunter um dasselbe `x` heben sich auf.
    #[test]
    fn an_up_step_and_a_down_step_of_the_same_size_cancel() {
        for factor in [300_000, ONE, 1_100_000, 5_000_000] {
            let x = 7_777;
            let back = step(step(factor, true, x), false, x);
            assert!(back.abs_diff(factor) <= 2, "{factor} -> {back}");
        }
    }

    /// Vorsichtiger sofort, mutiger erst nach der Aufwaermphase.
    #[test]
    fn it_warms_up_like_the_linear_controller() {
        let mut l = learner(48);
        let start = l.factor_ppm(M);
        for _ in 0..47 {
            l.observe(M, false);
        }
        assert_eq!(
            l.factor_ppm(M),
            start,
            "vor 48 Ausfuehrungen wird er nicht mutiger"
        );
        l.observe(M, true);
        assert!(l.factor_ppm(M) > start, "eine Ueberziehung wirkt sofort");
        let raised = l.factor_ppm(M);
        l.observe(M, false);
        assert!(l.factor_ppm(M) < raised, "nach 48 darf er sinken");
    }

    /// Ein selten laufendes Modell erbt, was der Verkehr der anderen ueber
    /// die Karte gelernt hat.
    #[test]
    fn a_rare_model_inherits_the_device_factor() {
        let mut l = learner(48);
        let mut ratios = Ratios {
            state: 3,
            lo: 250,
            hi: 350,
        };
        let _ = converge(&mut l, &mut ratios, 20_000);
        let device = l.device;
        assert!(device < ONE, "der Geraetefaktor hat gelernt: {device}");
        assert_eq!(l.observations(N), 0);
        // Das zweite Modell lief nie, und sein Faktor ist trotzdem der
        // Geraetefaktor — nicht der Startwert von 110 %.
        assert_eq!(l.factor_ppm(N), device);
        assert!(l.factor_ppm(N) < 1_100_000);
    }

    /// Nie unter den harten Boden, nie ueber die Obergrenze — und an einer
    /// Grenze sammelt sich keine Schuld an.
    #[test]
    fn the_hard_limits_hold_and_store_no_debt() {
        let mut l = learner(1);
        for _ in 0..200_000 {
            l.observe(M, false);
        }
        assert_eq!(l.factor_ppm(M), 200_000, "harter Boden 20 %");
        let device = l.device;
        let residual = l.residual[0];
        for _ in 0..10_000 {
            l.observe(M, false);
        }
        assert_eq!(
            (l.device, l.residual[0]),
            (device, residual),
            "keine Schuld"
        );
        // Eine einzige Ueberziehung hebt ihn sofort ueber den Boden.
        l.observe(M, true);
        assert!(l.factor_ppm(M) > 200_000);

        for _ in 0..10_000 {
            l.observe(M, true);
        }
        assert_eq!(l.factor_ppm(M), 10_000_000, "Obergrenze 1000 %");
    }

    /// Ein nicht verifiziertes Profil beginnt erhoeht und wartet wieder auf
    /// eigene Beobachtungen, bevor es mutiger wird.
    #[test]
    fn an_unverified_profile_starts_high_and_warms_up_again() {
        let mut l = learner(48);
        l.mark_unverified(
            M,
            SafetyMargin::DEFAULT,
            MarginController::UNVERIFIED_SURCHARGE,
        );
        assert_eq!(l.margin(M).as_percent(), 150);
        for _ in 0..47 {
            l.observe(M, false);
        }
        assert_eq!(l.margin(M).as_percent(), 150);
    }

    #[test]
    fn the_range_is_validated() {
        assert!(MarginLearning::new(50, 1_000, 48).is_some());
        assert!(MarginLearning::new(9, 1_000, 48).is_none(), "unter 10 %");
        assert!(
            MarginLearning::new(101, 1_000, 48).is_none(),
            "Boden ueber 100 %"
        );
        assert!(
            MarginLearning::new(50, 99, 48).is_none(),
            "Obergrenze unter 100 %"
        );
        assert!(MarginLearning::new(50, 10_001, 48).is_none());
        assert!(
            MarginLearning::new(50, 1_000, 0).is_none(),
            "ohne Aufwaermen"
        );
        assert_eq!(
            MarginLearning::default_range(),
            MarginLearning::new(50, 1_000, 48).unwrap()
        );
    }
}
