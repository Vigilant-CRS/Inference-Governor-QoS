//! Variantenwahl mit Hysterese (Spec 12, WP6).
//!
//! Regel aus Spec 12.3: absteigend nach Qualitaet sortieren, die erste nach
//! konservativer Planung machbare Variante waehlen.
//!
//! Zwei Ergaenzungen machen daraus ein benutzbares Verhalten:
//!
//! * **Hysterese** (Spec 12.4). Abwertung darf sofort erfolgen, Aufwertung erst
//!   nach stabiler Reserve. Ohne das flattert die Wahl im Grenzbereich zwischen
//!   zwei Varianten und erzeugt genau die zeitliche Instabilitaet, die das
//!   Produkt beseitigen soll.
//! * **Herkunftspruefung** (ADR-0007). Ist die Qualitaetsherkunft unbekannt,
//!   findet keine automatische Wahl statt. Auf einer geratenen Zahl zu
//!   degradieren wuerde die Wahrnehmung verschlechtern, waehrend die eigenen
//!   Metriken gruen bleiben.

use crate::estimator::RuntimeEstimator;
use crate::feasibility::{Feasibility, evaluate};
use crate::ids::{ModelIdx, VariantIdx};
use crate::model::ModelContract;
use crate::profile::SafetyMargin;
use crate::slots::SlotSet;
use crate::time::Instant;

/// Eine gewaehlte Variante samt ihrer Machbarkeitsrechnung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantSelection {
    /// Die gewaehlte Variante.
    pub variant: VariantIdx,
    /// Startzeit, Fertigstellung und Slack dieser Wahl.
    pub feasibility: Feasibility,
}

/// Das Ergebnis der Variantenaufloesung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Eine machbare Variante wurde gefunden.
    Feasible(VariantSelection),
    /// Keine Variante ist rechtzeitig machbar.
    ///
    /// Die schnellste wird trotzdem benannt: fuer `NEVER_DROP` muss der
    /// Scheduler auch verspaetete Arbeit ausfuehren koennen (Spec 11.4), und
    /// fuer alle anderen ist dies die Grundlage der Ablehnungsmeldung.
    Infeasible {
        /// Die schnellste verfuegbare Variante.
        fastest: VariantSelection,
    },
    /// Kein Slot kann dieses Modell derzeit aufnehmen.
    ///
    /// Kein Machbarkeitsurteil — der Request bleibt Kandidat.
    NoSlot,
    /// Der Vertrag nennt keine brauchbare Variante.
    NoVariant,
}

/// Der Hysteresezustand eines logischen Modells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VariantState {
    current: Option<VariantIdx>,
    since: Instant,
}

impl VariantState {
    /// Die zuletzt gewaehlte Variante.
    #[must_use]
    pub const fn current(&self) -> Option<VariantIdx> {
        self.current
    }

    /// Uebernimmt eine Wahl und startet die Verweildauer neu, falls sie sich
    /// geaendert hat.
    pub fn record(&mut self, chosen: VariantIdx, now: Instant) {
        if self.current != Some(chosen) {
            self.current = Some(chosen);
            self.since = now;
        }
    }

    /// Wie lange die aktuelle Variante schon gehalten wird.
    #[must_use]
    pub fn dwelled(&self, now: Instant) -> crate::time::Duration {
        now.saturating_since(self.since)
    }
}

/// Alles, was eine Planungsentscheidung ausser dem Vertrag braucht.
///
/// Gebuendelt, weil die Bestandteile immer gemeinsam auftreten und einzeln
/// durchgereicht eine Parameterliste ergaeben, die niemand mehr liest.
#[derive(Debug, Clone, Copy)]
pub struct PlanningContext<'a> {
    /// Der Belegungszustand des Backends.
    pub slots: &'a SlotSet,
    /// Die beobachteten Laufzeiten.
    ///
    /// Ein frisch angelegter Schaetzer ohne Beobachtungen faellt vollstaendig
    /// auf die Offline-Profile zurueck — die Variantenwahl funktioniert also
    /// von der ersten Sekunde an.
    pub estimator: &'a RuntimeEstimator,
    /// Die aktuell wirksame Sicherheitsmarge.
    pub margin: SafetyMargin,
    /// Die aktuelle Zeit.
    pub now: Instant,
}

