//! Online Runtime Estimator und Margenanpassung (Spec 13.2, 13.3, 30.3; WP11).
//!
//! Ein Offline-Profil altert. Die Hardware taktet ab, ein Nachbarmodell zieht
//! Speicherbandbreite, ein Treiberwechsel verschiebt alles um zehn Prozent. Der
//! Scheduler muss darauf reagieren, **ohne die Ursache zu kennen** — genau die
//! Argumentation aus Spec 13.2: wird die Hardware langsamer, steigt die
//! gemessene Laufzeit, und die Planung folgt dem Effekt statt der Ursache.
//!
//! ## Drei Bausteine
//!
//! * **[`RuntimeEstimator`]** — gleitendes Fenster beobachteter Laufzeiten je
//!   Modell, Variante und Slot-Belegungsgrad. Der Belegungsgrad gehoert dazu,
//!   weil er der Ersatz fuer die Interferenzmatrix ist (ADR-0006).
//! * **[`MarginController`]** — lernt die Sicherheitsmarge je Modell. Nach
//!   einer Unterprognose steigt sie schnell, nach einem eingehaltenen Plan
//!   faellt sie langsam — und zwar so, dass im Gleichgewicht genau der
//!   vereinbarte Anteil der Ausfuehrungen ueberzieht (ADR-0034). Der teurere
//!   Fehler wird schneller korrigiert; wie viel schneller, sagt das Ziel.
//! * **[`ProfileHealth`]** — der Circuit Breaker aus Spec 30.3. Liegt die
//!   Wirklichkeit dauerhaft weit neben dem Profil, ist das kein Fall fuer eine
//!   immer groessere Marge, sondern fuer eine Meldung.
//!
//! ## Warum die Quantile beim Schreiben berechnet werden
//!
//! Gelesen wird bei jeder Planungsentscheidung, geschrieben nur bei jeder
//! Fertigstellung. Das Sortieren gehoert deshalb auf die Schreibseite, damit
//! der Entscheidungspfad ein Tabellenzugriff bleibt (Spec 8.1).

use crate::ids::{MAX_MODELS, MAX_SLOTS, MAX_VARIANTS, ModelIdx, VariantIdx};
use crate::profile::{SafetyMargin, VariantProfile};
use crate::time::Duration;

/// Groesse des gleitenden Beobachtungsfensters je Zelle.
///
/// 64 Messwerte sind genug, um ein p95 zu stuetzen, und wenig genug, damit das
/// Fenster einer Aenderung innerhalb weniger Sekunden folgt. Fuer ein echtes
/// p99 waeren sie zu wenig — deshalb wird online p95 gefuehrt und das p99
/// weiterhin dem Offline-Profil entnommen.
pub const WINDOW: usize = 64;

/// Mindestzahl Beobachtungen, bevor eine Zelle ueberhaupt zaehlt.
///
/// Darunter waere jede Aussage Rauschen, und der Scheduler wuerde auf einen
/// einzelnen Ausreisser reagieren.
pub const MIN_OBSERVATIONS: usize = 16;

/// Der Gesundheitszustand eines Profils (Spec 30.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileHealth {
    /// Beobachtung und Profil passen zusammen.
    #[default]
    Fresh,
    /// Die Wirklichkeit liegt dauerhaft deutlich ueber dem Profil.
    ///
    /// Der Scheduler soll dann konservativer planen und die groesste Variante
    /// aus der automatischen Auswahl nehmen; eine immer weiter wachsende Marge
    /// waere die falsche Antwort auf ein kaputtes Profil.
    Degraded,
}

/// Eine Zelle: die Beobachtungen zu einem Modell, einer Variante und einem
/// Belegungsgrad.
#[derive(Debug, Clone, Copy)]
struct Cell {
    samples: [u32; WINDOW],
    len: usize,
    next: usize,
    p50_micros: u32,
    p95_micros: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            samples: [0; WINDOW],
            len: 0,
            next: 0,
            p50_micros: 0,
            p95_micros: 0,
        }
    }
}

