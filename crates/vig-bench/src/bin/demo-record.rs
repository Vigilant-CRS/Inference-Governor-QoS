//! `demo-record` — nimmt die Zeitleiste fuer das Demo-Video auf.
//!
//! Ein Clip wird als Kamera abgespielt. Detektorstroeme und ein
//! Sprachbildmodell bekommen seine Bilder — einmal direkt an Triton, einmal
//! ueber den Governor im selben Prozess. Die beiden Arme laufen
//! **nacheinander** auf derselben GPU mit denselben Frames: Gleichzeitig
//! wuerden sie sich gegenseitig bremsen, und der Vergleich waere keiner.
//!
//! Jede Antwort, jede Ablehnung und jeder Fehler landet als JSON-Zeile in der
//! Zeitleiste. `tools/demo/render.py` setzt daraus das Video zusammen und zeigt
//! zu jedem Bild nur, was zu diesem Zeitpunkt tatsaechlich angekommen war.
//!
//! ## Konfiguration
//!
//! Aus `VIG_DEMO_CONFIG`:
//!
//! * der **angezeigte Detektor** ist das Modell der Klasse `protected`;
//! * jedes Modell der Klassen `high` oder `normal` ist eine **weitere Kamera**
//!   mit demselben Bildstrom, zeitlich versetzt — Last, die im Video nicht zu
//!   sehen ist, aber dieselbe GPU braucht (Zeilen `detector_aux`);
//! * das **Sprachbildmodell** ist das Modell der Klasse `best_effort`.
//!
//! Der direkte Arm nimmt je Modell Endpunkt und `backend_model` der ersten
//! Variante; der Governor-Arm schickt die logischen Namen an den Governor.
//!
//! ## Umgebung
//!
//! * `VIG_DEMO_CONFIG`, `VIG_DEMO_ARM` (`direct`, `governed`, oder
//!   `profile-vlm`: nur das Profil des Sprachbildmodells messen).
//! * `VIG_DEMO_FRAMES` — Rohbilder RGB24, quadratisch, hintereinander;
//!   Kantenlaenge `VIG_DEMO_SIZE` (512).
//! * `VIG_DEMO_OUT` — Zeitleiste (JSONL).
//! * `VIG_DEMO_SOURCE_FPS` (30), `VIG_DEMO_PROMPT`, `VIG_DEMO_TOKENS` (16),
//!   `VIG_DEMO_VLM_GAP_MS` (0: die naechste Frage, sobald die Antwort da ist),
//!   `VIG_DEMO_LABEL`, `VIG_DEMO_GPU`, `VIG_DEMO_COMMIT`.
//!
//! ## Zeitleiste
//!
//! Erste Zeile `{"type":"header",...}`, danach je Auftrag eine Zeile
//! `{"type":"detector"|"detector_aux"|"vlm","frame","capture_ms","send_ms",
//! "done_ms","status",...}`. Zeiten in Millisekunden seit Frame 0 auf der Uhr
//! dieses Laufs; Frame `k` wurde bei `k · 1000 / fps` aufgenommen. `status` ist
//! `ok`, `refused:<grund>` (der Governor hat absichtlich abgewiesen),
//! `client_dropped` (der Client hatte schon vier offene Auftraege) oder
//! `error:<text>`.

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
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tonic::transport::Channel;
use vig_bench::pilot::{decode_rfdetr, f32_output, frame_tensor};
use vig_bench::workload::{connect, is_governor_refusal};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient;
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::infer_parameter::ParameterChoice;
use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{InferParameter, ModelInferRequest, ModelInferResponse};
use vig_protocol_oip::params::P_AGE_US;

/// Kantenlaenge der Rohbilder: `VIG_DEMO_SIZE`, Vorgabe 512. Sie muss zur
/// Eingabe des Detektors passen (RF-DETR Small 512, Medium 576, Nano 384).
fn frame_size() -> usize {
    static SIZE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| env("VIG_DEMO_SIZE").map_or(512, |v| v.parse().expect("VIG_DEMO_SIZE")))
}

