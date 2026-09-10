//! Der gRPC-Dienst: die Uebersetzung zwischen Draht und Scheduling-Kern.
//!
//! ## Zwei Betriebsarten je Modell
//!
//! * **Konfiguriert.** Der Request laeuft durch den Scheduler: Frische,
//!   Deadline, Zulassung, Variantenwahl.
//! ## Warum die Methodenkoerper geboxt sind
//!
//! Die generierten OIP-Typen sind gross; ein Dienstfuture, das mehrere davon
//! als lokale Variablen haelt, wird schnell zweistellig kilobyteschwer. Der
//! Dienst hat 21 Methoden, und tonic fasst sie zu einem Future zusammen, das
//! so gross ist wie das groesste — ein Stack-Ueberlauf unter Last waere die
//! Folge. `Box::pin` legt den Koerper auf den Heap und laesst im aeusseren
//! Future nur einen Zeiger zurueck.
//!
//! * **Unkonfiguriert.** Der Request wird unveraendert durchgereicht
//!   (Spec L-002). Das ist keine Notloesung, sondern die Integrationszusage:
//!   ein Kunde stellt den Endpunkt um und konfiguriert erst danach Modell fuer
//!   Modell die QoS-Regeln.

use crate::actor::Handle;
use crate::clock::MonotonicClock;
use crate::shm::{Region, ShmRegistry};
use vig_backend_triton::TritonClient;
use vig_config::schema::{Resolved, TrustMode};
use vig_core::{Criticality, Duration, PayloadRef, RequestDescriptor, RequestId, SupersessionKey};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;
// Der Dienst implementiert alle 21 Methoden des OIP-Dienstes; die Typen
// einzeln aufzuzaehlen waere eine Liste ohne Erkenntniswert, die bei jeder
// Protokollerweiterung nachgezogen werden muesste.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tonic::{Request, Response, Status};
#[allow(clippy::wildcard_imports)]
use vig_protocol_oip::inference::*;
use vig_protocol_oip::params::{self, GenerationSource, VigParams};

/// Hoechstalter, ab dem ein Client-Zeitstempel als „aus einer anderen Uhr"
/// gilt, wenn das Modell kein `max_age` konfiguriert hat (ADR-0011).
const DEFAULT_PLAUSIBLE_AGE: Duration = Duration::from_nanos_unbounded(5_000_000_000);

/// Der OIP-Dienst des Gateways.
#[derive(Debug)]
pub struct GatewayService {
    config: Arc<Resolved>,
    backend: Arc<TritonClient>,
    scheduler: Handle,
    clock: MonotonicClock,
    next_id: AtomicU64,
    shm: ShmRegistry,
    budget: ByteBudget,
    tokens: Option<crate::auth::Tokens>,
}

/// Ein Budget fuer gleichzeitig gehaltene Requestnutzlast.
///
/// Zaehlt Bytes, nicht Requests. Der Ereigniskanal des Actors begrenzt bereits
/// die Anzahl; die kostet aber je nach Tensorgroesse zwischen einem Kilobyte
/// und zig Megabyte. Eine Grenze, die beides nicht unterscheidet, ist entweder
/// zu eng fuer Bilder oder zu weit fuer Speicher.
#[derive(Debug)]
struct ByteBudget {
    limit: u64,
    used: Arc<AtomicU64>,
}

/// Gibt die reservierten Bytes zurueck, sobald der Request beantwortet ist.
///
/// Als Guard und nicht als Aufruf am Ende: jeder fruehe Rueckgabepfad — und
/// davon gibt es in `model_infer` mehrere — wuerde das Budget sonst dauerhaft
/// verkleinern, bis das Gateway ohne erkennbaren Grund alles ablehnt.
#[derive(Debug)]
struct BytePermit {
    used: Arc<AtomicU64>,
    bytes: u64,
}

