//! Invarianten aus dem Codereview vom 07.09.2026.
//!
//! Jeder Test hier hat einmal einen echten Fehler nachgewiesen. Sie stehen
//! bewusst zusammen und nicht verteilt in den Golden Tests: die Golden Tests
//! pruefen die Zusagen der Spezifikation, diese hier pruefen die Stellen, an
//! denen die Umsetzung von ihnen abgewichen ist. Beides braucht es.
//!
//! Am Ende stehen drei Kontrollen und ein Gegenbeispiel — sie sollen zeigen,
//! dass die Reparaturen nicht die jeweils entgegengesetzte Regel ueberdehnt
//! haben. Ein Test, der nur bestaetigt, was er messen soll, belegt wenig.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use vig_core::arrayvec::ArrayVec;
use vig_core::feasibility::{ExpectedArrival, GuardVerdict, guard_protected};
use vig_core::model::{Cooperative, ModelContract, Quality, QualityValue, Variant};
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use vig_core::queue::{ModelQueue, QueueConfig};
use vig_core::request::OverflowPolicy;
use vig_core::scheduler::{Action, Event, Scheduler};
use vig_core::slots::SlotSet;
use vig_core::{
    Criticality, Duration, Instant, ModelIdx, PayloadRef, QueuePolicy, RequestDescriptor,
    RequestId, SlotIdx, SupersessionKey, VariantIdx,
};

fn ms(v: u64) -> Duration {
    Duration::from_millis(v).unwrap()
}

fn at(v: u64) -> Instant {
    Instant::ZERO.checked_add(ms(v)).unwrap()
}

/// Ein Vertrag mit je einer Variante pro angegebener Laufzeit.
///
/// Die Qualitaet faellt mit dem Index, wie `ModelContract::validate` es
/// verlangt. Die Laufzeiten sind davon unabhaengig — genau darauf zielt
/// [`infeasible_fallback_chooses_the_measured_fastest_variant`].
fn contract(class: Criticality, policy: QueuePolicy, runtimes_ms: &[u64]) -> ModelContract {
    let mut variants = ArrayVec::new();
    for (i, runtime) in runtimes_ms.iter().enumerate() {
        let quality = 1_000_u16.saturating_sub(u16::try_from(i).unwrap_or(0).saturating_mul(100));
        variants
            .push(Variant {
                quality: QualityValue::measured(Quality::from_milli(quality).unwrap()),
                profile: VariantProfile::solo(RuntimeProfile::exact(ms(*runtime))),
            })
            .unwrap();
    }
    ModelContract {
        variants_interchangeable: true,
        criticality: class,
        queue: QueueConfig {
            policy,
            capacity: 64,
            overflow: OverflowPolicy::RejectNew,
        },
        period: None,
        deadline: ms(1_000),
        max_age: None,
        stateful: false,
        min_quality: None,
        variant_dwell: Duration::ZERO,
        variants,
        cooperative: None,
        extension: None,
    }
}

fn frame(id: u64, model: u16, generated_ms: u64, c: &ModelContract) -> RequestDescriptor {
    RequestDescriptor {
        id: RequestId(id),
        logical_model: ModelIdx(model),
        supersession_key: SupersessionKey(0),
        generation_time: at(generated_ms),
        arrival_time: at(generated_ms),
        absolute_deadline: at(generated_ms).checked_add(c.deadline),
        max_age: c.max_age,
        criticality: c.criticality,
        queue_policy: c.queue.policy,
        stateful: c.stateful,
        variant: None,
        payload: PayloadRef(id),
    }
}

fn scheduler(contracts: &[ModelContract], slots: SlotSet) -> Scheduler {
    let mut list = ArrayVec::new();
    for c in contracts {
        list.push(c.clone()).unwrap();
    }
    Scheduler::new(
        list,
        slots,
        OverloadController::new(OverloadConfig::default(), at(0)).unwrap(),
        SafetyMargin::NONE,
    )
    .unwrap()
}

fn event(s: &mut Scheduler, time_ms: u64, e: Event) -> Vec<Action> {
    let mut actions = Vec::new();
    s.on_event(at(time_ms), e, &mut |a| actions.push(a));
    actions
}

fn dispatched(actions: &[Action]) -> Vec<RequestId> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Dispatch { request, .. } => Some(*request),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Requestlebenszyklus
// ---------------------------------------------------------------------------

