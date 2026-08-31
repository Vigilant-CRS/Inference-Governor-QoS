//! Ein minimales OIP-Backend fuer Integrationstests.
//!
//! Bewusst ein **echter gRPC-Server** und kein Funktionsaufruf: die Aussage,
//! die geprueft werden soll, betrifft den Draht — Serialisierung,
//! Verbindungsaufbau, Nebenlaeufigkeit, Statuscodes. Ein direkter Aufruf
//! wuerde genau die Schicht ueberspringen, um die es geht.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::missing_panics_doc
)]

use onetimer_protocol_oip::inference::grpc_inference_service_server::{
    GrpcInferenceService, GrpcInferenceServiceServer,
};
#[allow(clippy::wildcard_imports)]
use onetimer_protocol_oip::inference::*;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tonic::{Request, Response, Status};

/// Ein Backend, das jede Inferenz eine feste Zeit lang „rechnet".
#[derive(Debug, Default)]
pub struct MockBackend {
    /// Wie lange eine Inferenz dauert.
    pub compute: Duration,
    /// Zaehler der ausgefuehrten Inferenzen.
    pub served: AtomicU64,
    /// Namen der Modelle, die tatsaechlich angefragt wurden.
    pub seen_models: std::sync::Mutex<Vec<String>>,
    /// Summe der Rohdatenbytes, die tatsaechlich uebertragen wurden.
    ///
    /// Auf dem Shm-Pfad muss dieser Zaehler null bleiben: der Request traegt
    /// dann nur eine Referenz.
    pub raw_bytes_seen: AtomicU64,
}

impl MockBackend {
    #[must_use]
    pub fn new(compute: Duration) -> Self {
        Self {
            compute,
            served: AtomicU64::new(0),
            seen_models: std::sync::Mutex::new(Vec::new()),
            raw_bytes_seen: AtomicU64::new(0),
        }
    }
}

