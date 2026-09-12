//! Der Vergleichslauf: derselbe Workload, zwei Governoren (Gate S, ADR-0001).
//!
//! ## Was gleich gehalten wird
//!
//! Beide Governoren bekommen **denselben** Ankunftsprozess, **dieselben**
//! Slots, **dieselben** Vertraege und **dieselben** Weckrufe. Die tatsaechliche
//! Backendlaufzeit eines Frames wird aus `(seed, request_id, variant)` gezogen
//! und ist damit **unabhaengig von der Reihenfolge**, in der ein Governor die
//! Arbeit startet. Ohne das wuerde allein die andere Dispatchreihenfolge eine
//! andere Zufallsfolge erzeugen und der Vergleich waere verfaelscht.
//!
//! ## Was der Governor nicht weiss
//!
//! Er plant mit `p99 * Sicherheitsmarge` aus dem Profil. Die tatsaechliche
//! Laufzeit wird aus der Verteilung gezogen. Der Scheduler kennt die Zukunft
//! also nicht — er kennt nur ihre Statistik.

// Ein ungueltiges Szenario ist ein Programmierfehler im Benchmark, kein
// Laufzeitfall. Der Lauf soll dann laut abbrechen, statt stillschweigend eine
// andere Konfiguration zu vermessen und ein Ergebnis zu melden, das zu keiner
// dokumentierten Konfiguration gehoert.
#![allow(clippy::expect_used)]

use crate::coverage::{Coverage, CoverageTracker};
use crate::rng::Pcg32;
use crate::scenario::Scenario;
use crate::workload::RuntimeDistribution;
use std::collections::HashMap;
use vig_core::arrayvec::ArrayVec;
use vig_core::ids::MAX_MODELS;
use vig_core::metrics::Metrics;
use vig_core::model::ModelContract;
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::profile::SafetyMargin;
use vig_core::request::RequestState;
use vig_core::scheduler::{Action, ActionSink, Event, Scheduler};
use vig_core::slots::SlotSet;
use vig_core::{
    Duration, Instant, ModelIdx, PayloadRef, RequestDescriptor, RequestId, SupersessionKey,
    VariantIdx,
};

use crate::baseline::BaselineScheduler;

/// Weckintervall des Simulators.
///
/// Beide Governoren werden identisch geweckt, damit kein Vorteil aus der
/// Weckstrategie entsteht.
const TICK: u64 = 1_000_000;

/// Sammelt die Aktionen eines Governors.
#[derive(Debug, Default)]
pub struct Actions(pub Vec<Action>);

impl ActionSink for Actions {
    fn emit(&mut self, action: Action) {
        self.0.push(action);
    }
}

/// Der zu vermessende Governor.
#[derive(Debug)]
pub enum Governor {
    /// Der Governor-Scheduler.
    Vigilant(Box<Scheduler>),
    /// Die FIFO-Baseline mit Prioritaet.
    Baseline(Box<BaselineScheduler>),
}

impl Governor {
    /// Reicht ein Ereignis an den Governor weiter.
    ///
    /// Oeffentlich fuer Messwerkzeuge, die den Entscheidungspfad selbst
    /// vermessen (`decision-bench`, ueber [`run_observed`]).
    pub fn on_event(&mut self, now: Instant, event: Event, sink: &mut Actions) {
        match self {
            Self::Vigilant(s) => s.on_event(now, event, sink),
            Self::Baseline(s) => s.on_event(now, event, sink),
        }
    }

    fn metrics(&self) -> &Metrics {
        match self {
            Self::Vigilant(s) => s.metrics(),
            Self::Baseline(s) => s.metrics(),
        }
    }
}