/// Bytes je Rohbild.
fn frame_bytes() -> usize {
    frame_size() * frame_size() * 3
}

/// Klassen im Ausgabetensor der oeffentlichen RF-DETR-Gewichte (COCO).
const CLASSES: usize = 91;
/// Ab dieser Konfidenz zaehlt eine Detektion.
const SCORE: f32 = 0.5;
/// Hoechstens so viele offene Auftraege je Detektorstrom — ein Client mit
/// endlichem Puffer, wie in `workload::drive`.
const IN_FLIGHT_CAP: usize = 4;

type Client = GrpcInferenceServiceClient<Channel>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    Direct,
    Governed,
}

/// Was ein Strom zum Senden braucht.
#[derive(Debug, Clone)]
struct Target {
    /// Logischer Name in der Konfiguration.
    name: String,
    /// Modellname im Request: logisch ueber den Governor, physisch direkt.
    model: String,
    endpoint: String,
    period_ms: Option<u64>,
    max_age_ms: Option<u64>,
    backend_model: String,
}

/// Die Zeitleiste, von mehreren Aufgaben beschrieben.
#[derive(Clone)]
struct Timeline(Arc<Mutex<std::io::BufWriter<std::fs::File>>>);

impl Timeline {
    fn write(&self, value: &serde_json::Value) {
        let mut out = self.0.lock().expect("Zeitleiste");
        serde_json::to_writer(&mut *out, value).expect("JSON");
        out.write_all(b"\n").expect("Zeitleiste schreibbar");
    }
}

/// Die Uhr eines Laufs: Frame `k` wurde bei `origin + k / fps` aufgenommen.
#[derive(Clone, Copy)]
struct Clock {
    origin: Instant,
    fps: f64,
    frame_count: u64,
    length: Duration,
}

impl Clock {
    fn frame_at(self, at: Instant) -> u64 {
        ((at.duration_since(self.origin).as_secs_f64() * self.fps) as u64).min(self.frame_count - 1)
    }

    fn capture_of(self, frame: u64) -> Instant {
        self.origin + Duration::from_secs_f64(frame as f64 / self.fps)
    }

    fn ms(self, at: Instant) -> f64 {
        (at.duration_since(self.origin).as_secs_f64() * 10_000.0).round() / 10.0
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn age_parameter(age: Duration) -> HashMap<String, InferParameter> {
    let mut parameters = HashMap::new();
    parameters.insert(
        P_AGE_US.to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(
                i64::try_from(age.as_micros()).unwrap_or(i64::MAX),
            )),
        },
    );
    parameters
}

fn detector_request(
    target: &Target,
    id: String,
    rgb: &[u8],
    age: Option<Duration>,
) -> ModelInferRequest {
    ModelInferRequest {
        model_name: target.model.clone(),
        id,
        parameters: age.map(age_parameter).unwrap_or_default(),
        inputs: vec![InferInputTensor {
            name: "input".to_owned(),
            datatype: "FP32".to_owned(),
            shape: vec![1, 3, frame_size() as i64, frame_size() as i64],
            parameters: HashMap::new(),
            contents: None,
        }],
        raw_input_contents: vec![frame_tensor(rgb, frame_size(), frame_size())],
        ..ModelInferRequest::default()
    }
}

fn vlm_request(
    target: &Target,
    id: String,
    rgb: &[u8],
    prompt: &str,
    tokens: i32,
    age: Option<Duration>,
) -> ModelInferRequest {
    let tensor = |name: &str, datatype: &str, shape: Vec<i64>| InferInputTensor {
        name: name.to_owned(),
        datatype: datatype.to_owned(),
        shape,
        parameters: HashMap::new(),
        contents: None,
    };
    ModelInferRequest {
        model_name: target.model.clone(),
        id,
        parameters: age.map(age_parameter).unwrap_or_default(),
        inputs: vec![
            tensor(
                "IMAGE",
                "UINT8",
                vec![frame_size() as i64, frame_size() as i64, 3],
            ),
            tensor("PROMPT", "BYTES", vec![1]),
            tensor("MAX_TOKENS", "INT32", vec![1]),
        ],
        raw_input_contents: vec![
            rgb.to_vec(),
            vig_protocol_oip::bytes::encode_bytes_element(prompt.as_bytes()).expect("Prompt"),
            tokens.to_le_bytes().to_vec(),
        ],
        ..ModelInferRequest::default()
    }
}

