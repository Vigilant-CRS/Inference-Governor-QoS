//! Golden Tests des Stale Work Collectors (Spec 28, WP2 Pflichttests).
//!
//! Diese Tests sind die ausfuehrbare Fassung der Produktzusagen. Sie sind
//! bewusst gegen die **oeffentliche** API geschrieben: was hier nicht
//! ausdrueckbar ist, kann ein Nutzer auch nicht verlassen.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use onetimer_core::queue::{DropReason, ModelQueue, QueueConfig, QueueConfigError};
use onetimer_core::request::OverflowPolicy;
use onetimer_core::{
    Criticality, Duration, Instant, ModelIdx, PayloadRef, QueuePolicy, RequestDescriptor,
    RequestId, RequestState, SupersessionKey,
};

fn ms(v: u64) -> Duration {
    Duration::from_millis(v).unwrap()
}

fn at(v: u64) -> Instant {
    Instant::ZERO.checked_add(ms(v)).unwrap()
}

/// Baut einen Request mit Capture-Zeit `generated_ms`.
fn frame(id: u64, generated_ms: u64) -> RequestDescriptor {
    RequestDescriptor {
        id: RequestId(id),
        logical_model: ModelIdx(0),
        supersession_key: SupersessionKey::DEFAULT,
        generation_time: at(generated_ms),
        arrival_time: at(generated_ms),
        absolute_deadline: at(generated_ms).checked_add(ms(30)),
        max_age: Some(ms(66)),
        criticality: Criticality::Protected,
        queue_policy: QueuePolicy::Latest,
        stateful: false,
        variant: None,
        payload: PayloadRef(id),
    }
}

fn with_policy(mut d: RequestDescriptor, p: QueuePolicy) -> RequestDescriptor {
    d.queue_policy = p;
    d
}

fn with_key(mut d: RequestDescriptor, key: u64) -> RequestDescriptor {
    d.supersession_key = SupersessionKey(key);
    d
}

