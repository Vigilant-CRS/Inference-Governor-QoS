//! `wp26` — löst die Zerlegung in Quanten die Aushungerung aus ADR-0012?
//!
//! Gate M3 hat gezeigt: ein nicht unterbrechbarer Block laeuft neben einer
//! 33-ms-Periode nie. ADR-0014 zerlegt generative Auftraege in Quanten, deren
//! Groesse sich aus dem Slack bis zur naechsten geschuetzten Ankunft ergibt.
//!
//! Gemessen werden drei Betriebsarten mit **demselben** Workload:
//!
//! * **direkt** — Detektor und Sprachmodell sprechen unmittelbar mit Triton.
//! * **Governor ohne Zerlegung** — der Zustand aus Gate M3.
//! * **Governor mit Zerlegung** — ADR-0014.
//!
//! Die Frage ist nicht, ob der Detektor geschuetzt wird — das ist belegt.
//! Die Frage ist, ob das Sprachmodell dabei **ueberhaupt vorankommt**.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines
)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vig_bench::shm::Region;
use vig_bench::workload::connect;
use vig_config::Config;
use vig_gateway::cooperative::{SAMPLING_PARAMETERS, TEXT_INPUT, read_text_output};
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::infer_parameter::ParameterChoice;
use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{
    InferParameter, ModelInferRequest, ModelMetadataRequest, SystemSharedMemoryRegisterRequest,
    SystemSharedMemoryUnregisterRequest,
};
use vig_protocol_oip::params::P_AGE_US;
use vig_sim::coverage::CoverageTracker;

const RUN_SECONDS: u64 = 30;
const DETECTOR_PERIOD_MS: u64 = 33;
const DETECTOR_MAX_AGE_MS: u64 = 66;
const PROMPT: &str = "Beschreibe knapp, was ein mobiler Roboter in einer Lagerhalle sieht:";
const GENERATE_TOKENS: u32 = 64;

/// Eine Umgebungsvariable, sonst der bisher fest eingebaute Wert.
///
/// Dieses Werkzeug war auf die Modelle dieser Maschine verdrahtet. Das
/// Reproduktionspaket (`tools/repro/`) faehrt denselben Vergleich mit
/// oeffentlichen Modellen, und ohne diese Schalter braeuchte es dafuer eine
/// zweite Kopie des Werkzeugs — also eine zweite Stelle, an der derselbe
/// Vergleich auseinanderlaufen kann. Die Vorgaben sind unveraendert, damit
/// die frueheren Messungen mit demselben Aufruf reproduzierbar bleiben.
fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn env_u64_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Der Triton mit dem Detektor.
fn detector_endpoint() -> String {
    env_or("VIG_WP26_DETECTOR_ENDPOINT", "127.0.0.1:8001")
}

/// Der Triton mit dem Sprachmodell — ein eigener Prozess, weil sich das
/// vLLM-Backend und das onnxruntime-Backend nicht in einen legen lassen.
fn llm_endpoint() -> String {
    env_or("VIG_WP26_LLM_ENDPOINT", "127.0.0.1:8011")
}

/// Der Modellname **im Backend**. Die logischen Namen der Konfiguration
/// (`detector`, `qwen`) bleiben fest: sie stehen in der Auswertung.
fn detector_model() -> String {
    env_or("VIG_WP26_DETECTOR_MODEL", "rfdetr")
}

fn llm_model() -> String {
    env_or("VIG_WP26_LLM_MODEL", "qwen")
}

struct Outcome {
    label: &'static str,
    detector_covered: u64,
    detector_total: u64,
    detector_response_age_p95_ms: u64,
    generations_finished: u64,
    generated_chars: u64,
}

