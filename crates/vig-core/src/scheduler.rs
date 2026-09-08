//! Der Single-Owner-Scheduler (Spec 9.4, WP3/WP9).
//!
//! Ein reiner Zustandsautomat: Ereignisse hinein, Aktionen hinaus. Er ruft
//! keine Uhr ab, oeffnet keine Verbindung und kennt Triton nicht. Genau
//! deshalb laeuft derselbe Code im Simulator und im Gateway, und ein
//! Live-Trace ist offline exakt reproduzierbar (Spec 30.1, 30.2).
//!
//! ## Die Entscheidungsreihenfolge
//!
//! Lexikographisch, nicht gewichtet (Spec 10.6). Es gibt keine Score-Funktion,
//! in der viele Best-Effort-Requests einen Protected-Request aufwiegen koennen:
//!
//! 1. Kritikalitaet absteigend,
//! 2. dann fruehste absolute Deadline (EDF, Spec 10.5),
//! 3. dann aelteste Generation Time.
//!
//! ## Warum der Scheduler absichtlich nichts tut
//!
//! Trifft ein Kandidat auf ein Veto aus dem Protected-Look-ahead, wird er
//! **verschoben, nicht abgelehnt**. Die Ressource bleibt kurz ungenutzt, damit
//! erwartbare wichtigere Arbeit rechtzeitig starten kann (Spec 10.7). Dieses
//! bewusste Idle ist der Punkt, an dem sich Vigilant von einem
//! work-conserving Scheduler trennt.

use crate::arrayvec::ArrayVec;
use crate::estimator::{MarginController, RuntimeEstimator};
use crate::feasibility::{DEFAULT_HORIZON, ExpectedArrival, GuardVerdict, guard_protected};
use crate::ids::{MAX_MODELS, ModelIdx, RequestId, SlotIdx, VariantIdx};
use crate::metrics::Metrics;
use crate::model::{ContractError, ModelContract};
use crate::overload::{OverloadController, OverloadState, PressureSample};
use crate::profile::SafetyMargin;
use crate::queue::{DropReason, MAX_QUEUE_CAPACITY, ModelQueue, QueueConfigError};
use crate::request::{Criticality, RequestDescriptor, RequestState};
use crate::slots::SlotSet;
use crate::time::{Duration, Instant};
use crate::variant::{PlanningContext, Resolution, VariantState, resolve};

/// Hoechstzahl gleichzeitig an das Backend uebergebener Requests.
const MAX_INFLIGHT: usize = crate::ids::MAX_SLOTS * crate::slots::MAX_SLOT_DEPTH;

/// Ein Ereignis, das den Scheduler erreicht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Ein neuer Request ist am Gateway eingetroffen.
    Arrival(RequestDescriptor),
    /// Das Backend meldet eine Fertigstellung.
    Completion {
        /// Der fertiggestellte Request.
        request: RequestId,
        /// Der Slot, der dadurch frei wird.
        slot: SlotIdx,
    },
    /// Das Backend meldet einen Fehler.
    BackendFailure {
        /// Der betroffene Request.
        request: RequestId,
        /// Der Slot, der dadurch frei wird.
        slot: SlotIdx,
    },
    /// Der Client zieht einen Request zurueck.
    ///
    /// Wirkt nur, solange der Request wartet. Was bereits am Backend ist,
    /// laeuft zu Ende — ein laufender Aufruf laesst sich nicht zuverlaessig
    /// zurueckholen, und ein halb abgebrochener Aufruf waere bei einem
    /// zustandsbehafteten Modell schlimmer als der zu Ende gefuehrte.
    Cancel {
        /// Der zurueckgezogene Request.
        request: RequestId,
    },
    /// Ein Weckruf ohne aeusseren Anlass.
    Tick,
}

/// Eine Anweisung des Schedulers an seine Umgebung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Den Request mit dieser Variante an das Backend weiterreichen.
    Dispatch {
        /// Der Request.
        request: RequestId,
        /// Das logische Modell.
        model: ModelIdx,
        /// Die gewaehlte physische Variante.
        variant: VariantIdx,
        /// Der Zielslot.
        slot: SlotIdx,
        /// Die konservativ prognostizierte Laufzeit.
        ///
        /// Bei einem Quantum die Dauer **dieses Quantums**, nicht die des
        /// gesamten Auftrags. Sonst waere die Slot-Planung falsch und die
        /// Zerlegung wirkungslos.
        predicted_runtime: Duration,
        /// Die Tokenzahl dieses Quantums, falls der Auftrag zerlegt wird.
        ///
        /// `None` fuer alles, was nicht zerlegbar ist — also fuer jedes
        /// Wahrnehmungsmodell (ADR-0014).
        quantum: Option<u32>,
    },
    /// Den Request terminal abschliessen und den Client informieren.
    Terminate {
        /// Der Request.
        request: RequestId,
        /// Der terminale Zustand.
        state: RequestState,
    },
    /// Eine beobachtete Backendlaufzeit melden.
    ///
    /// Die Rueckkopplung fuer den Online Runtime Estimator (Spec 13.2, WP11).
    /// Sie traegt den Belegungsgrad mit, unter dem gemessen wurde, weil genau
    /// das den Interferenzeffekt datengetrieben erfasst (ADR-0006).
    ObservedRuntime {
        /// Das logische Modell.
        model: ModelIdx,
        /// Die ausgefuehrte Variante.
        variant: VariantIdx,
        /// Der Belegungsgrad beim Start.
        occupancy: usize,
        /// Die gemessene Laufzeit.
        runtime: Duration,
    },
    /// Den Scheduler spaetestens zu diesem Zeitpunkt erneut aufrufen.
    ///
    /// Ohne diesen Weckruf bliebe eine non-work-conserving Entscheidung
    /// haengen: der Scheduler hat absichtlich nichts gestartet und wuerde ohne
    /// aeusseres Ereignis nie wieder nachsehen.
    WakeAt(Instant),
}

