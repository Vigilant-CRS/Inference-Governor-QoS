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
use crate::contract_ext::{BudgetSlack, CycleOutcome, MissWindow, WeaklyHardStatus};
use crate::estimator::{MarginController, RuntimeEstimator};
use crate::feasibility::{DEFAULT_HORIZON, ExpectedArrival, GuardVerdict, guard_protected};
use crate::hints::{Effect, Hint, HintPolicy, Hints, Rejection};
use crate::ids::{MAX_MODELS, ModelIdx, RequestId, SlotIdx, VariantIdx};
use crate::interference::{Interference, InterferenceVerdict};
use crate::metrics::Metrics;
use crate::model::{ContractError, ModelContract};
use crate::overload::{OverloadController, OverloadState, PressureSample};
use crate::predictor::{Mode, Predictor, ShadowLedger, StateClass};
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
    /// Der Hardwarezustand **beim Dispatch** (Review R07).
    ///
    /// Nicht der bei der Fertigstellung: dazwischen kann die Karte
    /// heruntergetaktet, gedrosselt oder in einen anderen Leistungsmodus
    /// gegangen sein. Die beobachtete Laufzeit gehoert in die Zelle des
    /// Zustands, unter dem sie **entstanden** ist — sonst fuellt sie eine
    /// Zelle, in der nie etwas gelaufen ist, und die Prognose liest sie
    /// spaeter als Beleg.
    ///
    /// Das ist der Grund, warum NV-06 im Schatten bleibt: die Zuordnung
    /// stimmt jetzt, aber ein Zustandswechsel **waehrend** der Ausfuehrung
    /// bleibt ein Fall, ueber den diese Zelle nichts aussagt.
    hardware_state: crate::predictor::StateClass,
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
    /// Die **Aufnahmezeit** des zuletzt gelieferten gueltigen Ergebnisses.
    ///
    /// Aufnahme, nicht Fertigstellung (ADR-0005, Review R02). Ein Verbraucher
    /// bewertet ein Ergebnis danach, wie alt die Welt darin ist — nicht
    /// danach, wann die Rechnung fertig wurde. Beides zu verwechseln laesst
    /// ein Bild frisch aussehen, das es nicht ist: Aufnahme bei 0 ms,
    /// Fertigstellung bei 50 ms, Abtastung bei 70 ms und ein Hoechstalter von
    /// 66 ms ist ein Miss — gerechnet ab Fertigstellung waeren es 20 ms und
    /// alles in Ordnung.
    last_valid: [Option<Instant>; MAX_MODELS],
    /// Bis wann das zuletzt gelieferte gueltige Ergebnis brauchbar war.
    ///
    /// `Aufnahme + max_age`, nicht die Fertigstellung: ein Ergebnis versorgt
    /// den Verbraucher genau so lange, wie sein Alter unter der vereinbarten
    /// Grenze bleibt (ADR-0005). Von dieser Zeit an laeuft die
    /// Versorgungsluecke — dieselbe Rechnung, die auch der Benchmarktracker
    /// fuehrt (`vig-sim::coverage`, Review R02/R03). Zwei Implementierungen
    /// derselben Groesse mit verschiedenen Regeln waren der Fehler.
    usable_until: [Option<Instant>; MAX_MODELS],
    /// Aufeinanderfolgende Requests ohne gueltiges Ergebnis je Modell.
    consecutive_misses: [u32; MAX_MODELS],
    /// Die gemessene, gerichtete Interferenz zwischen Modellen (NV-11).
    ///
    /// Leer heisst: nicht gemessen. Dann bleibt der Slot-Belegungsgrad die
    /// Naeherung, die er laut ADR-0006 immer war.
    interference: Interference,
    /// Die zustandsabhaengige Prognose (NV-06).
    ///
    /// Laeuft per Voreinstellung im Schatten: sie wird gefuettert und
    /// verglichen, entscheidet aber nichts. Erst wenn der Vergleich zeigt,
    /// dass sie besser plant und nicht nur mehr ablehnt, ist eine Umstellung
    /// vertretbar.
    predictor: Predictor,
    /// Der zuletzt beobachtete Hardwarezustand (NV-04).
    ///
    /// Der Kern misst ihn nicht — er hat keine Uhr und kein I/O. Er bekommt
    /// ihn gesagt, wie er auch die Zeit gesagt bekommt.
    hardware_state: StateClass,
    /// Die Revision der Profilidentitaet, unter der gerade geplant wird.
    profile_revision: u32,
    /// Hinweise der Anwendung innerhalb freigegebener Grenzen (NV-18).
    ///
    /// Voreinstellung: geschlossen. Wer Hinweise zulassen will, sagt es dem
    /// Governor ueber eine Freigabe.
    hints: Hints,
    /// Ob das Missbudget in die Kandidatenwahl eingeht (NV-24).
    ///
    /// Aus: der Vorrang folgt allein Kritikalitaet und Deadline, wie bisher.
    /// Ein: innerhalb derselben Kritikalitaetsklasse geht ein Strom vor,
    /// dessen naechster Zyklus ein Pflichtzyklus ist. **Nie** ueber
    /// Klassengrenzen — die Betreiberpolicy bleibt die Betreiberpolicy.
    ///
    /// Voreinstellung aus: dieses Paket liefert eine empirische Policy, keine
    /// formale Zusage. Wer sie einschaltet, soll es entschieden haben.
    miss_aware_policy: bool,
    /// Der Weakly-hard-Monitor je Modell, falls einer vereinbart ist (NV-02).
    ///
    /// `None`, wo kein Missbudget im Vertrag steht. Ein Monitor beobachtet;
    /// er erzwingt nichts. Die Kritikalitaetsklasse `Protected` ist noch kein
    /// Weakly-hard-Vertrag.
    miss_windows: [Option<MissWindow>; MAX_MODELS],
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

        // Je Vertrag mit Missbudget ein Monitor. `MissWindow::new` gibt
        // `None`, wo kein Verbrauchertakt vereinbart ist — ohne Takt liesse
        // sich nur raten, welcher Zyklus gerade laeuft.
        let mut miss_windows: [Option<MissWindow>; MAX_MODELS] = [const { None }; MAX_MODELS];
        for (i, contract) in contracts.iter().enumerate() {
            if let Some(extension) = contract.extension.as_ref()
                && let Some(cell) = miss_windows.get_mut(i)
            {
                *cell = MissWindow::new(extension);
            }
        }

        // Die Prognosetabelle bekommt die Form der Konfiguration, nicht die
        // des Maximums: vier Modelle mit je einer Variante brauchen 24 Zellen
        // und nicht zwoelftausend.
        let models = contracts.len();
        let variants = contracts
            .iter()
            .map(|c| c.variants.len())
            .max()
            .unwrap_or(1);
        let slot_count = slots.len();

        // Je Modell ein Margenregler mit Ziel (ADR-0034): vereinbart der
        // Vertrag ein Missbudget, gibt es den erlaubten Anteil ueberzogener
        // Plaene vor; sonst ein Prozent.
        let mut margins = [MarginController::new(margin); MAX_MODELS];
        for (i, contract) in contracts.iter().enumerate() {
            let budget = contract.extension.as_ref().and_then(|e| e.miss_budget);
            if let (Some(controller), Some(budget)) = (margins.get_mut(i), budget) {
                *controller =
                    controller.with_target_permille(MarginController::target_from_budget(budget));
            }
        }

        Ok(Self {
            contracts,
            queues,
            variant_states: [VariantState::default(); MAX_MODELS],
            next_expected: [None; MAX_MODELS],
            arrivals: [crate::arrival::ArrivalTracker::default(); MAX_MODELS],
            interference: Interference::new(),
            last_valid: [None; MAX_MODELS],
            usable_until: [None; MAX_MODELS],
            consecutive_misses: [0; MAX_MODELS],
            miss_windows,
            predictor: Predictor::with_shape(models, variants, slot_count),
            hardware_state: StateClass::default(),
            profile_revision: 0,
            miss_aware_policy: false,
            hints: Hints::new(HintPolicy::closed()),
            slots,
            overload,
            margin,
            margins,
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
            // Das Ziel aus dem Vertrag bleibt; nur der Startwert steigt.
            *controller = MarginController::provisional(self.margin)
                .with_target_permille(controller.target_permille());
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
        // NV-02: der Vertragstakt laeuft unabhaengig davon weiter, was gerade
        // eintrifft. Deshalb hier und nicht im Ankunftspfad — ein Governor,
        // der alles ablehnt, soll nicht dadurch gut dastehen, dass keine
        // Zyklen entstehen.
        self.observe_cycles(now);
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
        self.observe_for_predictor(&entry, compute);
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

        // Verbrauchersicht: wie lange war dieser Strom am Stueck ohne
        // brauchbares Ergebnis? Eine Abdeckungszahl mittelt das weg, und
        // gerade der zusammenhaengende Block ist das, was eine Regelung
        // umwirft.
        self.observe_supply(
            entry.descriptor.logical_model,
            now,
            entry.descriptor.generation_time,
            state,
        );

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

    /// Setzt die Freigabe fuer Anwendungshinweise (NV-18).
    ///
    /// Voreinstellung ist [`HintPolicy::closed`] — niemand wird gehoert.
    /// Eine engere Freigabe wirkt sofort auch auf bereits angenommene
    /// Hinweise; sie muessen dafuer nicht zurueckgenommen werden.
    pub fn set_hint_policy(&mut self, policy: HintPolicy) {
        self.hints = Hints::new(policy);
    }

    /// Nimmt einen Hinweis der Anwendung entgegen.
    ///
    /// # Errors
    ///
    /// Siehe [`Rejection`]. Ein abgelehnter Hinweis aendert nichts.
    pub fn offer_hint(&mut self, hint: Hint, now: Instant) -> Result<Effect, Rejection> {
        self.hints.offer(hint, now)
    }

    /// Was fuer diesen Strom gerade gilt (NV-18).
    #[must_use]
    pub fn hint_effect(&self, model: ModelIdx, now: Instant) -> Effect {
        self.hints.effect(model, now)
    }

    /// Das wirksame Hoechstalter eines Stroms, Hinweise eingerechnet.
    ///
    /// Ohne Hinweis der Wert aus dem Vertrag. Ein Hinweis kann ihn
    /// verschaerfen, und lockern nur, wenn der Betreiber es freigegeben hat.
    #[must_use]
    /// Das wirksame Hoechstalter fuer **diesen** Request (NV-18).
    ///
    /// Grundlage ist, was der Request mitbringt — ein Client darf sein
    /// eigenes Hoechstalter nennen. Darauf wirkt der Hinweis der Anwendung,
    /// und der gilt je Strom, nicht je Request.
    ///
    /// Ohne Hinweis ist das Ergebnis der Wert aus dem Request, und alles
    /// verhaelt sich wie vor NV-18.
    fn effective_max_age_for(
        &self,
        model: ModelIdx,
        descriptor: &RequestDescriptor,
        now: Instant,
    ) -> Option<Duration> {
        self.hints.effective_max_age(model, descriptor.max_age, now)
    }

    /// Das wirksame Hoechstalter eines **Stroms**, Hinweise eingerechnet
    /// (NV-18).
    ///
    /// Grundlage ist der Vertrag. Fuer einen einzelnen Request gilt
    /// stattdessen, was er selbst mitbringt — dafuer rechnet
    /// `effective_max_age_for`.
    #[must_use]
    pub fn effective_max_age(&self, model: ModelIdx, now: Instant) -> Option<Duration> {
        let base = self
            .contracts
            .get(model.get())
            .and_then(|contract| contract.max_age);
        self.hints.effective_max_age(model, base, now)
    }

    /// Ob das Missbudget in die Kandidatenwahl eingeht (NV-24).
    #[must_use]
    pub const fn miss_aware_policy(&self) -> bool {
        self.miss_aware_policy
    }

    /// Schaltet die missbudgetbewusste Kandidatenwahl (NV-24).
    ///
    /// Wirkt **nur innerhalb** einer Kritikalitaetsklasse. Ein Strom mit
    /// erschoepftem Budget geht dann vor einem mit Spielraum; ein
    /// `best_effort`-Strom geht deshalb nie vor einem `protected`. Der Vorrang
    /// zwischen Klassen ist die Betreiberpolicy und bleibt es.
    pub const fn set_miss_aware_policy(&mut self, enabled: bool) {
        self.miss_aware_policy = enabled;
    }

    /// Wie viele Misses das Fenster eines Modells noch vertraegt (NV-24).
    ///
    /// `None`, wo kein Missbudget vereinbart ist.
    #[must_use]
    pub fn budget_slack(&self, model: ModelIdx) -> Option<BudgetSlack> {
        self.miss_windows
            .get(model.get())
            .and_then(Option::as_ref)
            .map(MissWindow::slack)
    }

    /// Ob der naechste Verbraucherzyklus eines Modells ein Pflichtzyklus ist.
    #[must_use]
    pub fn next_cycle_is_mandatory(&self, model: ModelIdx) -> bool {
        self.miss_windows
            .get(model.get())
            .and_then(Option::as_ref)
            .is_some_and(MissWindow::next_is_mandatory)
    }

    /// Meldet dem Kern den beobachteten Hardwarezustand (NV-04, NV-06).
    ///
    /// Der Kern misst nichts; er bekommt den Zustand gesagt, wie er auch die
    /// Zeit gesagt bekommt. Aendert sich die Klasse, gelten die bisher
    /// gesammelten Zellen nicht mehr — sie werden bei der naechsten
    /// Beobachtung geleert, nicht fortgeschrieben.
    pub const fn observe_hardware(&mut self, state: StateClass, profile_revision: u32) {
        self.hardware_state = state;
        self.profile_revision = profile_revision;
    }

    /// Die Betriebsart der zustandsabhaengigen Prognose.
    #[must_use]
    pub const fn predictor_mode(&self) -> Mode {
        self.predictor.mode()
    }

    /// Schaltet die zustandsabhaengige Prognose scharf oder in den Schatten.
    ///
    /// Bewusst eine ausdrueckliche Handlung des Betreibers: eine Policy, die
    /// sich selbst scharfschaltet, sobald sie genug Daten hat, entzieht
    /// genau die Entscheidung, um die es geht.
    ///
    /// Der Metrikabzug wird sofort nachgezogen. Sonst meldete
    /// `vig_predictor_active` bis zur ersten Fertigstellung den alten Modus —
    /// und der Betreiber saehe nach dem Einschalten eine Null.
    pub fn set_predictor_mode(&mut self, mode: Mode) {
        self.predictor.set_mode(mode);
        self.publish_predictor();
    }

    /// Der Schattenvergleich der Prognose (NV-06).
    #[must_use]
    /// Die Prognosetabelle, fuer Pruefungen.
    ///
    /// Lesend: der Entscheidungspfad liest sie ueber
    /// [`Self::predictor_mode`] und den Estimator. Hier steht sie, damit ein
    /// Test nachsehen kann, in **welcher** Zelle eine Beobachtung gelandet
    /// ist — die Frage aus Review R07.
    pub const fn predictor(&self) -> &Predictor {
        &self.predictor
    }

    /// Der Schattenvergleich der Prognose.
    #[must_use]
    pub const fn predictor_ledger(&self) -> &ShadowLedger {
        self.predictor.ledger()
    }

    /// Fuettert die Prognose und vergleicht sie mit dem bisherigen Weg.
    ///
    /// Der Vergleich laeuft auf der Schreibseite, nicht im
    /// Entscheidungspfad: gelesen wird bei jeder Planungsentscheidung,
    /// geschrieben nur bei jeder Fertigstellung.
    fn observe_for_predictor(&mut self, entry: &Dispatched, compute: Duration) {
        let model = entry.descriptor.logical_model;
        let variant = entry.variant;
        // Der Zustand **beim Dispatch**, nicht der von jetzt (Review R07).
        // Zwischen Start und Ende kann die Karte heruntergetaktet oder
        // gedrosselt haben; die Laufzeit gehoert in die Zelle, unter der sie
        // entstanden ist.
        let mut state = entry.hardware_state;
        state.occupancy = u8::try_from(entry.occupancy).unwrap_or(u8::MAX);
        let revision = self.profile_revision;

        self.predictor
            .record(model, variant, state, revision, compute);

        // Was der bisherige Weg zu diesem Betriebspunkt gesagt haette.
        let Some(contract) = self.contracts.get(model.get()) else {
            return;
        };
        let Some(profile) = contract.variant(variant).map(|v| &v.profile) else {
            return;
        };
        let margin = self
            .margins
            .get(model.get())
            .map_or(self.margin, MarginController::margin);
        let Some(legacy) =
            self.estimator
                .conservative(model, variant, entry.occupancy, profile, margin)
        else {
            return;
        };
        // Verglichen wird, was die Planung tatsaechlich naehme: Zelle mit
        // Marge gegen Profil mit Marge. Ohne Marge auf der Zellenseite
        // zaehlte der Vergleich die fehlende Marge als „mutiger".
        let prediction = self
            .predictor
            .predict(model, variant, state, revision)
            .with_margin(margin);
        self.predictor.ledger_mut().compare(legacy, &prediction);
        self.publish_predictor();
    }

    /// Schreibt den Schattenvergleich in den Metrikabzug (NV-06).
    fn publish_predictor(&mut self) {
        let ledger = *self.predictor.ledger();
        self.metrics.predictor_comparisons = ledger.comparisons;
        self.metrics.predictor_fallbacks = ledger.fallbacks;
        self.metrics.predictor_more_conservative = ledger.more_conservative;
        self.metrics.predictor_more_optimistic = ledger.more_optimistic;
        self.metrics.predictor_active = u64::from(self.predictor.mode() == Mode::Active);
    }

    /// Traegt die seit dem letzten Ereignis vergangenen Verbraucherzyklen ein.
    ///
    /// Jeder Zyklus wird an **seinem eigenen** Zeitpunkt bewertet, nicht am
    /// aktuellen: bei `latest_state` versorgt ein Ergebnis von vor 20 ms auch
    /// den Zyklus, in dem nichts Neues ankam, solange es unter dem
    /// Hoechstalter bleibt. Die Alternative — nur den letzten Zeitpunkt zu
    /// kennen — wuerde ruhige Zyklen als Misses zaehlen und damit genau die
    /// Groesse verderben, um die es geht.
    fn observe_cycles(&mut self, now: Instant) {
        for index in 0..self.contracts.len() {
            let Some(contract) = self.contracts.get(index) else {
                continue;
            };
            let Some(extension) = contract.extension.as_ref() else {
                continue;
            };
            // Ohne Hoechstalter gibt es kein Kriterium fuer „vertragsgemaess
            // versorgt". Die Deadline ist keines: sie gilt je Request, nicht
            // je Verbraucherzyklus.
            let Some(max_age) = contract.max_age else {
                continue;
            };
            let require_new = extension.require_new_sample_each_cycle;
            let tick = extension.tick();
            let last_valid = self.last_valid.get(index).copied().flatten();
            let Some(window) = self.miss_windows.get_mut(index).and_then(Option::as_mut) else {
                continue;
            };
            window.advance_with(now, |at| {
                let Some(valid) = last_valid else {
                    return CycleOutcome::Missed;
                };
                // Ein Ergebnis, das es zu diesem Zyklus noch nicht gab, kann
                // ihn nicht versorgt haben.
                if valid.as_nanos() > at.as_nanos() {
                    return CycleOutcome::Missed;
                }
                let age = at.saturating_since(valid);
                if age.as_nanos() > max_age.as_nanos() {
                    return CycleOutcome::Missed;
                }
                // Verlangt der Vertrag je Zyklus einen neuen Messwert, genuegt
                // ein noch frisches Bestandsresultat nicht.
                if require_new && tick.is_some_and(|t| age.as_nanos() >= t.as_nanos()) {
                    return CycleOutcome::Missed;
                }
                CycleOutcome::Supplied
            });
        }
        self.publish_weakly_hard();
    }

    /// Schreibt den Monitorstand in den Metrikabzug.
    fn publish_weakly_hard(&mut self) {
        for index in 0..MAX_MODELS {
            let Some(window) = self.miss_windows.get(index).and_then(Option::as_ref) else {
                continue;
            };
            // Waehrend der Aufwaermphase steht 0 bei `violated` — das ist
            // keine Zusage, sondern die Aussage, dass noch nichts feststeht.
            let (misses, violated) = match window.status() {
                WeaklyHardStatus::Violated { misses, .. } => (misses, 1),
                WeaklyHardStatus::Warmup { .. } | WeaklyHardStatus::Holding => {
                    (window.misses_in_window(), 0)
                }
            };
            if let Some(cell) = self.metrics.weakly_hard_misses.get_mut(index) {
                *cell = misses;
            }
            if let Some(cell) = self.metrics.weakly_hard_violated.get_mut(index) {
                *cell = violated;
            }
            // NV-24: nicht „wie viele Misses waren es", sondern „wie viele
            // darf es noch geben" — die Groesse, mit der eine Policy arbeitet.
            if let Some(cell) = self.metrics.weakly_hard_misses_left.get_mut(index) {
                *cell = window.slack().misses_left;
            }
        }
    }

    /// Der Weakly-hard-Befund eines Modells, falls einer vereinbart ist.
    #[must_use]
    pub fn weakly_hard(&self, model: ModelIdx) -> Option<WeaklyHardStatus> {
        self.miss_windows
            .get(model.get())
            .and_then(Option::as_ref)
            .map(MissWindow::status)
    }

    /// Beobachtet die Versorgungslage eines Stroms.
    ///
    /// Ein gueltiges Ergebnis setzt die Kette zurueck und schliesst die
    /// laufende Luecke; alles andere verlaengert sie. `last_valid` ist
    /// `None`, solange noch nie etwas Brauchbares ankam — dann laeuft die
    /// Luecke ab dem ersten Ereignis dieses Stroms.
    ///
    /// `capture` ist die Aufnahmezeit des Ergebnisses, `now` seine
    /// Fertigstellung. Gespeichert wird die **Aufnahme**: sie ist die Zeit,
    /// gegen die ein Verbraucher sein Hoechstalter misst (ADR-0005). Ein
    /// spaeteres Ergebnis mit aelterer Aufnahme darf die gespeicherte nicht
    /// zurueckdrehen — deshalb das Maximum.
    fn observe_supply(
        &mut self,
        model: ModelIdx,
        now: Instant,
        capture: Instant,
        state: RequestState,
    ) {
        let index = model.get();
        if state == RequestState::CompletedValid {
            if let Some(cell) = self.consecutive_misses.get_mut(index) {
                *cell = 0;
            }
            // Auch die exportierte Zahl: sonst zeigt das Dashboard eine Kette,
            // die laengst gerissen ist — und ein Alarm, der nicht von selbst
            // verstummt, wird abgeschaltet.
            if let Some(slot) = self.metrics.consecutive_misses.get_mut(index) {
                *slot = 0;
            }
            if let Some(cell) = self.last_valid.get_mut(index) {
                let freshest = cell.map_or(capture, |previous| {
                    if capture.as_nanos() > previous.as_nanos() {
                        capture
                    } else {
                        previous
                    }
                });
                *cell = Some(freshest);
            }
            // Und bis wann es traegt. Ein Ergebnis, dessen Hoechstalter schon
            // bei der Auslieferung abgelaufen war, verlaengert die
            // Brauchbarkeit um keine Nanosekunde — es hat nie versorgt.
            let expires = self
                .contracts
                .get(index)
                .and_then(|c| c.max_age)
                .and_then(|max_age| capture.checked_add(max_age));
            if let Some(expires) = expires
                && expires.as_nanos() > now.as_nanos()
                && let Some(cell) = self.usable_until.get_mut(index)
            {
                let latest = cell.map_or(expires, |previous| {
                    if expires.as_nanos() > previous.as_nanos() {
                        expires
                    } else {
                        previous
                    }
                });
                *cell = Some(latest);
            }
            return;
        }

        if let Some(cell) = self.consecutive_misses.get_mut(index) {
            *cell = cell.saturating_add(1);
            if let Some(slot) = self.metrics.consecutive_misses.get_mut(index) {
                *slot = *cell;
            }
        }
        // Die Luecke laeuft ab dem Ablauf des letzten brauchbaren
        // Ergebnisses, nicht ab dessen Fertigstellung: dazwischen war der
        // Strom versorgt. Ohne je ein gueltiges Ergebnis laeuft sie ab dem
        // ersten Ereignis dieses Stroms.
        let since = self
            .usable_until
            .get(index)
            .copied()
            .flatten()
            .unwrap_or(now);
        let gap = now.saturating_since(since);
        if let Some(slot) = self.metrics.longest_gap_us.get_mut(index) {
            let micros =
                u32::try_from(gap.as_nanos().checked_div(1_000).unwrap_or(0)).unwrap_or(u32::MAX);
            *slot = (*slot).max(micros);
        }
    }

    /// Zaehlt Profile, die nicht mehr zu den Beobachtungen passen.
    ///
    /// Spec 30.3, der Circuit Breaker: liegt die gemessene Laufzeit weit ueber
    /// dem hinterlegten Profil, ist das Profil nicht falsch, sondern
    /// unzustaendig. `health()` sagte das bisher nur seinen eigenen Tests —
    /// ein Versprechen, das niemand einloest, ist schlechter als keines,
    /// weil der Betreiber sich darauf verlaesst.
    ///
    /// Einmal je Durchlauf und nicht je Kandidat: sonst zaehlte der Wert
    /// Planungsversuche und hiesse Profile.
    fn observe_profile_health(&mut self) {
        let occupancy = self.slots.occupancy();
        let mut degraded = 0_u64;
        for (i, contract) in self.contracts.iter().enumerate() {
            let Ok(raw) = u16::try_from(i) else { continue };
            let Some(best) = contract.variants.get(0) else {
                continue;
            };
            if self
                .estimator
                .health(ModelIdx(raw), VariantIdx(0), occupancy, &best.profile)
                == crate::estimator::ProfileHealth::Degraded
            {
                degraded = degraded.saturating_add(1);
            }
        }
        self.metrics.degraded_profiles = degraded;
    }

    /// Der Hauptdurchlauf: Stale sammeln, Ueberlast bewerten, dispatchen.
    fn schedule<S: ActionSink>(&mut self, now: Instant, sink: &mut S) {
        self.overload.evaluate(now);
        self.observe_profile_health();
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
                // Das **wirksame** Hoechstalter, Hinweise eingerechnet
                // (NV-18). Ein Aktionshorizont, den der Betreiber freigegeben
                // hat, verlaengert es fuer die Dauer seiner Frist; ein
                // erhoehter Bedarf verkuerzt es. Ohne diese Zeile blieb der
                // Hinweisregler ohne jede Wirkung auf eine Entscheidung —
                // gebaut, getestet, folgenlos (Review R09).
                let Some(limit) = self.effective_max_age_for(model, descriptor, now) else {
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
                &descriptor,
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
                self.metrics.count_switch(model, vs.current(), variant);
                vs.record(variant, now);
            }
            let _ = self.inflight.push(Dispatched {
                hardware_state: self.hardware_state,
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
        let worthless = self
            .effective_max_age_for(model, descriptor, now)
            .is_some_and(|limit| {
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
        request: &RequestDescriptor,
        full_runtime: Duration,
        now: Instant,
        forecast: I,
    ) -> (Duration, Option<u32>)
    where
        I: IntoIterator<Item = &'a ExpectedArrival>,
    {
        // Ein Vertrag mit `cooperative` sagt, dass das **Modell** zerlegbar
        // ist. Ob dieser Auftrag es wird, entscheidet die Ausfuehrung — ein
        // Request ohne Texteingang wird es nie, und eine zu teure Zerlegung
        // wird bewusst nicht gefahren. Ohne diese Abfrage schneidet der Kern
        // ein Quantum zu und meldet dessen Dauer, waehrend das Backend den
        // ganzen Auftrag rechnet: Look-ahead und Slotbelegung planen dann mit
        // einer Zahl, die um Groessenordnungen zu klein ist.
        if !request.decomposable {
            return (full_runtime, None);
        }
        let Some(cooperative) = self.contracts.get(model.get()).and_then(|c| c.cooperative) else {
            return (full_runtime, None);
        };
        let context_tokens = request.context_tokens;
        let budget = Self::best_effort_budget(now, forecast, criticality);
        // `ModelContract::validate` schliesst `min > max` aus. Die Untergrenze
        // wird hier trotzdem noch einmal gedeckelt: `clamp` panickt bei einem
        // leeren Intervall, und ein Panic im Dispatchpfad beendet unter
        // `panic = "abort"` den ganzen Governor. Eine Verteidigungslinie, die
        // nur bei einem Fehler an anderer Stelle wirkt, kostet hier nichts.
        let lower = cooperative.min_tokens.min(cooperative.max_total_tokens);
        // Mit dem Kontext, nicht ohne (NV-16): ein spaetes Quantum traegt einen
        // laengeren Prompt, und dessen erneute Berechnung geht vom selben
        // Budget ab. Ohne diesen Term faellt jedes Quantum gleich gross aus,
        // und die spaeten ziehen ueber ihre Luecke hinaus.
        //
        // Das hat eine Folge, die hier benannt sein soll: die Kosten des
        // kleinstmoeglichen Quantums **wachsen** mit dem Fortschritt des
        // Auftrags. Ab dem Punkt, an dem sie die Luecke zur naechsten
        // geschuetzten Ankunft uebersteigen, vetoiert der Look-ahead jede
        // weitere Fortsetzung — dauerhaft. Der Auftrag zaehlt dann in
        // `deferred_for_protected` und endet ueber `max_age` oder seine
        // Deadline, nicht ueber ein Ergebnis.
        //
        // Das ist kein Fehler, sondern die ehrliche Antwort: ohne wirksames
        // Prefix-Caching gibt es fuer diesen Auftrag ab dieser Kontextlaenge
        // keine Luecke mehr, in die er passt. Vor NV-16 fiel das nicht auf,
        // weil der Sockel konstant war — der Governor startete Quanten, die
        // ihre Luecke ueberzogen, und die geschuetzte Ankunft dahinter kam zu
        // spaet. Wer den Fall vermeiden will, setzt `max_overhead_permille`
        // und laesst den Auftrag ungeteilt laufen, oder er sorgt fuer einen
        // Cache. Ein Auftrag ohne `max_age` bleibt sonst stehen.
        let tokens = cooperative
            .tokens_in_with_context(budget, context_tokens)
            .clamp(lower, cooperative.max_total_tokens);
        // Sockel plus Erzeugungszeit. Ohne den Sockel meldete die Zuschneidung
        // eine Dauer, die das Quantum nie einhalten kann — und der Look-ahead
        // liesse es starten, weil er mit der falschen Zahl rechnet. Bei einem
        // Sockel in der Groessenordnung des Slack ist das der Unterschied
        // zwischen „passt knapp" und „passt grundsaetzlich nicht".
        let duration = cooperative.cost_of_with_context(tokens, context_tokens);
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
                predictor: &self.predictor,
                state: self.hardware_state,
                profile_revision: self.profile_revision,
                margin: self.margin_of(model),
                now,
                degrade: self.overload.state().forces_degradation(),
            },
        );
        // NV-11: die gemessene, gerichtete Interferenz kommt auf die
        // Prognose obendrauf — aber nur, wo sie gemessen ist. Der
        // Belegungsgrad bleibt die Naeherung fuer alles andere (ADR-0006), und
        // eine unbekannte Paarung bekommt keinen erfundenen Aufschlag.
        //
        // Bis hierher war die Tabelle gebaut, getestet und an nichts
        // angeschlossen: gemessen, berichtet, weggeworfen.
        let added = self.measured_interference(model);

        match resolution {
            Resolution::Feasible(sel) => Some(Plan {
                variant: sel.variant,
                optimistic_finish: self.optimistic_finish(
                    contract,
                    model,
                    sel.variant,
                    sel.feasibility.start,
                ),
                predicted_runtime: with_interference(
                    sel.feasibility
                        .finish
                        .saturating_since(sel.feasibility.start),
                    added,
                ),
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
                predicted_runtime: with_interference(
                    fastest
                        .feasibility
                        .finish
                        .saturating_since(fastest.feasibility.start),
                    added,
                ),
                feasible: false,
            }),
            Resolution::NoSlot | Resolution::NoVariant => None,
        }
    }

    /// Was die gemessene Interferenz zu diesem Modell gerade dazurechnet
    /// (NV-11, ADR-0026).
    ///
    /// Nachbarn sind die Modelle, die **jetzt** laufen. Ist die Paarung nicht
    /// gemessen, kommt nichts dazu: eine unbekannte Paarung bekommt keinen
    /// erfundenen Aufschlag, und der Belegungsgrad bleibt die Naeherung, die
    /// er laut ADR-0006 immer war.
    ///
    /// Gerichtet, nicht symmetrisch: ein 95-ms-VLM verlaengert einen
    /// 5-ms-Detektor um ein Vielfaches seiner eigenen Laufzeit, der Detektor
    /// das VLM kaum.
    fn measured_interference(&self, model: ModelIdx) -> Duration {
        if self.interference.measured_pairs() == 0 {
            return Duration::from_nanos_unbounded(0);
        }
        // Feste Groesse ohne Allokation: der Kern allokiert nicht im
        // Entscheidungspfad.
        let mut neighbours = [ModelIdx(0); MAX_MODELS];
        let mut count = 0_usize;
        for entry in self.inflight.iter() {
            let other = entry.descriptor.logical_model;
            if other == model {
                continue;
            }
            if neighbours
                .get(..count)
                .is_some_and(|seen| seen.contains(&other))
            {
                continue;
            }
            if let Some(slot) = neighbours.get_mut(count) {
                *slot = other;
                count = count.saturating_add(1);
            }
        }
        let Some(neighbours) = neighbours.get(..count) else {
            return Duration::from_nanos_unbounded(0);
        };
        match self.interference.lookup(model, neighbours) {
            InterferenceVerdict::Measured { added, .. } => added,
            InterferenceVerdict::Alone
            | InterferenceVerdict::UnmeasuredPair { .. }
            | InterferenceVerdict::NotExtrapolated { .. } => Duration::from_nanos_unbounded(0),
        }
    }

    /// Die gemessene Interferenztabelle setzen (NV-11).
    pub fn set_interference(&mut self, table: Interference) {
        self.interference = table;
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
        // Der Schluessel ist (Kritikalitaet, Pflichtzyklus, Deadline,
        // Generationszeit). Der Pflichtzyklus steht **nach** der
        // Kritikalitaet: er ordnet innerhalb einer Klasse um, nie ueber
        // Klassengrenzen (NV-24). Ist die Policy aus, ist er fuer alle
        // Modelle gleich und faellt damit heraus.
        let mut best: Option<(Criticality, bool, Instant, Instant, ModelIdx, RequestId)> = None;

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
                let mandatory = self.miss_aware_policy && self.next_cycle_is_mandatory(model);
                let key = (
                    descriptor.criticality,
                    mandatory,
                    deadline,
                    descriptor.generation_time,
                    model,
                    descriptor.id,
                );
                let better = match best {
                    None => true,
                    Some((c, m, d, g, _, _)) => {
                        (
                            core::cmp::Reverse(key.0),
                            core::cmp::Reverse(key.1),
                            key.2,
                            key.3,
                        ) < (core::cmp::Reverse(c), core::cmp::Reverse(m), d, g)
                    }
                };
                if better {
                    best = Some(key);
                }
            }
        }
        best.map(|(_, _, _, _, model, id)| (model, id))
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

/// Rechnet die gemessene Interferenz auf eine Prognose.
///
/// Eine eigene Funktion, weil beide Planzweige sie brauchen und weil sie
/// saettigen muss: eine Dauer, die ueberlaeuft, waere eine Zusage, die
/// niemand einhalten kann.
fn with_interference(base: Duration, added: Duration) -> Duration {
    Duration::from_nanos_unbounded(base.as_nanos().saturating_add(added.as_nanos()))
}
