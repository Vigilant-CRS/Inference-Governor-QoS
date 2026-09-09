//! Machbarkeitspruefung, Slack und Look-ahead (Spec 10.4, 10.7, 10.8; WP3).
//!
//! Zwei Fragen werden hier beantwortet:
//!
//! 1. **Ist dieser Request mit dieser Variante noch rechtzeitig machbar?**
//!    Nicht „ist die GPU frei", sondern „wann wird ein zulaessiger Slot frei,
//!    und reicht die Zeit danach noch" (ADR-0004).
//! 2. **Wuerde sein Start erwartbare, wichtigere Arbeit gefaehrden?**
//!    Das ist die Rechtfertigung fuer absichtliches Idle (Spec 10.7) — der
//!    Punkt, an dem Vigilant sich von einem work-conserving Scheduler trennt.
//!
//! ## Bewusst konservativ, bewusst grob
//!
//! Der Look-ahead simuliert nicht den vollstaendigen zukuenftigen Ablauf. Er
//! beantwortet nur: waere die erwartete geschuetzte Ankunft ohne diesen
//! Kandidaten machbar und mit ihm nicht? Nur dann wird vetoiert. Damit kann
//! das Veto niemals Arbeit blockieren, die ohnehin nicht zu retten war — ein
//! Scheduler, der aus Vorsicht idled, ohne etwas zu retten, waere schlechter
//! als FIFO.

use crate::arrayvec::ArrayVec;
use crate::ids::{MAX_MODELS, ModelIdx, RequestId, SlotIdx};
use crate::request::Criticality;
use crate::slots::SlotSet;
use crate::time::{Duration, Instant, Slack};

/// Standardhorizont des Look-ahead (Spec 10.8).
pub const DEFAULT_HORIZON: Duration = Duration::from_nanos_unbounded(100_000_000);

/// Das Ergebnis einer Machbarkeitspruefung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feasibility {
    /// Der Slot, auf dem gestartet wuerde.
    pub slot: SlotIdx,
    /// Der fruehestmoegliche Startzeitpunkt.
    pub start: Instant,
    /// Der prognostizierte Fertigstellungszeitpunkt.
    pub finish: Instant,
    /// Verbleibender Slack gegen die Deadline.
    ///
    /// `None`, wenn der Request keine Deadline traegt — dann ist er nie
    /// infeasible, sondern nur unterschiedlich dringend.
    pub slack: Option<Slack>,
}

impl Feasibility {
    /// Wahr, wenn die Deadline nach konservativer Planung noch haltbar ist.
    #[must_use]
    pub fn is_feasible(&self) -> bool {
        self.slack.is_none_or(|s| !s.is_infeasible())
    }

    /// Wie lange auf den Slot gewartet werden muesste.
    #[must_use]
    pub fn wait_from(&self, now: Instant) -> Duration {
        self.start.saturating_since(now)
    }
}

/// Eine erwartete zukuenftige Ankunft eines periodischen Modells.
///
/// Keine Zusage, dass der Request exakt dann eintrifft — eine
/// Scheduling-Prognose aus der konfigurierten Periode (Spec 10.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedArrival {
    /// Das erwartete logische Modell.
    pub model: ModelIdx,
    /// Die Wichtigkeitsklasse dieses Modells.
    pub criticality: Criticality,
    /// Der erwartete Ankunftszeitpunkt.
    pub at: Instant,
    /// Die absolute Deadline, die dieser Request dann haette.
    pub deadline: Instant,
    /// Die konservativ prognostizierte Laufzeit seiner besten Variante.
    pub runtime: Duration,
}

/// Prueft, ob ein Request mit gegebener Laufzeit noch rechtzeitig machbar ist.
///
/// Gibt `None` zurueck, wenn kein Slot dieses Modell je ausfuehren kann oder
/// alle in Frage kommenden Slots ihr Kreditkontingent erschoepft haben — in
/// beiden Faellen ist „jetzt nicht startbar" die richtige Antwort, nicht
/// „nicht machbar".
#[must_use]
pub fn evaluate(
    slots: &SlotSet,
    model: ModelIdx,
    now: Instant,
    runtime: Duration,
    deadline: Option<Instant>,
) -> Option<Feasibility> {
    let (slot, start) = slots.projected_start(model, now)?;
    let finish = start.checked_add(runtime)?;
    let slack = deadline.map(|d| d.signed_since(finish));
    Some(Feasibility {
        slot,
        start,
        finish,
        slack,
    })
}