/// Baut den Governor-Governor fuer ein Szenario.
///
/// # Panics
///
/// Nie im regulaeren Betrieb: die Szenarien in [`crate::scenario`] sind
/// gueltig. Ein Fehler hier bedeutet ein kaputtes Szenario und soll den Lauf
/// abbrechen, nicht stillschweigend eine andere Konfiguration messen.
#[must_use]
pub fn vig(scenario: &Scenario, margin: SafetyMargin) -> Governor {
    let contracts: ArrayVec<ModelContract, MAX_MODELS> = scenario.vig_contracts();
    let slots = SlotSet::homogeneous(scenario.slots, scenario.pipelining)
        .expect("Szenario nennt eine gueltige Slot-Konfiguration");
    let overload = OverloadController::new(OverloadConfig::default(), Instant::ZERO)
        .expect("Default-Ueberlastkonfiguration ist gueltig");
    let scheduler = Scheduler::new(contracts, slots, overload, margin)
        .expect("Szenario nennt gueltige Modellvertraege");
    Governor::Vigilant(Box::new(scheduler))
}

/// Wie [`vig`], mit dem Versorgungsschutz des Look-ahead (ADR-0041).
///
/// # Panics
///
/// Wie [`vig`].
#[must_use]
pub fn vig_protecting_supply(scenario: &Scenario, margin: SafetyMargin) -> Governor {
    match vig(scenario, margin) {
        Governor::Vigilant(mut scheduler) => {
            scheduler.set_protect_supply(true);
            Governor::Vigilant(scheduler)
        }
        Governor::Baseline(baseline) => Governor::Baseline(baseline),
    }
}

/// Baut die Baseline mit einer bestimmten Queue-Tiefe.
///
/// # Panics
///
/// Wie [`vig`].
#[must_use]
pub fn baseline(scenario: &Scenario, capacity: usize) -> Governor {
    let contracts = scenario.baseline_contracts(capacity);
    let slots = SlotSet::homogeneous(scenario.slots, scenario.pipelining)
        .expect("Szenario nennt eine gueltige Slot-Konfiguration");
    Governor::Baseline(Box::new(BaselineScheduler::new(contracts, slots, capacity)))
}

/// Das Ergebnis eines Laufs.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// Bezeichnung des Governors im Report.
    pub label: String,
    /// Die Zaehler des Governors.
    pub metrics: Metrics,
    /// Coverage und Age of Information je Stream.
    pub coverage: Vec<(&'static str, Coverage)>,
}

impl RunResult {
    /// Die schlechteste Abdeckung ueber alle geschuetzten Streams in Promille.
    #[must_use]
    pub fn worst_guarded_uncovered_permille(&self) -> u64 {
        self.coverage
            .iter()
            .map(|(_, c)| c.uncovered_permille())
            .max()
            .unwrap_or(0)
    }

    /// Die Abdeckungsluecke eines benannten Streams in Promille.
    #[must_use]
    pub fn uncovered_permille(&self, stream: &str) -> u64 {
        self.coverage
            .iter()
            .find(|(name, _)| *name == stream)
            .map_or(0, |(_, c)| c.uncovered_permille())
    }
}

/// Faehrt einen Lauf.
///
/// # Panics
///
/// Wenn das Szenario ungueltige Laufzeitprofile enthaelt (`p99 < p50` oder
/// `p50 == 0`). Das ist ein Konfigurationsfehler im Szenario, kein Laufzeitfall.
#[must_use]
pub fn run(scenario: &Scenario, governor: Governor, label: String, seed: u64) -> RunResult {
    run_observed(scenario, governor, label, seed, |g, now, event, actions| {
        g.on_event(now, event, actions);
    })
}

/// Faehrt einen Lauf und reicht jedes Ereignis durch `step`.
///
/// Dieselbe Ereignisfolge wie [`run`], bitgleich: `step` muss das Ereignis
/// an den Governor weitergeben und darf darum herum messen. So vermisst
/// `decision-bench` den Entscheidungspfad auf fremder Hardware, ohne eine
/// zweite, abweichende Simulationsschleife zu pflegen.
///
/// # Panics
///
/// Wie [`run`].
#[must_use]
pub fn run_observed<F>(
    scenario: &Scenario,
    governor: Governor,
    label: String,
    seed: u64,
    step: F,
) -> RunResult
where
    F: FnMut(&mut Governor, Instant, Event, &mut Actions),
{
    run_inner(scenario, governor, label, seed, None, step)
}

