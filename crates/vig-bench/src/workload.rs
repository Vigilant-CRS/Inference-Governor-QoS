//! Der Lasttreiber: Kameras, die unabhaengig vom Systemzustand weiter liefern.
//!
//! Der Punkt der Uebung ist, dass ein Sensor **nicht** langsamer wird, wenn das
//! System ueberlastet ist. Ein Treiber, der auf die Antwort wartet, bevor er
//! den naechsten Frame schickt, wuerde das Problem wegdefinieren, um das es
//! geht — und beiden Seiten einen Vorteil verschaffen, den es real nicht gibt.
//!
//! Beide Vergleichslaeufe bekommen denselben Treiber, dieselben Perioden,
//! dieselbe Obergrenze offener Requests und dieselben Frame-Nummern. Der
//! einzige Unterschied ist, wohin die Requests gehen.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tonic::transport::Channel;
use vig_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient;
use vig_protocol_oip::inference::infer_parameter::ParameterChoice;
use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{InferParameter, ModelInferRequest};
use vig_protocol_oip::params::P_AGE_US;
use vig_sim::coverage::{Coverage, CoverageTracker};

/// Die Eingabe, die ein Strom mitschickt.
///
/// Als **Shared-Memory-Referenz**: der Request nennt nur Region, Offset und
/// Groesse. Ein Gate-M3-Vergleich ueber den Copy-Pfad wuerde den Transport
/// messen statt das Scheduling, und der Governor traegt diese Kosten doppelt
/// (ADR-0003).
#[derive(Debug, Clone)]
pub struct InputSpec {
    /// Name des Eingabetensors laut Modellmetadaten.
    pub name: String,
    /// Datentyp laut Modellmetadaten.
    pub datatype: String,
    /// Vollstaendige Form einschliesslich Batchdimension.
    pub shape: Vec<i64>,
    /// Die beim Backend registrierte Region.
    ///
    /// `None`, wenn der Server kein Shared Memory kann. Dann reist die
    /// Nutzlast im Request — langsamer, aber ueberall verfuegbar. Welcher
    /// Weg moeglich ist, sagt der Server selbst
    /// (`vig_backend_triton::Capabilities`).
    pub region: Option<String>,
    /// Groesse des Tensors in Bytes.
    pub byte_size: u64,
}

/// Ein Sensorstrom im Lastmodell.
#[derive(Debug, Clone)]
pub struct StreamDef {
    /// Name im Report.
    pub name: &'static str,
    /// Modellname, den der Client anfragt.
    ///
    /// Ueber den Governor ist das der logische Name, direkt der physische.
    pub model: &'static str,
    /// Periode zwischen zwei Frames.
    pub period: Duration,
    /// Fachliches Hoechstalter fuer die Coverage-Bewertung.
    pub max_age: Duration,
    /// Obergrenze gleichzeitig offener Requests dieses Stroms.
    ///
    /// Bildet einen Client mit endlichem Puffer ab. Ohne Grenze wuerde der
    /// Treiber selbst unbegrenzt Speicher belegen und damit etwas anderes
    /// messen als das System.
    pub in_flight_cap: usize,
    /// Die Eingabe, falls das Backend eine braucht.
    ///
    /// `None` fuer das Mock-Backend, das keine Tensoren auswertet.
    pub input: Option<InputSpec>,
    /// Verwirft der Client selbst veraltete Frames?
    ///
    /// Das ist der naheliegende Eigenbau: nur der neueste Frame zaehlt, es ist
    /// immer nur einer unterwegs, und trifft ein neuer ein, waehrend der alte
    /// noch laeuft, wird der alte fallengelassen. Genau die LATEST-Semantik
    /// aus Spec 9.3 — aber im Client statt im Governor.
    ///
    /// Der Vergleich dieser Betriebsart gegen den Governor beantwortet den
    /// haertesten Einwand gegen das ganze Produkt: was kann der Governor, das
    /// eine Stunde Clientcode nicht auch kann?
    pub pump: bool,
}