/// Das Ergebnis der Look-ahead-Pruefung gegen erwartete geschuetzte Arbeit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardVerdict {
    /// Der Start ist unbedenklich.
    Clear,
    /// Der Start wuerde eine erwartete wichtigere Ankunft unmachbar machen.
    ///
    /// Der Scheduler soll dann warten — nicht ablehnen. Nach Ablauf der
    /// Blockade ist derselbe Request wieder ein Kandidat.
    WouldEndanger {
        /// Das gefaehrdete Modell.
        model: ModelIdx,
        /// Ab wann der Kandidat gefahrlos starten koennte.
        retry_after: Instant,
    },
}

/// Prueft, ob der Start eines Kandidaten erwartete wichtigere Arbeit gefaehrdet.
///
/// Nur Ankunftserwartungen innerhalb von `horizon` und mit **hoeherer**
/// Kritikalitaet als der Kandidat werden betrachtet. Ein Protected-Request
/// wird nie durch die Erwartung eines anderen Protected-Requests blockiert:
/// das waere kein Zulassungsproblem, sondern ein Schedulability-Problem, und
/// gehoert in den `doctor` (Spec 10.9), nicht in den Hot Path.
#[must_use]
pub fn guard_protected<'a, I>(
    slots: &SlotSet,
    candidate_model: ModelIdx,
    candidate_criticality: Criticality,
    candidate_runtime: Duration,
    now: Instant,
    forecast: I,
    horizon: Duration,
) -> GuardVerdict
where
    I: IntoIterator<Item = &'a ExpectedArrival>,
{
    let Some(limit) = now.checked_add(horizon) else {
        return GuardVerdict::Clear;
    };
    let Some(slot) = slots.ready_slot(candidate_model, now) else {
        return GuardVerdict::Clear;
    };

    // Der hypothetische Zustand nach dem Start des Kandidaten.
    let mut hypothetical = slots.snapshot();
    if hypothetical
        .dispatch(
            slot,
            RequestId(u64::MAX),
            candidate_model,
            now,
            candidate_runtime,
        )
        .is_err()
    {
        return GuardVerdict::Clear;
    }

    // Bis wann der Kandidat ueberhaupt Einfluss hat.
    //
    // Eine Ankunft, die **nach** seinem Ende erwartet wird, kann er nicht
    // verspaeten: der Slot ist dann wieder frei. Sie trotzdem zu pruefen
    // schreibt ihm die Verzoegerung zu, die die kumulative Reservierung der
    // *dazwischenliegenden* Ankuenfte erzeugt — und bestraft ihn fuer eine
    // Ueberlast, an der er unschuldig ist.
    //
    // Ohne diese Grenze staut sich der Pessimismus mit der Zahl der erwarteten
    // Ankuenfte im Horizont: bei 25-ms-Periode und 100-ms-Horizont sind das
    // vier, und die vierte ist praktisch immer knapp. Im Dauerlauf hat genau
    // das zwei von drei Stroemen dauerhaft ausgesperrt, waehrend jeder
    // Kurzlauf unauffaellig blieb.
    let Some(candidate_finish) = now.checked_add(candidate_runtime) else {
        return GuardVerdict::Clear;
    };

    // Die betrachteten Ankuenfte werden **kumulativ** reserviert, in der
    // Reihenfolge ihres Eintreffens. Wird jede einzeln gegen dieselbe leere
    // Belegung geprueft, passen zwei geschuetzte Jobs jeweils fuer sich und
    // reissen zusammen doch eine Deadline — der Guard sieht dann genau die
    // Ueberlastung nicht, gegen die er da ist. Die zeitliche Reihenfolge ist
    // dabei wesentlich: reservierte eine spaete Ankunft zuerst, bekaeme sie
    // einen Slot, der einer frueheren gehoert.
    let mut ordered: ArrayVec<ExpectedArrival, MAX_MODELS> = ArrayVec::new();
    for expected in forecast {
        if expected.at > limit || expected.criticality <= candidate_criticality {
            continue;
        }
        insert_by_arrival(&mut ordered, *expected);
    }

    // Der Vergleichszustand ohne Kandidat — ebenfalls kumulativ, sonst waere
    // die Frage „liegt es am Kandidaten?" gegen zwei verschiedene Massstaebe
    // gestellt.
    let mut baseline = slots.snapshot();

    for expected in ordered.iter() {
        // Sortiert nach Ankunftszeit: ab hier ist der Kandidat schon fertig,
        // und alles Weitere geht ihn nichts mehr an.
        if expected.at >= candidate_finish {
            break;
        }

        let without = feasible_for(&baseline, expected);
        let with = feasible_for(&hypothetical, expected);

        // Nur vetoieren, wenn der Kandidat die Ursache ist.
        if without && !with {
            let retry_after = hypothetical
                .projected_start(candidate_model, now)
                .map_or(expected.at, |(_, s)| s);
            return GuardVerdict::WouldEndanger {
                model: expected.model,
                retry_after,
            };
        }

        reserve(&mut baseline, expected);
        reserve(&mut hypothetical, expected);
    }
    GuardVerdict::Clear
}