impl Cell {
    fn record(&mut self, micros: u32) {
        if let Some(slot) = self.samples.get_mut(self.next) {
            *slot = micros;
        }
        self.next = self.next.saturating_add(1) % WINDOW;
        self.len = self.len.saturating_add(1).min(WINDOW);
        self.recompute();
    }

    /// Quantile neu berechnen. Laeuft auf der Schreibseite, nicht im
    /// Entscheidungspfad.
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
        self.p50_micros = quantile_of(slice, 50);
        self.p95_micros = quantile_of(slice, 95);
    }

    const fn is_ready(&self) -> bool {
        self.len >= MIN_OBSERVATIONS
    }
}

fn quantile_of(sorted: &[u32], percent: usize) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let index = sorted
        .len()
        .saturating_mul(percent)
        .checked_div(100)
        .unwrap_or(0)
        .min(sorted.len().saturating_sub(1));
    sorted.get(index).copied().unwrap_or(0)
}

/// Der Schaetzer ueber alle Modelle, Varianten und Belegungsgrade.
#[derive(Debug)]
pub struct RuntimeEstimator {
    cells: Vec<Cell>,
}

impl RuntimeEstimator {
    /// Legt einen leeren Schaetzer an.
    ///
    /// Die Tabelle wird einmal beim Start allokiert und danach nie wieder —
    /// der Entscheidungspfad bleibt allokationsfrei (Spec 8.1).
    #[must_use]
    pub fn new() -> Self {
        Self {
            cells: vec![Cell::default(); MAX_MODELS * MAX_VARIANTS * MAX_SLOTS],
        }
    }

    fn index(model: ModelIdx, variant: VariantIdx, occupancy: usize) -> Option<usize> {
        let m = model.get();
        let v = variant.get();
        let o = occupancy;
        if m >= MAX_MODELS || v >= MAX_VARIANTS || o >= MAX_SLOTS {
            return None;
        }
        m.checked_mul(MAX_VARIANTS)?
            .checked_add(v)?
            .checked_mul(MAX_SLOTS)?
            .checked_add(o)
    }

    /// Nimmt eine beobachtete Backendlaufzeit auf.
    pub fn record(
        &mut self,
        model: ModelIdx,
        variant: VariantIdx,
        occupancy: usize,
        observed: Duration,
    ) {
        let Some(index) = Self::index(model, variant, occupancy) else {
            return;
        };
        let micros = u32::try_from(observed.as_micros()).unwrap_or(u32::MAX);
        if let Some(cell) = self.cells.get_mut(index) {
            cell.record(micros);
        }
    }

    /// Die Anzahl Beobachtungen einer Zelle.
    #[must_use]
    pub fn observations(&self, model: ModelIdx, variant: VariantIdx, occupancy: usize) -> usize {
        Self::index(model, variant, occupancy)
            .and_then(|i| self.cells.get(i))
            .map_or(0, |cell| cell.len)
    }

    /// Das beobachtete p95, sofern genuegend Messwerte vorliegen.
    #[must_use]
    pub fn observed_p95(
        &self,
        model: ModelIdx,
        variant: VariantIdx,
        occupancy: usize,
    ) -> Option<Duration> {
        let cell = Self::index(model, variant, occupancy).and_then(|i| self.cells.get(i))?;
        if !cell.is_ready() {
            return None;
        }
        Some(Duration::from_nanos_unbounded(
            u64::from(cell.p95_micros).saturating_mul(1_000),
        ))
    }

    /// Das beobachtete p50, sofern genuegend Messwerte vorliegen.
    #[must_use]
    pub fn observed_p50(
        &self,
        model: ModelIdx,
        variant: VariantIdx,
        occupancy: usize,
    ) -> Option<Duration> {
        let cell = Self::index(model, variant, occupancy).and_then(|i| self.cells.get(i))?;
        if !cell.is_ready() {
            return None;
        }
        Some(Duration::from_nanos_unbounded(
            u64::from(cell.p50_micros).saturating_mul(1_000),
        ))
    }