fn text_request(model: &str, prompt: &str, max_tokens: u32) -> ModelInferRequest {
    let params = format!("{{\"max_tokens\": {max_tokens}, \"temperature\": 0.0}}");
    let prefixed = |value: &str| {
        let bytes = value.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() + 4);
        out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(bytes);
        out
    };
    let tensor = |name: &str| InferInputTensor {
        name: name.to_owned(),
        datatype: "BYTES".to_owned(),
        shape: vec![1],
        parameters: HashMap::new(),
        contents: None,
    };
    ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: "gen".to_owned(),
        parameters: HashMap::new(),
        inputs: vec![tensor(TEXT_INPUT), tensor(SAMPLING_PARAMETERS)],
        outputs: Vec::new(),
        raw_input_contents: vec![prefixed(prompt), prefixed(&params)],
    }
}

fn detector_request(
    model: &str,
    id: u64,
    spec: &(String, String, Vec<i64>, String, u64),
    age: Duration,
    with_params: bool,
) -> ModelInferRequest {
    let (name, datatype, shape, region, byte_size) = spec;
    let mut tensor_params = HashMap::new();
    tensor_params.insert(
        "shared_memory_region".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::StringParam(region.clone())),
        },
    );
    tensor_params.insert(
        "shared_memory_byte_size".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(
                i64::try_from(*byte_size).unwrap_or(i64::MAX),
            )),
        },
    );
    tensor_params.insert(
        "shared_memory_offset".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(0)),
        },
    );
    let mut parameters = HashMap::new();
    if with_params {
        parameters.insert(
            P_AGE_US.to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::Int64Param(
                    i64::try_from(age.as_micros()).unwrap_or(0),
                )),
            },
        );
    }
    ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: id.to_string(),
        parameters,
        inputs: vec![InferInputTensor {
            name: name.clone(),
            datatype: datatype.clone(),
            shape: shape.clone(),
            parameters: tensor_params,
            contents: None,
        }],
        outputs: Vec::new(),
        raw_input_contents: Vec::new(),
    }
}

