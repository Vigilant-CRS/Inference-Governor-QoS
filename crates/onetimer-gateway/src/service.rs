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
use onetimer_backend_triton::TritonClient;
use onetimer_config::schema::Resolved;
use onetimer_core::{Duration, PayloadRef, RequestDescriptor, RequestId, SupersessionKey};
use onetimer_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;
// Der Dienst implementiert alle 21 Methoden des OIP-Dienstes; die Typen
// einzeln aufzuzaehlen waere eine Liste ohne Erkenntniswert, die bei jeder
// Protokollerweiterung nachgezogen werden muesste.
#[allow(clippy::wildcard_imports)]
use onetimer_protocol_oip::inference::*;
use onetimer_protocol_oip::params::{self, GenerationSource, OneTimerParams};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tonic::{Request, Response, Status};

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
        Self {
            config,
            backend,
            scheduler,
            clock,
            next_id: AtomicU64::new(1),
        }
    }

    fn allocate_id(&self) -> RequestId {
        RequestId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    async fn raw(
        &self,
    ) -> Result<
        onetimer_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient<
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
        model: onetimer_core::ModelIdx,
        request: &ModelInferRequest,
    ) -> Result<RequestDescriptor, Status> {
        let extracted: OneTimerParams = params::extract(&request.parameters)
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

        if let GenerationSource::ArrivalFallback {
            rejected_client_value: true,
        } = source
        {
            // Der haeufigste Integrationsfehler: der Parameter ist gesetzt,
            // stammt aber aus einer anderen Zeitbasis. Ohne diese Meldung
            // arbeitet OneTimer stumm ohne die Semantik, fuer die er da ist.
            tracing::warn!(
                model = model.get(),
                "Generation-Time des Clients verworfen; Ankunftszeit wird verwendet"
            );
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
            criticality: extracted.class.unwrap_or(contract.criticality),
            queue_policy: contract.queue.policy,
            stateful: contract.stateful,
            variant: None,
            payload: PayloadRef(0),
        })
    }
}

#[tonic::async_trait]
impl GrpcInferenceService for GatewayService {
    async fn model_infer(
        &self,
        request: Request<ModelInferRequest>,
    ) -> Result<Response<ModelInferResponse>, Status> {
        let inner = request.into_inner();

        let Some(model) = self.config.model_index(&inner.model_name) else {
            // Unkonfiguriertes Modell: unveraendert durchreichen (Spec L-002).
            return self.raw().await?.model_infer(inner).await;
        };

        let descriptor = self.build_descriptor(model, &inner)?;
        let response = self.scheduler.submit(descriptor, inner).await?;
        Ok(Response::new(response))
    }

    async fn model_ready(
        &self,
        request: Request<ModelReadyRequest>,
    ) -> Result<Response<ModelReadyResponse>, Status> {
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
        self.raw()
            .await?
            .server_metadata(request.into_inner())
            .await
    }

    async fn model_config(
        &self,
        request: Request<ModelConfigRequest>,
    ) -> Result<Response<ModelConfigResponse>, Status> {
        Box::pin(async move { self.raw().await?.model_config(request.into_inner()).await }).await
    }

    async fn model_statistics(
        &self,
        request: Request<ModelStatisticsRequest>,
    ) -> Result<Response<ModelStatisticsResponse>, Status> {
        self.raw()
            .await?
            .model_statistics(request.into_inner())
            .await
    }

    async fn repository_index(
        &self,
        request: Request<RepositoryIndexRequest>,
    ) -> Result<Response<RepositoryIndexResponse>, Status> {
        self.raw()
            .await?
            .repository_index(request.into_inner())
            .await
    }

    async fn repository_model_load(
        &self,
        request: Request<RepositoryModelLoadRequest>,
    ) -> Result<Response<RepositoryModelLoadResponse>, Status> {
        self.raw()
            .await?
            .repository_model_load(request.into_inner())
            .await
    }

    async fn repository_model_unload(
        &self,
        request: Request<RepositoryModelUnloadRequest>,
    ) -> Result<Response<RepositoryModelUnloadResponse>, Status> {
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
        self.raw()
            .await?
            .system_shared_memory_status(request.into_inner())
            .await
    }

    async fn system_shared_memory_register(
        &self,
        request: Request<SystemSharedMemoryRegisterRequest>,
    ) -> Result<Response<SystemSharedMemoryRegisterResponse>, Status> {
        self.raw()
            .await?
            .system_shared_memory_register(request.into_inner())
            .await
    }

    async fn system_shared_memory_unregister(
        &self,
        request: Request<SystemSharedMemoryUnregisterRequest>,
    ) -> Result<Response<SystemSharedMemoryUnregisterResponse>, Status> {
        self.raw()
            .await?
            .system_shared_memory_unregister(request.into_inner())
            .await
    }

    async fn cuda_shared_memory_status(
        &self,
        request: Request<CudaSharedMemoryStatusRequest>,
    ) -> Result<Response<CudaSharedMemoryStatusResponse>, Status> {
        self.raw()
            .await?
            .cuda_shared_memory_status(request.into_inner())
            .await
    }

    async fn cuda_shared_memory_register(
        &self,
        request: Request<CudaSharedMemoryRegisterRequest>,
    ) -> Result<Response<CudaSharedMemoryRegisterResponse>, Status> {
        self.raw()
            .await?
            .cuda_shared_memory_register(request.into_inner())
            .await
    }

    async fn cuda_shared_memory_unregister(
        &self,
        request: Request<CudaSharedMemoryUnregisterRequest>,
    ) -> Result<Response<CudaSharedMemoryUnregisterResponse>, Status> {
        self.raw()
            .await?
            .cuda_shared_memory_unregister(request.into_inner())
            .await
    }

    async fn trace_setting(
        &self,
        request: Request<TraceSettingRequest>,
    ) -> Result<Response<TraceSettingResponse>, Status> {
        Box::pin(async move { self.raw().await?.trace_setting(request.into_inner()).await }).await
    }

    async fn log_settings(
        &self,
        request: Request<LogSettingsRequest>,
    ) -> Result<Response<LogSettingsResponse>, Status> {
        Box::pin(async move { self.raw().await?.log_settings(request.into_inner()).await }).await
    }

    type ModelStreamInferStream = tokio_stream_placeholder::Never;

    /// Streaming-Inferenz ist im MVP nicht unterstuetzt (Spec 16.1).
    ///
    /// Bewusst ein klarer Fehler statt eines stillen Durchreichens: ein
    /// gestreamter Request wuerde die Frische- und Zulassungslogik umgehen und
    /// dem Nutzer vorspiegeln, OneTimer wirke, wo er nicht wirkt.
    async fn model_stream_infer(
        &self,
        _request: Request<tonic::Streaming<ModelInferRequest>>,
    ) -> Result<Response<Self::ModelStreamInferStream>, Status> {
        Err(Status::unimplemented(
            "ModelStreamInfer wird von OneTimer nicht unterstuetzt; \
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
    use onetimer_protocol_oip::inference::ModelStreamInferResponse;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tonic::codegen::tokio_stream::Stream;

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