impl Drop for BytePermit {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl ByteBudget {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            used: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Reserviert `bytes`, wenn das Budget reicht.
    fn try_reserve(&self, bytes: u64) -> Option<BytePermit> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(bytes);
            if next > self.limit {
                return None;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(BytePermit {
                        used: Arc::clone(&self.used),
                        bytes,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

/// Die Nutzlastgroesse eines Requests.
///
/// Bei Shared Memory reist der Tensor als Referenz, und der Request selbst ist
/// wenige hundert Bytes gross. Genau dann soll das Budget auch nichts
/// nennenswertes verbrauchen.
///
/// Gezaehlt werden **beide** zulaessigen Darstellungen. OIP erlaubt Rohdaten in
/// `raw_input_contents` und typisierte Werte in `inputs[].contents`; nur die
/// erste zu zaehlen hiess, dass ein Client das Budget umgeht, ohne etwas
/// Unerlaubtes zu tun — er benutzt schlicht die andere Form.
fn payload_bytes(request: &ModelInferRequest) -> u64 {
    let raw = request
        .raw_input_contents
        .iter()
        .map(|c| c.len() as u64)
        .fold(0_u64, u64::saturating_add);

    let typed = request
        .inputs
        .iter()
        .filter_map(|i| i.contents.as_ref())
        .map(tensor_content_bytes)
        .fold(0_u64, u64::saturating_add);

    raw.saturating_add(typed)
}

/// Die Groesse eines typisierten Tensorinhalts in Bytes.
///
/// Nicht die Zahl der Elemente, sondern ihr Speicher: ein `fp64`-Wert kostet
/// achtmal so viel wie ein `bool`. Wer Elemente zaehlt, deckelt einen
/// Doublevektor bei einem Achtel seines tatsaechlichen Bedarfs.
fn tensor_content_bytes(c: &InferTensorContents) -> u64 {
    let of = |count: usize, width: usize| (count as u64).saturating_mul(width as u64);
    of(c.bool_contents.len(), size_of::<bool>())
        .saturating_add(of(c.int_contents.len(), size_of::<i32>()))
        .saturating_add(of(c.int64_contents.len(), size_of::<i64>()))
        .saturating_add(of(c.uint_contents.len(), size_of::<u32>()))
        .saturating_add(of(c.uint64_contents.len(), size_of::<u64>()))
        .saturating_add(of(c.fp32_contents.len(), size_of::<f32>()))
        .saturating_add(of(c.fp64_contents.len(), size_of::<f64>()))
        .saturating_add(
            c.bytes_contents
                .iter()
                .map(|b| b.len() as u64)
                .fold(0_u64, u64::saturating_add),
        )
}

impl GatewayService {
    /// Baut den Dienst.
    #[must_use]
    pub fn new(
        config: Arc<Resolved>,
        backend: Arc<TritonClient>,
        scheduler: Handle,
        clock: MonotonicClock,
    ) -> Self {
        let limit = config.max_inflight_bytes;
        Self {
            config,
            backend,
            scheduler,
            clock,
            next_id: AtomicU64::new(1),
            shm: ShmRegistry::new(),
            budget: ByteBudget::new(limit),
            tokens: None,
        }
    }

    /// Schaltet die Bearer-Token-Pruefung ein.
    ///
    /// Ohne Aufruf ist sie aus. Bewusst als eigener Schritt und nicht aus der
    /// Konfiguration gelesen: der Dienst laedt keine Dateien: das tut der
    /// Aufrufer, und er kann einen Ladefehler melden, bevor irgendetwas
    /// lauscht.
    #[must_use]
    pub fn with_tokens(mut self, tokens: crate::auth::Tokens) -> Self {
        self.tokens = Some(tokens);
        self
    }

    /// Der Griff auf den Scheduler-Actor.
    ///
    /// Fuer Tests und Betriebswerkzeuge, die den Zustand abfragen wollen, ohne
    /// den Griff getrennt durchreichen zu muessen.
    #[must_use]
    pub fn scheduler_handle(&self) -> Handle {
        self.scheduler.clone()
    }

    /// Prueft die Zugangsberechtigung einer Anfrage.
    fn authorize<T>(&self, request: &Request<T>) -> Result<(), Status> {
        match &self.tokens {
            Some(tokens) => tokens.check(request),
            None => Ok(()),
        }
    }

    /// Die wirksame Wichtigkeitsklasse eines Requests.
    ///
    /// Im offenen Modus gilt die Clientangabe. Im strikten Modus darf sie die
    /// Klasse nur **senken**: sonst setzt sich jeder Aufrufer selbst auf
    /// `protected`, und die Prioritaeten sind eine Empfehlung statt einer
    /// Zusage. Ein Client, der freiwillig zurücktritt, ist dagegen harmlos —
    /// und nuetzlich.
    fn effective_class(
        &self,
        declared: Option<Criticality>,
        configured: Criticality,
    ) -> Criticality {
        match (self.config.trust, declared) {
            (TrustMode::Open, Some(class)) => class,
            (TrustMode::Strict, Some(class)) if class < configured => class,
            _ => configured,
        }
    }

    /// Der Bestand durchgereichter Shared-Memory-Regionen.
    #[must_use]
    pub const fn shm_registry(&self) -> &ShmRegistry {
        &self.shm
    }

    fn allocate_id(&self) -> RequestId {
        RequestId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    async fn raw(
        &self,
    ) -> Result<
        vig_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient<
            tonic::transport::Channel,
        >,
        Status,
    > {
        self.backend
            .raw()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))
    }

    /// Baut die Scheduling-Metadaten aus Request und Konfiguration.
    ///
    /// Was der Client angibt, gewinnt; was er weglaesst, kommt aus dem Vertrag
    /// (Spec 16.3). Ein fehlerhafter Parameter fuehrt zur Ablehnung, nicht zu
    /// einem stillen Default.
    fn build_descriptor(
        &self,
        model: vig_core::ModelIdx,
        request: &ModelInferRequest,
    ) -> Result<RequestDescriptor, Status> {
        let extracted: VigParams = params::extract(&request.parameters)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let contract = self
            .config
            .contracts
            .get(model.get())
            .ok_or_else(|| Status::internal("Modellindex ohne Vertrag"))?;

        let arrival = self.clock.now();
        let plausible = contract
            .max_age
            .and_then(|m| m.checked_scale(10, 1))
            .unwrap_or(DEFAULT_PLAUSIBLE_AGE);
        let (generation, source) = params::resolve_generation(&extracted, arrival, plausible);

        match source {
            GenerationSource::ArrivalFallback {
                rejected_client_value: true,
            } => {
                // Der haeufigste Integrationsfehler: der Parameter ist gesetzt,
                // stammt aber aus einer anderen Zeitbasis. Ohne diese Meldung
                // arbeitet Vigilant stumm ohne die Semantik, fuer die er da ist.
                tracing::warn!(
                    model = model.get(),
                    "Generation-Time des Clients liegt in der Zukunft und wurde verworfen; Ankunftszeit wird verwendet"
                );
            }
            GenerationSource::ClampedAge => {
                tracing::warn!(
                    model = model.get(),
                    plausible_ms = plausible.as_nanos().checked_div(1_000_000).unwrap_or(0),
                    "Generation-Time des Clients ist unplausibel alt; auf die Plausibilitaetsgrenze geklemmt"
                );
            }
            GenerationSource::ArrivalFallback {
                rejected_client_value: false,
            }
            | GenerationSource::ClientAge
            | GenerationSource::ClientMonotonic => {}
        }

        let deadline = extracted.deadline.unwrap_or(contract.deadline);
        let absolute_deadline = generation.checked_add(deadline);
        if absolute_deadline.is_none() {
            return Err(Status::invalid_argument(
                "Generation Time plus Deadline ist nicht darstellbar",
            ));
        }

        Ok(RequestDescriptor {
            id: self.allocate_id(),
            logical_model: model,
            supersession_key: extracted
                .supersession_key
                .or(extracted.stream_id)
                .map_or(SupersessionKey::DEFAULT, SupersessionKey),
            generation_time: generation,
            arrival_time: arrival,
            absolute_deadline,
            max_age: extracted.max_age.or(contract.max_age),
            criticality: self.effective_class(extracted.class, contract.criticality),
            queue_policy: contract.queue.policy,
            stateful: contract.stateful,
            variant: None,
            payload: PayloadRef(0),
            context_tokens: 0,
            // Beides setzt der Actor, sobald feststeht, ob dieser Auftrag
            // wirklich zerlegt wird (NV-16).
            decomposable: false,
        })
    }
}

#[tonic::async_trait]
impl GrpcInferenceService for GatewayService {
    async fn model_infer(
        &self,
        request: Request<ModelInferRequest>,
    ) -> Result<Response<ModelInferResponse>, Status> {
        // Vor allem anderen: unautorisierte Arbeit soll nicht einmal die
        // Parameter kosten, die ihr Auslesen braucht.
        self.authorize(&request)?;
        let inner = request.into_inner();

        let Some(model) = self.config.model_index(&inner.model_name) else {
            // Unkonfiguriertes Modell: unveraendert durchreichen (Spec L-002).
            //
            // Im strikten Modus nicht: sonst genuegt der **physische**
            // Modellname, um die gesamte Steuerung zu umgehen. Wer
            // `detector_large` statt `detector` aufruft, laeuft dann ohne
            // Kredit, ohne Frischepruefung und ohne Look-ahead — und
            // verdraengt genau die geschuetzte Arbeit, die der Governor
            // schuetzen soll.
            if self.config.trust == TrustMode::Strict {
                return Err(Status::not_found(format!(
                    "Modell {} ist nicht konfiguriert; im strikten Modus wird nicht \
                     am Governor vorbei ausgefuehrt",
                    inner.model_name
                )));
            }
            return self.raw().await?.model_infer(inner).await;
        };

        // Bytebudget: der Ereigniskanal begrenzt die Anzahl offener Requests,
        // nicht ihren Speicher. Ohne diese Schranke haelt ein Aufrufer mit
        // grossen Tensoren den Prozess an die Speicherwand, lange bevor die
        // Requestzahl auffaellt.
        let bytes = payload_bytes(&inner);
        let Some(_permit) = self.budget.try_reserve(bytes) else {
            return Err(Status::resource_exhausted(format!(
                "Nutzlastbudget erschoepft: {bytes} Bytes angefordert, Obergrenze \
                 {} Bytes",
                self.config.max_inflight_bytes
            )));
        };

        let descriptor = self.build_descriptor(model, &inner)?;
        let logical = inner.model_name.clone();
        let mut response = self.scheduler.submit(descriptor, inner).await?;
        // Nach aussen existiert nur das logische Modell. Welche Variante
        // gelaufen ist, ist eine interne Entscheidung — steht ihr Name in der
        // Antwort, koppelt sich der Client daran, und die Variantenwahl waere
        // faktisch nicht mehr frei. `model_metadata` haelt es genauso.
        response.model_name = logical;
        Ok(Response::new(response))
    }

    async fn model_ready(
        &self,
        request: Request<ModelReadyRequest>,
    ) -> Result<Response<ModelReadyResponse>, Status> {
        self.authorize(&request)?;
        let mut inner = request.into_inner();
        if let Some(model) = self.config.model_index(&inner.name)
            && let Some(physical) = self.config.backend_model(model, 0)
        {
            // Ein logisches Modell ist bereit, wenn seine beste Variante es ist.
            inner.name = physical.to_owned();
        }
        self.raw().await?.model_ready(inner).await
    }

    async fn model_metadata(
        &self,
        request: Request<ModelMetadataRequest>,
    ) -> Result<Response<ModelMetadataResponse>, Status> {
        self.authorize(&request)?;
        let mut inner = request.into_inner();
        let logical = inner.name.clone();
        let mapped = self
            .config
            .model_index(&logical)
            .and_then(|model| self.config.backend_model(model, 0).map(ToOwned::to_owned));
        if let Some(physical) = mapped {
            inner.name = physical;
        }
        let mut response = self.raw().await?.model_metadata(inner).await?.into_inner();
        // Der Client hat nach dem logischen Modell gefragt und bekommt es auch
        // zurueck; welche Variante dahinterliegt, ist seine Sache nicht.
        response.name = logical;
        Ok(Response::new(response))
    }

    async fn server_live(
        &self,
        request: Request<ServerLiveRequest>,
    ) -> Result<Response<ServerLiveResponse>, Status> {
        Box::pin(async move { self.raw().await?.server_live(request.into_inner()).await }).await
    }

    async fn server_ready(
        &self,
        request: Request<ServerReadyRequest>,
    ) -> Result<Response<ServerReadyResponse>, Status> {
        Box::pin(async move { self.raw().await?.server_ready(request.into_inner()).await }).await
    }

    async fn server_metadata(
        &self,
        request: Request<ServerMetadataRequest>,
    ) -> Result<Response<ServerMetadataResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .server_metadata(request.into_inner())
            .await
    }

    async fn model_config(
        &self,
        request: Request<ModelConfigRequest>,
    ) -> Result<Response<ModelConfigResponse>, Status> {
        self.authorize(&request)?;
        Box::pin(async move { self.raw().await?.model_config(request.into_inner()).await }).await
    }

    async fn model_statistics(
        &self,
        request: Request<ModelStatisticsRequest>,
    ) -> Result<Response<ModelStatisticsResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .model_statistics(request.into_inner())
            .await
    }

    async fn repository_index(
        &self,
        request: Request<RepositoryIndexRequest>,
    ) -> Result<Response<RepositoryIndexResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .repository_index(request.into_inner())
            .await
    }