/// Kein Request verlaesst die Queue ohne Abschlussaktion.
///
/// Der Sammelpuffer fasste einmal 32 Eintraege, obwohl je Modell 64 warten
/// koennen. Der Ueberlauf wurde still verworfen: die Requests waren aus der
/// Queue verschwunden, aber niemand antwortete ihren Clients, und ihre
/// Payloads blieben bis zum Prozessende belegt.
#[test]
fn every_stale_request_gets_a_terminal_action() {
    let blocker = contract(Criticality::Protected, QueuePolicy::Fifo, &[1_000]);
    let queued = contract(Criticality::Normal, QueuePolicy::Fifo, &[1]);
    let mut s = scheduler(
        &[blocker.clone(), queued.clone()],
        SlotSet::homogeneous(1, 0).unwrap(),
    );
    // Belegt den einzigen Slot, damit die 64 Frames wirklich warten.
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &blocker)));
    for id in 2..=65 {
        let mut d = frame(id, 1, 0, &queued);
        d.max_age = Some(ms(10));
        event(&mut s, 0, Event::Arrival(d));
    }

    let actions = event(&mut s, 11, Event::Tick);
    let terminal = actions
        .iter()
        .filter(|a| matches!(a, Action::Terminate { .. }))
        .count();
    assert_eq!(
        terminal, 64,
        "alle 64 veralteten Requests haben die Queue verlassen und brauchen eine Antwort"
    );
}

/// Ein zurueckgezogener Request wird nicht mehr weitergereicht.
#[test]
fn a_cancelled_request_leaves_the_queue_with_a_terminal_state() {
    let blocker = contract(Criticality::Protected, QueuePolicy::Fifo, &[1_000]);
    let waiting = contract(Criticality::Normal, QueuePolicy::Fifo, &[1]);
    let mut s = scheduler(
        &[blocker.clone(), waiting.clone()],
        SlotSet::homogeneous(1, 0).unwrap(),
    );
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &blocker)));
    event(&mut s, 0, Event::Arrival(frame(2, 1, 0, &waiting)));

    let actions = event(
        &mut s,
        1,
        Event::Cancel {
            request: RequestId(2),
        },
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Terminate {
            request: RequestId(2),
            state: vig_core::RequestState::Cancelled
        }
    )));
    assert_eq!(s.metrics().cancelled, 1);

    // Und er taucht auch nicht auf, wenn der Slot frei wird.
    let after = event(
        &mut s,
        2,
        Event::Completion {
            request: RequestId(1),
            slot: SlotIdx(0),
        },
    );
    assert!(dispatched(&after).is_empty(), "zurueckgezogen bleibt weg");
}

// ---------------------------------------------------------------------------
// Queue-Policies
// ---------------------------------------------------------------------------

/// Modellweites `LATEST` gilt modellweit — auch bei fremden Keys.
///
/// Der Scopevergleich der Queue normalisierte korrekt, der anschliessende
/// Altersvergleich verlangte aber erneut Keygleichheit. Damit war die
/// modellweite Zusage durch eine blosse Clientkonvention abschaltbar.
#[test]
fn latest_supersedes_across_the_whole_model_even_with_differing_client_keys() {
    let c = contract(Criticality::Normal, QueuePolicy::Latest, &[1]);
    let mut q = ModelQueue::new(c.queue, false).unwrap();

    let mut older = frame(1, 0, 0, &c);
    older.supersession_key = SupersessionKey(11);
    let mut newer = frame(2, 0, 1, &c);
    newer.supersession_key = SupersessionKey(22);

    q.push(older);
    let out = q.push(newer);
    assert_eq!(q.len(), 1, "LATEST haelt genau einen Request je Modell");
    assert_eq!(out.evicted.len(), 1, "der aeltere wird explizit terminal");

    // Und die Gegenrichtung: ein spaet eintreffender aelterer Frame kommt
    // nicht mehr hinein.
    let mut late = frame(3, 0, 0, &c);
    late.supersession_key = SupersessionKey(33);
    assert!(!q.push(late).accepted());
}