/// `refused:<grund>` oder `error:<code>: <text>`.
fn status_text(status: &tonic::Status) -> String {
    let reason = status
        .metadata()
        .get("vig-reason")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if is_governor_refusal(status) {
        let reason = if reason.is_empty() {
            "backpressure"
        } else {
            reason
        };
        format!("refused:{reason}")
    } else {
        let message: String = status.message().chars().take(160).collect();
        format!("error:{:?}: {message}", status.code())
    }
}

fn boxes(response: &ModelInferResponse) -> Option<Vec<serde_json::Value>> {
    let dets = f32_output(response, "dets")?;
    let labels = f32_output(response, "labels")?;
    let round = |v: f32| (f64::from(v.clamp(0.0, 1.0)) * 10_000.0).round() / 10_000.0;
    Some(
        decode_rfdetr(&dets, &labels, CLASSES, SCORE)
            .into_iter()
            .map(|d| {
                serde_json::json!([
                    d.class,
                    (f64::from(d.score) * 1000.0).round() / 1000.0,
                    round(d.bbox.x1),
                    round(d.bbox.y1),
                    round(d.bbox.x2),
                    round(d.bbox.y2)
                ])
            })
            .collect(),
    )
}

fn text(response: &ModelInferResponse) -> Option<String> {
    let index = response.outputs.iter().position(|o| o.name == "TEXT")?;
    let raw = response.raw_output_contents.get(index)?;
    let bytes = vig_protocol_oip::bytes::decode_single_bytes_element(raw)?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Ein Governor im Prozess (wie in `vig-fit`).
struct RunningGateway {
    address: String,
    _handle: vig_gateway::Handle,
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
}

impl RunningGateway {
    async fn start(resolved: Arc<vig_config::schema::Resolved>) -> Self {
        let triton = Arc::new(vig_backend_triton::TritonClient::new(
            &resolved.backend_endpoint,
        ));
        let clock = MonotonicClock::start();
        let handle =
            actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
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
            _handle: handle,
            shutdown,
            server,
        }
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = self.server.await;
    }
}

fn targets(config: &Config, classes: &[&str]) -> Vec<Target> {
    config
        .models
        .iter()
        .filter(|(_, m)| classes.contains(&m.class.as_str()))
        .map(|(name, model)| {
            let variant = model.variants.first().expect("Variante");
            Target {
                name: name.clone(),
                model: variant.backend_model.clone(),
                endpoint: model
                    .backend_endpoint
                    .clone()
                    .unwrap_or_else(|| config.backend.grpc_endpoint.clone()),
                period_ms: model.contract.period_ms,
                max_age_ms: model.contract.max_age_ms,
                backend_model: variant.backend_model.clone(),
            }
        })
        .collect()
}

/// Misst das Sprachbildmodell direkt — 10 Aufrufe Aufwaermen, dann 100
/// nacheinander ueber denselben gRPC-Weg wie die Aufnahme — und gibt das
/// Profil als Zeile fuer die Konfiguration aus.
async fn profile_vlm(vlm: &Target, frames: &[u8], prompt: &str, tokens: i32) {
    let mut client = connect(&vlm.endpoint).await;
    let frame_count = frames.len() / frame_bytes();
    let mut samples = Vec::new();
    for i in 0..110_usize {
        let offset = (i * 7 % frame_count) * frame_bytes();
        let request = vlm_request(
            vlm,
            format!("profile-{i}"),
            &frames[offset..offset + frame_bytes()],
            prompt,
            tokens,
            None,
        );
        let start = Instant::now();
        client
            .model_infer(request)
            .await
            .expect("Sprachbildmodell antwortet");
        if i >= 10 {
            samples.push(start.elapsed().as_micros() as u64);
        }
    }
    samples.sort_unstable();
    let q = |p: usize| samples[(samples.len() * p / 100).min(samples.len() - 1)];
    println!(
        "profile: {{ p50_us: {}, p95_us: {}, p99_us: {}, samples: {} }}",
        q(50),
        q(95),
        q(99),
        samples.len()
    );
}

