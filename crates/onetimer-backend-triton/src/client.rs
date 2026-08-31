//! Der gRPC-Client zum Triton-Server.

use crate::error::BackendError;
use onetimer_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient;
use onetimer_protocol_oip::inference::{
    ModelInferRequest, ModelInferResponse, ModelMetadataRequest, ModelMetadataResponse,
    ModelReadyRequest, ServerLiveRequest, ServerReadyRequest,
};
use std::time::Duration as StdDuration;
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};

/// Timeout fuer Verbindungsaufbau und Gesundheitsabfragen.
///
/// Bewusst kurz: `onetimer doctor` und die Bereitschaftspruefung sollen schnell
/// eine Antwort geben, statt in einem langen TCP-Timeout zu haengen.
const CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(5);

/// Standardobergrenze fuer die Groesse einer einzelnen gRPC-Nachricht.
///
/// tonic setzt hier 4 MiB. Ein 1920x1080x3-uint8-Frame sind 6,2 MB — der
/// Default lehnt also einen gewoehnlichen Kamerarequest ab. Die Grenze bleibt
/// trotzdem eine Grenze: Spec 8.3 verbietet unbeschraenkte Allokationen aus
/// fremd kontrollierten Groessen, und ein Wert von „unbegrenzt" waere genau
/// das.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// HTTP/2-Fenster fuer einen einzelnen Stream.
///
/// Der Default von 64 KiB zwingt bei Tensornutzlasten zu einer Kette von
/// WINDOW_UPDATE-Runden und kostet damit ein Vielfaches der reinen
/// Uebertragungszeit. Bei einer Messung faellt das dem Proxy doppelt zur Last,
/// weil er zwei Verbindungen bedient — das waere aber ein Artefakt der
/// Voreinstellung und keine Eigenschaft des Governors.
pub const STREAM_WINDOW_BYTES: u32 = 4 * 1024 * 1024;

/// HTTP/2-Fenster fuer die gesamte Verbindung.
pub const CONNECTION_WINDOW_BYTES: u32 = 8 * 1024 * 1024;

/// Der Gesundheitszustand des Backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TritonHealth {
    /// Der Server antwortet.
    pub live: bool,
    /// Der Server ist bereit, Inferenz anzunehmen.
    pub ready: bool,
}

/// Ein Client zum Triton-Server.
///
/// `Channel` von tonic multiplext intern ueber HTTP/2 und ist billig zu
/// klonen; ein eigener Verbindungspool waere hier doppelte Arbeit. Die
/// Nebenlaeufigkeit gegenueber dem Backend begrenzt ohnehin die
/// Kreditkontrolle des Schedulers (ADR-0002), nicht der Transport.
#[derive(Debug)]
pub struct TritonClient {
    endpoint: String,
    max_message_bytes: usize,
    channel: Mutex<Option<Channel>>,
}