/// `LATEST_PER_KEY` verdraengt weiterhin nur innerhalb eines Keys.
///
/// Die Gegenprobe zur Reparatur: waere der Keyvergleich ganz entfallen,
/// wuerden sich zwei Kameras gegenseitig ausloeschen.
#[test]
fn latest_per_key_still_keeps_separate_keys_apart() {
    let c = contract(Criticality::Normal, QueuePolicy::LatestPerKey, &[1]);
    let mut q = ModelQueue::new(c.queue, false).unwrap();

    let mut cam_a = frame(1, 0, 0, &c);
    cam_a.supersession_key = SupersessionKey(11);
    cam_a.queue_policy = QueuePolicy::LatestPerKey;
    let mut cam_b = frame(2, 0, 1, &c);
    cam_b.supersession_key = SupersessionKey(22);
    cam_b.queue_policy = QueuePolicy::LatestPerKey;

    q.push(cam_a);
    q.push(cam_b);
    assert_eq!(q.len(), 2, "zwei Kameras verdraengen einander nicht");
}

/// FIFO wird nicht durch EDF umsortiert.
///
/// Der Scheduler waehlte modellintern nach Deadline. Ein spaeter
/// eingetroffener Frame mit frueherer Deadline ueberholte damit einen
/// frueheren — in einer Queue, deren einzige Zusage genau das ausschliesst.
/// Bei einer zustandsbehafteten Sequenz ist das nicht nur unfair, sondern
/// fachlich falsch (Spec 12.5, G-011).
#[test]
fn a_stateful_fifo_sequence_is_not_reordered_by_edf() {
    let mut c = contract(Criticality::Protected, QueuePolicy::Fifo, &[5]);
    c.stateful = true;
    let mut s = scheduler(&[c.clone()], SlotSet::homogeneous(1, 0).unwrap());

    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &c)));
    event(&mut s, 1, Event::Arrival(frame(2, 0, 1, &c)));
    let mut urgent = frame(3, 0, 2, &c);
    urgent.absolute_deadline = Some(at(20));
    event(&mut s, 2, Event::Arrival(urgent));

    let actions = event(
        &mut s,
        5,
        Event::Completion {
            request: RequestId(1),
            slot: SlotIdx(0),
        },
    );
    assert_eq!(
        dispatched(&actions),
        vec![RequestId(2)],
        "FIFO reicht das aeltere wartende Sequenzelement weiter"
    );
}

// ---------------------------------------------------------------------------
// Slots und Co-Run
// ---------------------------------------------------------------------------

/// Ein Co-Run-Verbot endet mit der Fertigstellung, nicht mit der Prognose.
///
/// Frueher hing das Verbot allein an `expected_finish`. Es fiel also genau
/// dann, wenn die Prognose zu optimistisch war — unter Ueberlast, wo es
/// gebraucht wird.
#[test]
fn a_corun_veto_lasts_until_the_work_actually_completes() {
    let mut slots = SlotSet::homogeneous(2, 0).unwrap();
    slots.forbid_corun(ModelIdx(0), ModelIdx(1));
    slots
        .dispatch(SlotIdx(0), RequestId(1), ModelIdx(0), at(0), ms(5))
        .unwrap();

    assert!(
        slots.ready_slot(ModelIdx(1), at(6)).is_none(),
        "die Prognose ist abgelaufen, die verbotene Arbeit laeuft noch"
    );
    slots.complete(SlotIdx(0), RequestId(1));
    assert!(
        slots.ready_slot(ModelIdx(1), at(6)).is_some(),
        "nach der echten Fertigstellung ist die Kombination wieder erlaubt"
    );
}

/// Ein blockierter Spitzenkandidat legt keinen unabhaengigen Slot still.
///
/// Frueher brach die Dispatchschleife ab, sobald der beste Kandidat keinen
/// Slot bekam. Ein Modell, das mit niemandem in Konflikt steht, wartete dann
/// mit, ohne dass irgendjemand etwas davon hatte.
#[test]
fn a_blocked_candidate_does_not_idle_an_independent_slot() {
    let a = contract(Criticality::Protected, QueuePolicy::Fifo, &[100]);
    let b = contract(Criticality::High, QueuePolicy::Fifo, &[1]);
    let c = contract(Criticality::Normal, QueuePolicy::Fifo, &[1]);
    let mut slots = SlotSet::homogeneous(2, 0).unwrap();
    slots.forbid_corun(ModelIdx(0), ModelIdx(1));

    let mut s = scheduler(&[a.clone(), b.clone(), c.clone()], slots);
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &a)));
    event(&mut s, 1, Event::Arrival(frame(2, 1, 1, &b)));
    let actions = event(&mut s, 2, Event::Arrival(frame(3, 2, 2, &c)));

    assert_eq!(
        dispatched(&actions),
        vec![RequestId(3)],
        "C darf den freien Slot nutzen, ohne A oder B zu schaden"
    );
}

