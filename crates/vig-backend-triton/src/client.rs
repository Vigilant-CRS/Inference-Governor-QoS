//! Der gRPC-Client zum Triton-Server.

use crate::error::BackendError;
use std::time::Duration as StdDuration;
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};
use vig_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient;
use vig_protocol_oip::inference::{
    ModelInferRequest, ModelInferResponse, ModelMetadataRequest, ModelMetadataResponse,
    ModelReadyRequest, ModelStatisticsRequest, ServerLiveRequest, ServerReadyRequest,
};

/// Was das Backend ueber die abgeschlossene Arbeit eines Modells meldet.
///
/// Der Nachweis, mit dem ein gehaltener Slotkredit zurueckgegeben wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evidence {
    /// Erfolgreich **und** fehlgeschlagen abgeschlossene Inferenzen.
    ///
    /// Beides zaehlt: eine fehlgeschlagene Inferenz hat die Recheneinheit
    /// genauso wieder freigegeben wie eine erfolgreiche.
    pub completed: u64,
    /// Zeitpunkt der letzten Anfrage, Millisekunden seit Epoch.
    pub last_inference_ms: u64,
}

/// Timeout fuer Verbindungsaufbau und Gesundheitsabfragen.
///
/// Bewusst kurz: `vig doctor` und die Bereitschaftspruefung sollen schnell
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

    /// Fuehrt eine Inferenz gegen ein **decoupled** Modell aus.
    ///
    /// Generative Backends — vLLM, TensorRT-LLM — sind in Triton
    /// grundsaetzlich decoupled und antworten ausschliesslich ueber
    /// `ModelStreamInfer`. Ein unaerer Aufruf scheitert dort mit
    /// „doesn't support models with decoupled transaction policy".
    ///
    /// Das widerspricht **nicht** der Entscheidung, dass Vigilant nach aussen
    /// kein Streaming anbietet (Spec 16.1). Diese Einschraenkung schuetzt die
    /// Frischelogik vor Clients, die sie umgehen wuerden. Welchen Aufruf der
    /// Adapter zum Backend hin benutzt, ist davon unberuehrt — nach aussen
    /// bleibt der Request unaer.
    ///
    /// Es wird genau eine Antwort erwartet und der Stream danach geschlossen.
    ///
    /// # Errors
    ///
    /// Siehe [`BackendError`]. Meldet das Backend im Stream einen Fehler, wird
    /// er als [`BackendError::Rejected`] durchgereicht; bleibt der Stream ohne
    /// Antwort, als [`BackendError::Malformed`].
    pub async fn infer_decoupled(
        &self,
        request: ModelInferRequest,
    ) -> Result<ModelInferResponse, BackendError> {
        use tokio_stream::StreamExt as _;

        let channel = self.channel().await?;
        let mut client = self.client(channel);
        let model = request.model_name.clone();
        let outbound = tokio_stream::once(request);

        let mut inbound = match client.model_stream_infer(outbound).await {
            Ok(response) => response.into_inner(),
            Err(status) => {
                let error = BackendError::from(status);
                if error.is_transport_failure() {
                    self.invalidate().await;
                }
                return Err(error);
            }
        };

        // Bis zur **letzten** Antwort lesen, nicht bis zur ersten. Ein
        // decoupled Modell darf mehrere Antworten senden; die erste ist keine
        // Fertigstellungsbestaetigung. Wer nach ihr zurueckkehrt, liefert bei
        // einem streamenden Modell Teiltext als Endergebnis aus und gibt den
        // Slot frei, waehrend das Backend noch rechnet — der Governor plant
        // dann gegen eine Belegung, die es gar nicht mehr gibt.
        //
        // Die Kehrseite gehoert benannt: ein Backend, das weder eine
        // Abschlussmarkierung sendet noch den Stream schliesst, haelt diesen
        // Aufruf jetzt offen, statt vorzeitig mit einem falschen Ergebnis
        // zurueckzukehren. Ein Inferenztimeout mit Quarantaenezustand fehlt
        // weiterhin (Review F10); mit dieser Korrektur wird er dringender.
        let mut payload: Option<ModelInferResponse> = None;
        let mut payloads = 0_usize;

        while let Some(message) = inbound.next().await {
            let message = message.map_err(BackendError::from)?;
            if !message.error_message.is_empty() {
                return Err(BackendError::Rejected {
                    code: tonic::Code::Internal,
                    message: message.error_message,
                });
            }
            let Some(response) = message.infer_response else {
                continue;
            };
            let last = is_final_response(&response);
            if response.raw_output_contents.is_empty() && response.outputs.is_empty() {
                // Eine reine Abschlussmarkierung ohne Nutzlast.
                if payload.is_none() {
                    payload = Some(response);
                }
            } else {
                payloads = payloads.saturating_add(1);
                payload = Some(response);
            }
            if last {
                break;
            }
        }

        // Mehrere Teilantworten muessten fachlich zusammengefuegt werden, und
        // wie, weiss nur das Modell. Sie stillschweigend auf die letzte zu
        // reduzieren waere ein falsches Ergebnis mit gruener Metrik. Solange
        // kein Aggregationsverfahren vereinbart ist, wird der Fall gemeldet.
        if payloads > 1 {
            return Err(BackendError::Malformed {
                detail: format!(
                    "{model}: {payloads} Teilantworten; Vigilant unterstuetzt derzeit nur \
                     decoupled Modelle mit genau einer vollstaendigen Antwort"
                ),
            });
        }

        payload.ok_or_else(|| BackendError::Malformed {
            detail: format!("{model}: der Stream endete ohne Antwort"),
        })
    }

    /// Der Ausfuehrungsnachweis eines Modells: abgeschlossene Inferenzen und
    /// der Zeitpunkt der letzten Anfrage.
    ///
    /// Das ist die Evidenz, mit der ein gehaltener Slotkredit zurueckgegeben
    /// wird. Ein abgelaufener Timer beweist nicht, dass die Recheneinheit
    /// fertig ist; ein gestiegener Abschlusszaehler tut es.
    ///
    /// `last_inference` ist Tritons Zeitstempel der letzten Anfrage in
    /// Millisekunden seit Epoch — eine **Wanduhr**, nicht die monotone
    /// Schedulingzeit. Sie wird ausschliesslich fuer diesen Abgleich benutzt
    /// und fliesst in keine Planungsentscheidung ein.
    ///
    /// # Errors
    ///
    /// Siehe [`BackendError`]. Meldet das Backend keine Statistik zu diesem
    /// Modell, ist das [`BackendError::UnknownModel`] — und kein Nachweis.
    pub async fn completion_evidence(&self, model: &str) -> Result<Evidence, BackendError> {
        let mut client = self.client(self.channel().await?);
        let response = client
            .model_statistics(ModelStatisticsRequest {
                name: model.to_owned(),
                version: String::new(),
            })
            .await
            .map_err(BackendError::from)?
            .into_inner();

        let stats = response
            .model_stats
            .into_iter()
            .find(|s| s.name == model)
            .ok_or_else(|| BackendError::UnknownModel {
                model: model.to_owned(),
            })?;

        let inference = stats.inference_stats.unwrap_or_default();
        let completed = inference
            .success
            .map_or(0, |d| d.count)
            .saturating_add(inference.fail.map_or(0, |d| d.count));

        Ok(Evidence {
            completed,
            last_inference_ms: stats.last_inference,
        })
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

/// Der Parameter, mit dem Triton die letzte Antwort eines decoupled Modells
/// markiert.
///
/// [Triton Decoupled Models](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/decoupled_models.html)
pub const FINAL_RESPONSE_PARAM: &str = "triton_final_response";

/// Wahr, wenn die Antwort als letzte des Streams markiert ist.
///
/// Fehlt der Parameter, gilt die Antwort **nicht** als letzte: dann
/// entscheidet das Stream-Ende. Die andere Annahme waere die gefaehrliche —
/// sie kehrte bei jedem Modell ohne Markierung zur ersten Teilantwort zurueck.
fn is_final_response(response: &ModelInferResponse) -> bool {
    use vig_protocol_oip::inference::infer_parameter::ParameterChoice;
    matches!(
        response
            .parameters
            .get(FINAL_RESPONSE_PARAM)
            .and_then(|p| p.parameter_choice.as_ref()),
        Some(ParameterChoice::BoolParam(true))
    )
}
