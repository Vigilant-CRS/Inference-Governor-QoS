//! `mix-record` — Mischlast aus Bild, Sprache und Text auf einer GPU.
//!
//! Interner Versuchsaufbau: ein Detektor (Bilder), eine Spracherkennung
//! (Audioschnipsel) und ein Sprachmodell (Text) teilen sich eine GPU — einmal
//! direkt an den Backends, einmal ueber den Governor im selben Prozess. Die
//! beiden Arme laufen **nacheinander**, mit denselben Eingaben.
//!
//! Anders als `demo-record` zeichnet dieses Werkzeug keine Zeitleiste fuer ein
//! Video auf, sondern beantwortet die Frage, ob die Kette traegt: Wie viele
//! Auftraege jedes Stroms kamen an, wie viele wies der Governor ab, wie viele
//! scheiterten, und wie frisch waren die Ergebnisse
//! (`vig_bench::workload::drive`, dieselbe Messung wie `vig-fit`).
//!
//! ## Der Aufrufweg des Sprachmodells
//!
//! vLLM-Modelle sind in Triton **decoupled** und antworten nur ueber
//! `ModelStreamInfer`; ein unaerer Aufruf wird abgelehnt. Direkt geht dieses
//! Werkzeug deshalb ueber `TritonClient::infer_decoupled`, ueber den Governor
//! unaer — genau die Zusage, die Vigilant macht (wie `wp26`). Beides ergibt
//! dieselbe Arbeit auf demselben Backend.
//!
//! ## Umgebung
//!
//! * `VIG_MIX_CONFIG` — Governor-Konfiguration: der Detektor ist das Modell der
//!   Klasse `protected`, die Spracherkennung das erste Modell der Klasse `high`
//!   oder `normal`, das Sprachmodell das der Klasse `best_effort`.
//! * `VIG_MIX_ARM` — `direct`, `governed` oder `profile` (misst Spracherkennung
//!   und Sprachmodell einzeln und gibt Profilzeilen aus).
//! * `VIG_MIX_FRAMES` — Vorlage fuer die Rohbilder (RGB24, quadratisch), z. B.
//!   `demo/frames/krakow-{size}.raw`. `{size}` wird durch die Kantenlaenge
//!   ersetzt, die das jeweilige Modell laut Backend erwartet.
//! * `VIG_MIX_AUDIO` — Mono-Audio als `f32` bei 16 kHz, Schnipsel
//!   `VIG_MIX_CHUNK_MS` (1000).
//! * `VIG_MIX_SECONDS` (30), `VIG_MIX_PROMPT`, `VIG_MIX_TOKENS` (48),
//!   `VIG_MIX_SAMPLES` (110, nur `profile`), `VIG_MIX_OUT`, `VIG_MIX_LABEL`.
//! * `VIG_MIX_LLM_PERIOD_MS` (0) — Abstand zwischen zwei Anfragen an das
//!   Sprachmodell, gemessen ab Beginn der vorigen. Null ist Dauerbeschuss und
//!   damit die haerteste Last; ein echter Assistent wird alle paar Sekunden
//!   gefragt. Wer eine Frischezusage daneben bewertet, sollte die Rate
//!   angeben, die der Dienst wirklich hat.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vig_bench::pilot::frame_tensor;
use vig_bench::workload::{
    InputSpec, StreamDef, StreamReport, connect, drive, is_governor_refusal,
};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{ModelInferRequest, ModelInferResponse};

/// Abtastrate der Audioschnipsel.
const SAMPLE_RATE: usize = 16_000;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Ein Modell der Konfiguration, so wie der jeweilige Arm es anspricht.
#[derive(Debug, Clone)]
struct Target {
    name: String,
    model: String,
    endpoint: String,
    period_ms: u64,
    max_age_ms: u64,
}

fn target(config: &Config, name: &str) -> Target {
    let model = config
        .models
        .get(name)
        .unwrap_or_else(|| panic!("Modell {name} fehlt in der Konfiguration"));
    let variant = model.variants.first().expect("Variante");
    Target {
        name: name.to_owned(),
        model: variant.backend_model.clone(),
        endpoint: model
            .backend_endpoint
            .clone()
            .unwrap_or_else(|| config.backend.grpc_endpoint.clone()),
        period_ms: model.contract.period_ms.unwrap_or(1000),
        max_age_ms: model.contract.max_age_ms.unwrap_or(2000),
    }
}