// ---------------------------------------------------------------------------
// Look-ahead
// ---------------------------------------------------------------------------

/// Der Guard reserviert den **gemeinsamen** Bedarf erwarteter Arbeit.
///
/// Wurde jede erwartete Ankunft einzeln gegen dieselbe Belegung geprueft,
/// passten zwei geschuetzte Jobs jeweils fuer sich und rissen zusammen doch
/// eine Deadline.
#[test]
fn the_guard_reserves_the_combined_demand_of_expected_requests() {
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let forecast = [
        ExpectedArrival {
            model: ModelIdx(0),
            criticality: Criticality::Protected,
            at: at(10),
            deadline: at(20),
            runtime: ms(5),
        },
        ExpectedArrival {
            model: ModelIdx(1),
            criticality: Criticality::Protected,
            at: at(10),
            deadline: at(20),
            runtime: ms(5),
        },
    ];

    // Ohne Kandidat: [10,15] und [15,20] — beide halten die Deadline 20.
    // Mit Kandidat [0,11]: [11,16] und [16,21] — der zweite reisst sie.
    let verdict = guard_protected(
        &slots,
        ModelIdx(2),
        Criticality::BestEffort,
        ms(11),
        at(0),
        &forecast,
        ms(100),
    );
    assert!(
        matches!(verdict, GuardVerdict::WouldEndanger { .. }),
        "zusammen passen sie nicht mehr: {verdict:?}"
    );
}

/// Der Guard rechnet mit der gelernten Laufzeit, nicht mit dem Offline-Profil.
///
/// Das geschuetzte Modell braucht gemessen zehnmal so lange wie sein Profil
/// verspricht. Plant der Look-ahead weiter mit dem Profil, laesst er
/// Best-Effort-Arbeit starten, die genau diese Ankunft verspaetet.
#[test]
fn the_guard_uses_the_learned_protected_runtime() {
    let mut p = contract(Criticality::Protected, QueuePolicy::Fifo, &[1]);
    p.period = Some(ms(100));
    p.deadline = ms(40);
    let b = contract(Criticality::BestEffort, QueuePolicy::Fifo, &[30]);
    let mut s = scheduler(&[p.clone(), b.clone()], SlotSet::homogeneous(1, 0).unwrap());

    // 16 Runden, in denen das Modell statt 1 ms je 10 ms braucht.
    for k in 0..16 {
        let id = RequestId(k + 1);
        event(&mut s, k * 100, Event::Arrival(frame(id.0, 0, k * 100, &p)));
        event(
            &mut s,
            k * 100 + 10,
            Event::Completion {
                request: id,
                slot: SlotIdx(0),
            },
        );
    }
    assert_eq!(
        s.estimator().observed_p95(ModelIdx(0), VariantIdx(0), 0),
        Some(ms(10)),
        "die Beobachtung liegt vor"
    );

    // Naechste geschuetzte Ankunft: 1600, Deadline 1640. B wuerde bis 1622
    // laufen; danach passt die gelernte Laufzeit nicht mehr. Mit der
    // Profillaufzeit von 1 ms haette sie gepasst.
    let actions = event(&mut s, 1592, Event::Arrival(frame(100, 1, 1592, &b)));
    assert!(
        dispatched(&actions).is_empty(),
        "B haette die geschuetzte Ankunft verspaetet"
    );
}

/// Das Quantum wird vor dem Guard bestimmt, nicht danach.
///
/// Prueft der Look-ahead die volle Joblaufzeit, vetoiert er jeden zerlegbaren
/// Auftrag, dessen Ganzes nicht mehr passt — auch dann, wenn genau dafuer ein
/// passendes Quantum zugeschnitten worden waere. Die Zerlegung bliebe
/// wirkungslos, und ein Vergleich „mit und ohne Quanten" muesste identisch
/// ausfallen.
#[test]
fn the_quantum_is_sized_before_the_guard_evaluates_it() {
    let mut protected = contract(Criticality::Protected, QueuePolicy::Fifo, &[2]);
    protected.period = Some(ms(20));
    protected.deadline = ms(5);
    let mut llm = contract(Criticality::BestEffort, QueuePolicy::Fifo, &[100]);
    llm.cooperative = Some(Cooperative {
        tokens_per_second: 1_000,
        min_tokens: 1,
        max_total_tokens: 100,
        base_cost: Duration::ZERO,
    });

    let mut s = scheduler(
        &[protected.clone(), llm.clone()],
        SlotSet::homogeneous(1, 0).unwrap(),
    );
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &protected)));
    event(
        &mut s,
        2,
        Event::Completion {
            request: RequestId(1),
            slot: SlotIdx(0),
        },
    );

    let actions = event(&mut s, 3, Event::Arrival(frame(2, 1, 3, &llm)));
    assert_eq!(
        dispatched(&actions),
        vec![RequestId(2)],
        "ein zugeschnittenes Quantum passt vor die naechste geschuetzte Ankunft"
    );
    let quantum = actions.iter().find_map(|a| match a {
        Action::Dispatch { quantum, .. } => *quantum,
        _ => None,
    });
    assert!(
        quantum.is_some(),
        "gestartet wird ein Quantum, nicht der Job"
    );
}