/// Faehrt einen Lauf mit vorgegebenen Aufnahmezeitpunkten je Strom.
///
/// Fuer Ankunftsprozesse, die kein fester Takt sind — etwa Lastspitzen, in
/// denen dieselbe Kamera zeitweise schneller liefert (`load-ramp bursts`).
/// Vertraege, Laufzeiten, Transport und Abdeckung kommen weiter aus dem
/// Szenario; nur die Aufnahmezeitpunkte ersetzen den periodischen Prozess.
/// `captures[i]` gehoert zu `scenario.streams[i]`; Zeitpunkte nach dem Ende
/// des Laufs werden ignoriert.
///
/// # Panics
///
/// Wie [`run`].
#[must_use]
pub fn run_captures(
    scenario: &Scenario,
    governor: Governor,
    label: String,
    seed: u64,
    captures: &[Vec<Instant>],
) -> RunResult {
    run_inner(
        scenario,
        governor,
        label,
        seed,
        Some(captures),
        Governor::on_event,
    )
}

fn run_inner<F>(
    scenario: &Scenario,
    mut governor: Governor,
    label: String,
    seed: u64,
    captures: Option<&[Vec<Instant>]>,
    mut step: F,
) -> RunResult
where
    F: FnMut(&mut Governor, Instant, Event, &mut Actions),
{
    // Laufzeitverteilungen je Modell und Variante.
    let dists: Vec<Vec<RuntimeDistribution>> = scenario
        .streams
        .iter()
        .map(|s| {
            s.variants
                .iter()
                .map(|v| {
                    RuntimeDistribution::from_percentiles(v.p50, v.p99)
                        .expect("Szenario nennt ein gueltiges Laufzeitprofil")
                })
                .collect()
        })
        .collect();

    let contracts = scenario.vig_contracts();
    let mut trackers: Vec<CoverageTracker> = scenario
        .streams
        .iter()
        .map(|s| CoverageTracker::new(s.period, s.max_age, Instant::ZERO, scenario.duration))
        .collect();

    let mut clock = crate::event::SimClock::new();
    let mut meta: HashMap<u64, (usize, Instant)> = HashMap::new();
    let mut pending: HashMap<u64, RequestDescriptor> = HashMap::new();
    schedule_arrivals(
        scenario,
        &contracts,
        seed,
        captures,
        &mut clock,
        &mut meta,
        &mut pending,
    );

    while let Some((now, event)) = clock.advance() {
        let mut actions = Actions::default();
        match event {
            crate::event::SimEvent::Arrival { key, .. } => {
                if let Some(descriptor) = pending.remove(&key.0) {
                    step(&mut governor, now, Event::Arrival(descriptor), &mut actions);
                }
            }
            crate::event::SimEvent::Completion { request, slot } => {
                step(
                    &mut governor,
                    now,
                    Event::Completion { request, slot },
                    &mut actions,
                );
            }
            crate::event::SimEvent::BackendFailure { request, slot } => {
                step(
                    &mut governor,
                    now,
                    Event::BackendFailure { request, slot },
                    &mut actions,
                );
            }
            crate::event::SimEvent::Tick | crate::event::SimEvent::EndOfRun => {
                step(&mut governor, now, Event::Tick, &mut actions);
            }
        }

        for action in &actions.0 {
            match *action {
                Action::Dispatch {
                    request,
                    model,
                    variant,
                    slot,
                    ..
                } => {
                    let runtime = sample_runtime(&dists, model, variant, request, seed);
                    let finish = now.checked_add(runtime).unwrap_or(now);
                    let _ = clock
                        .schedule(finish, crate::event::SimEvent::Completion { request, slot });
                }
                Action::Terminate { request, state } => {
                    if matches!(
                        state,
                        RequestState::CompletedValid | RequestState::CompletedObsolete
                    ) && let Some((model, generation)) = meta.get(&request.0)
                        && let Some(tracker) = trackers.get_mut(*model)
                    {
                        tracker.record_delivery(now, *generation);
                    }
                }
                Action::WakeAt(_) | Action::ObservedRuntime { .. } => {}
            }
        }
    }

    let coverage = scenario
        .streams
        .iter()
        .zip(trackers.iter())
        .map(|(s, t)| (s.name, t.finish()))
        .collect();

    RunResult {
        label,
        metrics: *governor.metrics(),
        coverage,
    }
}