fn by_class(config: &Config, class: &str) -> Vec<String> {
    config
        .models
        .iter()
        .filter(|(_, m)| m.class == class)
        .map(|(name, _)| name.clone())
        .collect()
}

/// Was ein Modell als Eingabe erwartet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Ein Bild mit dieser Kantenlaenge.
    Image(usize),
    Audio,
    Text,
}

/// Die Art eines Modells sagt das Backend selbst: ein Bild bringt vier
/// Dimensionen mit und damit seine Kantenlaenge, Audio heisst `AUDIO`, ein
/// Sprachmodell nimmt `text_input`. So muss die Konfiguration nichts
/// wiederholen, was Triton ohnehin weiss — und drei Detektoren mit 384, 512
/// und 576 Pixeln bekommen jeder das Bild, das zu ihm passt.
async fn kind_of(target: &Target) -> Kind {
    let client = vig_backend_triton::TritonClient::new(&target.endpoint);
    let meta = client
        .model_metadata(&target.model)
        .await
        .unwrap_or_else(|error| panic!("Metadaten fuer {}: {error}", target.model));
    for input in &meta.inputs {
        if input.name == "text_input" {
            return Kind::Text;
        }
        if input.name.eq_ignore_ascii_case("audio") {
            return Kind::Audio;
        }
        if input.shape.len() == 4 {
            let edge = input.shape.last().copied().unwrap_or_default();
            return Kind::Image(usize::try_from(edge).expect("Kantenlaenge"));
        }
    }
    panic!("unbekannte Eingabe fuer {}", target.model);
}

/// Rohbilder einer Kantenlaenge. `VIG_MIX_FRAMES` ist eine Vorlage: `{size}`
/// wird ersetzt, damit jede Bildgroesse ihre eigene Datei bekommt.
fn frames_of(pattern: &str, size: usize) -> Arc<Vec<Vec<u8>>> {
    let path = pattern.replace("{size}", &size.to_string());
    let raw = std::fs::read(&path).unwrap_or_else(|error| panic!("Rohbilder {path}: {error}"));
    let frames: Vec<Vec<u8>> = raw
        .chunks_exact(size * size * 3)
        .take(30)
        .map(|rgb| frame_tensor(rgb, size, size))
        .collect();
    assert!(!frames.is_empty(), "keine Rohbilder in {path}");
    Arc::new(frames)
}

fn image_input(size: usize, frames: Arc<Vec<Vec<u8>>>) -> InputSpec {
    InputSpec {
        name: "input".to_owned(),
        datatype: "FP32".to_owned(),
        shape: vec![1, 3, size as i64, size as i64],
        region: None,
        byte_size: (size * size * 3 * 4) as u64,
        payload: Some(frames),
    }
}

fn audio_input(chunk_samples: usize, chunks: Arc<Vec<Vec<u8>>>) -> InputSpec {
    InputSpec {
        name: "AUDIO".to_owned(),
        datatype: "FP32".to_owned(),
        shape: vec![chunk_samples as i64],
        region: None,
        byte_size: (chunk_samples * 4) as u64,
        payload: Some(chunks),
    }
}

/// Ein Textauftrag fuer das vLLM-Backend: Prompt und Samplingparameter als
/// laengenpraefigierte `BYTES`-Tensoren (wie `workload::build_text_request`).
fn text_request(model: &str, id: &str, prompt: &str, tokens: u32) -> ModelInferRequest {
    let sampling = format!("{{\"max_tokens\": {tokens}, \"temperature\": 0.0}}");
    let tensor = |name: &str| InferInputTensor {
        name: name.to_owned(),
        datatype: "BYTES".to_owned(),
        shape: vec![1],
        parameters: HashMap::new(),
        contents: None,
    };
    ModelInferRequest {
        model_name: model.to_owned(),
        id: id.to_owned(),
        inputs: vec![tensor("text_input"), tensor("sampling_parameters")],
        raw_input_contents: vec![
            vig_protocol_oip::bytes::encode_bytes_element(prompt.as_bytes()).expect("Prompt"),
            vig_protocol_oip::bytes::encode_bytes_element(sampling.as_bytes())
                .expect("Samplingparameter"),
        ],
        ..ModelInferRequest::default()
    }
}

