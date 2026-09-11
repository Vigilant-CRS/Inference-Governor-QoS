//! Ein Backend mit begrenzter Ausfuehrungskapazitaet.
//!
//! Bildet die Eigenschaft ab, um die es beim Produkt geht: **eine geteilte
//! Ressource mit endlich vielen gleichzeitigen Ausfuehrungen**. Mehr braucht
//! es nicht, um die Frage zu beantworten, ob der Governor etwas bringt — und
//! weniger waere unehrlich, denn ohne Kapazitaetsgrenze gibt es keine
//! Konkurrenz und damit nichts zu steuern.
//!
//! Die Laufzeit eines Requests haengt an `(seed, stream, frame)` und **nicht**
//! am Fortschritt eines gemeinsamen Zufallsstroms. Derselbe Frame bekommt in
//! beiden Vergleichslaeufen dieselbe Laufzeit, unabhaengig davon, in welcher
//! Reihenfolge er ausgefuehrt wird.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Semaphore;
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_sim::rng::Pcg32;
use vig_sim::workload::RuntimeDistribution;

pub use crate::service::BackendService;

/// Der Zustand des Backends.
#[derive(Debug)]
pub struct Backend {
    /// Ausfuehrungskapazitaet: so viele Inferenzen laufen hoechstens gleichzeitig.
    pub slots: Arc<Semaphore>,
    /// Laufzeitverteilung je physischem Modellnamen.
    pub runtimes: HashMap<String, RuntimeDistribution>,
    /// Seed der Laufzeitziehung.
    pub seed: u64,
    /// Anzahl ausgefuehrter Inferenzen.
    pub executed: AtomicU64,
    /// Summe der tatsaechlich verbrauchten Ausfuehrungszeit in Nanosekunden.
    pub busy_nanos: AtomicU64,
    /// Eine feste Antwort, falls gesetzt: Ausgabetensoren und ihre Rohdaten.
    ///
    /// Ohne sie antwortet das Backend leer — genug fuer Laufzeitfragen. Wer
    /// wissen will, ob ein Client eine Antwort auch **liest** (der
    /// Edge-Pilot dekodiert Detektionen), braucht Inhalt.
    pub fixed_output: Option<FixedOutput>,
}

/// Ausgabetensoren und Rohdaten einer festen Antwort.
pub type FixedOutput = (
    Vec<vig_protocol_oip::inference::model_infer_response::InferOutputTensor>,
    Vec<Vec<u8>>,
);

impl Backend {
    /// Baut ein Backend.
    #[must_use]
    pub fn new(
        slot_count: usize,
        runtimes: HashMap<String, RuntimeDistribution>,
        seed: u64,
    ) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(slot_count.max(1))),
            runtimes,
            seed,
            executed: AtomicU64::new(0),
            busy_nanos: AtomicU64::new(0),
            fixed_output: None,
        }
    }

    /// Dasselbe Backend, das auf jede Inferenz mit `output` antwortet.
    #[must_use]
    pub fn with_fixed_output(mut self, output: FixedOutput) -> Self {
        self.fixed_output = Some(output);
        self
    }

    /// Zieht die Laufzeit fuer einen Frame.
    ///
    /// Reproduzierbar aus `(seed, request_id)`: derselbe Frame kostet in jedem
    /// Lauf dasselbe.
    #[must_use]
    pub fn runtime_for(&self, model: &str, request_id: &str) -> std::time::Duration {
        let Some(distribution) = self.runtimes.get(model) else {
            return std::time::Duration::from_millis(1);
        };
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in request_id.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let mut rng = Pcg32::new(self.seed ^ hash, 11);
        std::time::Duration::from_nanos(distribution.sample(&mut rng).as_nanos())
    }

    /// Fuehrt eine Inferenz aus: Kapazitaet belegen, rechnen, freigeben.
    pub async fn execute(&self, model: &str, request_id: &str) {
        let runtime = self.runtime_for(model, request_id);
        let _permit = self.slots.acquire().await;
        tokio::time::sleep(runtime).await;
        self.executed.fetch_add(1, Ordering::Relaxed);
        self.busy_nanos.fetch_add(
            u64::try_from(runtime.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

/// Startet das Backend auf einem freien Port.
///
/// # Panics
///
/// Wenn kein Port gebunden werden kann. Im Benchmark ist das ein
/// Umgebungsproblem und soll den Lauf abbrechen.
pub async fn start(backend: Arc<Backend>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("freier Port fuer das Backend");
    let address = listener.local_addr().expect("lokale Adresse");
    let service = BackendService::new(backend);
    tokio::spawn(async move {
        let stream = tokio_stream::wrappers::TcpListenerStream::new(listener);
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
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    address
}