    /// Die konservative Planungslaufzeit (Spec 13.2).
    ///
    /// `max(offline_p99, online_p95) * margin`. Das Maximum und nicht der
    /// neuere Wert: der Schaetzer darf die Planung verschaerfen, aber nie
    /// optimistischer machen als das Profil. Wer sein Profil unterbieten will,
    /// misst es neu.
    #[must_use]
    pub fn conservative(
        &self,
        model: ModelIdx,
        variant: VariantIdx,
        occupancy: usize,
        offline: &VariantProfile,
        margin: SafetyMargin,
    ) -> Option<Duration> {
        let base = offline.at_occupancy(occupancy)?.p99;
        let effective = match self.observed_p95(model, variant, occupancy) {
            Some(online) => base.max(online),
            None => base,
        };
        margin.apply(effective)
    }

    /// Die optimistische Schaetzung fuer die Verwerfensentscheidung (ADR-0010).
    ///
    /// Auch hier das Maximum aus Profil und Beobachtung: eine optimistische
    /// Schaetzung, die *zu* optimistisch ist, laesst Arbeit laufen, die
    /// garantiert wertlos ankommt.
    #[must_use]
    pub fn optimistic(
        &self,
        model: ModelIdx,
        variant: VariantIdx,
        occupancy: usize,
        offline: &VariantProfile,
    ) -> Option<Duration> {
        let base = offline.at_occupancy(occupancy)?.p50;
        Some(match self.observed_p50(model, variant, occupancy) {
            Some(online) => base.max(online),
            None => base,
        })
    }

    /// Der Gesundheitszustand eines Profils (Spec 30.3).
    ///
    /// `Degraded`, wenn die beobachtete Laufzeit dauerhaft mehr als das
    /// Doppelte des Profil-p99 betraegt. Dann ist nicht die Marge zu klein,
    /// sondern das Profil falsch.
    #[must_use]
    pub fn health(
        &self,
        model: ModelIdx,
        variant: VariantIdx,
        occupancy: usize,
        offline: &VariantProfile,
    ) -> ProfileHealth {
        let (Some(observed), Some(profile)) = (
            self.observed_p50(model, variant, occupancy),
            offline.at_occupancy(occupancy),
        ) else {
            return ProfileHealth::Fresh;
        };
        if observed.as_nanos() > profile.p99.as_nanos().saturating_mul(2) {
            ProfileHealth::Degraded
        } else {
            ProfileHealth::Fresh
        }
    }
}

impl Default for RuntimeEstimator {
    fn default() -> Self {
        Self::new()
    }
}

/// Die selbst lernende Sicherheitsmarge eines Modells (Spec 13.3, ADR-0034).
///
/// ## Ein Regler mit Ziel
///
/// Die Marge steigt nach jeder Ausfuehrung, die ihre geplante Laufzeit
/// ueberzogen hat, und sinkt nach jeder, die es nicht tat. Wie weit, sagt
/// **ein Ziel**: der Anteil der Ausfuehrungen, die ueberziehen duerfen.
///
/// ```text
/// Ueberziehung:  + G * (1 - Ziel)
/// sonst:         - G * Ziel
/// ```
///
/// Im Gleichgewicht heben sich beide auf, und das ist genau dann der Fall,
/// wenn der Anteil der Ueberziehungen gleich dem Ziel ist. Das ist die
/// bekannte Online-Schaetzung eines Quantils, nur auf die Marge statt auf
/// den Messwert angewandt: der Regler findet die Marge, unter der der Plan
/// das gewuenschte Quantil der tatsaechlichen Laufzeit ist.
///
/// Die Fassung davor stieg um 10 und sank um 1 Prozentpunkt. Ihr
/// Gleichgewicht lag bei 10·p = 1·(1-p), also bei **p = 1/11 — jede elfte
/// Ausfuehrung ueberzog ihren Plan**, und niemand hatte diese Zahl gewaehlt.
/// Sie ergab sich aus zwei Schrittweiten.
///
/// ## Wo das Ziel herkommt
///
/// Aus dem **ausdruecklich vereinbarten** Missbudget des Vertrags, wenn es
/// eines gibt (`M/K`, begrenzt auf [`Self::MAX_TARGET_PERMILLE`]), sonst ein
/// Prozent: die Planung beginnt beim Profil-p99, und ein Prozent
/// Ueberziehung heisst, dass der Plan auch im Betrieb ein p99 bleibt.
///
/// Warum je Modell und nicht je Kamera: die Laufzeit eines Modells haengt
/// nicht davon ab, welcher Sensor das Bild geliefert hat. Welche Kamera
/// wichtiger ist, sagt der Vertrag, nicht die Marge.
///
/// ## Was bleibt
///
/// Die konfigurierte Marge ist der Boden. Der Regler darf vorsichtiger
/// werden als der Betreiber, nie leichtsinniger.
#[derive(Debug, Clone, Copy)]
pub struct MarginController {
    /// Die aktuelle Marge in Hundertstelprozent.
    ///
    /// Feiner als die Marge selbst, weil ein Ziel von einem Prozent Schritte
    /// von einem Zehntelprozentpunkt braucht.
    basis_points: u32,
    floor: u32,
    ceiling: u32,
    /// Wie viele Ausfuehrungen je tausend ihren Plan ueberziehen duerfen.
    target_permille: u32,
}

