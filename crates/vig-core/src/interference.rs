//! Gerichtete Interferenz zwischen Modellen (NV-11).
//!
//! ## Was bisher da war
//!
//! `no_corun`: eine Liste von Modellpaaren, die nicht gleichzeitig laufen
//! duerfen (ADR-0006). Symmetrisch, binaer, und aus einer Heuristik gewonnen —
//! wenn ein Modell das andere auf die doppelte Laufzeit bremst, gilt das Paar
//! als unvereinbar. Das ist als Anfang richtig und in drei Punkten zu grob:
//!
//! 1. **Interferenz ist nicht symmetrisch.** Ein 95-ms-VLM verlaengert einen
//!    5-ms-Detektor um ein Vielfaches seiner eigenen Laufzeit; der Detektor
//!    verlaengert den VLM um wenige Prozent. Eine symmetrische Tabelle muss
//!    sich fuer eine der beiden Zahlen entscheiden und ist damit fuer die
//!    andere falsch.
//! 2. **Ein Verhaeltnis ist keine Kosten.** „Faktor 2" heisst bei 5 ms etwas
//!    anderes als bei 95 ms. Geplant wird mit absoluten Zeiten, also gehoeren
//!    absolute Zeiten in die Tabelle.
//! 3. **Paardaten sagen nichts ueber drei.** Zwei Nachbarn kosten nicht die
//!    Summe der Einzelnachbarn — sie koennen weniger kosten, wenn sie sich
//!    gegenseitig verdraengen, und mehr, wenn sie zusammen eine Grenze reissen,
//!    die keiner allein erreicht.
//!
//! ## Die Regel dieses Moduls
//!
//! **Nichts wird hochgerechnet.** Fuer einen Nachbarn gilt der gemessene
//! Paarwert. Fuer zwei oder mehr gilt nur eine **ausdruecklich gemessene**
//! Kombination; gibt es keine, ist die Antwort „nicht gemessen" und nicht eine
//! Summe. Ein addierter Laufzeitbound sieht aus wie eine Zahl und ist eine
//! Erfindung.
//!
//! **Nicht gemessen heisst nicht „kostenlos".** Der Aufrufer bekommt einen
//! eigenen Befund und entscheidet daraus konservativ — im Zweifel
//! serialisieren, wie bisher. Eine fehlende Messung darf nicht als Null in
//! eine Prognose fliessen.
//!
//! **Die Konfliktart wird mitgefuehrt.** Zwei Modelle, die um Rechenwerke
//! streiten, verhalten sich anders als zwei, die um Speicherbandbreite
//! streiten — und wieder anders als zwei, deren Phasen sich ueberlappen. Fuer
//! die Planung ist das heute eine Zahl; fuer den Betreiber ist es der
//! Unterschied zwischen „mehr Slots helfen" und „mehr Slots helfen nicht".

use crate::ids::{MAX_MODELS, ModelIdx};
use crate::time::Duration;

/// Woran zwei Modelle sich behindern.
///
/// Fuer die Planung heute nur Dokumentation. Fuer den Betreiber der
/// Unterschied zwischen „mehr Slots helfen" und „mehr Slots helfen nicht".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictKind {
    /// Nicht bestimmt.
    #[default]
    Unspecified,
    /// Beide wollen Rechenwerke.
    Compute,
    /// Beide wollen Speicherbandbreite.
    MemoryBandwidth,
    /// Beide wollen Geraetespeicher.
    MemoryCapacity,
    /// Die Phasen ueberlappen sich — etwa Kopieren gegen Rechnen.
    Phase,
}

/// Was ueber die Zusatzkosten eines Betriebspunkts bekannt ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterferenceVerdict {
    /// Allein; keine Zusatzkosten.
    Alone,
    /// Gemessen.
    Measured {
        /// Die zusaetzliche Laufzeit.
        added: Duration,
        /// Woran es liegt, soweit bestimmt.
        kind: ConflictKind,
    },
    /// Fuer dieses Paar liegt keine Messung vor.
    UnmeasuredPair {
        /// Wer leidet.
        victim: ModelIdx,
        /// Wer stoert.
        co_tenant: ModelIdx,
    },
    /// Mehrere Nachbarn, und diese Kombination wurde nicht gemessen.
    ///
    /// Ausdruecklich **kein** hochgerechneter Wert: zwei Nachbarn kosten nicht
    /// die Summe der Einzelnachbarn.
    NotExtrapolated {
        /// Wie viele Nachbarn.
        co_tenants: usize,
    },
}