fn text_output(response: &ModelInferResponse) -> Option<String> {
    let index = response
        .outputs
        .iter()
        .position(|o| o.name == "text_output")?;
    let raw = response.raw_output_contents.get(index)?;
    let bytes = vig_protocol_oip::bytes::decode_single_bytes_element(raw)?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Was das Sprachmodell in einem Lauf erlebt hat.
#[derive(Debug, Default, Clone)]
struct LlmReport {
    sent: u64,
    delivered: u64,
    refused: u64,
    errors: u64,
    chars: u64,
    latencies_ms: Vec<u64>,
}

impl LlmReport {
    fn quantile(&self, percent: usize) -> u64 {
        if self.latencies_ms.is_empty() {
            return 0;
        }
        let mut sorted = self.latencies_ms.clone();
        sorted.sort_unstable();
        sorted[(sorted.len() * percent / 100).min(sorted.len() - 1)]
    }
}

/// Faehrt das Sprachmodell bis die Zeit um ist, mit `pace` Abstand zwischen
/// zwei Anfragen.
///
/// `pace` null heisst Dauerbeschuss: die naechste Anfrage geht ab, sobald die
/// vorige beantwortet ist. Das ist die haerteste Last — aber **nicht** die
/// eines echten Assistenten, der alle paar Sekunden gefragt wird. Wer eine
/// Frischezusage neben einem Sprachmodell bewerten will, misst mit der Rate,
/// die der Dienst wirklich hat; sonst misst er die Saettigung seines eigenen
/// Treibers und schreibt sie dem Governor zu.
async fn drive_llm(
    target: &Target,
    endpoint: &str,
    governed: bool,
    prompt: &str,
    tokens: u32,
    duration: Duration,
    pace: Duration,
) -> LlmReport {
    let direct = vig_backend_triton::TritonClient::new(endpoint);
    let mut unary = connect(endpoint).await;
    let mut report = LlmReport::default();
    let origin = Instant::now();
    let mut n = 0_u64;
    while origin.elapsed() < duration {
        n += 1;
        // Ueber den Governor gilt der logische Name der Konfiguration, direkt
        // der Name des Backends — wie bei den Bildstroemen auch.
        let name = if governed {
            &target.name
        } else {
            &target.model
        };
        let request = text_request(name, &format!("llm-{n}"), prompt, tokens);
        let started = Instant::now();
        report.sent += 1;
        let outcome = if governed {
            unary
                .model_infer(request)
                .await
                .map(tonic::Response::into_inner)
                .map_err(|status| (is_governor_refusal(&status), status.message().to_owned()))
        } else {
            direct
                .infer_decoupled(request)
                .await
                .map_err(|error| (false, error.to_string()))
        };
        match outcome {
            Ok(response) => {
                report.delivered += 1;
                report
                    .latencies_ms
                    .push(started.elapsed().as_millis() as u64);
                if let Some(text) = text_output(&response) {
                    report.chars += text.len() as u64;
                }
            }
            Err((refused, reason)) => {
                if refused {
                    report.refused += 1;
                } else {
                    report.errors += 1;
                    // Der erste Fehler sagt, woran die Kette haengt; alle
                    // weiteren zu drucken verdeckt ihn nur.
                    if report.errors == 1 {
                        eprintln!("  Sprachmodell, erster Fehler: {reason}");
                    }
                }
                // Eine Ablehnung ist kein Grund, das Backend zu bestuermen.
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        // Der Takt gilt ab dem **Beginn** der Anfrage, nicht ab ihrem Ende:
        // sonst haengt die Rate an der Antwortzeit, und genau die soll der
        // Governor ja veraendern duerfen. Dauert eine Antwort laenger als der
        // Takt, geht die naechste sofort — der Treiber holt nicht nach.
        let rest = pace.saturating_sub(started.elapsed());
        if !rest.is_zero() {
            tokio::time::sleep(rest).await;
        }
    }
    report
}

/// Ein Governor im Prozess (wie in `vig-fit` und `demo-record`).
struct RunningGateway {
    address: String,
    handle: vig_gateway::Handle,
    /// Die aufgeloeste Konfiguration — sie haelt die Modellnamen in der
    /// Reihenfolge, in der die Metrikreihen indiziert sind.
    resolved: Arc<vig_config::schema::Resolved>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
}

impl RunningGateway {
    /// Der Metrikabzug des Governors, mit den Modellnamen in Indexreihenfolge.
    ///
    /// Ohne diese Zahlen bleibt jede Erklaerung fuer eine verfehlte Zusage
    /// eine Vermutung: erst sie sagen, ob ein Strom im Rueckstand war und
    /// warum seine Auftraege abgewiesen wurden.
    async fn metrics(&self) -> Option<(vig_core::Metrics, Vec<String>)> {
        let metrics = self.handle.metrics().await.ok()?;
        Some((metrics, self.resolved.model_names.clone()))
    }

    async fn start(resolved: Arc<vig_config::schema::Resolved>) -> Self {
        let triton = Arc::new(vig_backend_triton::TritonClient::new(
            &resolved.backend_endpoint,
        ));
        let clock = MonotonicClock::start();
        let handle =
            actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
        let mine = Arc::clone(&resolved);
        let service = GatewayService::new(resolved, triton, handle.clone(), clock);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Port");
        let address = listener.local_addr().expect("Adresse").to_string();
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let stream = vig_bench::incoming(listener);
            let _ = tonic::transport::Server::builder()
                .initial_stream_window_size(vig_backend_triton::STREAM_WINDOW_BYTES)
                .initial_connection_window_size(vig_backend_triton::CONNECTION_WINDOW_BYTES)
                .add_service(
                    GrpcInferenceServiceServer::new(service)
                        .max_decoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
                        .max_encoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES),
                )
                .serve_with_incoming_shutdown(stream, async {
                    let _ = stopped.await;
                })
                .await;
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        Self {
            address,
            handle,
            resolved: mine,
            shutdown,
            server,
        }
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = self.server.await;
    }
}

fn report_json(report: &StreamReport) -> serde_json::Value {
    let coverage = &report.coverage;
    serde_json::json!({
        "stream": report.name,
        "emitted": report.emitted,
        "sent": report.sent,
        "delivered": report.delivered,
        "refused": report.refused,
        "errors": report.errors,
        "client_dropped": report.client_dropped,
        "connected": report.connected,
        "uncovered_permille": coverage.consumer_uncovered_permille(),
        "longest_gap_ms": coverage.longest_gap_ns / 1_000_000,
        "response_age_p50_ms": coverage.response_age_p50_ns / 1_000_000,
        "response_age_p95_ms": coverage.response_age_p95_ns / 1_000_000,
    })
}

/// Misst ein Modell einzeln und gibt die Profilzeile fuer die Konfiguration aus.
async fn profile(
    target: &Target,
    unary: bool,
    payload: Option<InputSpec>,
    prompt: &str,
    tokens: u32,
    samples: usize,
) {
    let mut latencies = Vec::new();
    let direct = vig_backend_triton::TritonClient::new(&target.endpoint);
    let mut client = connect(&target.endpoint).await;
    for i in 0..samples + 10 {
        let started = Instant::now();
        let ok = if unary {
            let spec = payload.as_ref().expect("Eingabe");
            let index = i % spec.payload.as_ref().map_or(1, |p| p.len());
            let request = ModelInferRequest {
                model_name: target.model.clone(),
                id: format!("profile-{i}"),
                inputs: vec![InferInputTensor {
                    name: spec.name.clone(),
                    datatype: spec.datatype.clone(),
                    shape: spec.shape.clone(),
                    parameters: HashMap::new(),
                    contents: None,
                }],
                raw_input_contents: vec![
                    spec.payload
                        .as_ref()
                        .and_then(|p| p.get(index).cloned())
                        .expect("Nutzlast"),
                ],
                ..ModelInferRequest::default()
            };
            client.model_infer(request).await.is_ok()
        } else {
            let request = text_request(&target.model, &format!("profile-{i}"), prompt, tokens);
            direct.infer_decoupled(request).await.is_ok()
        };
        if !ok {
            eprintln!("  {} Aufruf {i} gescheitert", target.name);
        }
        if i >= 10 {
            latencies.push(started.elapsed().as_micros() as u64);
        }
    }
    latencies.sort_unstable();
    let q = |p: usize| latencies[(latencies.len() * p / 100).min(latencies.len() - 1)];
    println!(
        "  {:<10} profile: {{ p50_us: {}, p95_us: {}, p99_us: {}, samples: {} }}",
        target.name,
        q(50),
        q(95),
        q(99),
        latencies.len()
    );
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("Tokio-Runtime");
    runtime.block_on(run());
}

async fn run() {
    let arm = env("VIG_MIX_ARM").unwrap_or_default();
    let governed = match arm.as_str() {
        "direct" | "profile" => false,
        "governed" => true,
        _ => {
            eprintln!("VIG_MIX_ARM muss direct, governed oder profile sein.");
            std::process::exit(2);
        }
    };
    let profile_only = arm == "profile";
    let config_path = env("VIG_MIX_CONFIG").expect("VIG_MIX_CONFIG");
    let config = Config::from_yaml(&std::fs::read_to_string(&config_path).expect("Konfiguration"))
        .expect("Konfiguration gueltig");
    let findings = config.diagnose();
    if !findings.is_empty() && !profile_only {
        eprintln!("Die Konfiguration hat offene Befunde:");
        for finding in &findings {
            eprintln!("  {finding}");
        }
        std::process::exit(2);
    }

    // Alle Modelle der Konfiguration, nach Wichtigkeit geordnet — beliebig
    // viele Kameras, Detektoren, Spracherkennungen und Sprachmodelle. Der
    // Bericht liest sich dann von oben nach unten wie die Rangfolge.
    let mut targets: Vec<Target> = Vec::new();
    for class in ["protected", "high", "normal", "best_effort"] {
        for name in by_class(&config, class) {
            targets.push(target(&config, &name));
        }
    }
    assert!(!targets.is_empty(), "keine Modelle in der Konfiguration");

    let seconds: u64 = env("VIG_MIX_SECONDS").map_or(30, |v| v.parse().expect("VIG_MIX_SECONDS"));
    let chunk_ms: usize =
        env("VIG_MIX_CHUNK_MS").map_or(1000, |v| v.parse().expect("VIG_MIX_CHUNK_MS"));
    let tokens: u32 = env("VIG_MIX_TOKENS").map_or(48, |v| v.parse().expect("VIG_MIX_TOKENS"));
    // Ohne Angabe Dauerbeschuss — die bisherige Betriebsart, damit aeltere
    // Messungen vergleichbar bleiben.
    let llm_pace = Duration::from_millis(
        env("VIG_MIX_LLM_PERIOD_MS").map_or(0, |v| v.parse().expect("VIG_MIX_LLM_PERIOD_MS")),
    );
    let samples: usize =
        env("VIG_MIX_SAMPLES").map_or(110, |v| v.parse().expect("VIG_MIX_SAMPLES"));
    let prompt = env("VIG_MIX_PROMPT").unwrap_or_else(|| {
        "The robot reports: two people ahead, a van blocking the ramp. Answer in one sentence."
            .to_owned()
    });

    // Die Art jedes Modells kommt vom Backend, nicht aus der Konfiguration.
    let mut kinds = Vec::new();
    for t in &targets {
        kinds.push(kind_of(t).await);
    }

    // Bilder: nur so viele, wie eine Sekunde braucht — die Nutzlast reist im
    // Request, und ein ganzer Clip im Speicher waeren Gigabyte. Je
    // Kantenlaenge eine Datei, denn nano, small und medium erwarten
    // verschiedene Groessen.
    let pattern = env("VIG_MIX_FRAMES").expect("VIG_MIX_FRAMES");
    let mut images: HashMap<usize, Arc<Vec<Vec<u8>>>> = HashMap::new();
    for kind in &kinds {
        if let Kind::Image(edge) = *kind {
            images
                .entry(edge)
                .or_insert_with(|| frames_of(&pattern, edge));
        }
    }

    // Audio: Schnipsel von `chunk_ms`, als f32-Bytes wie sie im Request reisen.
    let chunk_samples = SAMPLE_RATE * chunk_ms / 1000;
    let audio_chunks = if kinds.contains(&Kind::Audio) {
        let audio = std::fs::read(env("VIG_MIX_AUDIO").expect("VIG_MIX_AUDIO")).expect("Audio");
        let chunks: Vec<Vec<u8>> = audio
            .chunks_exact(chunk_samples * 4)
            .map(<[u8]>::to_vec)
            .collect();
        assert!(!chunks.is_empty(), "kein Audio");
        Arc::new(chunks)
    } else {
        Arc::new(Vec::new())
    };

    let input_for = |kind: Kind| -> Option<InputSpec> {
        match kind {
            Kind::Image(edge) => images
                .get(&edge)
                .map(|frames| image_input(edge, Arc::clone(frames))),
            Kind::Audio => Some(audio_input(chunk_samples, Arc::clone(&audio_chunks))),
            Kind::Text => None,
        }
    };

    if profile_only {
        println!("Profile, {samples} Aufrufe je Modell, direkt an den Backends:");
        for (t, kind) in targets.iter().zip(&kinds) {
            // Geboxt: die OIP-Typen machen das Future groesser als der Stack
            // eines Aufrufers vertragen soll (clippy.toml, 4096 Bytes).
            Box::pin(profile(
                t,
                *kind != Kind::Text,
                input_for(*kind),
                &prompt,
                tokens,
                samples,
            ))
            .await;
        }
        return;
    }

    let gateway = if governed {
        let resolved = Arc::new(config.resolve().expect("aufloesbar"));
        Some(RunningGateway::start(resolved).await)
    } else {
        None
    };
    let endpoint_of = |t: &Target| -> String {
        gateway
            .as_ref()
            .map_or_else(|| t.endpoint.clone(), |g| g.address.clone())
    };
    let model_of = |t: &Target| -> &'static str {
        let name = if governed {
            t.name.clone()
        } else {
            t.model.clone()
        };
        Box::leak(name.into_boxed_str())
    };

    let duration = Duration::from_secs(seconds);
    println!(
        "Arm {} · {} s · {} Modelle · {} Token",
        if governed { "governed" } else { "direct" },
        seconds,
        targets.len(),
        tokens
    );
    for (t, kind) in targets.iter().zip(&kinds) {
        let art = match *kind {
            Kind::Image(edge) => format!("Bild {edge} px, {} ms", t.period_ms),
            Kind::Audio => format!("Audio, {} ms", t.period_ms),
            Kind::Text => "Text".to_owned(),
        };
        println!("  {:<10} {:<22} {art}", t.name, t.model);
    }

    // Direkt liegen die Modelle auf mehreren Servern, ueber den Governor auf
    // einem. Deshalb faehrt jeder Endpunkt seinen eigenen Treiber,
    // gleichzeitig; die Sprachmodelle laufen als eigene Auftraege daneben.
    let mut groups: Vec<(String, Vec<StreamDef>)> = Vec::new();
    let mut llm_tasks: Vec<(String, tokio::task::JoinHandle<LlmReport>)> = Vec::new();
    for (t, kind) in targets.iter().zip(&kinds) {
        if *kind == Kind::Text {
            let endpoint = endpoint_of(t);
            let llm_target = t.clone();
            let llm_prompt = prompt.clone();
            llm_tasks.push((
                t.name.clone(),
                tokio::spawn(async move {
                    Box::pin(drive_llm(
                        &llm_target,
                        &endpoint,
                        governed,
                        &llm_prompt,
                        tokens,
                        duration,
                        llm_pace,
                    ))
                    .await
                }),
            ));
            continue;
        }
        let stream = StreamDef {
            name: Box::leak(t.name.clone().into_boxed_str()),
            model: model_of(t),
            period: Duration::from_millis(t.period_ms),
            max_age: Duration::from_millis(t.max_age_ms),
            in_flight_cap: if *kind == Kind::Audio { 2 } else { 4 },
            input: input_for(*kind),
            text: None,
            pump: false,
            burst: None,
        };
        let endpoint = endpoint_of(t);
        match groups.iter_mut().find(|(e, _)| *e == endpoint) {
            Some((_, streams)) => streams.push(stream),
            None => groups.push((endpoint, vec![stream])),
        }
    }
    let mut tasks = Vec::new();
    for (endpoint, streams) in groups {
        tasks.push(tokio::spawn(async move {
            drive(&endpoint, &streams, duration, governed).await
        }));
    }

    let mut reports = Vec::new();
    for task in tasks {
        reports.extend(task.await.expect("Treiber"));
    }
    let mut llm_reports = Vec::new();
    for (name, task) in llm_tasks {
        llm_reports.push((name, task.await.expect("Sprachmodell")));
    }
    reports.sort_by_key(|r| r.name);

    // Was der Governor selbst gesehen hat — abgefragt **vor** dem Stoppen,
    // danach gibt es die Zahlen nicht mehr.
    let governor = match gateway.as_ref() {
        Some(g) => g.metrics().await,
        None => None,
    };

    for report in &reports {
        println!(
            "  {:<10} gesendet {:>4} geliefert {:>4} abgewiesen {:>4} Fehler {:>3} unabgedeckt {:>4} ‰ Alter p50 {:>5} ms",
            report.name,
            report.sent,
            report.delivered,
            report.refused,
            report.errors,
            report.coverage.consumer_uncovered_permille(),
            report.coverage.response_age_p50_ns / 1_000_000,
        );
    }
    for (name, report) in &llm_reports {
        println!(
            "  {:<10} gesendet {:>4} geliefert {:>4} abgewiesen {:>4} Fehler {:>3} Zeichen {:>6} Dauer p50 {:>5} ms",
            name,
            report.sent,
            report.delivered,
            report.refused,
            report.errors,
            report.chars,
            report.quantile(50),
        );
    }

    if let Some((metrics, names)) = governor.as_ref() {
        println!("  — was der Governor sah —");
        for (i, name) in names.iter().enumerate() {
            let cell = |row: &[u32; vig_core::ids::MAX_MODELS]| row.get(i).copied().unwrap_or(0);
            let coverage = cell(&metrics.objective_coverage_permille);
            let deficit = cell(&metrics.objective_deficit_us);
            let slack = cell(&metrics.objective_slack_us);
            if coverage == 0 && deficit == 0 && slack == 0 {
                continue;
            }
            println!(
                "  {name:<10} Zusage erfuellt {coverage:>4} ‰  Luft {:>5} ms  Rueckstand {:>5} ms  Luecke {:>5} ms",
                slack / 1_000,
                deficit / 1_000,
                cell(&metrics.objective_gap_us) / 1_000,
            );
        }
        // Die Trennfrage: hat der Look-ahead zurueckgehalten, oder waren die
        // Slots voll?
        println!(
            "  abgewiesen: nicht machbar {}, keine Kapazitaet {}, ueberholt {}, veraltet {}",
            metrics.rejected_infeasible,
            metrics.rejected_capacity,
            metrics.superseded,
            metrics.stale,
        );
        println!(
            "  Rechenzeit {} ms, davon vergeblich {} ms — verfuegbar waren {} ms",
            metrics.total_compute_nanos / 1_000_000,
            metrics.stale_compute_nanos / 1_000_000,
            seconds * 1_000 * metrics.slots,
        );
        // ADR-0014: der Fixaufwand faellt **je Quantum** an. Er ist damit der
        // direkte Nachweis, ob wirklich zerlegt wurde — und wie fein.
        println!(
            "  Zerlegung: Sockel {} ms, Prefill {} ms, Dekodierung {} ms, laengster Kontext {} Token, abgelehnt {}",
            metrics.generative_fixed_us / 1_000,
            metrics.generative_prefill_us / 1_000,
            metrics.generative_decode_us / 1_000,
            metrics.generative_context_tokens,
            metrics.decomposition_refused,
        );
    }

    if let Some(path) = env("VIG_MIX_OUT") {
        let value = serde_json::json!({
            "arm": if governed { "governed" } else { "direct" },
            "governor": governor.as_ref().map(|(m, names)| serde_json::json!({
                "rejected_infeasible": m.rejected_infeasible,
                "rejected_capacity": m.rejected_capacity,
                "superseded": m.superseded,
                "stale": m.stale,
                "total_compute_ms": m.total_compute_nanos / 1_000_000,
                "stale_compute_ms": m.stale_compute_nanos / 1_000_000,
                "slots": m.slots,
                "models": names.iter().enumerate().map(|(i, n)| serde_json::json!({
                    "model": n,
                    "coverage_permille": m.objective_coverage_permille.get(i).copied().unwrap_or(0),
                    "gap_us": m.objective_gap_us.get(i).copied().unwrap_or(0),
                    "slack_us": m.objective_slack_us.get(i).copied().unwrap_or(0),
                    "deficit_us": m.objective_deficit_us.get(i).copied().unwrap_or(0),
                })).collect::<Vec<_>>(),
            })),
            "label": env("VIG_MIX_LABEL"),
            "seconds": seconds,
            "config": config_path,
            "tokens": tokens,
            "prompt": prompt,
            "streams": reports.iter().map(report_json).collect::<Vec<_>>(),
            "llms": llm_reports.iter().map(|(name, r)| serde_json::json!({
                "stream": name,
                "sent": r.sent,
                "delivered": r.delivered,
                "refused": r.refused,
                "errors": r.errors,
                "chars": r.chars,
                "latency_p50_ms": r.quantile(50),
                "latency_p95_ms": r.quantile(95),
            })).collect::<Vec<_>>(),
        });
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&value).expect("JSON") + "\n",
        )
        .expect("Bericht schreibbar");
        println!("Bericht: {path}");
    }
    if let Some(gateway) = gateway {
        gateway.stop().await;
    }
}