/// Empfaenger der Scheduler-Aktionen.
///
/// Als Trait statt als Rueckgabepuffer, damit der Kern keine Obergrenze fuer
/// die Zahl der Aktionen erfinden muss: die Umgebung entscheidet, ob sie in
/// einen Kanal, einen Vektor oder eine Testliste schreibt.
pub trait ActionSink {
    /// Nimmt eine Aktion entgegen.
    fn emit(&mut self, action: Action);
}

impl<F: FnMut(Action)> ActionSink for F {
    fn emit(&mut self, action: Action) {
        self(action);
    }
}

/// Ein an das Backend uebergebener Request samt seiner Planung.
#[derive(Debug, Clone, Copy)]
struct Dispatched {
    descriptor: RequestDescriptor,
    variant: VariantIdx,
    at: Instant,
    occupancy: usize,
    /// Die Laufzeit, mit der geplant wurde.
    ///
    /// Nur im Vergleich dazu ist die beobachtete Laufzeit eine Aussage: sie
    /// sagt, ob die **Prognose** falsch war. Eine verpasste Deadline sagt das
    /// nicht — die kann genauso aus Warteschlangenzeit entstehen.
    predicted: Duration,
}

/// Die Planung eines Kandidaten: welche Variante, wie lange, und reicht es.
#[derive(Debug, Clone, Copy)]
struct Plan {
    variant: VariantIdx,
    predicted_runtime: Duration,
    /// Optimistisch geschaetzte Fertigstellung (`p50`).
    ///
    /// Grundlage der Verwerfensentscheidung nach ADR-0010: verworfen wird nur,
    /// was selbst im guenstigen Fall wertlos waere.
    optimistic_finish: Instant,
    /// Ob die Deadline nach konservativer Planung noch haltbar ist.
    ///
    /// Steuert nur noch Metrik und Variantenwahl, nicht mehr das Verwerfen.
    feasible: bool,
}

/// Warum ein Scheduler nicht gebaut werden konnte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerError {
    /// Kein Modell konfiguriert.
    NoModels,
    /// Mehr Modelle als [`MAX_MODELS`].
    TooManyModels {
        /// Die geforderte Anzahl.
        requested: usize,
    },
    /// Ein Modellvertrag ist unzulaessig.
    Contract {
        /// Der Index des Modells.
        model: usize,
        /// Der Fehler.
        error: ContractError,
    },
    /// Eine Queue-Konfiguration ist unzulaessig.
    Queue {
        /// Der Index des Modells.
        model: usize,
        /// Der Fehler.
        error: QueueConfigError,
    },
}

impl core::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoModels => write!(f, "keine Modelle konfiguriert"),
            Self::TooManyModels { requested } => {
                write!(f, "{requested} Modelle, Maximum {MAX_MODELS}")
            }
            Self::Contract { model, error } => write!(f, "Modell {model}: {error}"),
            Self::Queue { model, error } => write!(f, "Modell {model}: {error}"),
        }
    }
}

impl core::error::Error for SchedulerError {}

/// Der Scheduler.
#[derive(Debug)]
pub struct Scheduler {
    contracts: ArrayVec<ModelContract, MAX_MODELS>,
    queues: ArrayVec<ModelQueue, MAX_MODELS>,
    variant_states: [VariantState; MAX_MODELS],
    next_expected: [Option<Instant>; MAX_MODELS],
    slots: SlotSet,
    overload: OverloadController,
    /// Die vom Betreiber gesetzte Ausgangsmarge.
    margin: SafetyMargin,
    /// Je Modell eine langsam angepasste Marge (Spec 13.3).
    margins: [MarginController; MAX_MODELS],
    /// Beobachtete Backendlaufzeiten je Modell, Variante und Belegungsgrad.
    estimator: RuntimeEstimator,
    /// Beobachtete Ankunftsabstaende je Modell.
    arrivals: [crate::arrival::ArrivalTracker; MAX_MODELS],
    horizon: Duration,
    inflight: ArrayVec<Dispatched, MAX_INFLIGHT>,
    metrics: Metrics,
}

impl Scheduler {
    /// Baut einen Scheduler aus geprueften Vertraegen.
    ///
    /// # Errors
    ///
    /// Siehe [`SchedulerError`]. Eine ungueltige Konfiguration verhindert den
    /// Start, statt still mit riskanten Defaults weiterzulaufen (Spec L-020).
    pub fn new(
        contracts: ArrayVec<ModelContract, MAX_MODELS>,
        slots: SlotSet,
        overload: OverloadController,
        margin: SafetyMargin,
    ) -> Result<Self, SchedulerError> {
        if contracts.is_empty() {
            return Err(SchedulerError::NoModels);
        }
        let mut queues = ArrayVec::new();
        for (i, contract) in contracts.iter().enumerate() {
            contract
                .validate()
                .map_err(|error| SchedulerError::Contract { model: i, error })?;
            let queue = ModelQueue::new(contract.queue, contract.stateful)
                .map_err(|error| SchedulerError::Queue { model: i, error })?;
            queues
                .push(queue)
                .map_err(|_| SchedulerError::TooManyModels {
                    requested: contracts.len(),
                })?;
        }
        // Die konfigurierten Perioden gehoeren in den Metrikabzug, damit sich
        // das Verhaeltnis zur beobachteten Rate ohne Kenntnis der
        // Konfigurationsdatei bilden laesst.
        let mut metrics = Metrics::default();
        for (i, contract) in contracts.iter().enumerate() {
            if let Some(period) = contract.period
                && let Some(cell) = metrics.contract_period_us.get_mut(i)
            {
                *cell = u32::try_from(period.as_nanos().checked_div(1_000).unwrap_or(0))
                    .unwrap_or(u32::MAX);
            }
        }

        Ok(Self {
            contracts,
            queues,
            variant_states: [VariantState::default(); MAX_MODELS],
            next_expected: [None; MAX_MODELS],
            arrivals: [crate::arrival::ArrivalTracker::default(); MAX_MODELS],
            slots,
            overload,
            margin,
            margins: [MarginController::new(margin); MAX_MODELS],
            estimator: RuntimeEstimator::new(),
            horizon: DEFAULT_HORIZON,
            inflight: ArrayVec::new(),
            metrics,
        })
    }