/// Plant alle Ankuenfte des Szenarios ein.
///
/// Die Reihenfolge ist durch `(Stream, n)` festgelegt und damit fuer beide
/// Governoren identisch — der Workload ist keine Variable des Vergleichs.
fn schedule_arrivals(
    scenario: &Scenario,
    contracts: &ArrayVec<ModelContract, MAX_MODELS>,
    seed: u64,
    captures: Option<&[Vec<Instant>]>,
    clock: &mut crate::event::SimClock,
    meta: &mut HashMap<u64, (usize, Instant)>,
    pending: &mut HashMap<u64, RequestDescriptor>,
) {
    let mut arrival_rng = Pcg32::new(seed, 1);
    let mut next_id = 0_u64;
    let end = Instant::ZERO
        .checked_add(scenario.duration)
        .unwrap_or(Instant::from_nanos(u64::MAX));

    for (model, stream) in scenario.streams.iter().enumerate() {
        let given = captures.map(|c| c.get(model).map_or(&[][..], Vec::as_slice));
        let mut n = 0_u64;
        loop {
            let capture = match given {
                Some(list) => {
                    let Some(at) = usize::try_from(n).ok().and_then(|i| list.get(i)) else {
                        break;
                    };
                    *at
                }
                None => stream.capture_spec().capture_at(n, &mut arrival_rng),
            };
            if capture >= end {
                break;
            }
            let arrival = stream.capture_spec().arrival_at(capture);
            next_id = next_id.saturating_add(1);
            let Some(contract) = contracts.get(model) else {
                break;
            };
            let descriptor = RequestDescriptor {
                id: RequestId(next_id),
                logical_model: ModelIdx(u16::try_from(model).unwrap_or(u16::MAX)),
                supersession_key: SupersessionKey::DEFAULT,
                generation_time: capture,
                arrival_time: arrival,
                absolute_deadline: capture.checked_add(contract.deadline),
                max_age: contract.max_age,
                criticality: contract.criticality,
                queue_policy: contract.queue.policy,
                stateful: false,
                variant: None,
                payload: PayloadRef(next_id),
                context_tokens: 0,
                // Der Simulator kennt keine Prompts; zerlegbar ist, was der
                // Vertrag zerlegbar nennt.
                decomposable: contract.cooperative.is_some(),
            };
            meta.insert(next_id, (model, capture));
            pending.insert(next_id, descriptor);
            let _ = clock.schedule(
                arrival,
                crate::event::SimEvent::Arrival {
                    model: descriptor.logical_model,
                    key: SupersessionKey(next_id),
                    generation: capture,
                },
            );
            n = n.saturating_add(1);
        }
    }

    // Gleichmaessige Weckrufe fuer beide Governoren.
    let mut t = 0_u64;
    while t < scenario.duration.as_nanos() {
        let _ = clock.schedule(Instant::from_nanos(t), crate::event::SimEvent::Tick);
        t = t.saturating_add(TICK);
    }
}

/// Zieht die tatsaechliche Laufzeit eines Frames.
///
/// Der Seed haengt an `(seed, request_id, variant)` und **nicht** am
/// Fortschritt eines gemeinsamen RNG-Stroms. Dadurch bekommt derselbe Frame in
/// beiden Laeufen dieselbe Laufzeit, unabhaengig davon, in welcher Reihenfolge
/// ein Governor Arbeit startet.
fn sample_runtime(
    dists: &[Vec<RuntimeDistribution>],
    model: ModelIdx,
    variant: VariantIdx,
    request: RequestId,
    seed: u64,
) -> Duration {
    let Some(per_model) = dists.get(model.get()) else {
        return Duration::ZERO;
    };
    let Some(dist) = per_model.get(variant.get()).or_else(|| per_model.first()) else {
        return Duration::ZERO;
    };
    let mut rng = Pcg32::new(seed ^ request.0.rotate_left(17), u64::from(variant.0) + 7);
    dist.sample(&mut rng)
}