impl MarginController {
    /// Die Verstaerkung in Hundertstelprozent: so weit steigt die Marge nach
    /// einer Ueberziehung, wenn das Ziel null waere (10 Prozentpunkte).
    ///
    /// Gross genug, dass eine einzelne Ueberziehung spuerbar vorsichtiger
    /// macht; die Asymmetrie zum Absenken ergibt sich aus dem Ziel und muss
    /// nicht zusaetzlich eingestellt werden.
    pub const GAIN_BASIS_POINTS: u32 = 1_000;
    /// Das Ziel ohne Vertragsangabe: ein Prozent.
    pub const DEFAULT_TARGET_PERMILLE: u32 = 10;
    /// Das hoechste Ziel, auch wenn der Vertrag mehr Misses erlaubt.
    ///
    /// Ein Missbudget zaehlt verpasste Verbraucherzyklen, nicht ueberzogene
    /// Plaene. Ein grosszuegiges Budget soll die Marge nicht so weit
    /// loesen, dass die Planung ihren Namen verliert.
    pub const MAX_TARGET_PERMILLE: u32 = 50;
    /// Aufschlag fuer ein nicht verifiziertes Profil (G-010, ADR-0016), in
    /// Prozentpunkten.
    ///
    /// Der Wert muss nicht richtig sein, weil der Estimator ihn korrigiert,
    /// sobald er eigene Beobachtungen hat. Er muss nur deutlich konservativ
    /// und begrenzt sein: zu gross kostet Durchsatz, zu klein waere ein
    /// stillschweigend falsches Versprechen — und genau das verbietet G-010.
    pub const UNVERIFIED_SURCHARGE: u32 = 40;

    /// Startet bei der uebergebenen Marge, mit dem Ziel von einem Prozent.
    #[must_use]
    pub fn new(start: SafetyMargin) -> Self {
        let basis_points = start.as_percent().saturating_mul(100);
        Self {
            basis_points,
            floor: basis_points,
            ceiling: SafetyMargin::MAX_PERCENT.saturating_mul(100),
            target_permille: Self::DEFAULT_TARGET_PERMILLE,
        }
    }

    /// Startet erhoeht, weil das Profil nicht verifiziert werden konnte.
    ///
    /// Der Boden bleibt die konfigurierte Marge: sobald der Online Estimator
    /// genug eigene Messungen hat, darf er bis dorthin zurueckregeln. Ein
    /// unbestaetigtes Profil ist ein Grund zur Vorsicht, kein Dauerurteil —
    /// und nach wenigen Sekunden Betrieb misst das System ohnehin selbst.
    #[must_use]
    pub fn provisional(configured: SafetyMargin) -> Self {
        let floor = configured.as_percent().saturating_mul(100);
        Self {
            basis_points: configured
                .as_percent()
                .saturating_add(Self::UNVERIFIED_SURCHARGE)
                .min(SafetyMargin::MAX_PERCENT)
                .saturating_mul(100),
            floor,
            ceiling: SafetyMargin::MAX_PERCENT.saturating_mul(100),
            target_permille: Self::DEFAULT_TARGET_PERMILLE,
        }
    }