    /// Setzt den Look-ahead-Horizont (Spec 10.8).
    pub const fn set_horizon(&mut self, horizon: Duration) {
        self.horizon = horizon;
    }

    /// Der Online Runtime Estimator, fuer Diagnose und Tests.
    #[must_use]
    pub const fn estimator(&self) -> &RuntimeEstimator {
        &self.estimator
    }

    /// Liefert dieses Modell dauerhaft schneller, als sein Vertrag erlaubt?
    ///
    /// `None`, solange zu wenig gemessen wurde. Ein Vertrag, den die Last
    /// sprengt, ist ein Befund und kein Betriebszustand: der Governor kann
    /// ihn einhalten oder die Last bedienen, aber nicht beides.
    #[must_use]
    pub fn arrival_exceeds_contract(&self, model: ModelIdx) -> Option<bool> {
        let tracker = self.arrivals.get(model.get())?;
        let period = self.contracts.get(model.get()).and_then(|c| c.period);
        tracker.exceeds(period)
    }

    /// Behandelt das Profil dieses Modells als nicht verifiziert (G-010).
    ///
    /// Wird beim Start aufgerufen, wenn der Fingerabdruck des Backends nicht
    /// zu dem passt, unter dem das Profil gemessen wurde. Die Planung wird
    /// dadurch vorsichtiger, ohne den Betrieb zu verweigern (ADR-0016).
    pub fn mark_profile_unverified(&mut self, model: ModelIdx) {
        if let Some(controller) = self.margins.get_mut(model.get()) {
            *controller = MarginController::provisional(self.margin);
        }
    }

    /// Die aktuell wirksame Marge eines Modells.
    #[must_use]
    pub fn margin_of(&self, model: ModelIdx) -> SafetyMargin {
        self.margins
            .get(model.get())
            .map_or(self.margin, MarginController::margin)
    }

    /// Setzt den Ueberlastzustand von aussen.
    ///
    /// Ausschliesslich fuer Tests und den Simulator: im Betrieb bestimmt ihn
    /// der Regler aus beobachtetem Druck. Eine Stufe von Hand zu setzen waere
    /// dort genau die Steuerung an den Messwerten vorbei, die dieses Produkt
    /// vermeiden soll.
    pub const fn force_overload_state(&mut self, state: crate::overload::OverloadState) {
        self.overload.force(state);
    }

    /// Die aktuellen Zaehler.
    #[must_use]
    pub const fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Der aktuelle Ueberlastzustand.
    #[must_use]
    pub const fn overload_state(&self) -> OverloadState {
        self.overload.state()
    }

    /// Die Slot-Belegung, fuer Diagnose und Tests.
    #[must_use]
    pub const fn slots(&self) -> &SlotSet {
        &self.slots
    }

    /// Verarbeitet ein Ereignis und emittiert die daraus folgenden Aktionen.
    pub fn on_event<S: ActionSink>(&mut self, now: Instant, event: Event, sink: &mut S) {
        match event {
            Event::Arrival(descriptor) => self.on_arrival(now, descriptor, sink),
            Event::Completion { request, slot } => self.on_completion(now, request, slot, sink),
            Event::BackendFailure { request, slot } => {
                self.on_failure(now, request, slot, sink);
            }
            Event::Cancel { request } => self.on_cancel(request, sink),
            Event::Tick => {}
        }
        self.schedule(now, sink);
    }

    fn on_arrival<S: ActionSink>(
        &mut self,
        now: Instant,
        descriptor: RequestDescriptor,
        sink: &mut S,
    ) {
        self.metrics.received = self.metrics.received.saturating_add(1);
        let model = descriptor.logical_model;

        // Wie schnell dieser Strom tatsaechlich liefert. Billig genug fuer den
        // heissen Pfad: ein Vergleich, eine Schiebeoperation, zwei Speicher.
        if let Some(tracker) = self.arrivals.get_mut(model.get()) {
            tracker.record(now);
            if let Some(observed) = tracker.observed()
                && let Some(cell) = self.metrics.arrival_period_us.get_mut(model.get())
            {
                *cell = u32::try_from(observed.as_nanos().checked_div(1_000).unwrap_or(0))
                    .unwrap_or(u32::MAX);
            }
        }

        // Die naechste Ankunft dieses Modells fortschreiben (Spec 10.8).
        if let Some(contract) = self.contracts.get(model.get())
            && let Some(period) = contract.period
            && let Some(expected) = now.checked_add(period)
            && let Some(slot) = self.next_expected.get_mut(model.get())
        {
            *slot = Some(expected);
        }

        let state = self.overload.state();
        if !state.admits(descriptor.criticality) {
            sink.emit(Action::Terminate {
                request: descriptor.id,
                state: RequestState::RejectedInfeasible,
            });
            self.metrics.rejected_infeasible = self.metrics.rejected_infeasible.saturating_add(1);
            return;
        }

        let Some(queue) = self.queues.get_mut(model.get()) else {
            sink.emit(Action::Terminate {
                request: descriptor.id,
                state: RequestState::RejectedInfeasible,
            });
            return;
        };

        let outcome = queue.push(descriptor);
        for eviction in outcome.evicted.iter() {
            self.count_drop(eviction.reason);
            sink.emit(Action::Terminate {
                request: eviction.id(),
                state: eviction.state(),
            });
        }
        if let Some(reason) = outcome.rejected {
            self.count_drop(reason);
            sink.emit(Action::Terminate {
                request: descriptor.id,
                state: reason.terminal_state(),
            });
        }
    }