impl InterferenceVerdict {
    /// Die Zusatzkosten, falls sie belegt sind.
    ///
    /// `None` heisst „nicht belegt" und nicht „null". Wer daraus null macht,
    /// plant eine Messung ein, die es nicht gibt.
    #[must_use]
    pub const fn added(&self) -> Option<Duration> {
        match self {
            Self::Alone => Some(Duration::from_nanos_unbounded(0)),
            Self::Measured { added, .. } => Some(*added),
            Self::UnmeasuredPair { .. } | Self::NotExtrapolated { .. } => None,
        }
    }

    /// Ob der Betriebspunkt belegt ist.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Alone | Self::Measured { .. })
    }
}

/// Ein gemessener Betriebspunkt mit mehreren Nachbarn.
#[derive(Debug, Clone, Copy)]
struct Combination {
    victim: ModelIdx,
    /// Bitmaske der Nachbarn.
    co_tenants: u32,
    added_us: u32,
    kind: ConflictKind,
}

/// Wie viele Mehrfachkombinationen gespeichert werden.
///
/// Eine Obergrenze, weil die Zahl moeglicher Kombinationen mit der
/// Modellanzahl exponentiell waechst. Wer mehr messen will, misst gezielt —
/// eine vollstaendige Abdeckung ist bei acht Modellen bereits jenseits jedes
/// Messbudgets (Spec L-003).
pub const MAX_COMBINATIONS: usize = 64;

/// Die gerichtete Interferenztabelle.
#[derive(Debug, Clone)]
pub struct Interference {
    /// `added_us[victim][co_tenant]`, in Mikrosekunden.
    added_us: Vec<u32>,
    /// Ob dieses Paar gemessen wurde.
    measured: Vec<bool>,
    /// Die Konfliktart je Paar.
    kinds: Vec<ConflictKind>,
    /// Ausdruecklich gemessene Mehrfachkombinationen.
    combinations: Vec<Combination>,
}

impl Default for Interference {
    fn default() -> Self {
        Self::new()
    }
}

impl Interference {
    /// Eine leere Tabelle.
    #[must_use]
    pub fn new() -> Self {
        let cells = MAX_MODELS.saturating_mul(MAX_MODELS);
        Self {
            added_us: vec![0; cells],
            measured: vec![false; cells],
            kinds: vec![ConflictKind::Unspecified; cells],
            combinations: Vec::new(),
        }
    }

    const fn index(victim: ModelIdx, co_tenant: ModelIdx) -> Option<usize> {
        let v = victim.get();
        let c = co_tenant.get();
        if v >= MAX_MODELS || c >= MAX_MODELS {
            return None;
        }
        match v.checked_mul(MAX_MODELS) {
            Some(base) => base.checked_add(c),
            None => None,
        }
    }

    /// Traegt eine gerichtete Paarmessung ein.
    ///
    /// `added` ist die **zusaetzliche** Laufzeit von `victim`, wenn
    /// `co_tenant` gleichzeitig laeuft — nicht die Gesamtlaufzeit und nicht
    /// ein Faktor. `record_pair(a, b, x)` sagt nichts ueber
    /// `record_pair(b, a, ...)`; beide Richtungen sind eigene Messungen.
    pub fn record_pair(
        &mut self,
        victim: ModelIdx,
        co_tenant: ModelIdx,
        added: Duration,
        kind: ConflictKind,
    ) {
        let Some(index) = Self::index(victim, co_tenant) else {
            return;
        };
        let micros = u32::try_from(added.as_micros()).unwrap_or(u32::MAX);
        if let Some(cell) = self.added_us.get_mut(index) {
            *cell = micros;
        }
        if let Some(cell) = self.measured.get_mut(index) {
            *cell = true;
        }
        if let Some(cell) = self.kinds.get_mut(index) {
            *cell = kind;
        }
    }