/// Ein Detektorstrom im Takt seiner Vertragsperiode, jeweils mit dem
/// aktuellen Bild — bei weiteren Kameras um `offset` Frames versetzt.
async fn detector_stream(
    det: Target,
    displayed: bool,
    offset: u64,
    client: Client,
    frames: Arc<Vec<u8>>,
    timeline: Timeline,
    clock: Clock,
    governed: bool,
) {
    let period = Duration::from_millis(det.period_ms.expect("Detektor braucht period_ms"));
    let kind = if displayed {
        "detector"
    } else {
        "detector_aux"
    };
    let permits = Arc::new(tokio::sync::Semaphore::new(IN_FLIGHT_CAP));
    let mut inflight = tokio::task::JoinSet::new();
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut n = 0_u64;
    loop {
        ticker.tick().await;
        while inflight.try_join_next().is_some() {}
        let send = Instant::now();
        if send.duration_since(clock.origin) >= clock.length {
            break;
        }
        let frame = clock.frame_at(send);
        let capture = clock.capture_of(frame);
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            timeline.write(&serde_json::json!({
                "type": kind, "stream": det.name, "frame": frame,
                "capture_ms": clock.ms(capture), "send_ms": clock.ms(send),
                "done_ms": clock.ms(send), "status": "client_dropped",
            }));
            continue;
        };
        let frames = Arc::clone(&frames);
        let timeline = timeline.clone();
        let mut client = client.clone();
        let det = det.clone();
        n += 1;
        let id = format!("{}-{n}", det.name);
        let source = (frame + offset) % clock.frame_count;
        inflight.spawn(async move {
            let _permit = permit;
            let at = source as usize * frame_bytes();
            let request = detector_request(
                &det,
                id,
                &frames[at..at + frame_bytes()],
                governed.then(|| Instant::now().saturating_duration_since(capture)),
            );
            let result = client.model_infer(request).await;
            let done = Instant::now();
            let mut line = serde_json::json!({
                "type": kind, "stream": det.name, "frame": frame,
                "capture_ms": clock.ms(capture), "send_ms": clock.ms(send),
                "done_ms": clock.ms(done),
            });
            match result {
                Ok(response) => {
                    let response = response.into_inner();
                    line["status"] = "ok".into();
                    if displayed {
                        line["boxes"] = boxes(&response).unwrap_or_default().into();
                    }
                }
                Err(status) => line["status"] = status_text(&status).into(),
            }
            timeline.write(&line);
        });
    }
    let _ = tokio::time::timeout(Duration::from_secs(30), async {
        while inflight.join_next().await.is_some() {}
    })
    .await;
}