    fn on_completion<S: ActionSink>(
        &mut self,
        now: Instant,
        request: RequestId,
        slot: SlotIdx,
        sink: &mut S,
    ) {
        self.slots.complete(slot, request);
        let Some(index) = self
            .inflight
            .iter()
            .position(|d| d.descriptor.id == request)
        else {
            return;
        };
        let Some(entry) = self.inflight.remove(index) else {
            return;
        };

        let compute = now.saturating_since(entry.at);
        self.metrics.total_compute_nanos = self
            .metrics
            .total_compute_nanos
            .saturating_add(compute.as_nanos());
        // Der Kern verarbeitet die Beobachtung selbst und meldet sie zusaetzlich
        // nach aussen: die Umgebung braucht sie fuer Metriken, der Scheduler
        // fuer die naechste Planung.
        self.estimator.record(
            entry.descriptor.logical_model,
            entry.variant,
            entry.occupancy,
            compute,
        );
        sink.emit(Action::ObservedRuntime {
            model: entry.descriptor.logical_model,
            variant: entry.variant,
            occupancy: entry.occupancy,
            runtime: compute,
        });

        // Stufe C der Stale-Pruefung (Spec 10.3): ist das Ergebnis bei
        // Fertigstellung noch aktuell? Die Rechenzeit ist bereits verbraucht;
        // sie wird als stale_compute erfasst, statt sie zu verschweigen.
        let obsolete = entry.descriptor.is_over_age(now);
        let missed = entry
            .descriptor
            .absolute_deadline
            .is_some_and(|deadline| now > deadline);

        let state = if obsolete {
            self.metrics.completed_obsolete = self.metrics.completed_obsolete.saturating_add(1);
            self.metrics.stale_compute_nanos = self
                .metrics
                .stale_compute_nanos
                .saturating_add(compute.as_nanos());
            RequestState::CompletedObsolete
        } else {
            self.metrics.completed_valid = self.metrics.completed_valid.saturating_add(1);
            RequestState::CompletedValid
        };

        if missed {
            self.metrics.deadline_misses = self.metrics.deadline_misses.saturating_add(1);
            if entry.descriptor.criticality.is_guarded() {
                self.metrics.protected_deadline_misses =
                    self.metrics.protected_deadline_misses.saturating_add(1);
            }
        }
        // Die Marge korrigiert **Prognosefehler**, nicht Vertragsverletzungen
        // (ADR-0013). Ausloeser ist deshalb allein, ob die Arbeit laenger
        // gedauert hat als geplant. Eine verpasste Deadline aus Wartezeit
        // wuerde durch eine groessere Marge nur schlimmer.
        let underpredicted = compute.as_nanos() > entry.predicted.as_nanos();
        if let Some(controller) = self.margins.get_mut(entry.descriptor.logical_model.get()) {
            if underpredicted {
                controller.tighten();
            } else {
                controller.relax();
            }
        }

        self.observe_pressure(now, entry.descriptor.criticality, missed || obsolete);
        sink.emit(Action::Terminate { request, state });
    }

    fn on_failure<S: ActionSink>(
        &mut self,
        now: Instant,
        request: RequestId,
        slot: SlotIdx,
        sink: &mut S,
    ) {
        self.slots.complete(slot, request);
        self.metrics.backend_failures = self.metrics.backend_failures.saturating_add(1);
        let index = self
            .inflight
            .iter()
            .position(|d| d.descriptor.id == request);
        if let Some(entry) = index.and_then(|i| self.inflight.remove(i)) {
            let compute = now.saturating_since(entry.at);
            self.metrics.total_compute_nanos = self
                .metrics
                .total_compute_nanos
                .saturating_add(compute.as_nanos());
            self.observe_pressure(now, entry.descriptor.criticality, true);
        }
        sink.emit(Action::Terminate {
            request,
            state: RequestState::Failed,
        });
    }

    /// Der Hauptdurchlauf: Stale sammeln, Ueberlast bewerten, dispatchen.
    fn schedule<S: ActionSink>(&mut self, now: Instant, sink: &mut S) {
        self.overload.evaluate(now);
        self.collect_stale(now, sink);

        // Bounded: jeder Durchlauf reicht hoechstens einen Request weiter, und
        // mehr als MAX_INFLIGHT koennen nie gleichzeitig offen sein.
        //
        // Die **strukturellen** Sperren werden ueber alle Durchlaeufe
        // mitgefuehrt: fehlender Kredit, belegter Slot, Co-Run-Verbot, keine
        // Variante. Keine davon entfaellt durch einen weiteren Dispatch — frei
        // wird ein Slot erst mit einer Fertigstellung, und die ist ein neues
        // Ereignis. Ohne das Mitfuehren pruefte jeder Durchlauf dieselben
        // blockierten Modelle erneut, bei voller Auslastung 32 Mal.
        //
        // Das Veto des Look-ahead wird **nicht** mitgefuehrt. Es ist nicht
        // monoton: es lautet „ohne dich ginge es, mit dir nicht". Belegt ein
        // anderer Dispatch inzwischen den Slot, kann die geschuetzte Ankunft
        // auch ohne den Kandidaten unmachbar werden — dann gibt es nichts mehr
        // zu retten, und ihn weiter zurueckzuhalten liesse einen Slot leer
        // stehen, ohne irgendjemandem zu helfen.
        let mut blocked = crate::slots::ModelMask::NONE;
        for _ in 0..MAX_INFLIGHT {
            if !self.dispatch_one(now, &mut blocked, sink) {
                break;
            }
        }
        if let Some(wake) = self.next_wakeup(now) {
            sink.emit(Action::WakeAt(wake));
        }
    }