// ---------------------------------------------------------------------------
// Variantenwahl und Konfiguration
// ---------------------------------------------------------------------------

/// Im infeasiblen Fall gewinnt die gemessen schnellste Variante.
///
/// Die Variantenliste ist nach Qualitaet sortiert, nicht nach Laufzeit. Die
/// zuletzt betrachtete Variante fuer die schnellste zu halten, verschenkt
/// Qualitaet dort, wo gar keine Zeit gewonnen wird.
#[test]
fn infeasible_fallback_chooses_the_measured_fastest_variant() {
    // Variante 0: hoehere Qualitaet **und** schneller.
    let mut c = contract(Criticality::Protected, QueuePolicy::Fifo, &[10, 20]);
    c.deadline = ms(1);
    let mut s = scheduler(&[c.clone()], SlotSet::homogeneous(1, 0).unwrap());

    let actions = event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &c)));
    let chosen = actions.iter().find_map(|a| match a {
        Action::Dispatch { variant, .. } => Some(*variant),
        _ => None,
    });
    assert_eq!(
        chosen,
        Some(VariantIdx(0)),
        "hoehere Qualitaet kann auf dieser Hardware auch schneller sein"
    );
}

/// Eine kooperative Konfiguration mit leerem Tokenintervall wird abgelehnt.
///
/// `min_tokens > max_total_tokens` erreichte frueher erst im Dispatch ein
/// `u32::clamp` und panickte dort. Mit `panic = "abort"` im Releaseprofil
/// beendet das den Governor — eine annehmbare Konfiguration darf das nicht
/// koennen.
#[test]
fn an_empty_cooperative_token_range_is_rejected_by_the_contract() {
    let mut c = contract(Criticality::Normal, QueuePolicy::Fifo, &[1]);
    c.cooperative = Some(Cooperative {
        tokens_per_second: 1_000,
        min_tokens: 8,
        max_total_tokens: 1,
        base_cost: Duration::ZERO,
    });
    assert!(c.validate().is_err());

    c.cooperative = Some(Cooperative {
        tokens_per_second: 0,
        min_tokens: 1,
        max_total_tokens: 8,
        base_cost: Duration::ZERO,
    });
    assert!(
        c.validate().is_err(),
        "ohne Rate ist jede Zerlegung geraten"
    );

    c.cooperative = Some(Cooperative {
        tokens_per_second: 1_000,
        min_tokens: 1,
        max_total_tokens: 8,
        base_cost: Duration::ZERO,
    });
    c.validate().unwrap();
}

/// Ein Quantum kostet einen festen Sockel plus Erzeugungszeit.
///
/// Auf der Messmaschine (WP26, 2026-09-08) sind das 18 ms je Auftrag bei rund
/// 4 ms je Token: ein 4-Token-Quantum dauert 22 ms, nicht 16 ms. Rechnet die
/// Zuschneidung rein proportional zur Tokenzahl, meldet sie dem Look-ahead
/// eine Dauer, die das Quantum nie einhalten kann — und der lässt es starten.
/// Ist der Sockel so groß wie der Slack, passt gar kein Quantum, und genau das
/// muss der Scheduler sehen können.
#[test]
fn a_quantum_costs_its_fixed_base_plus_generation_time() {
    let measured = Cooperative {
        tokens_per_second: 242,
        min_tokens: 4,
        max_total_tokens: 64,
        base_cost: ms(18),
    };

    // 4 Token: 18 ms Sockel + 16,5 ms Erzeugung.
    assert_eq!(measured.cost_of(4).as_nanos(), 34_528_925);
    // Der Sockel faellt auch beim kleinstmoeglichen Quantum an.
    assert_eq!(measured.cost_of(0), ms(18));

    // Ein Budget unterhalb des Sockels traegt kein Quantum.
    assert_eq!(measured.tokens_in(ms(10)), 0);
    // Und oberhalb zaehlt nur, was nach dem Sockel bleibt.
    assert_eq!(measured.tokens_in(ms(18 + 1000)), 242);
}

