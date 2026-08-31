//! Die Vergleichsbasis: FIFO mit modelluebergreifender Prioritaet.
//!
//! Spec 19.1 ist eindeutig: *„OneTimer darf nicht gegen einen absichtlich
//! schlecht konfigurierten Triton gewinnen."* Diese Baseline bildet deshalb
//! ab, was ein **gut konfigurierter** generischer Serving-Stack leistet:
//!
//! * bounded Queues je Modell — kein unbegrenztes Puffern,
//! * modelluebergreifende Prioritaet, wie sie Tritons Rate Limiter bietet
//!   (Spec 3.1: die Aussage „Triton hat keine Prioritaeten" waere falsch),
//! * dieselben Slots, dieselben Vertraege, dieselben Laufzeitprofile,
//! * dieselben Weckrufe.
//!
//! Was die Baseline **nicht** tut, ist genau die Produktthese:
//! keine Supersession, keine Alterspruefung, keine deadline-bewusste Zulassung,
//! keine Variantenwahl, kein Look-ahead. Sie arbeitet die Warteschlange ab.
//!
//! Die Queue-Tiefe ist bewusst ein Parameter des Vergleichs: eine flache Queue
//! wirft Arbeit weg, eine tiefe erzeugt Rueckstand. Der Report faehrt beide
//! Enden, damit die Baseline nicht an einer schlecht gewaehlten Zahl scheitert.

use onetimer_core::arrayvec::ArrayVec;
use onetimer_core::ids::{MAX_MODELS, ModelIdx, RequestId, SlotIdx};
use onetimer_core::metrics::Metrics;
use onetimer_core::model::ModelContract;
use onetimer_core::profile::SafetyMargin;
use onetimer_core::request::{Criticality, RequestDescriptor, RequestState};
use onetimer_core::scheduler::{Action, ActionSink, Event};
use onetimer_core::slots::SlotSet;
use onetimer_core::{Instant, VariantIdx};

/// Ein FIFO-Governor mit Prioritaetsordnung.
#[derive(Debug)]
pub struct BaselineScheduler {
    contracts: ArrayVec<ModelContract, MAX_MODELS>,
    queues: Vec<Vec<RequestDescriptor>>,
    capacity: usize,
    slots: SlotSet,
    inflight: Vec<(RequestDescriptor, Instant)>,
    metrics: Metrics,
}

impl BaselineScheduler {
    /// Baut die Baseline.
    #[must_use]
    pub fn new(
        contracts: ArrayVec<ModelContract, MAX_MODELS>,
        slots: SlotSet,
        capacity: usize,
    ) -> Self {
        let queues = vec![Vec::new(); contracts.len()];
        Self {
            contracts,
            queues,
            capacity,
            slots,
            inflight: Vec::new(),
            metrics: Metrics::default(),
        }
    }

    /// Die Zaehler des Laufs.
    #[must_use]
    pub const fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Verarbeitet ein Ereignis.
    pub fn on_event<S: ActionSink>(&mut self, now: Instant, event: Event, sink: &mut S) {
        match event {
            Event::Arrival(descriptor) => {
                self.metrics.received = self.metrics.received.saturating_add(1);
                let model = descriptor.logical_model.get();
                match self.queues.get_mut(model) {
                    Some(queue) if queue.len() < self.capacity => queue.push(descriptor),
                    Some(_) => {
                        // Volle Queue: der neue Request wird abgewiesen. Kein
                        // stilles unbegrenztes Puffern - auch die Baseline
                        // haelt sich an L-003.
                        self.metrics.rejected_capacity =
                            self.metrics.rejected_capacity.saturating_add(1);
                        sink.emit(Action::Terminate {
                            request: descriptor.id,
                            state: RequestState::RejectedInfeasible,
                        });
                    }
                    None => {}
                }
            }
            Event::Completion { request, slot } => self.complete(now, request, slot, sink),
            Event::BackendFailure { request, slot } => {
                self.slots.complete(slot, request);
                self.metrics.backend_failures = self.metrics.backend_failures.saturating_add(1);
                self.inflight.retain(|(d, _)| d.id != request);
                sink.emit(Action::Terminate {
                    request,
                    state: RequestState::Failed,
                });
            }
            Event::Tick => {}
        }
        self.dispatch(now, sink);
    }