/// Das Sprachbildmodell: eine Frage nach der anderen, jeweils zum aktuellen Bild.
async fn vlm_stream(
    vlm: Target,
    mut client: Client,
    frames: Arc<Vec<u8>>,
    timeline: Timeline,
    clock: Clock,
    governed: bool,
    prompt: String,
    tokens: i32,
    gap: Duration,
) {
    let mut n = 0_u64;
    while clock.origin.elapsed() < clock.length {
        let send = Instant::now();
        let frame = clock.frame_at(send);
        let capture = clock.capture_of(frame);
        let at = frame as usize * frame_bytes();
        let age = governed.then(|| send.saturating_duration_since(capture));
        let request = vlm_request(
            &vlm,
            format!("vlm-{n}"),
            &frames[at..at + frame_bytes()],
            &prompt,
            tokens,
            age,
        );
        n += 1;
        let result = client.model_infer(request).await;
        let done = Instant::now();
        let mut line = serde_json::json!({
            "type": "vlm", "frame": frame,
            "capture_ms": clock.ms(capture), "send_ms": clock.ms(send),
            "done_ms": clock.ms(done),
        });
        match result {
            Ok(response) => {
                let response = response.into_inner();
                line["status"] = "ok".into();
                line["text"] = text(&response).unwrap_or_default().into();
            }
            Err(status) => {
                line["status"] = status_text(&status).into();
                // Eine Ablehnung ist kein Grund, den Governor zu bestuermen.
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        timeline.write(&line);
        if !gap.is_zero() {
            tokio::time::sleep(gap).await;
        }
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

async fn run() {
    let arm_name = env("VIG_DEMO_ARM").unwrap_or_default();
    // `profile-vlm` misst nur das Profil des Sprachbildmodells, das die
    // Konfigurationspruefung verlangt; `vig profile` kann es nicht, weil es
    // Nulleingaben schickt und die Bildgroesse offen ist.
    let profile_only = arm_name == "profile-vlm";
    let arm = match arm_name.as_str() {
        "direct" | "profile-vlm" => Arm::Direct,
        "governed" => Arm::Governed,
        _ => {
            eprintln!(
                "VIG_DEMO_ARM muss direct, governed oder profile-vlm sein (siehe Moduldoku)."
            );
            std::process::exit(2);
        }
    };
    let config_path = env("VIG_DEMO_CONFIG").expect("VIG_DEMO_CONFIG");
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
    let frames = Arc::new(
        std::fs::read(env("VIG_DEMO_FRAMES").expect("VIG_DEMO_FRAMES")).expect("Rohbilder"),
    );
    let frame_count = (frames.len() / frame_bytes()) as u64;
    assert!(frame_count > 0, "keine Rohbilder");
    let fps: f64 = env("VIG_DEMO_SOURCE_FPS").map_or(30.0, |v| v.parse().expect("fps"));
    let prompt = env("VIG_DEMO_PROMPT").unwrap_or_else(|| {
        "What is ahead? Mention people, vehicles and traffic signals. One sentence.".to_owned()
    });
    let tokens: i32 = env("VIG_DEMO_TOKENS").map_or(16, |v| v.parse().expect("Tokens"));
    let gap =
        Duration::from_millis(env("VIG_DEMO_VLM_GAP_MS").map_or(0, |v| v.parse().expect("Pause")));

    let mut shown = targets(&config, &["protected"]);
    assert_eq!(shown.len(), 1, "genau ein Modell der Klasse protected");
    let mut aux = targets(&config, &["high", "normal"]);
    let mut vlms = targets(&config, &["best_effort"]);
    assert_eq!(vlms.len(), 1, "genau ein Modell der Klasse best_effort");
    if profile_only {
        profile_vlm(&vlms[0], &frames, &prompt, tokens).await;
        return;
    }
    let out_path = env("VIG_DEMO_OUT").expect("VIG_DEMO_OUT");
    let timeline = Timeline(Arc::new(Mutex::new(std::io::BufWriter::new(
        std::fs::File::create(&out_path).expect("Zeitleiste anlegbar"),
    ))));

    let gateway = match arm {
        Arm::Direct => None,
        Arm::Governed => {
            let resolved = Arc::new(config.resolve().expect("aufloesbar"));
            let gateway = RunningGateway::start(resolved).await;
            for target in shown
                .iter_mut()
                .chain(aux.iter_mut())
                .chain(vlms.iter_mut())
            {
                target.endpoint.clone_from(&gateway.address);
                target.model.clone_from(&target.name);
            }
            Some(gateway)
        }
    };
    let governed = arm == Arm::Governed;
    let det = shown.remove(0);
    let vlm = vlms.remove(0);
    let det_client = connect(&det.endpoint).await;
    let vlm_client = connect(&vlm.endpoint).await;
    let mut aux_clients = Vec::new();
    for target in &aux {
        aux_clients.push(connect(&target.endpoint).await);
    }

    // Aufwaermen, ausserhalb der Zeitleiste: Der erste Aufruf eines Modells
    // misst sonst Speicheranlage und Kernelwahl.
    let first = &frames[..frame_bytes()];
    for (target, client) in std::iter::once((&det, &det_client)).chain(aux.iter().zip(&aux_clients))
    {
        for i in 0..5 {
            let mut c = client.clone();
            let request = detector_request(
                target,
                format!("warm-{}-{i}", target.name),
                first,
                governed.then_some(Duration::ZERO),
            );
            if let Err(status) = c.model_infer(request).await {
                eprintln!("Aufwaermen {}: {}", target.name, status_text(&status));
            }
        }
    }
    for i in 0..2 {
        let mut c = vlm_client.clone();
        let request = vlm_request(
            &vlm,
            format!("warm-vlm-{i}"),
            first,
            &prompt,
            tokens,
            governed.then_some(Duration::ZERO),
        );
        if let Err(status) = c.model_infer(request).await {
            eprintln!("Aufwaermen Sprachbildmodell: {}", status_text(&status));
        }
    }

    let label = env("VIG_DEMO_LABEL").unwrap_or_else(|| {
        match arm {
            Arm::Direct => "NVIDIA Triton alone",
            Arm::Governed => "with Vigilant",
        }
        .to_owned()
    });
    timeline.write(&serde_json::json!({
        "type": "header",
        "arm": match arm { Arm::Direct => "direct", Arm::Governed => "governed" },
        "label": label,
        "source_fps": fps,
        "frames": frame_count,
        "detector": {
            "model": det.backend_model,
            "period_ms": det.period_ms,
            "max_age_ms": det.max_age_ms,
            "input_size": frame_size(),
        },
        "aux_cameras": aux.iter().map(|t| serde_json::json!({
            "name": t.name, "model": t.backend_model, "period_ms": t.period_ms,
        })).collect::<Vec<_>>(),
        "vlm": { "model": vlm.backend_model, "prompt": prompt, "max_tokens": tokens,
                 "max_age_ms": vlm.max_age_ms },
        "gpu": env("VIG_DEMO_GPU"),
        "commit": env("VIG_DEMO_COMMIT"),
        "config": config_path,
        "recorded_at": format!("{:?}", std::time::SystemTime::now()),
    }));

    let clock = Clock {
        origin: Instant::now(),
        fps,
        frame_count,
        length: Duration::from_secs_f64(frame_count as f64 / fps),
    };
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(vlm_stream(
        vlm,
        vlm_client,
        Arc::clone(&frames),
        timeline.clone(),
        clock,
        governed,
        prompt,
        tokens,
        gap,
    ));
    tasks.spawn(detector_stream(
        det,
        true,
        0,
        det_client,
        Arc::clone(&frames),
        timeline.clone(),
        clock,
        governed,
    ));
    for (index, (target, client)) in aux.into_iter().zip(aux_clients).enumerate() {
        // Jede weitere Kamera sieht dieselbe Strasse um einige Sekunden versetzt.
        let offset = (index as u64 + 1) * (frame_count / 4);
        tasks.spawn(detector_stream(
            target,
            false,
            offset,
            client,
            Arc::clone(&frames),
            timeline.clone(),
            clock,
            governed,
        ));
    }
    let _ = tokio::time::timeout(clock.length + Duration::from_secs(60), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    timeline
        .0
        .lock()
        .expect("Zeitleiste")
        .flush()
        .expect("Zeitleiste schreibbar");
    if let Some(gateway) = gateway {
        gateway.stop().await;
    }
    println!(
        "Zeitleiste: {out_path} ({frame_count} Frames, {} s)",
        clock.length.as_secs()
    );
}
