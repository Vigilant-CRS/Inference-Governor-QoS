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
use crate::time::{Duration, Instant};

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
    /// Die zustandsabhaengige Prognose (NV-06).
    ///
    /// Im Schattenbetrieb aendert sie nichts. Scharf geschaltet ersetzt sie
    /// die konservative Schaetzung dort, wo eine belegte Zelle vorliegt —
    /// auch nach unten. Genau das ist der Gewinn: ein Profil, das unter einem
    /// Leistungslimit gemessen wurde, ist ohne dieses Limit dauerhaft zu
    /// pessimistisch.
    pub predictor: &'a crate::predictor::Predictor,
    /// Der beobachtete Hardwarezustand, ohne Belegungsgrad.
    ///
    /// Den Belegungsgrad ergaenzt [`resolve`] selbst — er gehoert zum
    /// geplanten Dispatch und nicht zur Beobachtung.
    pub state: crate::predictor::StateClass,
    /// Die Revision der Profilidentitaet.
    pub profile_revision: u32,
    /// Die aktuell wirksame Sicherheitsmarge.
    pub margin: SafetyMargin,
    /// Die aktuelle Zeit.
    pub now: Instant,
    /// Ob der Ueberlastregler aktive Abwertung verlangt (Spec 14.2).
    ///
    /// Im Normalbetrieb gewinnt die **hoechste** noch machbare Qualitaet: das
    /// ist der Sinn der Variantenwahl. Unter Ueberlast ist das die falsche
    /// Reihenfolge — dort zaehlt, wie viel Zeit die Entscheidung anderen
    /// Stroemen laesst. Dann gewinnt die **schnellste** Variante, die die
    /// Mindestqualitaet noch erfuellt.
    ///
    /// Die Mindestqualitaet bleibt in beiden Faellen unantastbar: eine
    /// Abwertung unter das fachlich Brauchbare waere kein Kompromiss, sondern
    /// ein unbrauchbares Ergebnis, das trotzdem GPU-Zeit kostet.
    pub degrade: bool,
    /// Die Restblockierung, die diese Planung traegt (ADR-0035).
    ///
    /// Null, solange keine praemptierbare Arbeit laeuft oder das geplante
    /// Modell selbst nicht geschuetzt ist — dann aendert sich keine
    /// Entscheidung.
    pub residual: crate::time::Duration,
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
    let mut fastest_feasible: Option<(VariantSelection, Duration)> = None;
    if contract.variants.is_empty() {
        return Resolution::NoVariant;
    }
    let occupancy = slots.occupancy();

    // Die beste **freigegebene** Variante ist der Ausgangspunkt (NV-02).
    // Freigabe und Qualitaet sind verschiedene Aussagen: eine Variante kann
    // ueber der Mindestqualitaet liegen und trotzdem nie zertifiziert worden
    // sein. Ist keine freigegeben, gibt es nichts zu waehlen — und das ist
    // eine Ablehnung, keine stille Aufwertung auf die naechstbeste.
    let Some(first) = (0..contract.variants.len())
        .map(|i| VariantIdx(u16::try_from(i).unwrap_or(u16::MAX)))
        .find(|idx| contract.variant_approved(*idx))
    else {
        return Resolution::NoVariant;
    };
    let start = first.get();

    // Ohne automatische Variantenwahl bleibt es bei dieser einen; sie wird
    // nur noch auf Machbarkeit geprueft (ADR-0007).
    let end = if contract.auto_variant_selection() {
        contract.variants.len()
    } else {
        start.saturating_add(1)
    };

    let mut best_feasible: Option<VariantSelection> = None;
    let mut fastest: Option<(VariantSelection, Duration)> = None;
    let mut held: Option<VariantSelection> = None;
    let mut any_variant_considered = false;
    let mut any_slot_available = false;

    for i in start..end {
        let idx = VariantIdx(u16::try_from(i).unwrap_or(u16::MAX));
        // Qualitaet **und** Freigabe, in einer Frage: die Reihenfolge zweier
        // Bedingungen zu vergessen ist der billigste Weg zu einer
        // unautorisierten Lockerung.
        if !contract.variant_usable(idx) {
            continue;
        }
        let Some(variant) = contract.variant(idx) else {
            continue;
        };
        any_variant_considered = true;

        // Der Online-Schaetzer darf die Planung verschaerfen, aber nie
        // optimistischer machen als das Profil (Spec 13.2).
        let Some(legacy) = estimator.conservative(model, idx, occupancy, &variant.profile, margin)
        else {
            continue;
        };
        // NV-06: scharf geschaltet gilt eine belegte Zelle, auch wenn sie
        // kuerzer ist. Im Schatten bleibt es beim bisherigen Weg. Die Marge
        // gilt fuer beide: die Zelle ersetzt das Profil, nicht die Marge.
        let backend_runtime = match ctx.predictor.mode() {
            crate::predictor::Mode::Shadow => legacy,
            crate::predictor::Mode::Active => {
                let mut state = ctx.state;
                state.occupancy = u8::try_from(occupancy).unwrap_or(u8::MAX);
                ctx.predictor
                    .predict(model, idx, state, ctx.profile_revision)
                    .with_margin(margin)
                    .runtime()
                    .unwrap_or(legacy)
            }
        };
        // ADR-0035: laeuft praemptierbare Hintergrundarbeit, traegt diese
        // geschuetzte Arbeit deren gemessene Restblockierung. Als Untergrenze
        // ueber dem Alleinwert und nicht als Zuschlag: eine Zelle, die die
        // Ueberlappung schon beobachtet hat, zahlte sie sonst zweimal.
        let backend_runtime = if ctx.residual > crate::time::Duration::ZERO {
            estimator
                .conservative(model, idx, 0, &variant.profile, margin)
                .and_then(|solo| solo.checked_add(ctx.residual))
                .map_or(backend_runtime, |floor| backend_runtime.max(floor))
        } else {
            backend_runtime
        };
        // NV-10: Vor- und Nachverarbeitung entsteht ausserhalb des Backends
        // und faellt aus jedem Backendprofil heraus. Sie gehoert trotzdem in
        // die Planung — zwei Varianten mit verschiedener Eingabeaufloesung
        // unterscheiden sich hier oft mehr als in der Inferenz selbst.
        let Some(runtime) = backend_runtime.checked_add(variant.preprocess) else {
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

        // Die **gemessen** schnellste, nicht die zuletzt betrachtete. Die
        // Liste ist absteigend nach Qualitaet sortiert, nicht nach Laufzeit —
        // dass niedrigere Qualitaet auch schneller ist, gilt fuer eine
        // Aufloesungsreihe, aber nicht allgemein: ein kleineres Modell ohne
        // passenden Kernel kann auf derselben Hardware langsamer sein. Wer im
        // infeasiblen Fall die Qualitaetsreihenfolge fuer die Laufzeit haelt,
        // verschenkt dort Qualitaet, wo er nicht einmal Zeit gewinnt.
        let is_fastest = fastest.is_none_or(|(_, best)| runtime < best);
        if is_fastest {
            fastest = Some((selection, runtime));
        }

        if state.current() == Some(idx) {
            held = Some(selection);
        }
        if feasibility.is_feasible() {
            if best_feasible.is_none() {
                best_feasible = Some(selection);
            }
            if fastest_feasible.is_none_or(|(_, best)| runtime < best) {
                fastest_feasible = Some((selection, runtime));
            }
        }
    }

    // Unter Abwertung gewinnt die schnellste machbare Variante, nicht die
    // beste. Beide erfuellen `min_quality` — die Auswahlschleife laesst nichts
    // anderes zu.
    let chosen = if ctx.degrade {
        fastest_feasible
            .map(|(selection, _)| selection)
            .or(best_feasible)
    } else {
        best_feasible
    };

    match (chosen, fastest) {
        (Some(best), _) => Resolution::Feasible(apply_hysteresis(contract, state, now, best, held)),
        (None, Some((fastest, _))) => Resolution::Infeasible { fastest },
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