/// Startet das Mock-Backend auf einem freien Port.
pub async fn start(backend: Arc<MockBackend>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = Service { inner: backend };
    tokio::spawn(async move {
        let stream = tokio_stream::wrappers::TcpListenerStream::new(listener);
        // Dieselben Transportgrenzen wie im Gateway. Ein Backend mit engeren
        // Grenzen wuerde den Vergleich zugunsten des Proxys verfaelschen.
        let _ = tonic::transport::Server::builder()
            .initial_stream_window_size(onetimer_backend_triton::STREAM_WINDOW_BYTES)
            .initial_connection_window_size(onetimer_backend_triton::CONNECTION_WINDOW_BYTES)
            .add_service(
                GrpcInferenceServiceServer::new(service)
                    .max_decoding_message_size(onetimer_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(onetimer_backend_triton::DEFAULT_MAX_MESSAGE_BYTES),
            )
            .serve_with_incoming(stream)
            .await;
    });
    // Kurz warten, bis der Server tatsaechlich lauscht.
    tokio::time::sleep(Duration::from_millis(50)).await;
    address
}

struct Service {
    inner: Arc<MockBackend>,
}

// `#[tonic::async_trait]` schreibt `async fn` in boxed Futures um. Methoden,
// die aus einem `macro_rules!` stammen, sieht das Attributmakro nicht — sie
// muessen deshalb ausgeschrieben werden.

#[tonic::async_trait]
impl GrpcInferenceService for Service {
    async fn server_live(
        &self,
        _r: Request<ServerLiveRequest>,
    ) -> Result<Response<ServerLiveResponse>, Status> {
        Ok(Response::new(ServerLiveResponse { live: true }))
    }

    async fn server_ready(
        &self,
        _r: Request<ServerReadyRequest>,
    ) -> Result<Response<ServerReadyResponse>, Status> {
        Ok(Response::new(ServerReadyResponse { ready: true }))
    }

    async fn model_ready(
        &self,
        _r: Request<ModelReadyRequest>,
    ) -> Result<Response<ModelReadyResponse>, Status> {
        Ok(Response::new(ModelReadyResponse { ready: true }))
    }

    async fn model_metadata(
        &self,
        r: Request<ModelMetadataRequest>,
    ) -> Result<Response<ModelMetadataResponse>, Status> {
        Ok(Response::new(ModelMetadataResponse {
            name: r.into_inner().name,
            versions: vec!["1".to_owned()],
            platform: "mock".to_owned(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }))
    }

    async fn model_infer(
        &self,
        r: Request<ModelInferRequest>,
    ) -> Result<Response<ModelInferResponse>, Status> {
        let request = r.into_inner();
        self.inner
            .seen_models
            .lock()
            .unwrap()
            .push(request.model_name.clone());
        tokio::time::sleep(self.inner.compute).await;
        self.inner.served.fetch_add(1, Ordering::Relaxed);
        Ok(Response::new(ModelInferResponse {
            model_name: request.model_name,
            model_version: "1".to_owned(),
            id: request.id,
            parameters: std::collections::HashMap::new(),
            outputs: Vec::new(),
            raw_output_contents: Vec::new(),
        }))
    }

    type ModelStreamInferStream = tokio_stream::Empty<Result<ModelStreamInferResponse, Status>>;

    async fn model_stream_infer(
        &self,
        _r: Request<tonic::Streaming<ModelInferRequest>>,
    ) -> Result<Response<Self::ModelStreamInferStream>, Status> {
        Err(Status::unimplemented("model_stream_infer"))
    }

    async fn server_metadata(
        &self,
        _r: Request<ServerMetadataRequest>,
    ) -> Result<Response<ServerMetadataResponse>, Status> {
        Err(Status::unimplemented("server_metadata"))
    }

    async fn model_config(
        &self,
        _r: Request<ModelConfigRequest>,
    ) -> Result<Response<ModelConfigResponse>, Status> {
        Err(Status::unimplemented("model_config"))
    }

    async fn model_statistics(
        &self,
        _r: Request<ModelStatisticsRequest>,
    ) -> Result<Response<ModelStatisticsResponse>, Status> {
        Err(Status::unimplemented("model_statistics"))
    }

    async fn repository_index(
        &self,
        _r: Request<RepositoryIndexRequest>,
    ) -> Result<Response<RepositoryIndexResponse>, Status> {
        Err(Status::unimplemented("repository_index"))
    }

    async fn repository_model_load(
        &self,
        _r: Request<RepositoryModelLoadRequest>,
    ) -> Result<Response<RepositoryModelLoadResponse>, Status> {
        Err(Status::unimplemented("repository_model_load"))
    }

    async fn repository_model_unload(
        &self,
        _r: Request<RepositoryModelUnloadRequest>,
    ) -> Result<Response<RepositoryModelUnloadResponse>, Status> {
        Err(Status::unimplemented("repository_model_unload"))
    }

    async fn system_shared_memory_status(
        &self,
        _r: Request<SystemSharedMemoryStatusRequest>,
    ) -> Result<Response<SystemSharedMemoryStatusResponse>, Status> {
        Err(Status::unimplemented("system_shared_memory_status"))
    }

    async fn system_shared_memory_register(
        &self,
        _r: Request<SystemSharedMemoryRegisterRequest>,
    ) -> Result<Response<SystemSharedMemoryRegisterResponse>, Status> {
        Err(Status::unimplemented("system_shared_memory_register"))
    }

    async fn system_shared_memory_unregister(
        &self,
        _r: Request<SystemSharedMemoryUnregisterRequest>,
    ) -> Result<Response<SystemSharedMemoryUnregisterResponse>, Status> {
        Err(Status::unimplemented("system_shared_memory_unregister"))
    }

    async fn cuda_shared_memory_status(
        &self,
        _r: Request<CudaSharedMemoryStatusRequest>,
    ) -> Result<Response<CudaSharedMemoryStatusResponse>, Status> {
        Err(Status::unimplemented("cuda_shared_memory_status"))
    }

    async fn cuda_shared_memory_register(
        &self,
        _r: Request<CudaSharedMemoryRegisterRequest>,
    ) -> Result<Response<CudaSharedMemoryRegisterResponse>, Status> {
        Err(Status::unimplemented("cuda_shared_memory_register"))
    }

    async fn cuda_shared_memory_unregister(
        &self,
        _r: Request<CudaSharedMemoryUnregisterRequest>,
    ) -> Result<Response<CudaSharedMemoryUnregisterResponse>, Status> {
        Err(Status::unimplemented("cuda_shared_memory_unregister"))
    }

    async fn trace_setting(
        &self,
        _r: Request<TraceSettingRequest>,
    ) -> Result<Response<TraceSettingResponse>, Status> {
        Err(Status::unimplemented("trace_setting"))
    }

    async fn log_settings(
        &self,
        _r: Request<LogSettingsRequest>,
    ) -> Result<Response<LogSettingsResponse>, Status> {
        Err(Status::unimplemented("log_settings"))
    }
}