    fn collect_stale<S: ActionSink>(&mut self, now: Instant, sink: &mut S) {
        // Queueweise, nicht ueber alle Modelle gesammelt: `collect_stale`
        // entfernt die Eintraege bereits aus der Queue, und `Evictions` fasst
        // genau eine volle Queue. Ein gemeinsamer Puffer ueber alle Modelle
        // koennte ueberlaufen — und ein verworfener Eintrag waere ein Request,
        // der die Queue verlassen hat, aber nie eine Abschlussaktion bekommt:
        // der Client wartet dann bis zu seinem eigenen Timeout, und Antwort-
        // kanal wie Payload bleiben bis zum Prozessende belegt.
        for index in 0..self.queues.len() {
            let Some(queue) = self.queues.get_mut(index) else {
                continue;
            };
            let evicted = queue.collect_stale(now);
            for eviction in evicted.iter() {
                self.metrics.stale = self.metrics.stale.saturating_add(1);
                self.observe_pressure(now, eviction.descriptor.criticality, true);
                sink.emit(Action::Terminate {
                    request: eviction.id(),
                    state: eviction.state(),
                });
            }
        }

        if self.overload.state().aggressive_supersession() {
            self.collect_predictably_stale(now, sink);
        }
    }

    /// Verwirft wartende Arbeit, die bei Fertigstellung schon wertlos waere.
    ///
    /// Unter Frischedruck genuegt es nicht, das zu verwerfen, was **jetzt**
    /// zu alt ist. Ein Request, der die Altersgrenze erst waehrend seiner
    /// eigenen Ausfuehrung reisst, verbraucht dieselbe GPU-Zeit und liefert
    /// dasselbe Nichts — er belegt sie nur spaeter. Genau das ist die Arbeit,
    /// die unter Druck als erstes weg soll (Spec 10.3 Stufe B).
    ///
    /// Im Normalbetrieb geschieht das absichtlich **nicht** im Voraus: dort
    /// wird jeder Kandidat einzeln beim Dispatch geprueft, und eine Prognose,
    /// die sich noch aendern kann, soll dann keine Requests kosten.
    fn collect_predictably_stale<S: ActionSink>(&mut self, now: Instant, sink: &mut S) {
        let mut doomed: ArrayVec<(ModelIdx, RequestId, Criticality), MAX_QUEUE_CAPACITY> =
            ArrayVec::new();

        for index in 0..self.queues.len() {
            let Ok(raw) = u16::try_from(index) else {
                continue;
            };
            let model = ModelIdx(raw);
            let Some(queue) = self.queues.get(index) else {
                continue;
            };
            if !queue.config().policy.allows_stale_drop() {
                continue;
            }
            for descriptor in queue.iter() {
                let Some(limit) = descriptor.max_age else {
                    continue;
                };
                let Some(plan) = self.plan(model, descriptor, now) else {
                    continue;
                };
                if plan
                    .optimistic_finish
                    .saturating_since(descriptor.generation_time)
                    > limit
                    && doomed
                        .push((model, descriptor.id, descriptor.criticality))
                        .is_err()
                {
                    // Der Puffer fasst eine volle Queue. Mehr in einem
                    // Durchlauf zu verwerfen ist nicht noetig: der naechste
                    // Durchlauf kommt, und bis dahin ist nichts gestartet.
                    break;
                }
            }
        }

        for (model, request, criticality) in doomed.iter() {
            self.metrics.stale = self.metrics.stale.saturating_add(1);
            self.count_if_starved(*criticality);
            self.observe_pressure(now, *criticality, true);
            self.drop_candidate(*model, *request, RequestState::Stale, sink);
        }
    }

