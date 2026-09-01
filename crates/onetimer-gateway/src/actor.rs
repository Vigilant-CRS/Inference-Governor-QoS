//! Der Scheduler-Actor (Spec 9.4).
//!
//! Genau ein Task besitzt den Schedulerzustand. Netzwerkworker senden
//! Ereignisse ueber einen **bounded** Kanal, Backendaufrufe laufen als eigene
//! Tasks und melden ihr Ergebnis als Ereignis zurueck.
//!
//! Der Zuschnitt ist bewusst: keine Locks im Entscheidungspfad, deterministische
//! Zustandsuebergaenge, klare Backpressure und ein Trace, der offline im
//! Simulator nachgespielt werden kann (Spec 30.2).
//!
//! ## Warum der Kanal begrenzt ist
//!
//! Ein unbegrenzter Kanal waere eine Warteschlange, die der Scheduler nicht
//! sieht und nicht steuert — genau das Problem, das ADR-0002 fuer das Backend
//! beschreibt, nur auf der Eingangsseite. Ist der Kanal voll, wird der
//! Aufrufer gebremst, statt Speicher wachsen zu lassen (Spec L-003, 26.4).

use crate::clock::MonotonicClock;
use crate::cooperative::GenerativeJob;
use crate::outcome::{mark_obsolete, status_for};
use onetimer_backend_triton::{BackendError, TritonClient};
use onetimer_config::schema::Resolved;
use onetimer_core::overload::{OverloadConfig, OverloadController};
use onetimer_core::scheduler::{Action, Event, Scheduler, SchedulerError};
use onetimer_core::{Instant, Metrics, RequestDescriptor, RequestId, RequestState, SlotIdx};
use onetimer_protocol_oip::inference::{ModelInferRequest, ModelInferResponse};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tonic::Status;

/// Kapazitaet des Ereigniskanals.
///
/// Grosszuegig genug, um Ankunftsspitzen aufzunehmen, klein genug, damit
/// Ueberlast als Backpressure beim Aufrufer ankommt statt als Speicherwachstum.
const CHANNEL_CAPACITY: usize = 1_024;

/// Das Ergebnis, das ein wartender Client bekommt.
pub type Reply = Result<ModelInferResponse, Status>;

/// Eine Nachricht an den Actor.
#[derive(Debug)]
pub enum Msg {
    /// Ein neuer Request ist eingetroffen.
    Arrival {
        /// Die Scheduling-Metadaten.
        descriptor: Box<RequestDescriptor>,
        /// Der unveraenderte OIP-Request zur Weitergabe.
        request: Box<ModelInferRequest>,
        /// Wohin die Antwort geht.
        reply: oneshot::Sender<Reply>,
    },
    /// Ein Backendaufruf ist beendet.
    BackendDone {
        /// Der betroffene Request.
        request: RequestId,
        /// Der belegte Slot.
        slot: SlotIdx,
        /// Das Ergebnis.
        result: Box<Result<ModelInferResponse, BackendError>>,
    },
    /// Ein Weckruf.
    Tick,
    /// Momentaufnahme der Zaehler (fuer Metrikabfragen und Tests).
    Snapshot(oneshot::Sender<Metrics>),
}

/// Der Griff, ueber den das Gateway den Actor erreicht.
#[derive(Debug, Clone)]
pub struct Handle {
    tx: mpsc::Sender<Msg>,
}

impl Handle {
    /// Reicht einen Request ein und wartet auf sein Ergebnis.
    ///
    /// # Errors
    ///
    /// `ResourceExhausted`, wenn der Ereigniskanal voll ist — das ist die
    /// Backpressure des Gateways, nicht ein Fehler. `Internal`, wenn der Actor
    /// beendet wurde.
    pub async fn submit(&self, descriptor: RequestDescriptor, request: ModelInferRequest) -> Reply {
        let (reply, wait) = oneshot::channel();
        let msg = Msg::Arrival {
            descriptor: Box::new(descriptor),
            request: Box::new(request),
            reply,
        };
        self.tx.try_send(msg).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                Status::resource_exhausted("OneTimer nimmt derzeit keine weiteren Requests an")
            }
            mpsc::error::TrySendError::Closed(_) => {
                Status::internal("der Scheduler ist nicht mehr aktiv")
            }
        })?;
        wait.await
            .map_err(|_| Status::internal("der Scheduler hat den Request verworfen"))?
    }

    /// Liest die aktuellen Zaehler.
    ///
    /// # Errors
    ///
    /// Wenn der Actor beendet wurde.
    pub async fn metrics(&self) -> Result<Metrics, Status> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Msg::Snapshot(tx))
            .await
            .map_err(|_| Status::internal("der Scheduler ist nicht mehr aktiv"))?;
        rx.await
            .map_err(|_| Status::internal("der Scheduler hat nicht geantwortet"))
    }
}