/// Der Scheduler plant ein Quantum mit seinen **tatsächlichen** Kosten ein.
///
/// Der Fall aus WP26: 33-ms-Periode, rund 15 ms Detektorlaufzeit, also etwa
/// 18 ms Slack — und 18 ms Sockel je Quantum. Vor der Korrektur meldete die
/// Zuschneidung für ein 4-Token-Quantum die reine Erzeugungszeit. Slotbelegung
/// und Look-ahead rechneten damit mit einer Zahl, die das Quantum nie einhält;
/// gemessen dauert es das Doppelte.
#[test]
fn the_scheduler_reserves_the_real_cost_of_a_quantum() {
    let cooperative = Cooperative {
        tokens_per_second: 242,
        min_tokens: 4,
        max_total_tokens: 64,
        base_cost: ms(18),
    };
    let mut protected = contract(Criticality::Protected, QueuePolicy::Fifo, &[15]);
    protected.period = Some(ms(33));
    protected.deadline = ms(50);
    let mut llm = contract(Criticality::BestEffort, QueuePolicy::Fifo, &[1_000]);
    llm.cooperative = Some(cooperative);

    let mut s = scheduler(
        &[protected.clone(), llm.clone()],
        SlotSet::homogeneous(1, 0).unwrap(),
    );
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &protected)));
    event(
        &mut s,
        15,
        Event::Completion {
            request: RequestId(1),
            slot: SlotIdx(0),
        },
    );

    let actions = event(&mut s, 16, Event::Arrival(frame(2, 1, 16, &llm)));
    let planned = actions.iter().find_map(|a| match a {
        Action::Dispatch {
            predicted_runtime,
            quantum,
            ..
        } => Some((*predicted_runtime, *quantum)),
        _ => None,
    });
    let Some((runtime, Some(tokens))) = planned else {
        panic!("ein Quantum wurde gestartet: {planned:?}");
    };

    assert_eq!(
        runtime,
        cooperative.cost_of(tokens),
        "eingeplant wird Sockel plus Erzeugungszeit, nicht nur die Erzeugungszeit"
    );
    assert!(
        runtime > ms(18),
        "und damit mehr als der Sockel allein: {runtime:?}"
    );
}

/// Varianten mit verschiedener Schnittstelle werden nicht automatisch gewählt.
///
/// Der Governor wählt die Variante je Request und sagt es dem Client nicht.
/// Liefern zwei Varianten verschiedene Ausgabenamen oder -formen, bekommt der
/// Client nach einem Wechsel einen Backendfehler — oder, schlimmer, einen
/// Tensor mit anderer Bedeutung bei gleicher Form. Dann ist die automatische
/// Wahl abzuschalten, nicht zu warnen.
#[test]
fn variants_that_are_not_interchangeable_disable_automatic_selection() {
    let mut c = contract(Criticality::Protected, QueuePolicy::Fifo, &[10, 20]);
    assert!(
        c.auto_variant_selection(),
        "zwei gemessene Varianten sind normalerweise waehlbar"
    );

    c.variants_interchangeable = false;
    assert!(!c.auto_variant_selection());

    // Und der Dispatch bleibt dann bei der besten Variante, auch wenn die
    // kleinere schneller waere.
    c.deadline = ms(1);
    let mut s = scheduler(&[c.clone()], SlotSet::homogeneous(1, 0).unwrap());
    let actions = event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &c)));
    let chosen = actions.iter().find_map(|a| match a {
        Action::Dispatch { variant, .. } => Some(*variant),
        _ => None,
    });
    assert_eq!(chosen, Some(VariantIdx(0)));
}

// ---------------------------------------------------------------------------
// Wirkungen des Überlastreglers
// ---------------------------------------------------------------------------

