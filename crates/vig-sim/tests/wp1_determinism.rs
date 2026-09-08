//! WP1 Definition of Done (Spec 24, WP1):
//!
//! > 100.000+ synthetische Ereignisse deterministisch reproduzierbar;
//! > gleicher Seed ergibt identische Reihenfolge und Kennzahlen.
//!
//! Der Nachweis laeuft ueber einen Digest des vollstaendigen Ereignisstroms
//! (siehe `trace`). Zwei Laeufe mit gleichem Seed muessen bitgleiche Digests
//! liefern; ein anderer Seed muss einen anderen Digest liefern, sonst wuerde
//! der Test auch dann bestehen, wenn der Simulator gar nichts variiert.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::indexing_slicing
)]

use vig_core::{Duration, Instant, ModelIdx, RequestId, SlotIdx, SupersessionKey};
use vig_sim::{Pcg32, PeriodicStream, RuntimeDistribution, SimClock, SimEvent, TraceDigest};

fn ms(v: u64) -> Duration {
    Duration::from_millis(v).unwrap()
}

/// Ein Modell im Lastmodell des Laufs.
struct Model {
    stream: PeriodicStream,
    runtime: RuntimeDistribution,
}

/// Faehrt einen synthetischen Lauf und gibt den Digest zurueck.
///
/// Das Lastmodell entspricht dem Beispiel aus Spec 1.3: mehrere periodische
/// Wahrnehmungsmodelle plus eine langsame ereignisbasierte Last.
fn run(seed: u64, frames_per_model: u64) -> TraceDigest {
    let models = [
        // Detector: 30 Hz, 10ms p50
        Model {
            stream: PeriodicStream {
                period: ms(33),
                jitter: ms(2),
                transport: ms(3),
                phase: Duration::ZERO,
            },
            runtime: RuntimeDistribution::from_percentiles(ms(10), ms(22)).unwrap(),
        },
        // Depth: 15 Hz, 12ms p50
        Model {
            stream: PeriodicStream {
                period: ms(66),
                jitter: ms(3),
                transport: ms(4),
                phase: ms(7),
            },
            runtime: RuntimeDistribution::from_percentiles(ms(12), ms(26)).unwrap(),
        },
        // Pose: 30 Hz, 8ms p50
        Model {
            stream: PeriodicStream {
                period: ms(33),
                jitter: ms(2),
                transport: ms(3),
                phase: ms(11),
            },
            runtime: RuntimeDistribution::from_percentiles(ms(8), ms(19)).unwrap(),
        },
        // VLM-artige Hintergrundlast: 2 Hz, 200ms p50, schwerer Schwanz
        Model {
            stream: PeriodicStream {
                period: ms(500),
                jitter: ms(120),
                transport: ms(2),
                phase: ms(37),
            },
            runtime: RuntimeDistribution::from_percentiles(ms(200), ms(700)).unwrap(),
        },
    ];

    // Getrennte RNG-Streams: eine Aenderung an der Lastdefinition verschiebt
    // dann nicht die Laufzeitfolge und umgekehrt.
    let mut arrival_rng = Pcg32::new(seed, 1);
    let mut runtime_rng = Pcg32::new(seed, 2);

    let mut clock = SimClock::new();
    let mut digest = TraceDigest::new();

    // Ankuenfte einplanen.
    for (idx, model) in models.iter().enumerate() {
        for n in 0..frames_per_model {
            let capture = model.stream.capture_at(n, &mut arrival_rng);
            let arrival = model.stream.arrival_at(capture);
            clock
                .schedule(
                    arrival,
                    SimEvent::Arrival {
                        model: ModelIdx(u16::try_from(idx).unwrap()),
                        key: SupersessionKey(idx as u64),
                        generation: capture,
                    },
                )
                .unwrap();
        }
    }

    // Ereignisse abarbeiten. Jede Ankunft erzeugt eine Completion, damit der
    // Lauf beide Ereignisarten und beide RNG-Streams durchmischt.
    let mut next_request = 0_u64;
    let mut total_runtime_ns = 0_u64;
    let mut completions = 0_u64;

    while let Some((at, event)) = clock.advance() {
        digest.record(at, &event);
        if let SimEvent::Arrival { model, .. } = event {
            let dist = &models[model.get()].runtime;
            let runtime = dist.sample(&mut runtime_rng);
            total_runtime_ns = total_runtime_ns.saturating_add(runtime.as_nanos());
            next_request += 1;
            let finish = at
                .checked_add(runtime)
                .unwrap_or(Instant::from_nanos(u64::MAX));
            clock
                .schedule(
                    finish,
                    SimEvent::Completion {
                        request: RequestId(next_request),
                        slot: SlotIdx(u16::try_from(model.get() % 4).unwrap()),
                    },
                )
                .unwrap();
        } else if matches!(event, SimEvent::Completion { .. }) {
            completions += 1;
        }
    }

    // Kennzahlen gehen ebenfalls in den Digest ein: der DoD verlangt
    // identische Reihenfolge *und* identische Kennzahlen.
    digest.record_metric("events", clock.processed());
    digest.record_metric("completions", completions);
    digest.record_metric("total_runtime_ns", total_runtime_ns);
    digest
}

#[test]
fn wp1_same_seed_reproduces_run_exactly() {
    let a = run(0x5EED_0001, 15_000);
    let b = run(0x5EED_0001, 15_000);

    assert!(
        a.count() >= 100_000,
        "DoD verlangt mindestens 100.000 Ereignisse, waren {}",
        a.count()
    );
    assert_eq!(
        a, b,
        "gleicher Seed muss bitgleichen Trace erzeugen: {a} vs {b}"
    );
}

#[test]
fn wp1_different_seed_changes_the_run() {
    let a = run(0x5EED_0001, 15_000);
    let b = run(0x5EED_0002, 15_000);
    assert_ne!(
        a.value(),
        b.value(),
        "anderer Seed muss anderen Trace erzeugen, sonst prueft der Determinismustest nichts"
    );
    assert_eq!(
        a.count(),
        b.count(),
        "die Ereigniszahl ist von der Lastdefinition bestimmt"
    );
}

/// Der Digest muss auf eine Umsortierung reagieren, sonst waere er als
/// Reproduzierbarkeitsnachweis wertlos.
#[test]
fn digest_detects_reordering() {
    let at1 = Instant::from_nanos(1_000);
    let at2 = Instant::from_nanos(2_000);
    let e1 = SimEvent::Completion {
        request: RequestId(1),
        slot: SlotIdx(0),
    };
    let e2 = SimEvent::Completion {
        request: RequestId(2),
        slot: SlotIdx(0),
    };

    let mut forward = TraceDigest::new();
    forward.record(at1, &e1);
    forward.record(at2, &e2);

    let mut swapped = TraceDigest::new();
    swapped.record(at1, &e2);
    swapped.record(at2, &e1);

    assert_ne!(forward.value(), swapped.value());
}