/// Der Actor.
struct Actor {
    scheduler: Scheduler,
    config: Arc<Resolved>,
    /// Ein Client je Backend-Endpunkt.
    ///
    /// Vision- und Sprachmodelle laufen in getrennten Servern, weil ihre
    /// Backends unvereinbare Bibliotheksstaende brauchen. Die Kapazitaets-
    /// rechnung bleibt davon unberuehrt: die Slots modellieren die GPU, nicht
    /// den Prozess.
    backends: HashMap<String, Arc<TritonClient>>,
    clock: MonotonicClock,
    tx: mpsc::Sender<Msg>,
    /// Wartende Clients je Request.
    waiting: HashMap<RequestId, oneshot::Sender<Reply>>,
    /// Noch nicht weitergereichte OIP-Requests.
    ///
    /// Sie liegen hier, weil ein Request zwischen Ankunft und Dispatch noch
    /// verdraengt werden kann. Erst beim Dispatch wandert die Payload weiter.
    inbox: HashMap<RequestId, Box<ModelInferRequest>>,
    /// Bereits eingetroffene Backendantworten, die auf ihre Bewertung warten.
    responses: HashMap<RequestId, Result<ModelInferResponse, BackendError>>,
    /// Laufende zerlegte Auftraege (ADR-0014).
    jobs: HashMap<RequestId, GenerativeJob>,
    /// Die Beschreibung eines laufenden Auftrags, fuer seine Fortsetzung.
    descriptors: HashMap<RequestId, RequestDescriptor>,
    /// Kennungen fuer Fortsetzungsauftraege.
    next_id: u64,
    /// Naechster geplanter Weckruf.
    next_wake: Option<Instant>,
}

/// Startet den Actor und gibt seinen Griff zurueck.
///
/// # Errors
///
/// [`SchedulerError`], wenn die aufgeloeste Konfiguration keinen gueltigen
/// Scheduler ergibt.
pub fn spawn(
    config: Arc<Resolved>,
    backend: &Arc<TritonClient>,
    clock: MonotonicClock,
    unverified: &[onetimer_core::ModelIdx],
) -> Result<Handle, SchedulerError> {
    // Fuer jeden in der Konfiguration genannten Endpunkt ein Client. Der
    // uebergebene deckt den Standardendpunkt ab.
    let mut backends: HashMap<String, Arc<TritonClient>> = HashMap::new();
    for endpoint in config.endpoints() {
        let client = if endpoint == config.backend_endpoint {
            Arc::clone(backend)
        } else {
            Arc::new(TritonClient::new(&endpoint))
        };
        backends.insert(endpoint, client);
    }

    let overload = OverloadController::new(OverloadConfig::default(), clock.now())
        .map_err(|_| SchedulerError::NoModels)?;
    let mut scheduler = Scheduler::new(
        config.contracts.clone(),
        config.slots.clone(),
        overload,
        config.margin,
    )?;
    // G-010: Profile, deren Umgebung sich geaendert hat, werden vorsichtiger
    // geplant, bis der Estimator eigene Messungen hat (ADR-0016).
    for model in unverified {
        scheduler.mark_profile_unverified(*model);
    }

    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let actor = Actor {
        scheduler,
        config,
        backends,
        clock,
        tx: tx.clone(),
        waiting: HashMap::new(),
        inbox: HashMap::new(),
        responses: HashMap::new(),
        jobs: HashMap::new(),
        descriptors: HashMap::new(),
        // Fortsetzungen bekommen Kennungen aus einem eigenen Bereich, damit
        // sie sich nicht mit denen des Gateways ueberschneiden.
        next_id: u64::MAX.wrapping_div(2),
        next_wake: None,
    };
    tokio::spawn(actor.run(rx));
    Ok(Handle { tx })
}