/// Waehlt die hoechstwertige machbare Variante.
///
/// `state` wird nur gelesen; die Uebernahme der Wahl macht der Aufrufer ueber
/// [`VariantState::record`], damit eine verworfene Planung den Hysteresezustand
/// nicht verschiebt.
///
/// `estimator` liefert die beobachteten Laufzeiten. Ein frisch angelegter
/// Schaetzer ohne Beobachtungen faellt vollstaendig auf die Offline-Profile
/// zurueck — die Variantenwahl funktioniert also von der ersten Sekunde an.
#[must_use]
pub fn resolve(
    contract: &ModelContract,
    state: &VariantState,
    model: ModelIdx,
    deadline: Option<Instant>,
    ctx: &PlanningContext<'_>,
) -> Resolution {
    let (slots, estimator, margin, now) = (ctx.slots, ctx.estimator, ctx.margin, ctx.now);
    if contract.variants.is_empty() {
        return Resolution::NoVariant;
    }
    let occupancy = slots.occupancy();

    // Ohne automatische Variantenwahl bleibt es bei der besten Variante; sie
    // wird nur noch auf Machbarkeit geprueft (ADR-0007).
    let considered = if contract.auto_variant_selection() {
        contract.variants.len()
    } else {
        1
    };

    let mut best_feasible: Option<VariantSelection> = None;
    let mut fastest: Option<VariantSelection> = None;
    let mut held: Option<VariantSelection> = None;
    let mut any_variant_considered = false;
    let mut any_slot_available = false;

    for i in 0..considered {
        let idx = VariantIdx(u16::try_from(i).unwrap_or(u16::MAX));
        if !contract.meets_min_quality(idx) {
            continue;
        }
        let Some(variant) = contract.variant(idx) else {
            continue;
        };
        any_variant_considered = true;

        // Der Online-Schaetzer darf die Planung verschaerfen, aber nie
        // optimistischer machen als das Profil (Spec 13.2).
        let Some(runtime) = estimator.conservative(model, idx, occupancy, &variant.profile, margin)
        else {
            continue;
        };
        let Some(feasibility) = evaluate(slots, model, now, runtime, deadline) else {
            continue;
        };
        any_slot_available = true;
        let selection = VariantSelection {
            variant: idx,
            feasibility,
        };

        // Die Variantenliste ist absteigend nach Qualitaet sortiert (durch
        // `ModelContract::validate` erzwungen). Die zuletzt betrachtete ist
        // damit die schnellste.
        fastest = Some(selection);

        if state.current() == Some(idx) {
            held = Some(selection);
        }
        if feasibility.is_feasible() && best_feasible.is_none() {
            best_feasible = Some(selection);
        }
    }

    match (best_feasible, fastest) {
        (Some(best), _) => Resolution::Feasible(apply_hysteresis(contract, state, now, best, held)),
        (None, Some(fastest)) => Resolution::Infeasible { fastest },
        (None, None) if any_variant_considered && !any_slot_available => Resolution::NoSlot,
        (None, None) => Resolution::NoVariant,
    }
}

/// Bremst Aufwertungen, laesst Abwertungen sofort zu (Spec 12.4).
///
/// Die Asymmetrie ist beabsichtigt: eine Abwertung rettet eine Deadline und
/// muss sofort wirken. Eine Aufwertung verbessert nur die Qualitaet und darf
/// warten, bis die Reserve stabil ist — sonst pendelt die Wahl im Grenzbereich
/// zwischen zwei Varianten hin und her.
fn apply_hysteresis(
    contract: &ModelContract,
    state: &VariantState,
    now: Instant,
    proposed: VariantSelection,
    held: Option<VariantSelection>,
) -> VariantSelection {
    let Some(current) = state.current() else {
        return proposed;
    };
    // Kleinerer Index bedeutet hoehere Qualitaet: das ist eine Aufwertung.
    if proposed.variant >= current {
        return proposed;
    }
    if state.dwelled(now) >= contract.variant_dwell {
        return proposed;
    }
    // Aufwertung zu frueh. Bei der bisherigen Variante bleiben, solange sie
    // ihre Deadline noch haelt; sonst greift die Aufwertung trotzdem, denn
    // eine verletzte Deadline waegt schwerer als ein stabiles Qualitaetsbild.
    match held {
        Some(current_selection) if current_selection.feasibility.is_feasible() => current_selection,
        _ => proposed,
    }
}