    /// Traegt eine gemessene Mehrfachkombination ein.
    ///
    /// Nur so kommt der Governor zu einer Aussage ueber drei gleichzeitig
    /// laufende Modelle: durch Messung, nicht durch Rechnung.
    pub fn record_combination(
        &mut self,
        victim: ModelIdx,
        co_tenants: &[ModelIdx],
        added: Duration,
        kind: ConflictKind,
    ) {
        if co_tenants.len() < 2 || self.combinations.len() >= MAX_COMBINATIONS {
            return;
        }
        let Some(mask) = mask_of(co_tenants) else {
            return;
        };
        let entry = Combination {
            victim,
            co_tenants: mask,
            added_us: u32::try_from(added.as_micros()).unwrap_or(u32::MAX),
            kind,
        };
        if let Some(existing) = self
            .combinations
            .iter_mut()
            .find(|c| c.victim == victim && c.co_tenants == mask)
        {
            *existing = entry;
            return;
        }
        self.combinations.push(entry);
    }

    /// Wie viele Paare gemessen sind.
    #[must_use]
    pub fn measured_pairs(&self) -> usize {
        self.measured.iter().filter(|m| **m).count()
    }

    /// Wie viele Mehrfachkombinationen gemessen sind.
    #[must_use]
    pub fn measured_combinations(&self) -> usize {
        self.combinations.len()
    }

    /// Was die Zusatzkosten fuer `victim` bei diesen Nachbarn sind.
    ///
    /// Fuer keinen Nachbarn: [`InterferenceVerdict::Alone`]. Fuer einen: der
    /// gemessene Paarwert oder [`InterferenceVerdict::UnmeasuredPair`]. Fuer
    /// mehrere: nur eine ausdruecklich gemessene Kombination, sonst
    /// [`InterferenceVerdict::NotExtrapolated`].
    ///
    /// `victim` selbst wird aus der Nachbarliste entfernt — ein Modell stoert
    /// sich nicht selbst, auch wenn zwei seiner Instanzen laufen. Das waere
    /// eine Aussage ueber Instanzen und gehoert in den Belegungsgrad.
    #[must_use]
    pub fn lookup(&self, victim: ModelIdx, co_tenants: &[ModelIdx]) -> InterferenceVerdict {
        let mut others: [ModelIdx; MAX_MODELS] = [ModelIdx(0); MAX_MODELS];
        let mut count = 0_usize;
        for candidate in co_tenants {
            if *candidate == victim || candidate.get() >= MAX_MODELS {
                continue;
            }
            if others
                .get(..count)
                .is_some_and(|seen| seen.contains(candidate))
            {
                continue;
            }
            if let Some(slot) = others.get_mut(count) {
                *slot = *candidate;
                count = count.saturating_add(1);
            }
        }
        let Some(others) = others.get(..count) else {
            return InterferenceVerdict::Alone;
        };

        match others {
            [] => InterferenceVerdict::Alone,
            [single] => {
                let Some(index) = Self::index(victim, *single) else {
                    return InterferenceVerdict::UnmeasuredPair {
                        victim,
                        co_tenant: *single,
                    };
                };
                if self.measured.get(index).copied().unwrap_or(false) {
                    InterferenceVerdict::Measured {
                        added: Duration::from_nanos_unbounded(
                            u64::from(self.added_us.get(index).copied().unwrap_or(0))
                                .saturating_mul(1_000),
                        ),
                        kind: self.kinds.get(index).copied().unwrap_or_default(),
                    }
                } else {
                    InterferenceVerdict::UnmeasuredPair {
                        victim,
                        co_tenant: *single,
                    }
                }
            }
            many => {
                let Some(mask) = mask_of(many) else {
                    return InterferenceVerdict::NotExtrapolated {
                        co_tenants: many.len(),
                    };
                };
                match self
                    .combinations
                    .iter()
                    .find(|c| c.victim == victim && c.co_tenants == mask)
                {
                    Some(found) => InterferenceVerdict::Measured {
                        added: Duration::from_nanos_unbounded(
                            u64::from(found.added_us).saturating_mul(1_000),
                        ),
                        kind: found.kind,
                    },
                    // Kein hochgerechneter Wert. Ein addierter Bound sieht aus
                    // wie eine Zahl und ist eine Erfindung.
                    None => InterferenceVerdict::NotExtrapolated {
                        co_tenants: many.len(),
                    },
                }
            }
        }
    }

