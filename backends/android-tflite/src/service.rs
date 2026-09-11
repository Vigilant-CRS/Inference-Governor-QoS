//! Die OIP-Abbildung.
//!
//! Was der Governor von einem Backend braucht, steht in
//! `vig-backend-triton` und `vig-gateway`: Lebendigkeit und Bereitschaft,
//! Modellmetadaten, die Inferenz selbst und die Statistik, deren
//! Abschlusszaehler einen Slotkredit beendet (NV-00). Genau das ist hier
//! abgebildet, dazu Konfiguration, Serverdaten und Repositoryindex fuer
//! Clients, die sie abfragen. Alles Uebrige antwortet `Unimplemented` — ein
//! Backend, das mehr vortaeuscht, als es kann, versteckt Fehler.
//!
//! Nur der Kopierpfad: Android hat kein `/dev/shm`. Ein Request mit
//! Shared-Memory-Parametern wird abgelehnt, nicht still falsch gelesen.

use crate::models::ModelHandle;
use crate::tflite::ModelInfo;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tonic::{Request, Response, Status};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;
#[allow(clippy::wildcard_imports)]
use vig_protocol_oip::inference::*;

const SHM_PARAMETERS: [&str; 3] = [
    "shared_memory_region",
    "shared_memory_byte_size",
    "shared_memory_offset",
];

/// Der Dienst.
#[derive(Debug)]
pub(crate) struct TfliteService {
    models: Arc<HashMap<String, ModelHandle>>,
    platform: &'static str,
}

impl TfliteService {
    #[allow(clippy::implicit_hasher)]
    pub(crate) fn new(models: HashMap<String, ModelHandle>, platform: &'static str) -> Self {
        Self {
            models: Arc::new(models),
            platform,
        }
    }

    fn model(&self, name: &str) -> Result<&ModelHandle, Status> {
        self.models
            .get(name)
            .ok_or_else(|| Status::not_found(format!("unbekanntes Modell: {name}")))
    }

    fn sorted_names(&self) -> Vec<&String> {
        let mut names: Vec<&String> = self.models.keys().collect();
        names.sort();
        names
    }
}

fn uses_shared_memory(parameters: &HashMap<String, InferParameter>) -> bool {
    SHM_PARAMETERS
        .iter()
        .any(|key| parameters.contains_key(*key))
}

/// Ordnet die Rohpuffer eines Requests der Interpreterreihenfolge zu.
///
/// Zuordnung ueber den Namen, wie OIP es vorsieht; Datentyp und Bytezahl
/// muessen stimmen. Die Puffer werden aus dem Request genommen, nicht kopiert.
///
/// # Errors
///
/// `InvalidArgument` bei Shared Memory, typisierten `contents`, fehlenden,
/// doppelten oder unbekannten Eingaben, falschem Datentyp oder falscher
/// Groesse.
pub(crate) fn prepare_inputs(
    info: &ModelInfo,
    request: &mut ModelInferRequest,
) -> Result<Vec<Vec<u8>>, Status> {
    if request
        .inputs
        .iter()
        .any(|t| uses_shared_memory(&t.parameters))
        || request
            .outputs
            .iter()
            .any(|t| uses_shared_memory(&t.parameters))
    {
        return Err(Status::invalid_argument(
            "dieses Backend kennt kein Shared Memory; die Nutzlast gehoert in raw_input_contents",
        ));
    }
    if request.raw_input_contents.len() != request.inputs.len() {
        return Err(Status::invalid_argument(format!(
            "{} Eingaben, aber {} Rohpuffer; typisierte contents werden nicht unterstuetzt",
            request.inputs.len(),
            request.raw_input_contents.len()
        )));
    }
    let mut raw: Vec<Option<Vec<u8>>> = std::mem::take(&mut request.raw_input_contents)
        .into_iter()
        .map(Some)
        .collect();

    let mut ordered = Vec::with_capacity(info.inputs.len());
    for expected in &info.inputs {
        let position = request
            .inputs
            .iter()
            .position(|t| t.name == expected.name)
            .ok_or_else(|| Status::invalid_argument(format!("Eingabe {} fehlt", expected.name)))?;
        let given = request
            .inputs
            .get(position)
            .ok_or_else(|| Status::internal("Eingabeposition ausserhalb des Requests"))?;
        if given.datatype != expected.datatype {
            return Err(Status::invalid_argument(format!(
                "{}: Datentyp {}, erwartet {}",
                expected.name, given.datatype, expected.datatype
            )));
        }
        let bytes = raw
            .get_mut(position)
            .and_then(Option::take)
            .ok_or_else(|| Status::invalid_argument(format!("{}: doppelt", expected.name)))?;
        if bytes.len() != expected.byte_size {
            return Err(Status::invalid_argument(format!(
                "{}: {} Bytes, erwartet {}",
                expected.name,
                bytes.len(),
                expected.byte_size
            )));
        }
        ordered.push(bytes);
    }
    if raw.iter().any(Option::is_some) {
        return Err(Status::invalid_argument("unbekannte Eingabe im Request"));
    }
    Ok(ordered)
}