    /// Waehlt den naechsten Kandidaten und reicht ihn weiter.
    ///
    /// Gibt `true` zurueck, wenn etwas geschehen ist und ein weiterer Durchlauf
    /// sinnvoll sein kann.
    fn dispatch_one<S: ActionSink>(
        &mut self,
        now: Instant,
        blocked: &mut crate::slots::ModelMask,
        sink: &mut S,
    ) -> bool {
        let forecast = self.build_forecast(now);
        // Nur fuer diesen Durchlauf: das Look-ahead-Veto gilt gegen den
        // Belegungszustand von jetzt, nicht gegen den nach dem naechsten
        // Dispatch.
        let mut deferred = crate::slots::ModelMask::NONE;

        loop {
            let Some((model, id)) = self.best_candidate(blocked.union(deferred)) else {
                return false;
            };
            // Kein Kredit heisst: **dieses** Modell kann jetzt nicht starten.
            // Frueher brach hier die gesamte Dispatchschleife ab — ein
            // blockierter Spitzenkandidat liess damit Slots leer stehen, die
            // ein anderes Modell haette nutzen koennen, ohne irgendjemanden zu
            // gefaehrden. Der Veto-Mechanismus existiert genau dafuer.
            if !self.slots.has_credit(model) {
                *blocked = blocked.with(model);
                continue;
            }

            let Some(queue) = self.queues.get(model.get()) else {
                *blocked = blocked.with(model);
                continue;
            };
            let Some(descriptor) = queue.iter().find(|d| d.id == id).copied() else {
                *blocked = blocked.with(model);
                continue;
            };
            let Some(Plan {
                variant,
                predicted_runtime,
                optimistic_finish,
                feasible,
            }) = self.plan(model, &descriptor, now)
            else {
                // `NoSlot` oder `NoVariant`: beides betrifft nur dieses Modell.
                *blocked = blocked.with(model);
                continue;
            };

            if self.drop_if_worthless(now, model, &descriptor, optimistic_finish, sink) {
                continue;
            }
            // ADR-0014: zerlegbare Auftraege werden auf das Zeitbudget
            // zugeschnitten, das bis zur naechsten geschuetzten Ankunft bleibt.
            //
            // **Vor** dem Look-ahead, nicht danach: der Guard bewertet die
            // Dauer dessen, was tatsaechlich gestartet wird. Prueft er
            // stattdessen die volle Joblaufzeit, vetoiert er jeden zerlegbaren
            // Auftrag, dessen Ganzes nicht mehr passt — auch dann, wenn genau
            // dafuer ein passendes Quantum zugeschnitten worden waere. Die
            // Zerlegung bliebe dann wirkungslos, und die Messung „mit und ohne
            // Quanten identisch" waere ihr erwartetes Ergebnis.
            let (predicted_runtime, quantum) = self.size_quantum(
                model,
                descriptor.criticality,
                predicted_runtime,
                now,
                forecast.iter(),
            );

            // Look-ahead auf erwartbare wichtigere Arbeit (Spec 10.7).
            match guard_protected(
                &self.slots,
                model,
                descriptor.criticality,
                predicted_runtime,
                now,
                forecast.iter(),
                self.horizon,
            ) {
                GuardVerdict::WouldEndanger { retry_after, .. } => {
                    self.metrics.deferred_for_protected =
                        self.metrics.deferred_for_protected.saturating_add(1);
                    sink.emit(Action::WakeAt(retry_after));
                    deferred = deferred.with(model);
                    continue;
                }
                GuardVerdict::Clear => {}
            }

            let Some(slot) = self.slots.ready_slot(model, now) else {
                // Belegt oder durch ein Co-Run-Verbot gesperrt — wieder eine
                // Aussage ueber dieses Modell, nicht ueber den Rest.
                *blocked = blocked.with(model);
                continue;
            };

            // Der Belegungsgrad **vor** dem eigenen Dispatch: das ist die
            // Groesse, mit der geplant wurde, und nur unter demselben Index
            // ist die spaetere Beobachtung fuer die naechste Planung
            // verwertbar. Nach dem Dispatch gemessen waere jede Zelle um eins
            // verschoben und der Schaetzer wirkungslos.
            let occupancy_at_start = self.slots.occupancy();
            if self
                .slots
                .dispatch(slot, id, model, now, predicted_runtime)
                .is_err()
            {
                *blocked = blocked.with(model);
                continue;
            }

            if let Some(queue) = self.queues.get_mut(model.get()) {
                queue.take(id);
            }
            if let Some(vs) = self.variant_states.get_mut(model.get()) {
                vs.record(variant, now);
            }
            let _ = self.inflight.push(Dispatched {
                descriptor,
                variant,
                at: now,
                occupancy: occupancy_at_start,
                predicted: predicted_runtime,
            });
            self.metrics.forwarded = self.metrics.forwarded.saturating_add(1);
            self.metrics.count_variant(variant);
            if !feasible {
                // Erst hier zaehlen, nicht in der Kandidatenschleife: ein
                // Kandidat kann mehrfach geprueft und wieder zurueckgestellt
                // werden. Ein Zaehler, der Planungsversuche zaehlt und
                // Requests heisst, ist schlimmer als kein Zaehler.
                self.metrics.dispatched_late = self.metrics.dispatched_late.saturating_add(1);
            }
            sink.emit(Action::Dispatch {
                request: id,
                model,
                variant,
                slot,
                predicted_runtime,
                quantum,
            });
            return true;
        }
    }

    /// Verwirft einen Kandidaten, dessen Ergebnis bei Ankunft wertlos waere.
    ///
    /// ADR-0009: Verworfen wird, was bei Fertigstellung fachlich wertlos
    /// waere — nicht, was lediglich seine Deadline verfehlt. Die frueher an
    /// dieser Stelle stehende Deadline-Ablehnung fuehrte unter Ueberlast dazu,
    /// dass gar nichts mehr ausgefuehrt wurde: null Deadline-Misses, null
    /// nuetzliche Ergebnisse.
    ///
    /// Gibt `true` zurueck, wenn der Kandidat verworfen wurde.
    fn drop_if_worthless<S: ActionSink>(
        &mut self,
        now: Instant,
        model: ModelIdx,
        descriptor: &RequestDescriptor,
        optimistic_finish: Instant,
        sink: &mut S,
    ) -> bool {
        let worthless = descriptor.max_age.is_some_and(|limit| {
            optimistic_finish.saturating_since(descriptor.generation_time) > limit
        });
        if !(worthless && descriptor.queue_policy.allows_stale_drop()) {
            return false;
        }
        self.drop_candidate(model, descriptor.id, RequestState::Stale, sink);
        self.metrics.stale = self.metrics.stale.saturating_add(1);
        self.count_if_starved(descriptor.criticality);
        self.observe_pressure(now, descriptor.criticality, true);
        true
    }