/// Das Ergebnis eines Stroms.
#[derive(Debug, Clone)]
pub struct StreamReport {
    /// Name des Stroms.
    pub name: &'static str,
    /// Vom Sensor erzeugte Frames.
    pub emitted: u64,
    /// Tatsaechlich gesendete Requests.
    pub sent: u64,
    /// Frames, die der Client wegen seiner eigenen Grenze nicht senden konnte.
    pub client_dropped: u64,
    /// Beantwortete Requests.
    pub delivered: u64,
    /// Vom System abgewiesene Requests.
    pub rejected: u64,
    /// Abdeckung und Age of Information.
    pub coverage: Coverage,
}

struct StreamState {
    tracker: Mutex<CoverageTracker>,
    emitted: AtomicU64,
    sent: AtomicU64,
    client_dropped: AtomicU64,
    delivered: AtomicU64,
    rejected: AtomicU64,
    permits: Arc<Semaphore>,
}

/// Faehrt den Lauf und gibt je Strom einen Bericht zurueck.
///
/// `via_governor` steuert nur, ob der Client die Governor-Altersangabe
/// mitschickt; das Ziel bestimmt der Aufrufer ueber `endpoint`.
///
/// # Panics
///
/// Wenn keine Verbindung zum Ziel aufgebaut werden kann.
pub async fn drive(
    endpoint: &str,
    streams: &[StreamDef],
    duration: Duration,
    via_governor: bool,
) -> Vec<StreamReport> {
    let origin = Instant::now();
    let core_duration = vig_core::Duration::from_nanos_unbounded(
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
    );

    let mut states = Vec::new();
    for stream in streams {
        states.push(Arc::new(StreamState {
            tracker: Mutex::new(CoverageTracker::new(
                to_core(stream.period),
                to_core(stream.max_age),
                vig_core::Instant::ZERO,
                core_duration,
            )),
            emitted: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            client_dropped: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            permits: Arc::new(Semaphore::new(stream.in_flight_cap)),
        }));
    }

    let mut tasks = Vec::new();
    for (index, stream) in streams.iter().enumerate() {
        let Some(state) = states.get(index).cloned() else {
            continue;
        };
        let stream = stream.clone();
        // Ist das Ziel gerade weg, faellt dieser Strom fuer dieses Fenster aus
        // und meldet null Lieferungen — der Lauf geht weiter.
        let Ok(client) = try_connect(endpoint).await else {
            continue;
        };
        tasks.push(tokio::spawn(async move {
            if stream.pump {
                run_stream_pump(client, stream, state, origin, duration, via_governor).await;
            } else {
                run_stream(client, stream, state, origin, duration, via_governor).await;
            }
        }));
    }
    for task in tasks {
        let _ = task.await;
    }

    streams
        .iter()
        .zip(states.iter())
        .map(|(stream, state)| StreamReport {
            name: stream.name,
            emitted: state.emitted.load(Ordering::Relaxed),
            sent: state.sent.load(Ordering::Relaxed),
            client_dropped: state.client_dropped.load(Ordering::Relaxed),
            delivered: state.delivered.load(Ordering::Relaxed),
            rejected: state.rejected.load(Ordering::Relaxed),
            coverage: state.tracker.lock().map_or(
                Coverage {
                    covered: 0,
                    total: 0,
                    response_age_p50_ns: 0,
                    response_age_p95_ns: 0,
                    response_age_p99_ns: 0,
                    peak_aoi_ns: 0,
                    delivered: 0,
                },
                |tracker| tracker.finish(),
            ),
        })
        .collect()
}

