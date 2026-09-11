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
/// Scheduling-Prognose aus der konfigurierten Periode (Spec 10.8). Je
/// bewachtem Modell genau eine: die naechste, gleich wie weit sie weg ist
/// (ADR-0036).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedArrival {
    /// Das erwartete logische Modell.
    pub model: ModelIdx,
    /// Die Wichtigkeitsklasse dieses Modells.
    pub criticality: Criticality,
    /// Der erwartete Ankunftszeitpunkt.
    pub at: Instant,
    /// Die absolute Deadline, die dieser Request dann haette.
    ///
    /// Ab seiner erwarteten **Aufnahme**, nicht ab der Ankunft — wie die
    /// Deadline, die der Dispatch fuer denselben Frame rechnet (Spec L-010,
    /// ADR-0036).
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
/// Nur Ankunftserwartungen mit **hoeherer** Kritikalitaet als der Kandidat
/// werden betrachtet, und nur solche, die er verspaeten kann: die vor seinem
/// Ende eintreffen. Einen festen Horizont gibt es nicht mehr (ADR-0036) — er
/// liess einen Strom mit einer Periode ueber dem Horizont gegen jede Arbeit
/// ungeschuetzt, die ueber seine naechste Ankunft hinwegreichte.
///
/// Ein Protected-Request wird nie durch die Erwartung eines anderen
/// Protected-Requests blockiert:
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
) -> GuardVerdict
where
    I: IntoIterator<Item = &'a ExpectedArrival>,
{
    guard_protected_with_residual(
        slots,
        candidate_model,
        candidate_criticality,
        candidate_runtime,
        now,
        forecast,
        Duration::ZERO,
    )
}

/// Wie [`guard_protected`], fuer einen Kandidaten, dessen Arbeit die
/// geschuetzte unterbricht (ADR-0035).
///
/// Ein praemptierbarer Kandidat belegt keinen geschuetzten Slot; er
/// verspaetet eine geschuetzte Ankunft nicht um seine Laufzeit, sondern
/// um die gemessene Restblockierung `candidate_residual`. Die Frage lautet
/// dann nicht mehr „passen 90 ms in die Luecke", sondern „passt die
/// Restblockierung in den Slack der geschuetzten Arbeit".
///
/// Gerechnet wird fuer **alle** erwarteten Ankuenfte, nicht nur fuer die vor
/// dem geplanten Ende: ein unterbrochener Auftrag endet spaeter, als seine
/// Laufzeit sagt, und belastet solange jede Ankunft.
///
/// Mit `candidate_residual = 0` ist das exakt [`guard_protected`].
#[must_use]
pub fn guard_protected_with_residual<'a, I>(
    slots: &SlotSet,
    candidate_model: ModelIdx,
    candidate_criticality: Criticality,
    candidate_runtime: Duration,
    now: Instant,
    forecast: I,
    candidate_residual: Duration,
) -> GuardVerdict
where
    I: IntoIterator<Item = &'a ExpectedArrival>,
{
    let preemptible = candidate_residual > Duration::ZERO;
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
        if expected.criticality <= candidate_criticality {
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
        // und alles Weitere geht ihn nichts mehr an. Ein praemptierbarer
        // Kandidat ist es nicht — unterbrochen endet er spaeter als geplant.
        if !preemptible && expected.at >= candidate_finish {
            break;
        }

        // Mit dem Kandidaten laeuft die geschuetzte Ankunft um seine
        // Restblockierung laenger. Ohne Praemption ist das null.
        let mut burdened = *expected;
        burdened.runtime = Duration::from_nanos_unbounded(
            expected
                .runtime
                .as_nanos()
                .saturating_add(candidate_residual.as_nanos()),
        );

        let without = feasible_for(&baseline, expected);
        let with = feasible_for(&hypothetical, &burdened);

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
        reserve(&mut hypothetical, &burdened);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;
    use crate::slots::ModelMask;

    const DETECTOR: ModelIdx = ModelIdx(0);
    const VLM: ModelIdx = ModelIdx(1);

    fn ms(v: u64) -> Duration {
        Duration::from_nanos_unbounded(v.saturating_mul(1_000_000))
    }

    fn at(v: u64) -> Instant {
        Instant::from_nanos(v.saturating_mul(1_000_000))
    }

    /// Ein Detektor, der in 10 ms kommt, 15 ms rechnet und 33 ms Frist hat.
    fn detector_soon() -> ExpectedArrival {
        ExpectedArrival {
            model: DETECTOR,
            criticality: Criticality::Protected,
            at: at(10),
            deadline: at(43),
            runtime: ms(15),
        }
    }

    fn with_lane() -> SlotSet {
        let mut slots = SlotSet::homogeneous(1, 0).unwrap();
        slots
            .add_preemptible_lanes(1, ModelMask::NONE.with(VLM))
            .unwrap();
        slots
    }

    /// ADR-0012: auf einem einzigen Slot verhindert ein 90-ms-Block die
    /// naechste geschuetzte Ankunft — der Look-ahead haelt ihn zurueck.
    #[test]
    fn a_non_preemptible_block_is_held_back() {
        let slots = SlotSet::homogeneous(1, 0).unwrap();
        let verdict = guard_protected(
            &slots,
            VLM,
            Criticality::BestEffort,
            ms(90),
            at(0),
            [detector_soon()].iter(),
        );
        assert!(matches!(verdict, GuardVerdict::WouldEndanger { .. }));
    }

    /// ADR-0035: auf einer Spur, mit 5 ms gemessener Restblockierung, passt
    /// derselbe Auftrag — der Detektor braucht 20 statt 15 ms und haelt seine
    /// Frist.
    #[test]
    fn a_preemptible_job_starts_when_its_residual_fits() {
        let verdict = guard_protected_with_residual(
            &with_lane(),
            VLM,
            Criticality::BestEffort,
            ms(90),
            at(0),
            [detector_soon()].iter(),
            ms(5),
        );
        assert_eq!(verdict, GuardVerdict::Clear);
    }

    /// Passt die Restblockierung nicht in den Slack, bleibt es beim Veto:
    /// 15 + 30 ms ab 10 ms enden nach der Frist bei 43 ms.
    #[test]
    fn a_preemptible_job_is_held_back_when_its_residual_does_not_fit() {
        let verdict = guard_protected_with_residual(
            &with_lane(),
            VLM,
            Criticality::BestEffort,
            ms(90),
            at(0),
            [detector_soon()].iter(),
            ms(30),
        );
        assert!(matches!(
            verdict,
            GuardVerdict::WouldEndanger {
                model: DETECTOR,
                ..
            }
        ));
    }

    /// Ein unterbrochener Auftrag endet spaeter als geplant; eine Ankunft
    /// nach seinem nominellen Ende traegt die Restblockierung trotzdem.
    #[test]
    fn a_preemptible_job_burdens_arrivals_after_its_nominal_end() {
        let late = ExpectedArrival {
            at: at(60),
            deadline: at(80),
            ..detector_soon()
        };
        // 20 ms Laufzeit, 50 ms Frist: nominell ist der Kandidat bei 50 ms
        // fertig. Mit 10 ms Restblockierung braucht der Detektor 25 ms und
        // reisst ab 60 ms seine Frist bei 80 ms.
        let verdict = guard_protected_with_residual(
            &with_lane(),
            VLM,
            Criticality::BestEffort,
            ms(50),
            at(0),
            [late].iter(),
            ms(10),
        );
        assert!(matches!(verdict, GuardVerdict::WouldEndanger { .. }));
    }
}