/// Prueft die angeforderten Ausgaben, bevor gerechnet wird.
///
/// # Errors
///
/// `InvalidArgument` fuer einen Namen, den das Modell nicht hat.
pub(crate) fn validate_outputs(
    info: &ModelInfo,
    requested: &[model_infer_request::InferRequestedOutputTensor],
) -> Result<(), Status> {
    for wanted in requested {
        if !info.outputs.iter().any(|t| t.name == wanted.name) {
            return Err(Status::invalid_argument(format!(
                "unbekannte Ausgabe: {}",
                wanted.name
            )));
        }
    }
    Ok(())
}

/// Baut die Antwort. Keine angeforderten Ausgaben heisst: alle.
pub(crate) fn response(
    info: &ModelInfo,
    model_name: String,
    id: String,
    requested: &[model_infer_request::InferRequestedOutputTensor],
    outputs: Vec<Vec<u8>>,
) -> ModelInferResponse {
    let mut tensors = Vec::new();
    let mut raw = Vec::new();
    for (tensor, bytes) in info.outputs.iter().zip(outputs) {
        if requested.is_empty() || requested.iter().any(|r| r.name == tensor.name) {
            tensors.push(model_infer_response::InferOutputTensor {
                name: tensor.name.clone(),
                datatype: tensor.datatype.to_owned(),
                shape: tensor.shape.clone(),
                parameters: HashMap::new(),
                contents: None,
            });
            raw.push(bytes);
        }
    }
    ModelInferResponse {
        model_name,
        model_version: "1".to_owned(),
        id,
        parameters: HashMap::new(),
        outputs: tensors,
        raw_output_contents: raw,
    }
}

