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

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tonic::{Request, Response, Status};
use vig_protocol_oip::inference::grpc_inference_service_server::{
    GrpcInferenceService, GrpcInferenceServiceServer,
};
#[allow(clippy::wildcard_imports)]
use vig_protocol_oip::inference::*;

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
    /// Shared-Memory-Registrierungen und -Abmeldungen, die das Backend
    /// erreicht haben.
    pub shm_calls: AtomicU64,
    /// Die registrierten Regionen, wie sie der Status zeigt.
    pub shm_regions: std::sync::Mutex<
        std::collections::BTreeMap<String, system_shared_memory_status_response::RegionStatus>,
    >,
    /// Zeichen, die ein generatives Modell je Aufruf erzeugt.
    ///
    /// `0` schaltet die Textausgabe ab; das Backend antwortet dann wie ein
    /// gewoehnliches Vision-Modell mit leerer Nutzlast. Mit einem Wert > 0
    /// verhaelt es sich wie ein Textmodell: es liest den Prompt und haengt
    /// die entsprechende Zahl Zeichen an. Damit laesst sich pruefen, ob die
    /// Zerlegung ueber mehrere Quanten tatsaechlich fortsetzt.
    pub chars_per_call: usize,
    /// Die Prompts, die das Backend je Aufruf gesehen hat.
    ///
    /// Bei einer echten Zerlegung waechst der Prompt von Quantum zu Quantum
    /// um das bisher Erzeugte.
    pub seen_prompts: std::sync::Mutex<Vec<String>>,
    /// Das `max_tokens` je Aufruf, so wie es am Draht ankam.
    ///
    /// Die Obergrenze, die der Governor durchsetzt. Sie muss auch dann
    /// dastehen, wenn gar nicht zerlegt wird — ungeteilt heisst nicht
    /// unbegrenzt (NV-16, Spec 8.3).
    pub seen_max_tokens: std::sync::Mutex<Vec<Option<u32>>>,
    /// Wie viele **Teilantworten mit Nutzlast** der Stream-Endpunkt sendet.
    ///
    /// Ein decoupled Modell darf mehrere senden. Mit `1` verhaelt sich das
    /// Backend wie ein gewoehnliches unaeres Modell, das nur ueber den
    /// Stream-Endpunkt erreichbar ist.
    pub stream_payloads: usize,
    /// Ob nach den Nutzlasten eine leere Abschlussmarkierung folgt.
    pub stream_final_marker: bool,
    /// Laesst den Aufruf haengen, statt zu antworten.
    ///
    /// Ein Backend, das erreichbar ist und nie antwortet, ist der Fall, gegen
    /// den das Inferenztimeout existiert. Er laesst sich nicht durch einen
    /// Verbindungsfehler nachstellen: dort kommt eine Antwort, nur eine
    /// schlechte.
    pub hang: bool,
    /// Wie viele Inferenzen die Statistik als abgeschlossen meldet.
    ///
    /// Der Nachweis, mit dem ein gehaltener Slotkredit endet. Getrennt von
    /// `served`, damit ein Test „das Backend rechnet noch" darstellen kann:
    /// angenommen ja, abgeschlossen nein.
    pub completed: AtomicU64,
    /// Zeitstempel der letzten Anfrage, Millisekunden seit Epoch.
    pub last_inference_ms: AtomicU64,
    /// Wenn gesetzt, scheitert jeder Inferenzaufruf mit diesem Code.
    ///
    /// Damit laesst sich ein Abbruch **waehrend** des Aufrufs nachstellen —
    /// im Unterschied zu einem abgelehnten Verbindungsaufbau, der etwas ganz
    /// anderes ueber die Ausfuehrung aussagt.
    pub fail_with: std::sync::Mutex<Option<tonic::Code>>,
    /// Wenn nicht leer, meldet die Statistik je Eintrag eine Modellversion
    /// mit diesem Abschlusszaehler statt eines einzigen Eintrags.
    ///
    /// Triton liefert ohne Versionsfilter eine Statistik je geladener
    /// Version (Review R02).
    pub stat_versions: std::sync::Mutex<Vec<(String, u64)>>,
}