/// Unter Abwertung gewinnt die schnellste machbare Variante, nicht die beste.
///
/// `forces_degradation()` gab es, aber niemand rief es auf: der Regler meldete
/// eine Stufe, die nichts tat. Im Normalbetrieb ist die höchste machbare
/// Qualität richtig — unter Überlast zählt, wie viel Zeit die Entscheidung
/// anderen Strömen lässt.
#[test]
fn degradation_prefers_the_fastest_feasible_variant() {
    use vig_core::estimator::RuntimeEstimator;
    use vig_core::variant::{PlanningContext, Resolution, VariantState, resolve};

    // Zwei Varianten, beide machbar: hohe Qualität 10 ms, niedrige 2 ms.
    let mut c = contract(Criticality::Normal, QueuePolicy::Fifo, &[10, 2]);
    c.deadline = ms(1_000);
    let slots = SlotSet::homogeneous(1, 0).unwrap();
    let estimator = RuntimeEstimator::new();
    let state = VariantState::default();

    let plan = |degrade: bool| {
        resolve(
            &c,
            &state,
            ModelIdx(0),
            Some(at(1_000)),
            &PlanningContext {
                slots: &slots,
                estimator: &estimator,
                predictor: &vig_core::predictor::Predictor::new(),
                state: vig_core::predictor::StateClass::default(),
                profile_revision: 0,
                margin: SafetyMargin::NONE,
                now: at(0),
                degrade,
            },
        )
    };

    let Resolution::Feasible(normal) = plan(false) else {
        panic!("im Normalbetrieb ist etwas machbar");
    };
    assert_eq!(
        normal.variant,
        VariantIdx(0),
        "ohne Druck gewinnt die hoechste Qualitaet"
    );

    let Resolution::Feasible(degraded) = plan(true) else {
        panic!("unter Abwertung ebenfalls");
    };
    assert_eq!(
        degraded.variant,
        VariantIdx(1),
        "unter Abwertung die schnellste machbare"
    );
}

/// Unter Frischedruck wird auch das verworfen, was erst beim Fertigwerden
/// zu alt wäre.
///
/// Ein Request, der die Altersgrenze während seiner eigenen Ausführung reißt,
/// verbraucht dieselbe GPU-Zeit und liefert dasselbe Nichts — er belegt sie
/// nur später. `aggressive_supersession()` beschrieb genau das und wurde nie
/// aufgerufen.
#[test]
fn freshness_pressure_drops_work_that_would_be_stale_on_arrival() {
    use vig_core::overload::OverloadState;

    // Laufzeit 50 ms, Hoechstalter 20 ms: was hier wartet, ist bei
    // Fertigstellung sicher zu alt.
    let mut c = contract(Criticality::Normal, QueuePolicy::Fifo, &[50]);
    c.max_age = Some(ms(20));
    let blocker = contract(Criticality::Protected, QueuePolicy::Fifo, &[1_000]);

    let mut s = scheduler(
        &[blocker.clone(), c.clone()],
        SlotSet::homogeneous(1, 0).unwrap(),
    );
    // Belegt den Slot, damit die Arbeit wirklich wartet.
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &blocker)));
    let mut waiting = frame(2, 1, 0, &c);
    waiting.max_age = Some(ms(20));
    event(&mut s, 0, Event::Arrival(waiting));

    // Ohne Druck bleibt sie liegen: die Prognose kann sich noch aendern.
    let quiet = event(&mut s, 1, Event::Tick);
    assert!(
        !quiet.iter().any(|a| matches!(
            a,
            Action::Terminate {
                request: RequestId(2),
                ..
            }
        )),
        "im Normalbetrieb wird nicht auf Verdacht verworfen"
    );

    // Unter Frischedruck schon.
    s.force_overload_state(OverloadState::FreshnessPressure);
    let pressed = event(&mut s, 2, Event::Tick);
    assert!(
        pressed.iter().any(|a| matches!(
            a,
            Action::Terminate {
                request: RequestId(2),
                state: vig_core::RequestState::Stale
            }
        )),
        "unter Druck geht Arbeit weg, die ohnehin wertlos ankaeme"
    );
}