    /// Ob dieses Paar so teuer ist, dass Nebenlaeufigkeit sich nicht lohnt.
    ///
    /// **Eine Heuristik mit einer Annahme, keine Durchsatzaussage.** Sie
    /// stimmt fuer zwei Auftraege aehnlicher Laenge: kostet der Nachbar mehr
    /// Zusatzzeit, als das Modell allein braucht, dann brauchen zwei Auftraege
    /// nebeneinander laenger als nacheinander, und die Latenz ist obendrein
    /// schlechter.
    ///
    /// Sie stimmt **nicht** allgemein. Bei sehr unterschiedlichen Laufzeiten
    /// kann Nebenlaeufigkeit den Durchsatz erhoehen, obwohl der kurze Auftrag
    /// stark leidet — und ob das gut ist, entscheidet der Vertrag und nicht
    /// diese Funktion. Sie liefert einen Vorschlag fuer `no_corun`; die
    /// Entscheidung trifft der Betreiber.
    #[must_use]
    pub fn concurrency_unprofitable(
        &self,
        victim: ModelIdx,
        co_tenant: ModelIdx,
        solo: Duration,
    ) -> Option<bool> {
        let added = match self.lookup(victim, &[co_tenant]) {
            InterferenceVerdict::Measured { added, .. } => added,
            InterferenceVerdict::Alone => Duration::from_nanos_unbounded(0),
            InterferenceVerdict::UnmeasuredPair { .. }
            | InterferenceVerdict::NotExtrapolated { .. } => return None,
        };
        Some(added.as_nanos() >= solo.as_nanos())
    }
}