    async fn repository_model_load(
        &self,
        request: Request<RepositoryModelLoadRequest>,
    ) -> Result<Response<RepositoryModelLoadResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .repository_model_load(request.into_inner())
            .await
    }

    async fn repository_model_unload(
        &self,
        request: Request<RepositoryModelUnloadRequest>,
    ) -> Result<Response<RepositoryModelUnloadResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .repository_model_unload(request.into_inner())
            .await
    }

    // Shared-Memory-Endpunkte werden unveraendert durchgereicht. Damit kann ein
    // Client seine Regionen schon heute registrieren; der Governor beruehrt die
    // Payload dabei nie (ADR-0003).
    async fn system_shared_memory_status(
        &self,
        request: Request<SystemSharedMemoryStatusRequest>,
    ) -> Result<Response<SystemSharedMemoryStatusResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .system_shared_memory_status(request.into_inner())
            .await
    }

    async fn system_shared_memory_register(
        &self,
        request: Request<SystemSharedMemoryRegisterRequest>,
    ) -> Result<Response<SystemSharedMemoryRegisterResponse>, Status> {
        self.authorize(&request)?;
        Box::pin(async move {
            let inner = request.into_inner();
            let region = Region {
                byte_size: inner.byte_size,
                offset: inner.offset,
                cuda: false,
            };
            let name = inner.name.clone();
            let response = self
                .raw()
                .await?
                .system_shared_memory_register(inner)
                .await?;
            // Erst nach der Bestaetigung buchen: sonst fuehrte Vigilant
            // Regionen, die es im Backend gar nicht gibt.
            self.shm.record(name, region);
            Ok(response)
        })
        .await
    }

    async fn system_shared_memory_unregister(
        &self,
        request: Request<SystemSharedMemoryUnregisterRequest>,
    ) -> Result<Response<SystemSharedMemoryUnregisterResponse>, Status> {
        self.authorize(&request)?;
        Box::pin(async move {
            let inner = request.into_inner();
            let name = inner.name.clone();
            let response = self
                .raw()
                .await?
                .system_shared_memory_unregister(inner)
                .await?;
            self.shm.forget(&name);
            Ok(response)
        })
        .await
    }

    async fn cuda_shared_memory_status(
        &self,
        request: Request<CudaSharedMemoryStatusRequest>,
    ) -> Result<Response<CudaSharedMemoryStatusResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .cuda_shared_memory_status(request.into_inner())
            .await
    }

    async fn cuda_shared_memory_register(
        &self,
        request: Request<CudaSharedMemoryRegisterRequest>,
    ) -> Result<Response<CudaSharedMemoryRegisterResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .cuda_shared_memory_register(request.into_inner())
            .await
    }

    async fn cuda_shared_memory_unregister(
        &self,
        request: Request<CudaSharedMemoryUnregisterRequest>,
    ) -> Result<Response<CudaSharedMemoryUnregisterResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .cuda_shared_memory_unregister(request.into_inner())
            .await
    }

    async fn trace_setting(
        &self,
        request: Request<TraceSettingRequest>,
    ) -> Result<Response<TraceSettingResponse>, Status> {
        self.authorize(&request)?;
        Box::pin(async move { self.raw().await?.trace_setting(request.into_inner()).await }).await
    }

    async fn log_settings(
        &self,
        request: Request<LogSettingsRequest>,
    ) -> Result<Response<LogSettingsResponse>, Status> {
        self.authorize(&request)?;
        Box::pin(async move { self.raw().await?.log_settings(request.into_inner()).await }).await
    }

    type ModelStreamInferStream = tokio_stream_placeholder::Never;

    /// Streaming-Inferenz ist im MVP nicht unterstuetzt (Spec 16.1).
    ///
    /// Bewusst ein klarer Fehler statt eines stillen Durchreichens: ein
    /// gestreamter Request wuerde die Frische- und Zulassungslogik umgehen und
    /// dem Nutzer vorspiegeln, Vigilant wirke, wo er nicht wirkt.
    async fn model_stream_infer(
        &self,
        request: Request<tonic::Streaming<ModelInferRequest>>,
    ) -> Result<Response<Self::ModelStreamInferStream>, Status> {
        self.authorize(&request)?;
        Err(Status::unimplemented(
            "ModelStreamInfer wird von Vigilant nicht unterstuetzt; \
             fuer gestreamte Inferenz direkt gegen das Backend arbeiten",
        ))
    }
}

/// Ein Stream, der nie etwas liefert.
///
/// Platzhalter fuer den nicht unterstuetzten Streaming-Endpunkt: der
/// zugehoerige Aufruf endet immer mit `Unimplemented`, der Typ wird trotzdem
/// gebraucht.
mod tokio_stream_placeholder {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tonic::codegen::tokio_stream::Stream;
    use vig_protocol_oip::inference::ModelStreamInferResponse;

    /// Ein leerer Stream.
    #[derive(Debug)]
    pub struct Never;

    impl Stream for Never {
        type Item = Result<ModelStreamInferResponse, tonic::Status>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(None)
        }
    }
}