/// Faehrt Detektor und Sprachmodell gleichzeitig gegen einen Endpunkt.
/// Faehrt Detektor und Sprachmodell gleichzeitig.
///
/// Beide bekommen einen eigenen Endpunkt: direkt sprechen sie mit ihren
/// jeweiligen Servern, ueber den Governor mit demselben Gateway.
async fn measure(
    label: &'static str,
    detector_endpoint: &str,
    llm_endpoint: &str,
    detector_model: &str,
    qwen_model: &str,
    spec: &(String, String, Vec<i64>, String, u64),
    via_governor: bool,
) -> Outcome {
    let origin = Instant::now();
    let duration = Duration::from_secs(RUN_SECONDS);
    let tracker = Arc::new(Mutex::new(CoverageTracker::new(
        vig_core::Duration::from_nanos_unbounded(DETECTOR_PERIOD_MS * 1_000_000),
        vig_core::Duration::from_nanos_unbounded(DETECTOR_MAX_AGE_MS * 1_000_000),
        vig_core::Instant::ZERO,
        vig_core::Duration::from_nanos_unbounded(RUN_SECONDS * 1_000_000_000),
    )));
    let finished = Arc::new(AtomicU64::new(0));
    let chars = Arc::new(AtomicU64::new(0));

    // --- Detektor: 30 Hz, unabhaengig vom Systemzustand -------------------
    let detector = {
        let client = connect(detector_endpoint).await;
        let tracker = Arc::clone(&tracker);
        let model = detector_model.to_owned();
        let spec = spec.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(DETECTOR_PERIOD_MS));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut frame = 0_u64;
            let permits = Arc::new(tokio::sync::Semaphore::new(8));
            loop {
                ticker.tick().await;
                let capture = Instant::now();
                if capture.duration_since(origin) >= duration {
                    break;
                }
                frame += 1;
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    continue;
                };
                let mut client = client.clone();
                let tracker = Arc::clone(&tracker);
                let model = model.clone();
                let spec = spec.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let request =
                        detector_request(&model, frame, &spec, capture.elapsed(), via_governor);
                    if client.model_infer(request).await.is_ok()
                        && let Ok(mut t) = tracker.lock()
                    {
                        t.record_delivery(
                            vig_core::Instant::from_nanos(
                                u64::try_from(Instant::now().duration_since(origin).as_nanos())
                                    .unwrap_or(0),
                            ),
                            vig_core::Instant::from_nanos(
                                u64::try_from(capture.duration_since(origin).as_nanos())
                                    .unwrap_or(0),
                            ),
                        );
                    }
                });
            }
        })
    };

    // --- Sprachmodell: laeuft durchgehend, so oft es darf ------------------
    // Der Aufrufweg unterscheidet sich notwendigerweise, und das ist keine
    // Unfairness, sondern die Schnittstellenzusage:
    //
    //   direkt zu Triton  -> decoupled, also `ModelStreamInfer`
    //   ueber den Governor -> unaer, denn genau das bietet Vigilant an
    //
    // Beides fuehrt zur selben Arbeit auf demselben Backend; Vigilant
    // uebersetzt intern auf den Stream-Aufruf. Wuerde der Client auch am
    // Gateway streamen, lehnte es ihn ab — bewusst, weil gestreamte Requests
    // die Frischelogik umgehen wuerden (Spec 16.1).
    let generator = {
        let direct = vig_backend_triton::TritonClient::new(llm_endpoint);
        let mut governed = connect(llm_endpoint).await;
        let model = qwen_model.to_owned();
        let finished = Arc::clone(&finished);
        let chars = Arc::clone(&chars);
        tokio::spawn(async move {
            while Instant::now().duration_since(origin) < duration {
                let request = text_request(&model, PROMPT, GENERATE_TOKENS);
                let outcome = if via_governor {
                    governed
                        .model_infer(request)
                        .await
                        .map(tonic::Response::into_inner)
                        .map_err(|e| e.to_string())
                } else {
                    direct
                        .infer_decoupled(request)
                        .await
                        .map_err(|e| e.to_string())
                };
                match outcome {
                    Ok(response) => {
                        finished.fetch_add(1, Ordering::Relaxed);
                        if let Some(text) = read_text_output(&response) {
                            chars.fetch_add(
                                u64::try_from(text.len()).unwrap_or(0),
                                Ordering::Relaxed,
                            );
                        }
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        })
    };

    let _ = detector.await;
    let _ = generator.await;

    let coverage = tracker
        .lock()
        .map_or(vig_sim::coverage::Coverage::default(), |t| t.finish());

    Outcome {
        label,
        detector_covered: coverage.covered,
        detector_total: coverage.total,
        detector_response_age_p95_ms: coverage.response_age_p95_ns / 1_000_000,
        generations_finished: finished.load(Ordering::Relaxed),
        generated_chars: chars.load(Ordering::Relaxed),
    }
}

async fn start_gateway(config: &Config) -> (String, vig_gateway::Handle) {
    let resolved = Arc::new(config.resolve().expect("aufloesbar"));
    let clock = MonotonicClock::start();
    let triton = Arc::new(vig_backend_triton::TritonClient::new(
        &resolved.backend_endpoint,
    ));
    let handle = actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler");
    let service = GatewayService::new(resolved, triton, handle.clone(), clock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Port");
    let address = listener.local_addr().expect("Adresse").to_string();
    tokio::spawn(async move {
        let stream = vig_bench::incoming(listener);
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
    tokio::time::sleep(Duration::from_millis(200)).await;
    (address, handle)
}

fn config_yaml(cooperative: bool) -> String {
    // Die Zerlegung plant mit gemessenen Groessen: Erzeugungsrate und fester
    // Sockel je Quantum. Auf anderer Hardware und mit anderem Modell sind es
    // andere Zahlen — `vig calibrate` misst sie, und den Sockel nennt
    // ausserdem die Kostenprobe am Anfang dieses Laufs.
    let block = if cooperative {
        format!(
            "\n    cooperative: {{ tokens_per_second: {}, min_tokens: 4, \
             max_total_tokens: {GENERATE_TOKENS}, base_cost_us: {} }}",
            env_u64_or("VIG_WP26_TOKENS_PER_SECOND", 242),
            env_u64_or("VIG_WP26_BASE_COST_US", 18_000),
        )
    } else {
        String::new()
    };
    let detector_endpoint = detector_endpoint();
    let llm_endpoint = llm_endpoint();
    let detector_model = detector_model();
    let llm_model = llm_model();
    // Das Profil des Detektors gehoert zur Maschine, nicht zum Werkzeug.
    let p50 = env_u64_or("VIG_WP26_DETECTOR_P50_US", 14_916);
    let p95 = env_u64_or("VIG_WP26_DETECTOR_P95_US", 17_081);
    let p99 = env_u64_or("VIG_WP26_DETECTOR_P99_US", 17_470);
    format!(
        r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "{detector_endpoint}"
  slots: 1
  pipelining_depth: 0
  safety_margin_percent: 110
models:
  detector:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 33, deadline_ms: 50, max_age_ms: 66 }}
    variants:
      - id: detector
        backend_model: {detector_model}
        quality: {{ value: 1.0, source: user_declared }}
        profile: {{ p50_us: {p50}, p95_us: {p95}, p99_us: {p99}, samples: 120 }}
  qwen:
    decoupled: true
    # Getrennter Server: das vLLM-Backend braucht einen anderen
    # Bibliotheksstand als onnxruntime und laesst sich nicht in denselben
    # Prozess legen. Die Slots modellieren trotzdem die GPU, nicht den Prozess.
    backend_endpoint: "{llm_endpoint}"
    class: best_effort
    queue: {{ policy: fifo, capacity: 2, overflow: backpressure_client }}
    contract: {{ deadline_ms: 20000, max_age_ms: 40000 }}{block}
    variants:
      - id: main
        backend_model: {llm_model}
        quality: {{ value: 1.0, source: user_declared }}
        profile: {{ p50_us: 1100000, p95_us: 1400000, p99_us: 1600000, samples: 100 }}
"#
    )
}

#[expect(clippy::print_stdout, reason = "Benchmark-Ausgabe")]
fn report(outcomes: &[Outcome]) {
    println!(
        "\n  Betriebsart                  | Detektor-Abdeckung | Antwortalter p95 | Generierungen | Zeichen"
    );
    println!(
        "  -----------------------------|--------------------|---------|---------------|--------"
    );
    for o in outcomes {
        let covered = o
            .detector_covered
            .saturating_mul(100)
            .checked_div(o.detector_total)
            .unwrap_or(0);
        println!(
            "  {:<28} | {covered:>16} % | {:>4} ms | {:>13} | {:>7}",
            o.label, o.detector_response_age_p95_ms, o.generations_finished, o.generated_chars
        );
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("Tokio-Runtime");
    runtime.block_on(run());
}

/// Misst, was ein Generierungsauftrag unabhaengig von seiner Laenge kostet.
///
/// ADR-0014 leitet die Quantengroesse aus `Token / Rate` ab. Diese Annahme
/// gilt nur, wenn die Kosten proportional zur Tokenzahl sind. Gibt es einen
/// festen Sockel je Auftrag — Prefill, Round-Trip, Scheduling im Backend —,
/// dann hat ein Quantum eine Mindestdauer, die keine Zerlegung unterschreitet.
async fn probe_quantum_cost() {
    let client = vig_backend_triton::TritonClient::new(llm_endpoint());
    let model = llm_model();
    println!("  Token je Auftrag | Dauer  | davon Sockel");
    println!("  -----------------|--------|-------------");
    let mut baseline = 0_u128;
    for tokens in [1_u32, 2, 4, 8, 16, 32, 64] {
        let mut total = 0_u128;
        let rounds = 5;
        for _ in 0..rounds {
            let request = text_request(&model, PROMPT, tokens);
            let started = Instant::now();
            if client.infer_decoupled(request).await.is_ok() {
                total += started.elapsed().as_millis();
            }
        }
        let mean = total / rounds;
        if tokens == 1 {
            baseline = mean;
        }
        println!("  {tokens:>16} | {mean:>4} ms | {baseline:>10} ms");
    }
    println!();
}

async fn run() {
    println!("wp26: loest die Zerlegung in Quanten die Aushungerung aus ADR-0012?");
    println!(
        "Detektor {} bei {DETECTOR_PERIOD_MS} ms, Sprachmodell {}, \
         ein Slot, {RUN_SECONDS} s je Betriebsart\n",
        detector_model(),
        llm_model()
    );

    let triton = vig_backend_triton::TritonClient::new(detector_endpoint());
    let metadata = triton
        .raw()
        .await
        .expect("Backend erreichbar")
        .model_metadata(ModelMetadataRequest {
            name: detector_model(),
            version: String::new(),
        })
        .await
        .expect("Metadaten")
        .into_inner();
    let input = metadata.inputs.first().expect("Eingabe");
    let shape: Vec<i64> = input
        .shape
        .iter()
        .enumerate()
        .map(|(i, d)| if *d < 0 && i == 0 { 1 } else { *d })
        .collect();
    let elements: i64 = shape.iter().copied().product();
    let byte_size = u64::try_from(elements).unwrap_or(0) * 4;
    let region = Region::create("vig_wp26", byte_size).expect("Shm-Region");
    let _ = triton
        .raw()
        .await
        .expect("Backend")
        .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
            name: region.name.clone(),
        })
        .await;
    triton
        .raw()
        .await
        .expect("Backend")
        .system_shared_memory_register(SystemSharedMemoryRegisterRequest {
            name: region.name.clone(),
            key: region.key.clone(),
            offset: 0,
            byte_size,
        })
        .await
        .expect("Shm registrieren");
    let spec = (
        input.name.clone(),
        input.datatype.clone(),
        shape,
        region.name.clone(),
        byte_size,
    );

    println!("\n  Kosten eines Generierungsauftrags nach Laenge:\n");
    Box::pin(probe_quantum_cost()).await;

    let mut outcomes = Vec::new();

    outcomes.push(
        measure(
            "direkt zu Triton",
            &detector_endpoint(),
            &llm_endpoint(),
            &detector_model(),
            &llm_model(),
            &spec,
            false,
        )
        .await,
    );

    let plain = Config::from_yaml(&config_yaml(false)).expect("Konfiguration");
    let (endpoint, _h1) = start_gateway(&plain).await;
    outcomes.push(
        measure(
            "Governor ohne Zerlegung",
            &endpoint,
            &endpoint,
            "detector",
            "qwen",
            &spec,
            true,
        )
        .await,
    );

    let chunked = Config::from_yaml(&config_yaml(true)).expect("Konfiguration");
    let (endpoint, h2) = start_gateway(&chunked).await;
    outcomes.push(
        measure(
            "Governor mit Zerlegung",
            &endpoint,
            &endpoint,
            "detector",
            "qwen",
            &spec,
            true,
        )
        .await,
    );

    report(&outcomes);

    if let Ok(m) = h2.metrics().await {
        println!(
            "\n  Mit Zerlegung: angenommen {} weitergereicht {} unmachbar-abgelehnt {} \
             kapazitaet-abgelehnt {} stale {} verspaetet {} zurueckgestellt {} \
             ausgehungert {}\n  Protected-Deadline-Misses {}",
            m.received,
            m.forwarded,
            m.rejected_infeasible,
            m.rejected_capacity,
            m.stale,
            m.dispatched_late,
            m.deferred_for_protected,
            m.best_effort_starved,
            m.protected_deadline_misses,
        );
    }
}