    /// Dasselbe mit einem anderen Ziel, begrenzt auf `1..=MAX_TARGET_PERMILLE`.
    ///
    /// Null ist kein Ziel: ein Regler, der keine einzige Ueberziehung
    /// hinnimmt, sinkt nie und steigt mit jeder — er waere eine Ratsche.
    #[must_use]
    pub const fn with_target_permille(mut self, permille: u32) -> Self {
        self.target_permille = if permille == 0 {
            1
        } else if permille > Self::MAX_TARGET_PERMILLE {
            Self::MAX_TARGET_PERMILLE
        } else {
            permille
        };
        self
    }

    /// Das Ziel aus einem vereinbarten Missbudget: `M/K` in Promille.
    #[must_use]
    pub fn target_from_budget(budget: crate::contract_ext::MissBudget) -> u32 {
        let permille = u64::from(budget.max_misses)
            .saturating_mul(1_000)
            .checked_div(u64::from(budget.window_cycles))
            .unwrap_or(u64::from(Self::DEFAULT_TARGET_PERMILLE));
        u32::try_from(permille).unwrap_or(u32::MAX)
    }

    /// Das Ziel dieses Reglers, in Promille.
    #[must_use]
    pub const fn target_permille(&self) -> u32 {
        self.target_permille
    }

    /// Die aktuelle Marge, auf ganze Prozent **aufgerundet**.
    ///
    /// Aufgerundet, weil die Marge eine Sicherheitsgroesse ist: ein Rest von
    /// einem halben Prozentpunkt gehoert zur Vorsicht, nicht zum Mut.
    #[must_use]
    pub fn margin(&self) -> SafetyMargin {
        SafetyMargin::from_percent(self.basis_points.div_ceil(100)).unwrap_or(SafetyMargin::DEFAULT)
    }

    /// Die aktuelle Marge in Hundertstelprozent, fuer Pruefungen.
    #[must_use]
    pub const fn basis_points(&self) -> u32 {
        self.basis_points
    }

    /// Reagiert auf eine Ausfuehrung, die ihren Plan ueberzogen hat.
    pub fn tighten(&mut self) {
        let step = Self::scaled(1_000_u32.saturating_sub(self.target_permille));
        self.basis_points = self.basis_points.saturating_add(step).min(self.ceiling);
    }

    /// Reagiert auf eine Ausfuehrung, die ihren Plan eingehalten hat.
    ///
    /// Nie unter den konfigurierten Startwert: der ist eine bewusste
    /// Entscheidung des Betreibers und keine Obergrenze, die der Regler
    /// unterbieten darf.
    pub fn relax(&mut self) {
        let step = Self::scaled(self.target_permille);
        self.basis_points = self.basis_points.saturating_sub(step).max(self.floor);
    }