impl MockBackend {
    #[must_use]
    pub fn new(compute: Duration) -> Self {
        Self {
            compute,
            served: AtomicU64::new(0),
            seen_models: std::sync::Mutex::new(Vec::new()),
            raw_bytes_seen: AtomicU64::new(0),
            shm_calls: AtomicU64::new(0),
            shm_regions: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            chars_per_call: 0,
            seen_prompts: std::sync::Mutex::new(Vec::new()),
            seen_max_tokens: std::sync::Mutex::new(Vec::new()),
            stream_payloads: 1,
            stream_final_marker: false,
            hang: false,
            completed: AtomicU64::new(0),
            last_inference_ms: AtomicU64::new(0),
            fail_with: std::sync::Mutex::new(None),
            stat_versions: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Ein Backend, das erreichbar ist und nie antwortet.
    #[must_use]
    pub fn hanging() -> Self {
        Self {
            hang: true,
            ..Self::new(Duration::from_millis(0))
        }
    }

    /// Ein decoupled Backend mit `payloads` Teilantworten.
    #[must_use]
    pub fn streaming(compute: Duration, payloads: usize, final_marker: bool) -> Self {
        Self {
            stream_payloads: payloads,
            stream_final_marker: final_marker,
            ..Self::new(compute)
        }
    }

    /// Ein Backend, das sich wie ein generatives Textmodell verhaelt.
    #[must_use]
    pub fn generative(compute: Duration, chars_per_call: usize) -> Self {
        Self {
            chars_per_call,
            ..Self::new(compute)
        }
    }

    /// Traegt die Rohdatenbytes eines angekommenen Requests ein.
    ///
    /// Bis hierher wurde `raw_bytes_seen` nirgends erhoeht: der Zaehler blieb
    /// auf jedem Pfad null, und die Pruefung „auf dem Shm-Pfad reisen keine
    /// Rohdaten" bestand, gleichgueltig was der Governor tat. Die Gegenprobe
    /// in `the_shm_path_never_carries_the_payload` haelt das jetzt fest.
    fn count_raw_bytes(&self, request: &ModelInferRequest) {
        let bytes: usize = request.raw_input_contents.iter().map(Vec::len).sum();
        self.raw_bytes_seen
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }
}

/// Kodiert einen String so, wie OIP Rohdaten fuer `BYTES` erwartet.
fn length_prefixed(value: &str) -> Vec<u8> {
    vig_protocol_oip::bytes::encode_bytes_element(value.as_bytes()).unwrap()
}

/// Liest `max_tokens` aus den Samplingparametern.
///
/// Bewusst eine winzige Textsuche statt eines JSON-Parsers: der Test soll
/// belegen, was der Governor durchsetzt, nicht eine Abhaengigkeit mehr in die
/// Testhilfe holen.
fn read_max_tokens(sampling: &str) -> Option<u32> {
    let rest = sampling.split("\"max_tokens\"").nth(1)?;
    let digits: String = rest
        .trim_start_matches([':', ' '])
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Liest einen laengenpraefigierten String.
fn read_length_prefixed(bytes: &[u8]) -> Option<String> {
    let element = vig_protocol_oip::bytes::decode_single_bytes_element(bytes)?;
    String::from_utf8(element.to_vec()).ok()
}

/// Die Parameter einer Antwort, die als letzte markiert ist — oder nicht.
fn final_parameters(last: bool) -> std::collections::HashMap<String, InferParameter> {
    let mut out = std::collections::HashMap::new();
    if last {
        out.insert(
            vig_backend_triton::FINAL_RESPONSE_PARAM.to_owned(),
            InferParameter {
                parameter_choice: Some(infer_parameter::ParameterChoice::BoolParam(true)),
            },
        );
    }
    out
}

/// Startet das Mock-Backend auf einem freien Port.
pub async fn start(backend: Arc<MockBackend>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = Service { inner: backend };
    tokio::spawn(async move {
        // TCP_NODELAY wie `vig serve` und Triton. `serve_with_incoming`
        // uebergeht die Einstellung des Builders; ohne sie wartet eine
        // Antwort gelegentlich auf das verzoegerte ACK des Clients, rund 40 ms
        // (docs/benchmark/arm-serve.md).
        let stream = tonic::transport::server::TcpIncoming::from(listener).with_nodelay(Some(true));
        // Dieselben Transportgrenzen wie im Gateway. Ein Backend mit engeren
        // Grenzen wuerde den Vergleich zugunsten des Proxys verfaelschen.
        let _ = tonic::transport::Server::builder()
            .initial_stream_window_size(vig_backend_triton::STREAM_WINDOW_BYTES)
            .initial_connection_window_size(vig_backend_triton::CONNECTION_WINDOW_BYTES)
            .add_service(
                GrpcInferenceServiceServer::new(service)
                    .max_decoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES),
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
        self.inner.count_raw_bytes(&request);
        self.inner
            .seen_models
            .lock()
            .unwrap()
            .push(request.model_name.clone());
        if let Some(code) = *self.inner.fail_with.lock().unwrap() {
            return Err(Status::new(code, "mock bricht ab"));
        }
        if self.inner.hang {
            self.inner.served.fetch_add(1, Ordering::Relaxed);
            // Laenger als jedes Testtimeout, aber endlich: ein Test darf nicht
            // von einem aufgegebenen Task abhaengen.
            tokio::time::sleep(Duration::from_secs(300)).await;
            return Err(Status::deadline_exceeded("mock haengt"));
        }
        tokio::time::sleep(self.inner.compute).await;
        self.inner.served.fetch_add(1, Ordering::Relaxed);

        let mut outputs = Vec::new();
        let mut raw_output_contents = Vec::new();
        if self.inner.chars_per_call > 0 {
            let prompt = request
                .raw_input_contents
                .first()
                .and_then(|raw| read_length_prefixed(raw))
                .unwrap_or_default();
            self.inner.seen_prompts.lock().unwrap().push(prompt);
            let max = request
                .raw_input_contents
                .get(1)
                .and_then(|raw| read_length_prefixed(raw))
                .and_then(|sampling| read_max_tokens(&sampling));
            self.inner.seen_max_tokens.lock().unwrap().push(max);
            outputs.push(model_infer_response::InferOutputTensor {
                name: "text_output".to_owned(),
                datatype: "BYTES".to_owned(),
                shape: vec![1],
                parameters: std::collections::HashMap::new(),
                contents: None,
            });
            raw_output_contents.push(length_prefixed(&"x".repeat(self.inner.chars_per_call)));
        }

        Ok(Response::new(ModelInferResponse {
            model_name: request.model_name,
            model_version: "1".to_owned(),
            id: request.id,
            parameters: std::collections::HashMap::new(),
            outputs,
            raw_output_contents,
        }))
    }

    type ModelStreamInferStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<ModelStreamInferResponse, Status>> + Send>,
    >;

    async fn model_stream_infer(
        &self,
        r: Request<tonic::Streaming<ModelInferRequest>>,
    ) -> Result<Response<Self::ModelStreamInferStream>, Status> {
        use tokio_stream::StreamExt as _;

        let mut inbound = r.into_inner();
        let request = inbound
            .next()
            .await
            .transpose()?
            .ok_or_else(|| Status::invalid_argument("kein Request im Stream"))?;
        self.inner.count_raw_bytes(&request);
        tokio::time::sleep(self.inner.compute).await;
        self.inner.served.fetch_add(1, Ordering::Relaxed);

        let mut messages = Vec::new();
        for index in 0..self.inner.stream_payloads {
            let last = !self.inner.stream_final_marker
                && index.saturating_add(1) == self.inner.stream_payloads;
            messages.push(Ok(ModelStreamInferResponse {
                error_message: String::new(),
                infer_response: Some(ModelInferResponse {
                    model_name: request.model_name.clone(),
                    model_version: "1".to_owned(),
                    id: request.id.clone(),
                    parameters: final_parameters(last),
                    outputs: Vec::new(),
                    raw_output_contents: vec![length_prefixed(&format!("teil{index}"))],
                }),
            }));
        }
        if self.inner.stream_final_marker {
            messages.push(Ok(ModelStreamInferResponse {
                error_message: String::new(),
                infer_response: Some(ModelInferResponse {
                    model_name: request.model_name,
                    model_version: "1".to_owned(),
                    id: request.id,
                    parameters: final_parameters(true),
                    outputs: Vec::new(),
                    raw_output_contents: Vec::new(),
                }),
            }));
        }
        Ok(Response::new(Box::pin(tokio_stream::iter(messages))))
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
        r: Request<ModelStatisticsRequest>,
    ) -> Result<Response<ModelStatisticsResponse>, Status> {
        let name = r.into_inner().name;
        let last_inference = self.inner.last_inference_ms.load(Ordering::Relaxed);
        let entry = |version: String, count: u64| ModelStatistics {
            name: name.clone(),
            version,
            last_inference,
            inference_count: 0,
            execution_count: 0,
            inference_stats: Some(InferStatistics {
                success: Some(StatisticDuration { count, ns: 0 }),
                ..Default::default()
            }),
            batch_stats: Vec::new(),
            memory_usage: Vec::new(),
            response_stats: std::collections::HashMap::new(),
        };
        let versions = self.inner.stat_versions.lock().unwrap().clone();
        let model_stats = if versions.is_empty() {
            vec![entry(
                "1".to_owned(),
                self.inner.completed.load(Ordering::Relaxed),
            )]
        } else {
            versions
                .into_iter()
                .map(|(version, count)| entry(version, count))
                .collect()
        };
        Ok(Response::new(ModelStatisticsResponse { model_stats }))
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

    // Der Status zeigt, was registriert ist — wie Triton alle Regionen, ohne
    // nach Aufrufern zu unterscheiden. Die Einschraenkung ist Sache des
    // Governors.
    async fn system_shared_memory_status(
        &self,
        r: Request<SystemSharedMemoryStatusRequest>,
    ) -> Result<Response<SystemSharedMemoryStatusResponse>, Status> {
        let name = r.into_inner().name;
        let regions = self.inner.shm_regions.lock().unwrap();
        let listed: std::collections::HashMap<_, _> = regions
            .iter()
            .filter(|(n, _)| name.is_empty() || **n == name)
            .map(|(n, s)| (n.clone(), s.clone()))
            .collect();
        if !name.is_empty() && listed.is_empty() {
            return Err(Status::not_found(format!("keine Region {name}")));
        }
        Ok(Response::new(SystemSharedMemoryStatusResponse {
            regions: listed,
        }))
    }

    // Registrierung und Abmeldung werden angenommen und gezaehlt: die
    // Sicherheitspruefungen des Governors liegen **davor**, und ein Test muss
    // sehen koennen, ob eine Anfrage das Backend erreicht hat.
    async fn system_shared_memory_register(
        &self,
        r: Request<SystemSharedMemoryRegisterRequest>,
    ) -> Result<Response<SystemSharedMemoryRegisterResponse>, Status> {
        self.inner.shm_calls.fetch_add(1, Ordering::Relaxed);
        let r = r.into_inner();
        self.inner.shm_regions.lock().unwrap().insert(
            r.name.clone(),
            system_shared_memory_status_response::RegionStatus {
                name: r.name,
                key: r.key,
                offset: r.offset,
                byte_size: r.byte_size,
            },
        );
        Ok(Response::new(SystemSharedMemoryRegisterResponse::default()))
    }

    async fn system_shared_memory_unregister(
        &self,
        r: Request<SystemSharedMemoryUnregisterRequest>,
    ) -> Result<Response<SystemSharedMemoryUnregisterResponse>, Status> {
        self.inner.shm_calls.fetch_add(1, Ordering::Relaxed);
        let name = r.into_inner().name;
        let mut regions = self.inner.shm_regions.lock().unwrap();
        if name.is_empty() {
            regions.clear();
        } else {
            regions.remove(&name);
        }
        Ok(Response::new(
            SystemSharedMemoryUnregisterResponse::default(),
        ))
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