    /// Schneidet einen zerlegbaren Auftrag auf das verbleibende Zeitbudget zu.
    ///
    /// Gibt die Laufzeit des **Quantums** zurueck, nicht die des gesamten
    /// Auftrags: die Slot-Planung und der Look-ahead muessen bewerten, was
    /// tatsaechlich gestartet wird, sonst waere die Zerlegung wirkungslos.
    ///
    /// Fuer alles, was nicht zerlegbar ist, bleibt die Laufzeit unveraendert.
    fn size_quantum<'a, I>(
        &self,
        model: ModelIdx,
        criticality: Criticality,
        full_runtime: Duration,
        now: Instant,
        forecast: I,
    ) -> (Duration, Option<u32>)
    where
        I: IntoIterator<Item = &'a ExpectedArrival>,
    {
        let Some(cooperative) = self.contracts.get(model.get()).and_then(|c| c.cooperative) else {
            return (full_runtime, None);
        };
        let budget = Self::best_effort_budget(now, forecast, criticality);
        // `ModelContract::validate` schliesst `min > max` aus. Die Untergrenze
        // wird hier trotzdem noch einmal gedeckelt: `clamp` panickt bei einem
        // leeren Intervall, und ein Panic im Dispatchpfad beendet unter
        // `panic = "abort"` den ganzen Governor. Eine Verteidigungslinie, die
        // nur bei einem Fehler an anderer Stelle wirkt, kostet hier nichts.
        let lower = cooperative.min_tokens.min(cooperative.max_total_tokens);
        let tokens = cooperative
            .tokens_in(budget)
            .clamp(lower, cooperative.max_total_tokens);
        // Sockel plus Erzeugungszeit. Ohne den Sockel meldete die Zuschneidung
        // eine Dauer, die das Quantum nie einhalten kann — und der Look-ahead
        // liesse es starten, weil er mit der falschen Zahl rechnet. Bei einem
        // Sockel in der Groessenordnung des Slack ist das der Unterschied
        // zwischen „passt knapp" und „passt grundsaetzlich nicht".
        let duration = cooperative.cost_of(tokens);
        (
            duration.max(Duration::from_nanos_unbounded(1)),
            Some(tokens),
        )
    }

    /// Das Zeitbudget, das eine nicht geschuetzte Arbeit jetzt verbrauchen darf,
    /// ohne eine erwartete geschuetzte Ankunft zu verzoegern.
    ///
    /// Das Budget reicht bis zur **Ankunft**, nicht bis zu ihrem spaetesten
    /// zulaessigen Start. Der Unterschied ist die Deadline-Reserve, und die
    /// planmaessig aufzuzehren waere falsch: sie ist dafuer da, Jitter,
    /// Laufzeitausreisser und Prognosefehler aufzufangen. Ein Governor, der
    /// sie bei jedem Quantum vollstaendig verbraucht, laesst die geschuetzte
    /// Arbeit dauerhaft am Rand ihres Vertrags laufen — und die erste
    /// Abweichung wird dann zum Miss.
    ///
    /// Gemessen (WP26, ADR-0015): mit der Reserve im Budget entstehen Quanten,
    /// die der Look-ahead fast immer vetoiert; das Sprachmodell kam auf einen
    /// einzigen Auftrag in 30 Sekunden. Die engere Regel erzeugt Quanten, die
    /// in die Leerlaufluecken passen.
    fn best_effort_budget<'a, I>(now: Instant, forecast: I, candidate: Criticality) -> Duration
    where
        I: IntoIterator<Item = &'a ExpectedArrival>,
    {
        let mut budget: Option<Duration> = None;
        for expected in forecast {
            if expected.criticality <= candidate {
                continue;
            }
            let available = expected.at.saturating_since(now);
            budget = Some(budget.map_or(available, |b: Duration| b.min(available)));
        }
        budget.unwrap_or(Duration::MAX_CONTRACT)
    }

    /// Loest Variante und Laufzeitprognose fuer einen Kandidaten auf.
    ///
    /// Gibt `None` zurueck, wenn derzeit ueberhaupt nicht geplant werden kann —
    /// kein Slot fuer dieses Modell oder keine brauchbare Variante. Das ist
    /// kein Machbarkeitsurteil; der Request bleibt Kandidat.
    fn plan(&self, model: ModelIdx, descriptor: &RequestDescriptor, now: Instant) -> Option<Plan> {
        let contract = self.contracts.get(model.get())?;
        let state = self.variant_states.get(model.get())?;
        let resolution = resolve(
            contract,
            state,
            model,
            descriptor.absolute_deadline,
            &PlanningContext {
                slots: &self.slots,
                estimator: &self.estimator,
                margin: self.margin_of(model),
                now,
                degrade: self.overload.state().forces_degradation(),
            },
        );
        match resolution {
            Resolution::Feasible(sel) => Some(Plan {
                variant: sel.variant,
                optimistic_finish: self.optimistic_finish(
                    contract,
                    model,
                    sel.variant,
                    sel.feasibility.start,
                ),
                predicted_runtime: sel
                    .feasibility
                    .finish
                    .saturating_since(sel.feasibility.start),
                feasible: true,
            }),
            Resolution::Infeasible { fastest } => Some(Plan {
                variant: fastest.variant,
                optimistic_finish: self.optimistic_finish(
                    contract,
                    model,
                    fastest.variant,
                    fastest.feasibility.start,
                ),
                predicted_runtime: fastest
                    .feasibility
                    .finish
                    .saturating_since(fastest.feasibility.start),
                feasible: false,
            }),
            Resolution::NoSlot | Resolution::NoVariant => None,
        }
    }

    /// Die optimistisch geschaetzte Fertigstellung einer Variante (ADR-0010).
    fn optimistic_finish(
        &self,
        contract: &ModelContract,
        model: ModelIdx,
        variant: VariantIdx,
        start: Instant,
    ) -> Instant {
        contract
            .variant(variant)
            .and_then(|v| {
                self.estimator
                    .optimistic(model, variant, self.slots.occupancy(), &v.profile)
            })
            .and_then(|runtime| start.checked_add(runtime))
            .unwrap_or(start)
    }

    /// Der beste wartende Kandidat nach lexikographischer Ordnung (Spec 10.6).
    fn best_candidate(&self, vetoed: crate::slots::ModelMask) -> Option<(ModelIdx, RequestId)> {
        let mut best: Option<(Criticality, Instant, Instant, ModelIdx, RequestId)> = None;

        for (i, queue) in self.queues.iter().enumerate() {
            let model = ModelIdx(u16::try_from(i).unwrap_or(u16::MAX));
            if vetoed.contains(model) {
                continue;
            }
            // Bei reihenfolgeerhaltender Policy ist nur der Kopf der Queue
            // Kandidat. Sonst waehlte EDF modellintern den Request mit der
            // frueheren Deadline — und ein spaeter eingetroffener Frame
            // ueberholte einen frueheren in einer Queue, deren einzige Zusage
            // genau das ausschliesst.
            let ordered = queue.config().policy.preserves_arrival_order();
            for descriptor in queue.iter().take(if ordered { 1 } else { usize::MAX }) {
                // Ohne Deadline zaehlt der Request als maximal geduldig; er
                // wird erst beruecksichtigt, wenn nichts Dringenderes wartet.
                let deadline = descriptor
                    .absolute_deadline
                    .unwrap_or(Instant::from_nanos(u64::MAX));
                let key = (
                    descriptor.criticality,
                    deadline,
                    descriptor.generation_time,
                    model,
                    descriptor.id,
                );
                let better = match best {
                    None => true,
                    Some((c, d, g, _, _)) => {
                        (core::cmp::Reverse(key.0), key.1, key.2) < (core::cmp::Reverse(c), d, g)
                    }
                };
                if better {
                    best = Some(key);
                }
            }
        }
        best.map(|(_, _, _, model, id)| (model, id))
    }

    /// Die erwarteten geschuetzten Ankuenfte im Look-ahead-Horizont.
    fn build_forecast(&self, now: Instant) -> ArrayVec<ExpectedArrival, MAX_MODELS> {
        let mut out = ArrayVec::new();
        let limit = now.checked_add(self.horizon);
        for (i, contract) in self.contracts.iter().enumerate() {
            if !contract.criticality.is_guarded() {
                continue;
            }
            let Some(Some(expected_at)) = self.next_expected.get(i).copied() else {
                continue;
            };
            if limit.is_some_and(|l| expected_at > l) {
                continue;
            }
            let Some(deadline) = expected_at.checked_add(contract.deadline) else {
                continue;
            };
            // Die beste Variante ist die konservative Annahme: sie ist die
            // langsamste, und der Look-ahead soll nicht optimistisch sein.
            let Some(best) = contract.variants.get(0) else {
                continue;
            };
            let model = ModelIdx(u16::try_from(i).unwrap_or(u16::MAX));
            // Dieselbe Laufzeitschaetzung, die auch der Dispatch benutzt:
            // Online-Beobachtung ueber Offline-Profil, mit der **gelernten**
            // Marge dieses Modells. Rechnet der Schutz stattdessen mit dem
            // reinen Profil und der globalen Startmarge, plant er die
            // geschuetzte Ankunft weiterhin mit einer Laufzeit, von der
            // laengst gemessen ist, dass sie zu kurz war — und laesst Arbeit
            // starten, die genau diese Ankunft verspaetet. Der Look-ahead darf
            // nicht weniger wissen als der Dispatch.
            let Some(runtime) = self.estimator.conservative(
                model,
                VariantIdx(0),
                self.slots.occupancy(),
                &best.profile,
                self.margin_of(model),
            ) else {
                continue;
            };
            let _ = out.push(ExpectedArrival {
                model,
                criticality: contract.criticality,
                at: expected_at.max(now),
                deadline,
                runtime,
            });
        }
        out
    }

    /// Der naechste Zeitpunkt, zu dem der Scheduler ohnehin nachsehen sollte.
    fn next_wakeup(&self, now: Instant) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        for f in self.slots.inflight_iter() {
            if f.expected_finish > now {
                earliest =
                    Some(earliest.map_or(f.expected_finish, |e: Instant| e.min(f.expected_finish)));
            }
        }
        earliest
    }

    /// Nimmt einen wartenden Request aus seiner Queue.
    ///
    /// Ohne Wirkung, wenn er dort nicht mehr steht: dann ist er entweder schon
    /// unterwegs zum Backend oder bereits terminal. Beides ist kein Fehler —
    /// ein Abbruch und eine Fertigstellung koennen sich ueberholen.
    fn on_cancel<S: ActionSink>(&mut self, request: RequestId, sink: &mut S) {
        for index in 0..self.queues.len() {
            let Some(queue) = self.queues.get_mut(index) else {
                continue;
            };
            if queue.take(request).is_some() {
                self.metrics.cancelled = self.metrics.cancelled.saturating_add(1);
                sink.emit(Action::Terminate {
                    request,
                    state: RequestState::Cancelled,
                });
                return;
            }
        }
    }

    fn drop_candidate<S: ActionSink>(
        &mut self,
        model: ModelIdx,
        id: RequestId,
        state: RequestState,
        sink: &mut S,
    ) {
        if let Some(queue) = self.queues.get_mut(model.get()) {
            queue.take(id);
        }
        sink.emit(Action::Terminate { request: id, state });
    }

    fn observe_pressure(&mut self, now: Instant, criticality: Criticality, violated: bool) {
        self.overload.observe(
            now,
            PressureSample {
                guarded: criticality.is_guarded(),
                violated,
            },
        );
    }

    /// Zaehlt einen Best-Effort-Request, der nie gelaufen ist (ADR-0012).
    fn count_if_starved(&mut self, criticality: Criticality) {
        if matches!(criticality, Criticality::BestEffort) {
            self.metrics.best_effort_starved = self.metrics.best_effort_starved.saturating_add(1);
        }
    }

    fn count_drop(&mut self, reason: DropReason) {
        match reason {
            DropReason::Superseded | DropReason::ArrivedOutOfOrder => {
                self.metrics.superseded = self.metrics.superseded.saturating_add(1);
            }
            DropReason::OverAge => {
                self.metrics.stale = self.metrics.stale.saturating_add(1);
            }
            DropReason::QueueFull | DropReason::Backpressure => {
                self.metrics.rejected_capacity = self.metrics.rejected_capacity.saturating_add(1);
            }
        }
    }
}