/// Die Bitmaske einer Nachbarmenge.
///
/// `None`, wenn ein Index ausserhalb liegt — eine halb gebildete Maske wuerde
/// eine andere Kombination treffen als gemeint.
fn mask_of(models: &[ModelIdx]) -> Option<u32> {
    let mut mask = 0_u32;
    for model in models {
        let bit = model.get();
        if bit >= 32 {
            return None;
        }
        mask |= 1_u32 << bit;
    }
    Some(mask)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    const DETECTOR: ModelIdx = ModelIdx(0);
    const POSE: ModelIdx = ModelIdx(1);
    const VLM: ModelIdx = ModelIdx(2);

    // -- Richtung ----------------------------------------------------------

    #[test]
    fn interference_is_directed_and_stays_directed() {
        // Das Gegenbeispiel aus Dokument 02: der VLM verlaengert den Detektor
        // um ein Vielfaches seiner eigenen Laufzeit, der Detektor den VLM um
        // wenige Prozent. Eine symmetrische Tabelle muesste sich fuer eine
        // der beiden Zahlen entscheiden.
        let mut table = Interference::new();
        table.record_pair(DETECTOR, VLM, ms(90), ConflictKind::Compute);
        table.record_pair(VLM, DETECTOR, ms(4), ConflictKind::Compute);

        assert_eq!(
            table.lookup(DETECTOR, &[VLM]).added(),
            Some(ms(90)),
            "der Detektor leidet stark"
        );
        assert_eq!(
            table.lookup(VLM, &[DETECTOR]).added(),
            Some(ms(4)),
            "der VLM kaum"
        );
    }

    #[test]
    fn one_direction_measured_does_not_measure_the_other() {
        let mut table = Interference::new();
        table.record_pair(DETECTOR, VLM, ms(90), ConflictKind::Compute);
        assert!(table.lookup(DETECTOR, &[VLM]).is_known());
        assert_eq!(
            table.lookup(VLM, &[DETECTOR]),
            InterferenceVerdict::UnmeasuredPair {
                victim: VLM,
                co_tenant: DETECTOR,
            }
        );
    }

    // -- Absolute Kosten ---------------------------------------------------

    #[test]
    fn the_table_holds_absolute_time_not_a_factor() {
        let mut table = Interference::new();
        table.record_pair(DETECTOR, POSE, ms(3), ConflictKind::MemoryBandwidth);
        let InterferenceVerdict::Measured { added, kind } = table.lookup(DETECTOR, &[POSE]) else {
            panic!("gemessen erwartet");
        };
        assert_eq!(added.as_millis(), 3);
        assert_eq!(kind, ConflictKind::MemoryBandwidth);
    }

    #[test]
    fn the_conflict_kind_survives() {
        let mut table = Interference::new();
        table.record_pair(DETECTOR, POSE, ms(3), ConflictKind::Phase);
        table.record_pair(DETECTOR, VLM, ms(90), ConflictKind::MemoryCapacity);
        assert!(matches!(
            table.lookup(DETECTOR, &[POSE]),
            InterferenceVerdict::Measured {
                kind: ConflictKind::Phase,
                ..
            }
        ));
        assert!(matches!(
            table.lookup(DETECTOR, &[VLM]),
            InterferenceVerdict::Measured {
                kind: ConflictKind::MemoryCapacity,
                ..
            }
        ));
    }

    // -- Nichts hochrechnen -----------------------------------------------

    #[test]
    fn two_neighbours_are_not_the_sum_of_two_pairs() {
        // Der Kern von NV-11: aus zwei Paarmessungen folgt keine Aussage
        // ueber drei gleichzeitig laufende Modelle.
        let mut table = Interference::new();
        table.record_pair(DETECTOR, POSE, ms(3), ConflictKind::Compute);
        table.record_pair(DETECTOR, VLM, ms(90), ConflictKind::Compute);
        assert_eq!(
            table.lookup(DETECTOR, &[POSE, VLM]),
            InterferenceVerdict::NotExtrapolated { co_tenants: 2 },
            "93 ms waere eine Erfindung, die aussieht wie eine Messung"
        );
        assert_eq!(table.lookup(DETECTOR, &[POSE, VLM]).added(), None);
    }

    #[test]
    fn a_measured_combination_answers_for_three() {
        let mut table = Interference::new();
        table.record_pair(DETECTOR, POSE, ms(3), ConflictKind::Compute);
        table.record_pair(DETECTOR, VLM, ms(90), ConflictKind::Compute);
        // Gemessen, und ausdruecklich **weniger** als die Summe: die beiden
        // Nachbarn verdraengen sich gegenseitig.
        table.record_combination(DETECTOR, &[POSE, VLM], ms(88), ConflictKind::Compute);
        assert_eq!(table.lookup(DETECTOR, &[POSE, VLM]).added(), Some(ms(88)));
        assert_eq!(table.measured_combinations(), 1);
    }

    #[test]
    fn the_combination_order_does_not_matter() {
        let mut table = Interference::new();
        table.record_combination(DETECTOR, &[POSE, VLM], ms(88), ConflictKind::Compute);
        assert_eq!(table.lookup(DETECTOR, &[VLM, POSE]).added(), Some(ms(88)));
    }

    #[test]
    fn a_combination_for_one_victim_says_nothing_about_another() {
        let mut table = Interference::new();
        table.record_combination(DETECTOR, &[POSE, VLM], ms(88), ConflictKind::Compute);
        assert_eq!(
            table.lookup(POSE, &[DETECTOR, VLM]),
            InterferenceVerdict::NotExtrapolated { co_tenants: 2 }
        );
    }

    #[test]
    fn a_second_measurement_of_the_same_combination_replaces_the_first() {
        let mut table = Interference::new();
        table.record_combination(DETECTOR, &[POSE, VLM], ms(88), ConflictKind::Compute);
        table.record_combination(DETECTOR, &[POSE, VLM], ms(95), ConflictKind::Compute);
        assert_eq!(table.measured_combinations(), 1);
        assert_eq!(table.lookup(DETECTOR, &[POSE, VLM]).added(), Some(ms(95)));
    }

    #[test]
    fn a_pair_recorded_as_a_combination_is_refused() {
        // Ein Paar gehoert in die gerichtete Tabelle, nicht in die
        // Kombinationsliste — sonst gibt es zwei Wahrheiten fuer denselben
        // Betriebspunkt.
        let mut table = Interference::new();
        table.record_combination(DETECTOR, &[VLM], ms(90), ConflictKind::Compute);
        assert_eq!(table.measured_combinations(), 0);
    }

    // -- Nicht gemessen ist nicht kostenlos --------------------------------

    #[test]
    fn an_unmeasured_pair_is_not_free() {
        let table = Interference::new();
        assert_eq!(
            table.lookup(DETECTOR, &[VLM]).added(),
            None,
            "wer daraus null macht, plant eine Messung ein, die es nicht gibt"
        );
        assert!(!table.lookup(DETECTOR, &[VLM]).is_known());
    }

    #[test]
    fn running_alone_costs_nothing_and_that_is_measured() {
        let table = Interference::new();
        assert_eq!(table.lookup(DETECTOR, &[]), InterferenceVerdict::Alone);
        assert_eq!(
            table.lookup(DETECTOR, &[]).added(),
            Some(Duration::from_nanos_unbounded(0))
        );
        assert!(table.lookup(DETECTOR, &[]).is_known());
    }

    #[test]
    fn a_model_does_not_disturb_itself() {
        let table = Interference::new();
        assert_eq!(
            table.lookup(DETECTOR, &[DETECTOR]),
            InterferenceVerdict::Alone,
            "zwei Instanzen desselben Modells sind eine Aussage ueber den \
             Belegungsgrad, nicht ueber Interferenz"
        );
    }

    #[test]
    fn a_repeated_neighbour_counts_once() {
        let mut table = Interference::new();
        table.record_pair(DETECTOR, VLM, ms(90), ConflictKind::Compute);
        assert_eq!(table.lookup(DETECTOR, &[VLM, VLM]).added(), Some(ms(90)));
    }

    // -- Die 2x-Regel als Heuristik ---------------------------------------

    #[test]
    fn the_doubling_heuristic_needs_a_measurement() {
        let table = Interference::new();
        assert_eq!(
            table.concurrency_unprofitable(DETECTOR, VLM, ms(5)),
            None,
            "ohne Messung gibt es keinen Vorschlag, auch keinen vorsichtigen"
        );
    }

    #[test]
    fn the_doubling_heuristic_fires_at_the_stated_line() {
        let mut table = Interference::new();
        table.record_pair(DETECTOR, VLM, ms(5), ConflictKind::Compute);
        assert_eq!(
            table.concurrency_unprofitable(DETECTOR, VLM, ms(5)),
            Some(true),
            "Zusatzzeit gleich Sololaufzeit ist genau die Verdoppelung"
        );
        assert_eq!(
            table.concurrency_unprofitable(DETECTOR, VLM, ms(6)),
            Some(false)
        );
    }

    #[test]
    fn the_heuristic_says_nothing_about_unequal_job_lengths() {
        // Der VLM leidet kaum unter dem Detektor, also schlaegt die
        // Heuristik hier nichts vor — und das ist richtig, denn ob der
        // Durchsatzgewinn den Latenzverlust des Detektors wert ist,
        // entscheidet der Vertrag.
        let mut table = Interference::new();
        table.record_pair(VLM, DETECTOR, ms(4), ConflictKind::Compute);
        assert_eq!(
            table.concurrency_unprofitable(VLM, DETECTOR, ms(95)),
            Some(false)
        );
    }

    // -- Grenzen ----------------------------------------------------------

    #[test]
    fn the_combination_list_is_bounded() {
        let mut table = Interference::new();
        for i in 0..(MAX_COMBINATIONS + 20) {
            let victim = ModelIdx(u16::try_from(i % MAX_MODELS).unwrap_or(0));
            table.record_combination(victim, &[POSE, VLM], ms(1), ConflictKind::Compute);
        }
        assert!(table.measured_combinations() <= MAX_COMBINATIONS);
    }

    #[test]
    fn an_index_outside_the_table_is_refused_not_wrapped() {
        let mut table = Interference::new();
        let far = ModelIdx(u16::try_from(MAX_MODELS).unwrap_or(0));
        table.record_pair(far, VLM, ms(10), ConflictKind::Compute);
        assert_eq!(table.measured_pairs(), 0);
        assert_eq!(table.lookup(far, &[VLM]).added(), None);
    }

    #[test]
    fn the_pair_table_counts_directions_separately() {
        let mut table = Interference::new();
        table.record_pair(DETECTOR, VLM, ms(90), ConflictKind::Compute);
        assert_eq!(table.measured_pairs(), 1);
        table.record_pair(VLM, DETECTOR, ms(4), ConflictKind::Compute);
        assert_eq!(table.measured_pairs(), 2);
    }
}