    /// `GAIN_BASIS_POINTS * permille / 1000`.
    fn scaled(permille: u32) -> u32 {
        Self::GAIN_BASIS_POINTS
            .saturating_mul(permille)
            .checked_div(1_000)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;
    use crate::profile::RuntimeProfile;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    fn offline(p50: u64, p99: u64) -> VariantProfile {
        VariantProfile::solo(
            RuntimeProfile::new(ms(p50), ms(u64::midpoint(p50, p99)), ms(p99), 1_000).unwrap(),
        )
    }

    const M: ModelIdx = ModelIdx(0);
    const V: VariantIdx = VariantIdx(0);

    /// Ein nicht verifiziertes Profil wird vorsichtiger geplant — und der
    /// Estimator darf genau bis zur konfigurierten Marge zurueck, nie darunter.
    #[test]
    fn provisional_margin_starts_high_and_relaxes_to_the_configured_floor() {
        let configured = SafetyMargin::from_percent(110).unwrap();
        let mut controller = MarginController::provisional(configured);
        assert_eq!(
            controller.margin().as_percent(),
            110 + MarginController::UNVERIFIED_SURCHARGE
        );

        for _ in 0..1_000 {
            controller.relax();
        }
        assert_eq!(controller.margin().as_percent(), 110);
    }

    /// Der Aufschlag darf die Obergrenze nicht ueberschreiten.
    #[test]
    fn provisional_margin_stays_within_the_ceiling() {
        let high = SafetyMargin::from_percent(SafetyMargin::MAX_PERCENT).unwrap();
        let controller = MarginController::provisional(high);
        assert_eq!(controller.margin().as_percent(), SafetyMargin::MAX_PERCENT);
    }

    /// Ein Zyklus aus einer Ueberziehung und 99 eingehaltenen Plaenen bringt
    /// die Marge bei einem Ziel von einem Prozent genau dorthin zurueck, wo
    /// sie war — das ist das Gleichgewicht, und es liegt beim Ziel.
    #[test]
    fn at_the_target_rate_the_margin_holds_still() {
        let mut controller = MarginController::new(SafetyMargin::from_percent(110).unwrap());
        // Erst ueber den Boden heben, sonst verdeckt der Boden das Absenken.
        for _ in 0..5 {
            controller.tighten();
        }
        let start = controller.basis_points();
        for _ in 0..10 {
            controller.tighten();
            for _ in 0..99 {
                controller.relax();
            }
        }
        assert_eq!(controller.basis_points(), start);
    }

    /// Doppelt so viele Ueberziehungen wie erlaubt: die Marge steigt.
    #[test]
    fn above_the_target_rate_the_margin_rises() {
        let mut controller = MarginController::new(SafetyMargin::from_percent(110).unwrap());
        let start = controller.basis_points();
        for _ in 0..10 {
            controller.tighten();
            controller.tighten();
            for _ in 0..98 {
                controller.relax();
            }
        }
        assert!(controller.basis_points() > start);
    }

    /// Die Rate, bei der die alte Fassung stillhielt — jede elfte Ausfuehrung
    /// ueberzieht —, ist jetzt kein Gleichgewicht mehr, sondern ein Grund,
    /// deutlich vorsichtiger zu werden.
    #[test]
    fn one_overrun_in_eleven_is_no_longer_accepted() {
        let mut controller = MarginController::new(SafetyMargin::from_percent(110).unwrap());
        for _ in 0..20 {
            controller.tighten();
            for _ in 0..10 {
                controller.relax();
            }
        }
        assert!(
            controller.margin().as_percent() >= 250,
            "{}",
            controller.margin().as_percent()
        );
    }

    /// Das Ziel kommt aus einem vereinbarten Missbudget, begrenzt nach oben;
    /// null ist kein Ziel.
    #[test]
    fn the_target_follows_the_contract_within_bounds() {
        use crate::contract_ext::MissBudget;
        let two_in_hundred = MissBudget {
            max_misses: 2,
            window_cycles: 100,
            max_consecutive: Some(1),
        };
        assert_eq!(MarginController::target_from_budget(two_in_hundred), 20);

        let base = MarginController::new(SafetyMargin::DEFAULT);
        assert_eq!(base.target_permille(), 10);
        assert_eq!(base.with_target_permille(800).target_permille(), 50);
        assert_eq!(base.with_target_permille(0).target_permille(), 1);
    }

    /// Ein einzelner Ausreisser darf die Planung nicht verschieben.
    #[test]
    fn too_few_observations_are_ignored() {
        let mut estimator = RuntimeEstimator::new();
        estimator.record(M, V, 0, ms(500));
        assert_eq!(estimator.observations(M, V, 0), 1);
        assert_eq!(estimator.observed_p95(M, V, 0), None);

        let profile = offline(10, 16);
        assert_eq!(
            estimator
                .conservative(M, V, 0, &profile, SafetyMargin::NONE)
                .unwrap()
                .as_millis(),
            16,
            "solange nichts gesichert ist, gilt das Profil"
        );
    }

    /// Spec 13.2: `max(offline, online)`. Der Schaetzer darf verschaerfen,
    /// aber nie optimistischer werden als das Profil.
    #[test]
    fn the_estimator_may_tighten_but_never_loosen() {
        let profile = offline(10, 16);

        // Backend wird langsamer: 30 ms statt 10 ms.
        let mut slower = RuntimeEstimator::new();
        for _ in 0..MIN_OBSERVATIONS {
            slower.record(M, V, 0, ms(30));
        }
        assert_eq!(
            slower
                .conservative(M, V, 0, &profile, SafetyMargin::NONE)
                .unwrap()
                .as_millis(),
            30,
            "die Beobachtung setzt sich durch"
        );

        // Backend ist schneller als das Profil: das Profil bleibt massgeblich.
        let mut faster = RuntimeEstimator::new();
        for _ in 0..MIN_OBSERVATIONS {
            faster.record(M, V, 0, ms(2));
        }
        assert_eq!(
            faster
                .conservative(M, V, 0, &profile, SafetyMargin::NONE)
                .unwrap()
                .as_millis(),
            16,
            "wer sein Profil unterbieten will, misst es neu"
        );
    }

    /// ADR-0006: der Belegungsgrad ist der Ersatz fuer die Interferenzmatrix.
    #[test]
    fn observations_are_kept_per_occupancy_level() {
        let mut estimator = RuntimeEstimator::new();
        for _ in 0..MIN_OBSERVATIONS {
            estimator.record(M, V, 0, ms(10));
            estimator.record(M, V, 1, ms(25));
        }
        assert_eq!(estimator.observed_p50(M, V, 0).unwrap().as_millis(), 10);
        assert_eq!(
            estimator.observed_p50(M, V, 1).unwrap().as_millis(),
            25,
            "unter Nebenlast wird dieselbe Variante langsamer"
        );
    }

    /// Das Fenster gleitet: nach genug neuen Werten sind die alten weg.
    #[test]
    fn the_window_forgets_old_observations() {
        let mut estimator = RuntimeEstimator::new();
        for _ in 0..WINDOW {
            estimator.record(M, V, 0, ms(50));
        }
        assert_eq!(estimator.observed_p50(M, V, 0).unwrap().as_millis(), 50);

        for _ in 0..WINDOW {
            estimator.record(M, V, 0, ms(10));
        }
        assert_eq!(
            estimator.observed_p50(M, V, 0).unwrap().as_millis(),
            10,
            "eine ueberstandene Lastspitze darf nicht dauerhaft nachwirken"
        );
    }

    /// Spec 30.3: liegt die Wirklichkeit dauerhaft weit neben dem Profil, ist
    /// nicht die Marge zu klein, sondern das Profil falsch.
    #[test]
    fn a_hopeless_profile_is_reported_instead_of_compensated() {
        let profile = offline(10, 16);
        let mut estimator = RuntimeEstimator::new();
        for _ in 0..MIN_OBSERVATIONS {
            estimator.record(M, V, 0, ms(20));
        }
        assert_eq!(estimator.health(M, V, 0, &profile), ProfileHealth::Fresh);

        let mut broken = RuntimeEstimator::new();
        for _ in 0..MIN_OBSERVATIONS {
            broken.record(M, V, 0, ms(90));
        }
        assert_eq!(broken.health(M, V, 0, &profile), ProfileHealth::Degraded);
    }

    /// Spec 13.3: schnell straffen, langsam entspannen, harte Grenzen.
    ///
    /// Wie viel langsamer, sagt seit ADR-0034 das Ziel: bei einem Prozent
    /// steigt die Marge um 9,9 Prozentpunkte und sinkt um 0,1.
    #[test]
    fn the_margin_tightens_fast_and_relaxes_slowly() {
        let mut controller = MarginController::new(SafetyMargin::DEFAULT);
        assert_eq!(controller.margin().as_percent(), 110);

        controller.tighten();
        assert_eq!(controller.margin().as_percent(), 120);
        let raised = controller.basis_points();

        controller.relax();
        assert_eq!(
            raised.saturating_sub(controller.basis_points()),
            10,
            "Entspannung ist langsamer: ein Zehntelprozentpunkt je eingehaltenem Plan"
        );

        for _ in 0..1_000 {
            controller.relax();
        }
        assert_eq!(
            controller.margin().as_percent(),
            110,
            "nie unter den vom Betreiber gesetzten Startwert"
        );

        for _ in 0..1_000 {
            controller.tighten();
        }
        assert_eq!(
            controller.margin().as_percent(),
            SafetyMargin::MAX_PERCENT,
            "und nie ueber die harte Obergrenze"
        );
    }
}