impl Actor {
    async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        loop {
            let sleep = self.sleep_until_next_wake();
            let msg = tokio::select! {
                received = rx.recv() => match received {
                    Some(msg) => msg,
                    None => break,
                },
                () = sleep => Msg::Tick,
            };
            self.handle(msg);
        }
        tracing::info!("Scheduler-Actor beendet");
    }

    /// Wartet bis zum naechsten geplanten Weckruf.
    ///
    /// Ohne diesen Weckruf bliebe eine non-work-conserving Entscheidung
    /// haengen: der Scheduler hat absichtlich nichts gestartet und wuerde ohne
    /// aeusseres Ereignis nie wieder nachsehen (Spec 10.7).
    fn sleep_until_next_wake(&self) -> tokio::time::Sleep {
        let now = self.clock.now();
        let delay = match self.next_wake {
            Some(wake) if wake > now => {
                std::time::Duration::from_nanos(wake.saturating_since(now).as_nanos())
            }
            // Kein geplanter Weckruf: lange schlafen, bis ein Ereignis kommt.
            Some(_) => std::time::Duration::from_millis(1),
            None => std::time::Duration::from_secs(3_600),
        };
        tokio::time::sleep(delay)
    }

    fn handle(&mut self, msg: Msg) {
        let now = self.clock.now();
        let mut actions = Vec::new();
        let mut sink = |action: Action| actions.push(action);

        match msg {
            Msg::Arrival {
                descriptor,
                request,
                reply,
            } => {
                let id = descriptor.id;
                self.waiting.insert(id, reply);
                self.inbox.insert(id, request);
                self.scheduler
                    .on_event(now, Event::Arrival(*descriptor), &mut sink);
            }
            Msg::BackendDone {
                request,
                slot,
                result,
            } => {
                self.responses.insert(request, *result);
                self.scheduler
                    .on_event(now, Event::Completion { request, slot }, &mut sink);
            }
            Msg::Tick => self.scheduler.on_event(now, Event::Tick, &mut sink),
            Msg::Snapshot(tx) => {
                // Die Margen liegen nicht im Zaehlerblock, sondern in den
                // Reglern. Sie gehoeren trotzdem in den Snapshot: ueber Stunden
                // gelesen zeigen sie, ob das System zur Ruhe kommt.
                let mut metrics = *self.scheduler.metrics();
                for (index, slot) in metrics.margin_percent.iter_mut().enumerate() {
                    if let Ok(model) = u16::try_from(index) {
                        *slot = self
                            .scheduler
                            .margin_of(onetimer_core::ModelIdx(model))
                            .as_percent();
                    }
                }
                let _ = tx.send(metrics);
                return;
            }
        }

        for action in actions {
            self.apply(now, action);
        }
    }

    fn apply(&mut self, now: Instant, action: Action) {
        match action {
            Action::Dispatch {
                request,
                model,
                variant,
                slot,
                quantum,
                ..
            } => {
                self.forward(request, model, variant, slot, quantum);
            }
            Action::Terminate { request, state } => self.finish(request, state),
            Action::WakeAt(at) => {
                self.next_wake = Some(match self.next_wake {
                    Some(existing) if existing <= at && existing > now => existing,
                    _ => at,
                });
            }
            // Die Rueckkopplung fuer den Online Runtime Estimator (WP11).
            // Noch verarbeitet sie niemand; sie wird bewusst emittiert, damit
            // die Schnittstelle steht, bevor der Schaetzer gebaut wird.
            Action::ObservedRuntime { .. } => {}
        }
    }

    /// Reicht einen Request an das Backend weiter.
    fn forward(
        &mut self,
        request: RequestId,
        model: onetimer_core::ModelIdx,
        variant: onetimer_core::VariantIdx,
        slot: SlotIdx,
        quantum: Option<u32>,
    ) {
        let Some(mut oip) = self.inbox.remove(&request) else {
            tracing::warn!(%request, "Dispatch ohne zugehoerigen Request");
            return;
        };
        let Some(backend_model) = self.config.backend_model(model, variant.get()) else {
            tracing::error!(%request, "keine Backendvariante fuer die Auswahl");
            self.finish(request, RequestState::Failed);
            return;
        };

        // Der einzige Eingriff in die Nutzlast: das logische Modell wird durch
        // die gewaehlte physische Variante ersetzt (Spec 12.1). Alles andere
        // bleibt unangetastet, damit unbekannte OIP-Felder ueberleben
        // (Spec 6.1).
        backend_model.clone_into(&mut oip.model_name);

        // Bei einem zerlegten Auftrag wird nicht der urspruengliche Request
        // weitergereicht, sondern das naechste Quantum: Prompt plus bisher
        // Erzeugtes, begrenzt auf die vom Scheduler bestimmte Tokenzahl.
        if let (Some(tokens), Some(job)) = (quantum, self.jobs.get(&request)) {
            let quantum_request = job.build_quantum(&oip, tokens);
            *oip = quantum_request;
            backend_model.clone_into(&mut oip.model_name);
        }

        // Eigenschaft des Modells, nicht der Zerlegung: ein decoupled Modell
        // braucht den Stream-Aufruf auch ungeteilt. Nach aussen bleibt der
        // Request unaer.
        let decoupled = self.config.is_decoupled(model);
        let Some(backend) = self.backend_for(model) else {
            tracing::error!(%request, "kein Backend fuer dieses Modell");
            self.finish(request, RequestState::Failed);
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = if decoupled {
                backend.infer_decoupled(*oip).await
            } else {
                backend.infer(*oip).await
            };
            let _ = tx
                .send(Msg::BackendDone {
                    request,
                    slot,
                    result: Box::new(result),
                })
                .await;
        });
    }

    /// Reiht das naechste Quantum eines Auftrags als neue Ankunft ein.
    ///
    /// Bewusst als **vollwertige Ankunft**: das Quantum muss die Zulassung
    /// erneut durchlaufen. Waere es privilegiert, koennte ein einmal
    /// gestarteter generativer Auftrag geschuetzte Arbeit dauerhaft
    /// verdraengen — genau das, was die Zerlegung verhindern soll.
    ///
    /// Die Generation Time bleibt die des urspruenglichen Auftrags. Ein
    /// Auftrag, der insgesamt zu lange braucht, altert damit korrekt und wird
    /// verworfen, statt unbegrenzt weiterzulaufen.
    fn continue_job(&mut self, request: RequestId, job: GenerativeJob) {
        let Some(descriptor) = self.descriptors.remove(&request) else {
            return;
        };
        let Some(reply) = self.waiting.remove(&request) else {
            return;
        };
        let Some(oip) = self.inbox.remove(&request) else {
            return;
        };
        self.responses.remove(&request);

        self.next_id = self.next_id.saturating_add(1);
        let continuation = RequestId(self.next_id);
        let mut next = descriptor;
        next.id = continuation;

        self.jobs.insert(continuation, job);
        self.descriptors.insert(continuation, next);
        self.waiting.insert(continuation, reply);
        self.inbox.insert(continuation, oip);

        let now = self.clock.now();
        let mut actions = Vec::new();
        self.scheduler
            .on_event(now, Event::Arrival(next), &mut |action: Action| {
                actions.push(action);
            });
        for action in actions {
            self.apply(now, action);
        }
    }

    /// Der Client fuer das Backend eines Modells.
    fn backend_for(&self, model: onetimer_core::ModelIdx) -> Option<Arc<TritonClient>> {
        self.backends
            .get(self.config.endpoint_of(model))
            .map(Arc::clone)
    }

    /// Beantwortet einen wartenden Client — oder setzt einen zerlegten
    /// Auftrag fort.
    fn finish(&mut self, request: RequestId, state: RequestState) {
        // Ein Quantum, das erfolgreich war und den Auftrag noch nicht beendet
        // hat, ist keine Antwort an den Client, sondern der Anlass fuer das
        // naechste Quantum.
        if state == RequestState::CompletedValid
            && let Some(mut job) = self.jobs.remove(&request)
            && let Some(Ok(response)) = self.responses.get(&request)
        {
            let finished = job.absorb(response);
            if !finished && self.waiting.contains_key(&request) {
                self.continue_job(request, job);
                return;
            }
            // Fertig: die gesammelte Antwort an den Client.
            if let Some(Ok(response)) = self.responses.get_mut(&request) {
                *response = job.build_response(response);
            }
        }

        let Some(reply) = self.waiting.remove(&request) else {
            return;
        };
        self.inbox.remove(&request);
        self.descriptors.remove(&request);

        let payload = self.responses.remove(&request);
        let outcome = match (state, payload) {
            (RequestState::CompletedValid, Some(Ok(response))) => Ok(response),
            (RequestState::CompletedObsolete, Some(Ok(mut response))) => {
                mark_obsolete(&mut response);
                Ok(response)
            }
            (_, Some(Err(error))) => Err(Status::unavailable(error.to_string())),
            (state, _) => Err(status_for(state)
                .unwrap_or_else(|| Status::internal("unerwarteter Requestzustand"))),
        };
        let _ = reply.send(outcome);
    }
}
