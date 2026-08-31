//! Der gRPC-Dienst des Benchmark-Backends.
//!
//! Implementiert die Endpunkte, die der Vergleich braucht. Alles Uebrige
//! antwortet mit `Unimplemented` — ein Backend, das mehr vortaeuscht, als es
//! kann, wuerde Fehler verstecken statt sie zu zeigen.

use crate::backend::Backend;
use onetimer_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;
#[allow(clippy::wildcard_imports)]
use onetimer_protocol_oip::inference::*;
use std::collections::HashMap;
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Der Dienst.
#[derive(Debug)]
pub struct BackendService {
    backend: Arc<Backend>,
}

impl BackendService {
    /// Baut den Dienst.
    #[must_use]
    pub const fn new(backend: Arc<Backend>) -> Self {
        Self { backend }
    }
}

#[tonic::async_trait]
impl GrpcInferenceService for BackendService {
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
            platform: "bench".to_owned(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }))
    }

    async fn model_infer(
        &self,
        r: Request<ModelInferRequest>,
    ) -> Result<Response<ModelInferResponse>, Status> {
        let request = r.into_inner();
        self.backend.execute(&request.model_name, &request.id).await;
        Ok(Response::new(ModelInferResponse {
            model_name: request.model_name,
            model_version: "1".to_owned(),
            id: request.id,
            parameters: HashMap::new(),
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