    fn complete<S: ActionSink>(
        &mut self,
        now: Instant,
        request: RequestId,
        slot: SlotIdx,
        sink: &mut S,
    ) {
        self.slots.complete(slot, request);
        let Some(index) = self.inflight.iter().position(|(d, _)| d.id == request) else {
            return;
        };
        let (descriptor, started) = self.inflight.swap_remove(index);

        let compute = now.saturating_since(started);
        self.metrics.total_compute_nanos = self
            .metrics
            .total_compute_nanos
            .saturating_add(compute.as_nanos());

        // Dieselbe Bewertung wie bei OneTimer: war das Ergebnis bei
        // Fertigstellung noch aktuell? Die Baseline wird dafuer nicht
        // bestraft - es wird nur gemessen, wie viel ihrer Rechenzeit in
        // bereits wertlose Ergebnisse floss.
        let obsolete = descriptor.is_over_age(now);
        if obsolete {
            self.metrics.completed_obsolete = self.metrics.completed_obsolete.saturating_add(1);
            self.metrics.stale_compute_nanos = self
                .metrics
                .stale_compute_nanos
                .saturating_add(compute.as_nanos());
        } else {
            self.metrics.completed_valid = self.metrics.completed_valid.saturating_add(1);
        }
        if descriptor.absolute_deadline.is_some_and(|d| now > d) {
            self.metrics.deadline_misses = self.metrics.deadline_misses.saturating_add(1);
            if descriptor.criticality.is_guarded() {
                self.metrics.protected_deadline_misses =
                    self.metrics.protected_deadline_misses.saturating_add(1);
            }
        }
        sink.emit(Action::Terminate {
            request,
            state: if obsolete {
                RequestState::CompletedObsolete
            } else {
                RequestState::CompletedValid
            },
        });
    }

    /// Reicht weiter, solange Kapazitaet da ist: hoechste Prioritaet zuerst,
    /// innerhalb einer Klasse in Ankunftsreihenfolge.
    fn dispatch<S: ActionSink>(&mut self, now: Instant, sink: &mut S) {
        loop {
            let Some((model, index)) = self.pick() else {
                return;
            };
            let model_idx = ModelIdx(u16::try_from(model).unwrap_or(u16::MAX));
            let Some(slot) = self.slots.ready_slot(model_idx, now) else {
                return;
            };

            let Some(queue) = self.queues.get_mut(model) else {
                return;
            };
            if index >= queue.len() {
                return;
            }
            let descriptor = queue.remove(index);

            // Immer die beste Variante: die Baseline waehlt nicht.
            let Some(contract) = self.contracts.get(model) else {
                return;
            };
            let Some(variant) = contract.variants.get(0) else {
                return;
            };
            let Ok(predicted) = variant.profile.conservative_at(0, SafetyMargin::NONE) else {
                return;
            };

            if self
                .slots
                .dispatch(slot, descriptor.id, model_idx, now, predicted)
                .is_err()
            {
                return;
            }
            self.inflight.push((descriptor, now));
            self.metrics.forwarded = self.metrics.forwarded.saturating_add(1);
            self.metrics.count_variant(VariantIdx::BEST);
            sink.emit(Action::Dispatch {
                request: descriptor.id,
                model: model_idx,
                variant: VariantIdx::BEST,
                slot,
                predicted_runtime: predicted,
            });
        }
    }

    /// Der naechste Kandidat: hoechste Kritikalitaet, dann aelteste Ankunft.
    fn pick(&self) -> Option<(usize, usize)> {
        let mut best: Option<(Criticality, Instant, usize, usize)> = None;
        for (model, queue) in self.queues.iter().enumerate() {
            for (index, descriptor) in queue.iter().enumerate() {
                let key = (
                    descriptor.criticality,
                    descriptor.arrival_time,
                    model,
                    index,
                );
                let better = match best {
                    None => true,
                    Some((c, a, _, _)) => {
                        (core::cmp::Reverse(key.0), key.1) < (core::cmp::Reverse(c), a)
                    }
                };
                if better {
                    best = Some(key);
                }
            }
        }
        best.map(|(_, _, model, index)| (model, index))
    }
}