async fn run_stream(
    client: GrpcInferenceServiceClient<Channel>,
    stream: StreamDef,
    state: Arc<StreamState>,
    origin: Instant,
    duration: Duration,
    via_governor: bool,
) {
    let mut ticker = tokio::time::interval(stream.period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut frame = 0_u64;

    loop {
        ticker.tick().await;
        let capture = Instant::now();
        if capture.duration_since(origin) >= duration {
            break;
        }
        frame = frame.saturating_add(1);
        state.emitted.fetch_add(1, Ordering::Relaxed);

        // Der Client hat einen endlichen Puffer. Ist er voll, geht der Frame
        // verloren - in beiden Laeufen gleichermassen.
        let Ok(permit) = Arc::clone(&state.permits).try_acquire_owned() else {
            state.client_dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        };

        let mut client = client.clone();
        let state = Arc::clone(&state);
        let id = format!("{}:{frame}", stream.name);
        let model = stream.model;
        let input = stream.input.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let age = capture.elapsed();
            let request = build_request(model, &id, via_governor, age, input.as_ref());
            state.sent.fetch_add(1, Ordering::Relaxed);

            match client.model_infer(request).await {
                Ok(_) => {
                    state.delivered.fetch_add(1, Ordering::Relaxed);
                    let completion = Instant::now().duration_since(origin);
                    let generation = capture.duration_since(origin);
                    if let Ok(mut tracker) = state.tracker.lock() {
                        tracker.record_delivery(
                            vig_core::Instant::from_nanos(
                                u64::try_from(completion.as_nanos()).unwrap_or(u64::MAX),
                            ),
                            vig_core::Instant::from_nanos(
                                u64::try_from(generation.as_nanos()).unwrap_or(u64::MAX),
                            ),
                        );
                    }
                }
                Err(_) => {
                    state.rejected.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }
}

/// Der Eigenbau: LATEST-Semantik im Client.
///
/// Ein Erzeuger legt jeden Frame in ein Fach, das nur den neuesten haelt; ein
/// Sender nimmt ihn heraus, sobald die vorige Antwort da ist. Ueberschriebene
/// Frames sind verworfen — sie waren beim Absenden schon nicht mehr die
/// aktuelle Lage.
///
/// Was diese Schleife nicht kann, ist der Kern des Vergleichs: sie sieht nur
/// ihren eigenen Strom. Zwei solche Pumpen nebeneinander wissen nichts
/// voneinander, und am Server entscheidet weiter die Ankunftsreihenfolge.
async fn run_stream_pump(
    client: GrpcInferenceServiceClient<Channel>,
    stream: StreamDef,
    state: Arc<StreamState>,
    origin: Instant,
    duration: Duration,
    via_governor: bool,
) {
    let slot: Arc<Mutex<Option<(u64, Instant)>>> = Arc::new(Mutex::new(None));
    let ready = Arc::new(tokio::sync::Notify::new());

    let producer = {
        let slot = Arc::clone(&slot);
        let ready = Arc::clone(&ready);
        let state = Arc::clone(&state);
        let period = stream.period;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut frame = 0_u64;
            loop {
                ticker.tick().await;
                let capture = Instant::now();
                if capture.duration_since(origin) >= duration {
                    break;
                }
                frame = frame.saturating_add(1);
                state.emitted.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut guard) = slot.lock()
                    && guard.replace((frame, capture)).is_some()
                {
                    // Ein noch nicht gesendeter Frame wurde ueberholt.
                    state.client_dropped.fetch_add(1, Ordering::Relaxed);
                }
                ready.notify_one();
            }
        })
    };

    let mut client = client;
    loop {
        if Instant::now().duration_since(origin) >= duration {
            break;
        }
        let next = slot.lock().ok().and_then(|mut guard| guard.take());
        let Some((frame, capture)) = next else {
            // Nichts zu tun; auf den naechsten Frame warten, aber nicht
            // ueber das Ende des Laufs hinaus.
            let _ = tokio::time::timeout(Duration::from_millis(5), ready.notified()).await;
            continue;
        };

        let id = format!("{}:{frame}", stream.name);
        let age = capture.elapsed();
        let request = build_request(stream.model, &id, via_governor, age, stream.input.as_ref());
        state.sent.fetch_add(1, Ordering::Relaxed);

        match client.model_infer(request).await {
            Ok(_) => {
                state.delivered.fetch_add(1, Ordering::Relaxed);
                let completion = Instant::now().duration_since(origin);
                let generation = capture.duration_since(origin);
                if let Ok(mut tracker) = state.tracker.lock() {
                    tracker.record_delivery(
                        vig_core::Instant::from_nanos(
                            u64::try_from(completion.as_nanos()).unwrap_or(u64::MAX),
                        ),
                        vig_core::Instant::from_nanos(
                            u64::try_from(generation.as_nanos()).unwrap_or(u64::MAX),
                        ),
                    );
                }
            }
            Err(_) => {
                state.rejected.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    producer.abort();
}

fn build_request(
    model: &str,
    id: &str,
    via_governor: bool,
    age: Duration,
    input: Option<&InputSpec>,
) -> ModelInferRequest {
    let mut parameters = HashMap::new();
    if via_governor {
        // ADR-0011: der hosttopologieunabhaengige Weg. Der Client sagt, wie alt
        // das Sensordatum beim Senden war.
        parameters.insert(
            P_AGE_US.to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::Int64Param(
                    i64::try_from(age.as_micros()).unwrap_or(i64::MAX),
                )),
            },
        );
    }
    let mut raw_contents = Vec::new();
    let inputs = input.map_or_else(Vec::new, |spec| {
        let Some(region) = spec.region.clone() else {
            // Kopierpfad: die Nullbytes reisen im Request mit.
            raw_contents.push(vec![0_u8; usize::try_from(spec.byte_size).unwrap_or(0)]);
            return vec![InferInputTensor {
                name: spec.name.clone(),
                datatype: spec.datatype.clone(),
                shape: spec.shape.clone(),
                parameters: HashMap::new(),
                contents: None,
            }];
        };
        let mut tensor_params = HashMap::new();
        tensor_params.insert(
            "shared_memory_region".to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::StringParam(region)),
            },
        );
        tensor_params.insert(
            "shared_memory_byte_size".to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::Int64Param(
                    i64::try_from(spec.byte_size).unwrap_or(i64::MAX),
                )),
            },
        );
        tensor_params.insert(
            "shared_memory_offset".to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::Int64Param(0)),
            },
        );
        vec![InferInputTensor {
            name: spec.name.clone(),
            datatype: spec.datatype.clone(),
            shape: spec.shape.clone(),
            parameters: tensor_params,
            contents: None,
        }]
    });

    ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: id.to_owned(),
        parameters,
        inputs,
        outputs: Vec::new(),
        // Leer, wenn die Nutzlast als Referenz reist; sonst die Rohbytes.
        raw_input_contents: raw_contents,
    }
}