fn queue(policy: QueuePolicy, capacity: usize, overflow: OverflowPolicy) -> ModelQueue {
    ModelQueue::new(
        QueueConfig {
            policy,
            capacity,
            overflow,
        },
        false,
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// G-001 - Latest supersedes queued
// ---------------------------------------------------------------------------

#[test]
fn g001_latest_supersedes_queued() {
    let mut q = queue(QueuePolicy::Latest, 1, OverflowPolicy::RejectNew);

    let r1 = frame(1, 0);
    let out1 = q.push(r1);
    assert!(out1.accepted());
    assert!(out1.evicted.is_empty());

    let r2 = frame(2, 33);
    let out2 = q.push(r2);

    assert!(out2.accepted(), "der juengere Frame muss eingereiht werden");
    assert_eq!(
        out2.evicted.len(),
        1,
        "genau ein Vorgaenger wird verdraengt"
    );
    let ev = out2.evicted.get(0).unwrap();
    assert_eq!(ev.id(), RequestId(1));
    assert_eq!(ev.state(), RequestState::Superseded);
    assert_eq!(ev.reason, DropReason::Superseded);

    assert_eq!(q.len(), 1);
    assert_eq!(q.front().unwrap().id, RequestId(2));
}

/// Spec 11.1, Invariante: hoechstens ein wartender Request je LATEST-Scope.
#[test]
fn latest_queue_depth_never_exceeds_one() {
    let mut q = queue(QueuePolicy::Latest, 4, OverflowPolicy::RejectNew);
    for n in 0..500_u64 {
        let f = frame(n, n * 33);
        let out = q.push(f);
        assert!(out.accepted(), "ein juengerer Frame wird immer angenommen");
        assert_eq!(q.scope_depth(&f), 1, "Invariante verletzt bei n={n}");
        assert_eq!(q.len(), 1);
    }
}

/// Bei Netzwerkumordnung darf ein aelterer Frame den juengeren nicht ersetzen.
#[test]
fn an_older_frame_arriving_late_does_not_replace_a_newer_one() {
    let mut q = queue(QueuePolicy::Latest, 2, OverflowPolicy::RejectNew);

    assert!(q.push(frame(2, 66)).accepted());
    let out = q.push(frame(1, 33));

    assert!(!out.accepted());
    assert_eq!(out.rejected, Some(DropReason::ArrivedOutOfOrder));
    assert_eq!(
        out.rejected.unwrap().terminal_state(),
        RequestState::Superseded
    );
    assert_eq!(q.len(), 1);
    assert_eq!(q.front().unwrap().id, RequestId(2), "der juengere bleibt");
}

// ---------------------------------------------------------------------------
// G-002 - Running is not magically cancelled
// ---------------------------------------------------------------------------

/// Ein bereits an das Backend uebergebener Request kann nicht mehr verdraengt
/// werden — eine laufende GPU-Inferenz ist nicht zuverlaessig zurueckholbar
/// (Spec 3.1). Die Zustandsmaschine muss diesen Uebergang verweigern.
#[test]
fn g002_forwarded_cannot_be_superseded() {
    assert!(
        !RequestState::Forwarded.can_transition_to(RequestState::Superseded),
        "Forwarded -> Superseded waere ein Versprechen, das die GPU nicht haelt"
    );

    for allowed in [
        RequestState::CompletedValid,
        RequestState::CompletedObsolete,
        RequestState::Failed,
    ] {
        assert!(RequestState::Forwarded.can_transition_to(allowed));
    }

    // Eine bereits entnommene Arbeit liegt nicht mehr in der Queue und kann
    // daher von einem neuen Request gar nicht mehr erreicht werden.
    let mut q = queue(QueuePolicy::Latest, 1, OverflowPolicy::RejectNew);
    assert!(q.push(frame(1, 0)).accepted());
    let running = q.take(RequestId(1)).unwrap();
    assert_eq!(running.id, RequestId(1));

    let out = q.push(frame(2, 33));
    assert!(out.accepted());
    assert!(
        out.evicted.is_empty(),
        "der laufende Request wird nicht angetastet"
    );
}

#[test]
fn every_request_reaches_exactly_one_terminal_state() {
    for state in [
        RequestState::Superseded,
        RequestState::Stale,
        RequestState::RejectedInfeasible,
        RequestState::CompletedValid,
        RequestState::CompletedObsolete,
        RequestState::Failed,
    ] {
        assert!(state.is_terminal());
        for next in [
            RequestState::Queued,
            RequestState::Admitted,
            RequestState::Forwarded,
        ] {
            assert!(!state.can_transition_to(next), "{state:?} ist terminal");
        }
    }
    for state in [
        RequestState::Received,
        RequestState::Queued,
        RequestState::Admitted,
        RequestState::Forwarded,
    ] {
        assert!(!state.is_terminal());
    }
}

// ---------------------------------------------------------------------------
// G-003 - FIFO order
// ---------------------------------------------------------------------------

#[test]
fn g003_fifo_preserves_order_and_never_supersedes() {
    let mut q = queue(QueuePolicy::Fifo, 4, OverflowPolicy::RejectNew);
    for n in 1..=3_u64 {
        let out = q.push(with_policy(frame(n, n * 10), QueuePolicy::Fifo));
        assert!(out.accepted());
        assert!(out.evicted.is_empty(), "FIFO verdraengt niemals");
    }
    assert_eq!(q.len(), 3);

    let order: Vec<u64> = std::iter::from_fn(|| q.take_front().map(|d| d.id.0)).collect();
    assert_eq!(order, vec![1, 2, 3]);
}

// ---------------------------------------------------------------------------
// G-004 - NeverDrop overflow
// ---------------------------------------------------------------------------

#[test]
fn g004_never_drop_overflows_explicitly_instead_of_silently() {
    let mut q = queue(
        QueuePolicy::NeverDrop,
        2,
        OverflowPolicy::BackpressureClient,
    );
    for n in 1..=2_u64 {
        assert!(
            q.push(with_policy(frame(n, n * 10), QueuePolicy::NeverDrop))
                .accepted()
        );
    }

    let out = q.push(with_policy(frame(3, 30), QueuePolicy::NeverDrop));
    assert!(!out.accepted(), "die Queue ist voll");
    assert_eq!(
        out.rejected,
        Some(DropReason::Backpressure),
        "explizite Backpressure"
    );
    assert!(
        out.evicted.is_empty(),
        "kein bereits eingereihter Request wird geopfert"
    );
    assert_eq!(q.len(), 2, "der Inhalt bleibt unveraendert");
}

#[test]
fn never_drop_is_immune_to_freshness_rules() {
    let mut q = queue(QueuePolicy::NeverDrop, 4, OverflowPolicy::RejectNew);
    let mut old = with_policy(frame(1, 0), QueuePolicy::NeverDrop);
    old.max_age = Some(ms(10));
    assert!(q.push(old).accepted());

    // Weit ueber max_age hinaus.
    let stale = q.collect_stale(at(10_000));
    assert!(
        stale.is_empty(),
        "NEVER_DROP wird nicht wegen Alters entfernt"
    );
    assert_eq!(q.len(), 1);

    // Und auch nicht durch einen juengeren Request.
    let out = q.push(with_policy(frame(2, 100), QueuePolicy::NeverDrop));
    assert!(out.accepted());
    assert!(out.evicted.is_empty());
    assert_eq!(q.len(), 2);
}

#[test]
fn reject_oldest_non_protected_never_sacrifices_guarded_work() {
    let mut q = queue(
        QueuePolicy::Fifo,
        2,
        OverflowPolicy::RejectOldestNonProtected,
    );

    let mut normal = with_policy(frame(1, 0), QueuePolicy::Fifo);
    normal.criticality = Criticality::Normal;
    let mut protected = with_policy(frame(2, 10), QueuePolicy::Fifo);
    protected.criticality = Criticality::Protected;

    assert!(q.push(normal).accepted());
    assert!(q.push(protected).accepted());

    // Der Normal-Request weicht.
    let out = q.push(with_policy(frame(3, 20), QueuePolicy::Fifo));
    assert!(out.accepted());
    assert_eq!(out.evicted.len(), 1);
    assert_eq!(out.evicted.get(0).unwrap().id(), RequestId(1));

    // Jetzt warten nur noch geschuetzte Requests: der neue wird abgelehnt,
    // statt geschuetzte Arbeit zu verdraengen.
    let mut protected2 = with_policy(frame(4, 30), QueuePolicy::Fifo);
    protected2.criticality = Criticality::Protected;
    let out = q.push(protected2);
    assert!(!out.accepted());
    assert_eq!(out.rejected, Some(DropReason::QueueFull));
    assert!(out.evicted.is_empty());
}

// ---------------------------------------------------------------------------
// LATEST_PER_KEY
// ---------------------------------------------------------------------------

#[test]
fn latest_per_key_isolates_streams() {
    let mut q = queue(QueuePolicy::LatestPerKey, 8, OverflowPolicy::RejectNew);

    for key in 0..4_u64 {
        let f = with_key(with_policy(frame(key, 0), QueuePolicy::LatestPerKey), key);
        assert!(q.push(f).accepted());
    }
    assert_eq!(q.len(), 4, "vier Kameras koexistieren");

    // Ein neuer Frame von Kamera 2 verdraengt nur Kamera 2.
    let newer = with_key(with_policy(frame(99, 33), QueuePolicy::LatestPerKey), 2);
    let out = q.push(newer);
    assert!(out.accepted());
    assert_eq!(out.evicted.len(), 1);
    assert_eq!(out.evicted.get(0).unwrap().id(), RequestId(2));
    assert_eq!(q.len(), 4);
    assert_eq!(q.scope_depth(&newer), 1);
}

// ---------------------------------------------------------------------------
// Stufe B - Pre-dispatch Freshness
// ---------------------------------------------------------------------------

#[test]
fn stale_collection_uses_generation_time_not_arrival_time() {
    let mut q = queue(QueuePolicy::LatestPerKey, 8, OverflowPolicy::RejectNew);

    // Ein Frame, der bei Ankunft schon 60ms alt ist, bei max_age 66ms.
    let mut late = with_policy(frame(1, 0), QueuePolicy::LatestPerKey);
    late.arrival_time = at(60);
    late.max_age = Some(ms(66));
    assert!(q.push(late).accepted());

    // 10ms nach Ankunft ist er 70ms alt gemessen an der Capture-Zeit.
    let evicted = q.collect_stale(at(70));
    assert_eq!(
        evicted.len(),
        1,
        "das Alter zaehlt ab Capture, nicht ab Ankunft"
    );
    assert_eq!(evicted.get(0).unwrap().state(), RequestState::Stale);
    assert!(q.is_empty());
}

#[test]
fn requests_without_max_age_never_go_stale() {
    let mut q = queue(QueuePolicy::LatestPerKey, 4, OverflowPolicy::RejectNew);
    let mut ageless = with_policy(frame(1, 0), QueuePolicy::LatestPerKey);
    ageless.max_age = None;
    assert!(q.push(ageless).accepted());
    assert!(q.collect_stale(at(3_600_000)).is_empty());
    assert_eq!(q.len(), 1);
}

// ---------------------------------------------------------------------------
// G-011 - Stateful cannot Latest
// ---------------------------------------------------------------------------

#[test]
fn g011_stateful_model_cannot_use_a_superseding_policy() {
    for policy in [QueuePolicy::Latest, QueuePolicy::LatestPerKey] {
        let cfg = QueueConfig {
            policy,
            capacity: 2,
            overflow: OverflowPolicy::RejectNew,
        };
        let err = ModelQueue::new(cfg, true).unwrap_err();
        assert_eq!(err, QueueConfigError::StatefulCannotSupersede { policy });
    }

    for policy in [QueuePolicy::Fifo, QueuePolicy::NeverDrop] {
        let cfg = QueueConfig {
            policy,
            capacity: 2,
            overflow: OverflowPolicy::RejectNew,
        };
        assert!(
            ModelQueue::new(cfg, true).is_ok(),
            "{policy:?} ist fuer stateful zulaessig"
        );
    }
}

/// Auch wenn die Queue-Policy supersedieren darf, bleibt ein einzelner
/// stateful-Request unantastbar.
#[test]
fn a_stateful_request_is_never_superseded() {
    let mut q = queue(QueuePolicy::Latest, 4, OverflowPolicy::RejectNew);
    let mut seq = frame(1, 0);
    seq.stateful = true;
    assert!(q.push(seq).accepted());

    let out = q.push(frame(2, 33));
    assert!(out.accepted());
    assert!(
        out.evicted.is_empty(),
        "die Sequenz darf keine Frames verlieren"
    );
    assert_eq!(q.len(), 2);
}

// ---------------------------------------------------------------------------
// Konfigurationsvalidierung (L-020)
// ---------------------------------------------------------------------------

#[test]
fn invalid_capacity_is_rejected_not_repaired() {
    let zero = QueueConfig {
        policy: QueuePolicy::Fifo,
        capacity: 0,
        overflow: OverflowPolicy::RejectNew,
    };
    assert_eq!(
        ModelQueue::new(zero, false).unwrap_err(),
        QueueConfigError::ZeroCapacity
    );

    let huge = QueueConfig {
        policy: QueuePolicy::Fifo,
        capacity: 10_000,
        overflow: OverflowPolicy::RejectNew,
    };
    assert_eq!(
        ModelQueue::new(huge, false).unwrap_err(),
        QueueConfigError::CapacityTooLarge { requested: 10_000 }
    );
}

/// Spec L-003: es darf keine unbegrenzte Warteschlange geben. Auch unter
/// Dauerlast bleibt die Belegung durch die Kapazitaet beschraenkt.
#[test]
fn queue_stays_bounded_under_sustained_overload() {
    let mut q = queue(QueuePolicy::Fifo, 8, OverflowPolicy::RejectNew);
    let mut rejected = 0_u32;
    for n in 0..100_000_u64 {
        let out = q.push(with_policy(frame(n, n), QueuePolicy::Fifo));
        if !out.accepted() {
            rejected += 1;
        }
        assert!(q.len() <= 8, "Kapazitaet ueberschritten bei n={n}");
    }
    assert!(
        rejected > 99_000,
        "unter Dauerueberlast muss abgelehnt werden, waren {rejected}"
    );
}