/// Fuegt eine erwartete Ankunft sortiert nach Ankunftszeit ein.
///
/// Einfuegesortierung ueber hoechstens [`MAX_MODELS`] Elemente: der Look-ahead
/// laeuft im Hot Path, und eine Allokation waere hier teurer als der Vergleich.
///
/// Der Puffer kann nicht ueberlaufen: die Prognose enthaelt hoechstens einen
/// Eintrag je Modell, und mehr als [`MAX_MODELS`] Modelle gibt es nicht. Ein
/// Ueberlauf waere hier ausserdem die harmlose Richtung — eine uebersehene
/// Ankunft kostet ein Veto, sie erfindet keines. Der Guard wuerde dann weniger
/// schuetzen, aber niemals Arbeit blockieren, die gar nicht gefaehrdet ist.
fn insert_by_arrival(
    ordered: &mut ArrayVec<ExpectedArrival, MAX_MODELS>,
    expected: ExpectedArrival,
) {
    let mut at = ordered.len();
    for (i, existing) in ordered.iter().enumerate() {
        if expected.at < existing.at {
            at = i;
            break;
        }
    }
    let _ = ordered.insert(at, expected);
}

/// Belegt den Slot, den eine erwartete Ankunft voraussichtlich bekommt.
///
/// Findet sich kein zulaessiger Slot, bleibt der Zustand unveraendert. Das ist
/// die konservative Richtung: die Ankunft gilt dann als nicht reserviert und
/// nimmt niemandem etwas weg.
fn reserve(slots: &mut SlotSet, expected: &ExpectedArrival) {
    if let Some((slot, start)) = slots.projected_start(expected.model, expected.at)
        && let Some(until) = start.checked_add(expected.runtime)
    {
        slots.reserve_until(slot, until);
    }
}

/// Waere die erwartete Ankunft in diesem Belegungszustand machbar?
fn feasible_for(slots: &SlotSet, expected: &ExpectedArrival) -> bool {
    slots
        .projected_start(expected.model, expected.at)
        .and_then(|(_, start)| start.checked_add(expected.runtime))
        .is_some_and(|finish| finish <= expected.deadline)
}

/// Die absolute Deadline eines Requests aus seiner Generation Time.
///
/// Spec L-010 und Golden Test G-005: die Deadline haengt an der Capture-Zeit,
/// nicht an der Ankunft am Gateway. Ein bereits 25 ms alter Frame mit 30-ms-
/// Vertrag bekommt nicht noch einmal 30 ms.
///
/// Gibt `None` bei Ueberlauf zurueck; der Aufrufer muss den Request dann
/// ablehnen (Spec G-012).
#[must_use]
pub fn absolute_deadline(generation_time: Instant, relative: Duration) -> Option<Instant> {
    generation_time.checked_add(relative)
}