fn statistics(name: &str, handle: &ModelHandle) -> ModelStatistics {
    let s = &handle.stats;
    // Erst die Zaehler (Acquire), dann die Zeiten: `record` erhoeht den
    // Zaehler zuletzt.
    let success = s.success.load(Ordering::Acquire);
    let fail = s.fail.load(Ordering::Acquire);
    let duration = |count: u64, ns: u64| Some(StatisticDuration { count, ns });
    ModelStatistics {
        name: name.to_owned(),
        version: "1".to_owned(),
        last_inference: s.last_inference_ms.load(Ordering::Relaxed),
        inference_count: success,
        execution_count: success,
        inference_stats: Some(InferStatistics {
            success: duration(success, s.success_ns.load(Ordering::Relaxed)),
            fail: duration(fail, s.fail_ns.load(Ordering::Relaxed)),
            queue: duration(success, s.queue_ns.load(Ordering::Relaxed)),
            compute_infer: duration(success, s.compute_ns.load(Ordering::Relaxed)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn tensor_metadata(
    info: &[crate::tflite::TensorInfo],
) -> Vec<model_metadata_response::TensorMetadata> {
    info.iter()
        .map(|t| model_metadata_response::TensorMetadata {
            name: t.name.clone(),
            datatype: t.datatype.to_owned(),
            shape: t.shape.clone(),
        })
        .collect()
}

#[tonic::async_trait]
impl GrpcInferenceService for TfliteService {
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
        // Der Server nimmt erst Verbindungen an, wenn alle Modelle geladen
        // sind (main.rs); ab da ist er bereit.
        Ok(Response::new(ServerReadyResponse { ready: true }))
    }

    async fn model_ready(
        &self,
        r: Request<ModelReadyRequest>,
    ) -> Result<Response<ModelReadyResponse>, Status> {
        let ready = self.models.contains_key(&r.into_inner().name);
        Ok(Response::new(ModelReadyResponse { ready }))
    }

    async fn server_metadata(
        &self,
        _r: Request<ServerMetadataRequest>,
    ) -> Result<Response<ServerMetadataResponse>, Status> {
        Ok(Response::new(ServerMetadataResponse {
            name: "vig-tflite".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            extensions: vec!["statistics".to_owned()],
        }))
    }

    async fn model_metadata(
        &self,
        r: Request<ModelMetadataRequest>,
    ) -> Result<Response<ModelMetadataResponse>, Status> {
        let name = r.into_inner().name;
        let handle = self.model(&name)?;
        Ok(Response::new(ModelMetadataResponse {
            name,
            versions: vec!["1".to_owned()],
            platform: self.platform.to_owned(),
            inputs: tensor_metadata(&handle.info.inputs),
            outputs: tensor_metadata(&handle.info.outputs),
        }))
    }

    async fn model_infer(
        &self,
        r: Request<ModelInferRequest>,
    ) -> Result<Response<ModelInferResponse>, Status> {
        let mut request = r.into_inner();
        let handle = self.model(&request.model_name)?;
        let inputs = prepare_inputs(&handle.info, &mut request)?;
        validate_outputs(&handle.info, &request.outputs)?;
        let done = handle.infer(inputs).await.map_err(Status::internal)?;
        Ok(Response::new(response(
            &handle.info,
            request.model_name,
            request.id,
            &request.outputs,
            done.outputs,
        )))
    }

    type ModelStreamInferStream = tokio_stream::Empty<Result<ModelStreamInferResponse, Status>>;

    async fn model_stream_infer(
        &self,
        _r: Request<tonic::Streaming<ModelInferRequest>>,
    ) -> Result<Response<Self::ModelStreamInferStream>, Status> {
        Err(Status::unimplemented(
            "model_stream_infer: kein entkoppeltes Modell in diesem Backend",
        ))
    }

    async fn model_config(
        &self,
        r: Request<ModelConfigRequest>,
    ) -> Result<Response<ModelConfigResponse>, Status> {
        let name = r.into_inner().name;
        self.model(&name)?;
        Ok(Response::new(ModelConfigResponse {
            config: Some(ModelConfig {
                name,
                platform: self.platform.to_owned(),
                backend: "tflite".to_owned(),
                max_batch_size: 0,
                ..Default::default()
            }),
        }))
    }

    async fn model_statistics(
        &self,
        r: Request<ModelStatisticsRequest>,
    ) -> Result<Response<ModelStatisticsResponse>, Status> {
        let name = r.into_inner().name;
        let model_stats = if name.is_empty() {
            self.sorted_names()
                .into_iter()
                .filter_map(|n| self.models.get(n).map(|h| statistics(n, h)))
                .collect()
        } else {
            vec![statistics(&name, self.model(&name)?)]
        };
        Ok(Response::new(ModelStatisticsResponse { model_stats }))
    }

    async fn repository_index(
        &self,
        _r: Request<RepositoryIndexRequest>,
    ) -> Result<Response<RepositoryIndexResponse>, Status> {
        Ok(Response::new(RepositoryIndexResponse {
            models: self
                .sorted_names()
                .into_iter()
                .map(|name| repository_index_response::ModelIndex {
                    name: name.clone(),
                    version: "1".to_owned(),
                    state: "READY".to_owned(),
                    reason: String::new(),
                })
                .collect(),
        }))
    }

    async fn repository_model_load(
        &self,
        _r: Request<RepositoryModelLoadRequest>,
    ) -> Result<Response<RepositoryModelLoadResponse>, Status> {
        Err(Status::unimplemented(
            "repository_model_load: Modelle stehen fest",
        ))
    }

    async fn repository_model_unload(
        &self,
        _r: Request<RepositoryModelUnloadRequest>,
    ) -> Result<Response<RepositoryModelUnloadResponse>, Status> {
        Err(Status::unimplemented(
            "repository_model_unload: Modelle stehen fest",
        ))
    }

    async fn system_shared_memory_status(
        &self,
        _r: Request<SystemSharedMemoryStatusRequest>,
    ) -> Result<Response<SystemSharedMemoryStatusResponse>, Status> {
        Err(Status::unimplemented("kein Shared Memory"))
    }

    async fn system_shared_memory_register(
        &self,
        _r: Request<SystemSharedMemoryRegisterRequest>,
    ) -> Result<Response<SystemSharedMemoryRegisterResponse>, Status> {
        Err(Status::unimplemented("kein Shared Memory"))
    }

    async fn system_shared_memory_unregister(
        &self,
        _r: Request<SystemSharedMemoryUnregisterRequest>,
    ) -> Result<Response<SystemSharedMemoryUnregisterResponse>, Status> {
        Err(Status::unimplemented("kein Shared Memory"))
    }

    async fn cuda_shared_memory_status(
        &self,
        _r: Request<CudaSharedMemoryStatusRequest>,
    ) -> Result<Response<CudaSharedMemoryStatusResponse>, Status> {
        Err(Status::unimplemented("kein CUDA"))
    }

    async fn cuda_shared_memory_register(
        &self,
        _r: Request<CudaSharedMemoryRegisterRequest>,
    ) -> Result<Response<CudaSharedMemoryRegisterResponse>, Status> {
        Err(Status::unimplemented("kein CUDA"))
    }

    async fn cuda_shared_memory_unregister(
        &self,
        _r: Request<CudaSharedMemoryUnregisterRequest>,
    ) -> Result<Response<CudaSharedMemoryUnregisterResponse>, Status> {
        Err(Status::unimplemented("kein CUDA"))
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use crate::models::Runner;
    use crate::tflite::TensorInfo;
    use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;

    fn tensor(name: &str, datatype: &'static str, shape: &[i64], bytes: usize) -> TensorInfo {
        TensorInfo {
            name: name.to_owned(),
            datatype,
            shape: shape.to_vec(),
            byte_size: bytes,
        }
    }

    /// Ein Detektor wie EfficientDet-Lite0: eine UINT8-Eingabe, zwei Ausgaben.
    fn detector_info() -> ModelInfo {
        ModelInfo {
            inputs: vec![tensor("images", "UINT8", &[1, 2, 2, 3], 12)],
            outputs: vec![
                tensor("boxes", "FP32", &[1, 1, 4], 16),
                tensor("scores", "FP32", &[1, 1], 4),
            ],
        }
    }

    fn service(fail: bool) -> TfliteService {
        let info = detector_info();
        let handle = ModelHandle::spawn("detector", move || {
            let runner: Runner = Box::new(move |inputs: &[Vec<u8>]| {
                if fail {
                    return Err("Invoke gescheitert".to_owned());
                }
                // Die Summe der Eingabe in jedes Ausgabebyte: so sieht der
                // Test, dass die richtigen Bytes angekommen sind.
                let sum = inputs[0].iter().fold(0_u8, |a, b| a.wrapping_add(*b));
                Ok(vec![vec![sum; 16], vec![sum; 4]])
            });
            Ok((info, runner))
        })
        .unwrap();
        TfliteService::new(
            HashMap::from([("detector".to_owned(), handle)]),
            "tflite_gpu",
        )
    }

    fn request(bytes: Vec<u8>) -> ModelInferRequest {
        ModelInferRequest {
            model_name: "detector".to_owned(),
            id: "r1".to_owned(),
            inputs: vec![model_infer_request::InferInputTensor {
                name: "images".to_owned(),
                datatype: "UINT8".to_owned(),
                shape: vec![1, 2, 2, 3],
                ..Default::default()
            }],
            raw_input_contents: vec![bytes],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn metadata_names_every_tensor_as_the_interpreter_sees_it() {
        let meta = service(false)
            .model_metadata(Request::new(ModelMetadataRequest {
                name: "detector".to_owned(),
                version: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(meta.platform, "tflite_gpu");
        assert_eq!(meta.inputs[0].name, "images");
        assert_eq!(meta.inputs[0].datatype, "UINT8");
        assert_eq!(meta.inputs[0].shape, vec![1, 2, 2, 3]);
        assert_eq!(meta.outputs.len(), 2);
    }

    #[tokio::test]
    async fn an_inference_returns_every_output_and_counts_as_evidence() {
        let svc = service(false);
        let response = svc
            .model_infer(Request::new(request(vec![1; 12])))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.id, "r1");
        assert_eq!(response.outputs.len(), 2);
        assert_eq!(response.raw_output_contents[0], vec![12_u8; 16]);

        let stats = svc
            .model_statistics(Request::new(ModelStatisticsRequest {
                name: "detector".to_owned(),
                version: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();
        let s = &stats.model_stats[0];
        assert_eq!(
            s.inference_stats
                .as_ref()
                .unwrap()
                .success
                .as_ref()
                .unwrap()
                .count,
            1
        );
        assert!(s.last_inference > 0);
    }

    #[tokio::test]
    async fn requested_outputs_select_a_subset() {
        let mut req = request(vec![0; 12]);
        req.outputs = vec![model_infer_request::InferRequestedOutputTensor {
            name: "scores".to_owned(),
            parameters: HashMap::new(),
        }];
        let response = service(false)
            .model_infer(Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.outputs.len(), 1);
        assert_eq!(response.outputs[0].name, "scores");
        assert_eq!(response.raw_output_contents[0].len(), 4);
    }

    #[tokio::test]
    async fn shared_memory_is_refused_not_misread() {
        let mut req = request(Vec::new());
        req.raw_input_contents.clear();
        req.inputs[0].parameters.insert(
            "shared_memory_region".to_owned(),
            InferParameter {
                parameter_choice: Some(infer_parameter::ParameterChoice::StringParam(
                    "vig_detector".to_owned(),
                )),
            },
        );
        let error = service(false)
            .model_infer(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("Shared Memory"));
    }

    #[tokio::test]
    async fn a_wrong_size_or_type_is_refused_before_anything_runs() {
        let svc = service(false);
        let error = svc
            .model_infer(Request::new(request(vec![0; 11])))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);

        let mut req = request(vec![0; 12]);
        req.inputs[0].datatype = "FP32".to_owned();
        let error = svc.model_infer(Request::new(req)).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);

        let mut req = request(vec![0; 12]);
        req.outputs = vec![model_infer_request::InferRequestedOutputTensor {
            name: "gibt es nicht".to_owned(),
            parameters: HashMap::new(),
        }];
        let error = svc.model_infer(Request::new(req)).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);

        // Nichts davon hat gerechnet, also auch nichts gezaehlt.
        let s = statistics("detector", svc.model("detector").unwrap());
        let inference = s.inference_stats.unwrap();
        assert_eq!(inference.success.unwrap().count, 0);
        assert_eq!(inference.fail.unwrap().count, 0);
    }

    #[tokio::test]
    async fn an_unknown_model_is_not_found() {
        let svc = service(false);
        let mut req = request(vec![0; 12]);
        req.model_name = "pose".to_owned();
        assert_eq!(
            svc.model_infer(Request::new(req)).await.unwrap_err().code(),
            tonic::Code::NotFound
        );
        let ready = svc
            .model_ready(Request::new(ModelReadyRequest {
                name: "pose".to_owned(),
                version: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!ready.ready);
    }

    #[tokio::test]
    async fn a_failed_invoke_is_internal_and_counted_as_failed() {
        let svc = service(true);
        let error = svc
            .model_infer(Request::new(request(vec![0; 12])))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::Internal);
        let s = statistics("detector", svc.model("detector").unwrap());
        assert_eq!(s.inference_stats.unwrap().fail.unwrap().count, 1);
    }

    #[tokio::test]
    async fn statistics_without_a_name_cover_every_model() {
        let stats = service(false)
            .model_statistics(Request::new(ModelStatisticsRequest::default()))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(stats.model_stats.len(), 1);
        assert_eq!(stats.model_stats[0].name, "detector");
    }

    /// Der Weg, den der Governor geht: sein eigener Client gegen dieses
    /// Backend ueber echtes gRPC. Bereitschaft, Metadaten, Inferenz und der
    /// Abschlussnachweis, mit dem ein Slotkredit endet.
    #[tokio::test]
    async fn the_governors_own_client_sees_a_complete_backend() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let incoming =
                tonic::transport::server::TcpIncoming::from(listener).with_nodelay(Some(true));
            let _ = tonic::transport::Server::builder()
                .add_service(GrpcInferenceServiceServer::new(service(false)))
                .serve_with_incoming(incoming)
                .await;
        });

        let client = vig_backend_triton::TritonClient::new(address.to_string());
        let health = client.health().await.unwrap();
        assert!(health.live && health.ready);
        assert!(client.model_ready("detector").await.unwrap());
        let meta = client.model_metadata("detector").await.unwrap();
        assert_eq!(meta.inputs[0].datatype, "UINT8");

        let before = client.completion_evidence("detector").await.unwrap();
        let response = client.infer(request(vec![2; 12])).await.unwrap();
        assert_eq!(response.raw_output_contents[1], vec![24_u8; 4]);
        let after = client.completion_evidence("detector").await.unwrap();
        assert_eq!(after.completed, before.completed + 1, "der Nachweis steigt");
        assert!(after.last_inference_ms >= before.last_inference_ms);
    }
}