impl TritonClient {
    /// Erzeugt einen Client, ohne die Verbindung sofort aufzubauen.
    ///
    /// Der Aufbau erfolgt beim ersten Aufruf. So kann der Gateway starten,
    /// bevor das Backend bereit ist, und `doctor` bekommt trotzdem eine klare
    /// Fehlermeldung, wenn es das nie wird.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_max_message_bytes(endpoint, DEFAULT_MAX_MESSAGE_BYTES)
    }

    /// Erzeugt einen Client mit einer eigenen Nachrichtenobergrenze.
    #[must_use]
    pub fn with_max_message_bytes(endpoint: impl Into<String>, max_message_bytes: usize) -> Self {
        Self {
            endpoint: endpoint.into(),
            max_message_bytes,
            channel: Mutex::new(None),
        }
    }

    /// Die konfigurierte Nachrichtenobergrenze.
    #[must_use]
    pub const fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    /// Der konfigurierte Endpunkt.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Liefert einen verbundenen Kanal, notfalls durch Neuaufbau.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unreachable`], wenn kein Kanal aufgebaut werden kann.
    async fn channel(&self) -> Result<Channel, BackendError> {
        let mut guard = self.channel.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.clone());
        }
        let uri = format!("http://{}", self.endpoint);
        let channel = Endpoint::from_shared(uri)
            .map_err(|e| BackendError::Unreachable {
                endpoint: self.endpoint.clone(),
                cause: e.to_string(),
            })?
            .connect_timeout(CONNECT_TIMEOUT)
            .initial_stream_window_size(STREAM_WINDOW_BYTES)
            .initial_connection_window_size(CONNECTION_WINDOW_BYTES)
            // Nagle aus: der Governor verschickt kleine Steuerpakete, bei denen
            // 40 ms Verzoegerung die gesamte Deadline aufbrauchen wuerden.
            .tcp_nodelay(true)
            .connect()
            .await
            .map_err(|e| BackendError::Unreachable {
                endpoint: self.endpoint.clone(),
                cause: e.to_string(),
            })?;
        *guard = Some(channel.clone());
        Ok(channel)
    }

    /// Ein Client mit den konfigurierten Groessengrenzen.
    fn client(&self, channel: Channel) -> GrpcInferenceServiceClient<Channel> {
        GrpcInferenceServiceClient::new(channel)
            .max_decoding_message_size(self.max_message_bytes)
            .max_encoding_message_size(self.max_message_bytes)
    }

    /// Verwirft den zwischengespeicherten Kanal.
    ///
    /// Nach einem Transportfehler; der naechste Aufruf baut neu auf.
    async fn invalidate(&self) {
        *self.channel.lock().await = None;
    }

    /// Ein roher Client auf dem gemeinsamen Kanal.
    ///
    /// Fuer die Endpunkte, die das Gateway unveraendert durchreicht. Spec L-002
    /// verlangt transparentes Forwarding; die Alternative waere, alle 21
    /// Methoden des Dienstes hier noch einmal von Hand zu spiegeln, ohne dass
    /// dabei etwas entstuende, was der Adapter nicht schon koennte.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unreachable`], wenn kein Kanal aufgebaut werden kann.
    pub async fn raw(&self) -> Result<GrpcInferenceServiceClient<Channel>, BackendError> {
        Ok(self.client(self.channel().await?))
    }

    /// Fragt Lebendigkeit und Bereitschaft ab.
    ///
    /// # Errors
    ///
    /// [`BackendError`], wenn das Backend nicht erreichbar ist.
    pub async fn health(&self) -> Result<TritonHealth, BackendError> {
        let mut client = self.client(self.channel().await?);
        let live = client
            .server_live(ServerLiveRequest {})
            .await
            .map_err(BackendError::from)?
            .into_inner()
            .live;
        let ready = client
            .server_ready(ServerReadyRequest {})
            .await
            .map_err(BackendError::from)?
            .into_inner()
            .ready;
        Ok(TritonHealth { live, ready })
    }

    /// Prueft, ob ein Modell bereit ist.
    ///
    /// # Errors
    ///
    /// [`BackendError`], wenn das Backend nicht erreichbar ist.
    pub async fn model_ready(&self, model: &str) -> Result<bool, BackendError> {
        let mut client = self.client(self.channel().await?);
        let response = client
            .model_ready(ModelReadyRequest {
                name: model.to_owned(),
                version: String::new(),
            })
            .await
            .map_err(BackendError::from)?;
        Ok(response.into_inner().ready)
    }

    /// Holt die Metadaten eines Modells.
    ///
    /// # Errors
    ///
    /// [`BackendError::UnknownModel`], wenn das Backend das Modell nicht kennt.
    pub async fn model_metadata(&self, model: &str) -> Result<ModelMetadataResponse, BackendError> {
        let mut client = self.client(self.channel().await?);
        let response = client
            .model_metadata(ModelMetadataRequest {
                name: model.to_owned(),
                version: String::new(),
            })
            .await
            .map_err(|status| {
                if status.code() == tonic::Code::NotFound {
                    BackendError::UnknownModel {
                        model: model.to_owned(),
                    }
                } else {
                    BackendError::from(status)
                }
            })?;
        Ok(response.into_inner())
    }

    /// Fuehrt eine Inferenz aus.
    ///
    /// Der Aufrufer hat `model_name` bereits auf die gewaehlte physische
    /// Variante gesetzt; der Adapter waehlt nichts aus.
    ///
    /// # Errors
    ///
    /// Siehe [`BackendError`]. Bei einem Transportfehler wird der
    /// zwischengespeicherte Kanal verworfen, damit der naechste Aufruf neu
    /// aufbaut. Der Request selbst wird **nicht** wiederholt — darueber
    /// entscheidet der Scheduler anhand der Frische (Spec 30.4).
    pub async fn infer(
        &self,
        request: ModelInferRequest,
    ) -> Result<ModelInferResponse, BackendError> {
        let channel = self.channel().await?;
        let mut client = self.client(channel);
        match client.model_infer(request).await {
            Ok(response) => Ok(response.into_inner()),
            Err(status) => {
                let error = BackendError::from(status);
                if error.is_transport_failure() {
                    self.invalidate().await;
                }
                Err(error)
            }
        }
    }
}