fn to_core(d: Duration) -> vig_core::Duration {
    vig_core::Duration::from_nanos_unbounded(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Baut eine Clientverbindung mit denselben Transportgrenzen wie der Governor.
///
/// # Panics
///
/// Wenn keine Verbindung aufgebaut werden kann.
pub async fn connect(endpoint: &str) -> GrpcInferenceServiceClient<Channel> {
    #[allow(clippy::expect_used)]
    try_connect(endpoint).await.expect("Verbindung zum Ziel")
}

/// Wie [`connect`], aber ohne Panik bei unerreichbarem Ziel.
///
/// Fuer kurze Benchmarks ist ein Abbruch die richtige Antwort — laeuft das
/// Backend nicht, ist die Messung sinnlos. Fuer einen Dauerlauf ueber Stunden
/// waere sie falsch: ein einzelner Aussetzer des Backends darf nicht die
/// gesamte Nacht kosten, sondern gehoert protokolliert und ueberstanden.
///
/// # Errors
///
/// Wenn der Endpunkt ungueltig ist oder die Verbindung nicht zustande kommt.
pub async fn try_connect(
    endpoint: &str,
) -> Result<GrpcInferenceServiceClient<Channel>, tonic::transport::Error> {
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{endpoint}"))?
        .initial_stream_window_size(vig_backend_triton::STREAM_WINDOW_BYTES)
        .initial_connection_window_size(vig_backend_triton::CONNECTION_WINDOW_BYTES)
        .tcp_nodelay(true)
        .connect()
        .await?;
    Ok(GrpcInferenceServiceClient::new(channel)
        .max_decoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
        .max_encoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES))
}