/// Die längste Versorgungslücke wird je Strom festgehalten.
///
/// Eine Abdeckungsrate mittelt weg, was eine Regelung umwirft: zehn verstreute
/// Ausfälle und ein Block von zehn ergeben dieselbe Rate. Im Betrieb ist
/// gerade der Block das, was auffällt.
#[test]
fn the_longest_supply_gap_is_recorded_per_stream() {
    let mut c = contract(Criticality::Protected, QueuePolicy::Fifo, &[5]);
    c.max_age = Some(ms(10));
    let mut s = scheduler(&[c.clone()], SlotSet::homogeneous(1, 0).unwrap());

    // Ein gültiges Ergebnis setzt die Kette.
    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &c)));
    event(
        &mut s,
        5,
        Event::Completion {
            request: RequestId(1),
            slot: SlotIdx(0),
        },
    );
    assert_eq!(s.metrics().consecutive_misses[0], 0);
    assert_eq!(s.metrics().longest_gap_us[0], 0);

    // Danach drei Ergebnisse, die bei Fertigstellung zu alt sind.
    for (k, id) in [(50_u64, 2_u64), (100, 3), (150, 4)] {
        let mut d = frame(id, 0, k, &c);
        d.max_age = Some(ms(10));
        event(&mut s, k, Event::Arrival(d));
        event(
            &mut s,
            k + 40,
            Event::Completion {
                request: RequestId(id),
                slot: SlotIdx(0),
            },
        );
    }

    assert_eq!(
        s.metrics().consecutive_misses[0],
        3,
        "drei Fehlschlaege am Stueck"
    );
    // Die Lücke reicht vom letzten gültigen Ergebnis bis zum letzten Miss.
    assert_eq!(s.metrics().longest_gap_us[0], 185_000);

    // Ein gültiges Ergebnis schließt sie und setzt die Kette zurück.
    event(&mut s, 200, Event::Arrival(frame(5, 0, 198, &c)));
    event(
        &mut s,
        203,
        Event::Completion {
            request: RequestId(5),
            slot: SlotIdx(0),
        },
    );
    assert_eq!(s.metrics().consecutive_misses[0], 0);
    assert_eq!(
        s.metrics().longest_gap_us[0],
        185_000,
        "die Hoechstmarke bleibt stehen"
    );
}

// ---------------------------------------------------------------------------
// Kontrollen und Gegenbeispiele
// ---------------------------------------------------------------------------

/// Kontrolle: das Co-Run-Verbot wirkte auch vorher schon vor Ablauf der
/// Prognose. Die Reparatur hat es verlaengert, nicht erfunden.
#[test]
fn control_corun_veto_also_holds_before_the_prediction_expires() {
    let mut slots = SlotSet::homogeneous(2, 0).unwrap();
    slots.forbid_corun(ModelIdx(0), ModelIdx(1));
    slots
        .dispatch(SlotIdx(0), RequestId(1), ModelIdx(0), at(0), ms(5))
        .unwrap();
    assert!(slots.ready_slot(ModelIdx(1), at(4)).is_none());
}

/// Kontrolle: `LATEST` mit gleichem Key verdraengt weiterhin.
#[test]
fn control_latest_with_the_same_key_still_supersedes() {
    let c = contract(Criticality::Normal, QueuePolicy::Latest, &[1]);
    let mut q = ModelQueue::new(c.queue, false).unwrap();
    q.push(frame(1, 0, 0, &c));
    let out = q.push(frame(2, 0, 1, &c));
    assert_eq!(q.len(), 1);
    assert_eq!(out.evicted.len(), 1);
}

/// Gegenbeispiel zur allgemeinen Behauptung „Best-Effort-Laufzeit groesser
/// als die geschuetzte Periode heisst: nie ausfuehrbar".
///
/// Ein 30-ms-Job startet hier sehr wohl, obwohl die geschuetzte Periode 20 ms
/// betraegt — weil die geschuetzte Deadline 100 ms Spielraum laesst. Die
/// Aussage gilt nur mit zusaetzlichen Annahmen und gehoert deshalb nicht als
/// allgemeiner Satz in die Dokumentation.
#[test]
fn counterexample_runtime_longer_than_period_does_not_imply_starvation() {
    let mut p = contract(Criticality::Protected, QueuePolicy::Fifo, &[1]);
    p.period = Some(ms(20));
    p.deadline = ms(100);
    let b = contract(Criticality::BestEffort, QueuePolicy::Fifo, &[30]);
    let mut s = scheduler(&[p.clone(), b.clone()], SlotSet::homogeneous(1, 0).unwrap());

    event(&mut s, 0, Event::Arrival(frame(1, 0, 0, &p)));
    event(
        &mut s,
        1,
        Event::Completion {
            request: RequestId(1),
            slot: SlotIdx(0),
        },
    );
    let actions = event(&mut s, 2, Event::Arrival(frame(2, 1, 2, &b)));
    assert_eq!(dispatched(&actions), vec![RequestId(2)]);
}
