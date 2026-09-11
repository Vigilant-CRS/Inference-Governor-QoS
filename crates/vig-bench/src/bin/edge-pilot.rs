//! `edge-pilot` — der Vig-Edge-Pilot (`docs/pilot/edge-pilot.md`).
//!
//! Vier Kameras spielen annotierte Alarm-Clips in Echtzeit ab, der
//! interne Detektor (RF-DETR, 23 Klassen) ist `protected`, ein Sprachmodell
//! schreibt Lageberichte als `best_effort`. Gemessen wird, was der schnelle
//! Alarmpfad unter Last verliert — gegen einen Referenzdurchlauf ohne jede
//! Konkurrenz, und im Vergleich zu Triton direkt.
//!
//! ## Ablauf
//!
//! 1. **Referenz.** Jedes Frame jeder Kamera einzeln, direkt, ohne
//!    Konkurrenz. Daraus: die Detektionen ohne Wartezeit (Obergrenze der
//!    Trefferquote), die Laufzeitquantile des Detektors (Profil und
//!    Lastpunkte), Alarmlatenz und Fehlalarme der Referenz.
//! 2. **Lastpunkte** nach serialisierter Auslastung des Detektors, aus dem
//!    gemessenen Median berechnet: die Rate folgt der Karte, nicht einer
//!    Annahme.
//! 3. **Arme** je Lastpunkt und Wiederholung: Triton direkt, Vigilant,
//!    Vigilant ohne Bericht (fuer K5). Je Arm die bessere Puffertiefe.
//! 4. **Urteil** gegen die vor der Messung festgelegten Kriterien K1–K7.
//!
//! ## Was dieses Werkzeug nicht tut
//!
//! Es liest nur vorbereitete Daten: sie kommen aus
//! `tools/pilot/prepare-alarm.sh` (bzw. `prepare-data.sh`), der Detektor aus
//! einer Kopie in einem eigenen Triton-Modellverzeichnis.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines,
    clippy::float_cmp,
    // Ein Messwerkzeug: eine kaputte Umgebung soll den Lauf abbrechen, nicht
    // still umgangen werden (vig-bench/src/lib.rs).
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_arguments
)]

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vig_backend_triton::TritonClient;
use vig_bench::pilot::{
    self, AlarmBook, ArmConfig, ArmReport, ArrivalRule, BoxN, CameraDef, GtFilter, LlmDef, Sequence,
};
use vig_bench::shm::Region;
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::{
    ModelMetadataRequest, SystemSharedMemoryRegisterRequest, SystemSharedMemoryUnregisterRequest,
};

/// Das Verzeichnis der aufbereiteten Daten und Modelle (`VIG_PILOT_DIR`).
///
/// Liegt ausserhalb des Repositorys: weder Bilder noch Annotationen noch
/// Gewichte gehoeren in die Versionsverwaltung.
fn pilot_dir() -> String {
    std::env::var("VIG_PILOT_DIR").unwrap_or_else(|_| "pilot".to_owned())
}
/// Alarmobjekt, Klasse A, Klasse B in der Klassenkarte des internen 23-Klassen-Detektors.
const ALARM_CLASSES: [usize; 3] = [13, 14, 15];
/// Person im selben Detektor.
const PERSON_CLASS: usize = 0;
/// Lastpunkte: Name, Kameras, serialisierte Auslastung des Detektors.
const SCENARIOS: [(&str, usize, f64); 4] =
    [("A", 2, 0.5), ("B", 4, 0.9), ("C", 4, 1.25), ("D", 4, 2.0)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dataset {
    Guns,
    Mot,
}

#[derive(Debug, Clone)]
struct Options {
    dataset: Dataset,
    data: String,
    seconds: u64,
    repeats: usize,
    scenarios: Vec<String>,
    caps: Vec<usize>,
    detector: String,
    model: String,
    llm: Option<String>,
    llm_model: String,
    /// Hoechstens so viele Kameras laden (die Funktionsprobe braucht zwei).
    max_cameras: usize,
}

fn options() -> Options {
    let mut o = Options {
        dataset: Dataset::Alarm,
        data: format!("{}/guns", pilot_dir()),
        seconds: 60,
        repeats: 3,
        scenarios: SCENARIOS.iter().map(|s| s.0.to_owned()).collect(),
        caps: vec![1, 4],
        detector: "127.0.0.1:8202".to_owned(),
        model: "edge_detector".to_owned(),
        llm: Some("127.0.0.1:8011".to_owned()),
        llm_model: "qwen".to_owned(),
        max_cameras: 4,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--dataset" => {
                o.dataset = if value == "mot" {
                    o.data = pilot_dir();
                    Dataset::Mot
                } else {
                    Dataset::Alarm
                };
                i += 1;
            }
            "--data" => {
                o.data = value;
                i += 1;
            }
            "--seconds" => {
                o.seconds = value.parse().expect("--seconds");
                i += 1;
            }
            "--repeats" => {
                o.repeats = value.parse().expect("--repeats");
                i += 1;
            }
            "--scenario" => {
                o.scenarios = value.split(',').map(str::to_owned).collect();
                i += 1;
            }
            "--caps" => {
                o.caps = value
                    .split(',')
                    .map(|c| c.parse().expect("--caps"))
                    .collect();
                i += 1;
            }
            "--triton" => {
                o.detector = value;
                i += 1;
            }
            "--model" => {
                o.model = value;
                i += 1;
            }
            "--llm" => {
                o.llm = Some(value);
                i += 1;
            }
            "--no-llm" => o.llm = None,
            "--smoke" => {
                o.seconds = 12;
                o.repeats = 1;
                o.scenarios = vec!["A".to_owned()];
                o.caps = vec![4];
                // Lastpunkt A braucht zwei Kameras; die Referenz ueber alle
                // vier allein dauerte rund 100 s.
                o.max_cameras = 2;
            }
            other => panic!("unbekannte Option {other}"),
        }
        i += 1;
    }
    o
}

/// Eine geladene Kamera: Annotation und Rohframes.
struct Loaded {
    name: String,
    sequence: Arc<Sequence>,
    frames: Arc<Vec<u8>>,
    size: usize,
}

fn load(options: &Options) -> Vec<Loaded> {
    let names: Vec<String> = match options.dataset {
        Dataset::Alarm => (0..4).map(|k| format!("cam{k}")).collect(),
        Dataset::Mot => ["MOT16-02", "MOT16-04", "MOT16-09", "MOT16-11"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
    };
    names
        .iter()
        .take(options.max_cameras)
        .map(|name| {
            let base = format!("{}/{name}", options.data);
            let meta = std::fs::read_to_string(format!("{base}.meta")).expect("meta lesbar");
            let mut fields = meta
                .split_whitespace()
                .map(|v| v.parse::<usize>().expect("meta"));
            let fps = fields.next().expect("fps");
            let frames = fields.next().expect("frames");
            let rgb = std::fs::read(format!("{base}.rgb")).expect("Rohframes lesbar");
            let size = ((rgb.len() / frames.max(1) / 3) as f64).sqrt().round() as usize;
            assert_eq!(
                size * size * 3 * frames,
                rgb.len(),
                "{name}: Rohframes passen nicht"
            );
            let csv = std::fs::read_to_string(format!("{base}.gt.csv")).expect("gt lesbar");
            let (filter, rule) = match options.dataset {
                Dataset::Alarm => (GtFilter::ALL, ArrivalRule::FirstAppearance),
                Dataset::Mot => (GtFilter::PERSONS, ArrivalRule::AfterStart),
            };
            let mut sequence = Sequence::from_csv(name, fps as u32, frames, &csv, filter, rule)
                .expect("Annotation lesbar");
            if let Ok(clips) = std::fs::read_to_string(format!("{base}.clips.csv")) {
                sequence = sequence.with_negative(&pilot::negative_ranges(&clips).expect("clips"));
            }
            Loaded {
                name: name.clone(),
                sequence: Arc::new(sequence),
                frames: Arc::new(rgb),
                size,
            }
        })
        .collect()
}

/// Was der Referenzdurchlauf ergibt.
struct Reference {
    /// Zielobjekt-Detektionen je Kamera und Frame.
    ideal: Vec<Arc<pilot::IdealTable>>,
    /// Detektorlaufzeit (Round-Trip ohne Konkurrenz), Mikrosekunden.
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    samples: usize,
    /// Treffer je Klassenindex auf annotierten Frames — die Plausibilitaet
    /// der Zielklassen.
    hits_by_class: Vec<u64>,
}

async fn reference(
    cameras: &[Loaded],
    triton: &TritonClient,
    model: &str,
    input: &(String, String, Vec<i64>),
    classes: usize,
    targets: &[usize],
    threshold: f32,
    slots: &[Vec<(String, std::path::PathBuf)>],
) -> Reference {
    let mut latencies = Vec::new();
    let mut ideal = Vec::new();
    let mut hits_by_class = vec![0_u64; classes];
    for (camera, camera_slots) in cameras.iter().zip(slots) {
        let (name, path) = camera_slots.first().expect("mindestens eine Region");
        let frame_bytes = camera.size * camera.size * 3;
        let mut table = Vec::with_capacity(camera.sequence.frames());
        for frame in 0..camera.sequence.frames() {
            let rgb = &camera.frames[frame * frame_bytes..(frame + 1) * frame_bytes];
            std::fs::write(path, pilot::frame_tensor(rgb, camera.size, camera.size))
                .expect("Region beschreibbar");
            let request = reference_request(model, input, name, camera.size);
            let started = Instant::now();
            let response = triton.infer(request).await.expect("Referenzinferenz");
            latencies.push(started.elapsed().as_micros() as u64);
            let dets = pilot::f32_output(&response, "dets").unwrap_or_default();
            let labels = pilot::f32_output(&response, "labels").unwrap_or_default();
            let decoded = pilot::decode_rfdetr(&dets, &labels, classes, pilot::SCORE_THRESHOLD);
            let truth: Vec<BoxN> = camera
                .sequence
                .objects(frame)
                .iter()
                .map(|o| o.bbox)
                .collect();
            for (class, hits) in hits_by_class.iter_mut().enumerate() {
                let boxes: Vec<BoxN> = decoded
                    .iter()
                    .filter(|d| d.class == class)
                    .map(|d| d.bbox)
                    .collect();
                *hits += pilot::match_boxes(&truth, &boxes, threshold)
                    .into_iter()
                    .filter(|&m| m)
                    .count() as u64;
            }
            table.push(
                decoded
                    .iter()
                    .filter(|d| targets.contains(&d.class))
                    .map(|d| d.bbox)
                    .collect::<Vec<_>>(),
            );
        }
        ideal.push(Arc::new(table));
    }
    Reference {
        p50_us: pilot::percentile(&latencies, 50),
        p95_us: pilot::percentile(&latencies, 95),
        p99_us: pilot::percentile(&latencies, 99),
        samples: latencies.len(),
        ideal,
        hits_by_class,
    }
}

fn reference_request(
    model: &str,
    input: &(String, String, Vec<i64>),
    region: &str,
    size: usize,
) -> vig_protocol_oip::inference::ModelInferRequest {
    use vig_protocol_oip::inference::InferParameter;
    use vig_protocol_oip::inference::infer_parameter::ParameterChoice;
    use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
    let mut params = std::collections::HashMap::new();
    params.insert(
        "shared_memory_region".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::StringParam(region.to_owned())),
        },
    );
    params.insert(
        "shared_memory_byte_size".to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(
                i64::try_from(size * size * 12).unwrap_or(i64::MAX),
            )),
        },
    );
    vig_protocol_oip::inference::ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: "referenz".to_owned(),
        parameters: std::collections::HashMap::new(),
        inputs: vec![InferInputTensor {
            name: input.0.clone(),
            datatype: input.1.clone(),
            shape: input.2.clone(),
            parameters: params,
            contents: None,
        }],
        outputs: Vec::new(),
        raw_input_contents: Vec::new(),
    }
}

/// Die Referenz-Alarmlatenz: jedes Frame geliefert nach seiner Aufnahme plus
/// dem Median der Laufzeit, ueber einen Durchgang der Wiedergabeliste.
fn reference_alarm(
    camera: &Loaded,
    ideal: &pilot::IdealTable,
    p50: Duration,
    iou: f32,
) -> pilot::AlarmSummary {
    let mut book = AlarmBook::new(Arc::clone(&camera.sequence), iou);
    for (frame, boxes) in ideal.iter().enumerate() {
        let at = camera.sequence.capture_offset(frame as u64) + p50;
        book.on_delivery(frame as u64, at, boxes);
    }
    let one_pass = camera
        .sequence
        .capture_offset(camera.sequence.frames() as u64)
        .saturating_sub(Duration::from_millis(1));
    book.finish(one_pass)
}

/// Ein Gateway, das sich wieder beenden laesst (wie in `frontier`).
struct Gateway {
    address: String,
    handle: vig_gateway::Handle,
    stop: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
}

impl Gateway {
    async fn start(yaml: &str) -> Self {
        let config = Config::from_yaml(yaml).expect("Konfiguration gueltig");
        let resolved = Arc::new(config.resolve().expect("aufloesbar"));
        let clock = MonotonicClock::start();
        let triton = Arc::new(TritonClient::new(&resolved.backend_endpoint));
        let handle =
            actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
        handle.observe_hardware();
        let service = GatewayService::new(resolved, triton, handle.clone(), clock);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Port");
        let address = listener.local_addr().expect("Adresse").to_string();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let incoming = vig_bench::incoming(listener);
            let _ = tonic::transport::Server::builder()
                .initial_stream_window_size(vig_backend_triton::STREAM_WINDOW_BYTES)
                .initial_connection_window_size(vig_backend_triton::CONNECTION_WINDOW_BYTES)
                .add_service(
                    GrpcInferenceServiceServer::new(service)
                        .max_decoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
                        .max_encoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES),
                )
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = stopped.await;
                })
                .await;
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        Self {
            address,
            handle,
            stop,
            server,
        }
    }

    async fn finish(self) {
        let _ = self.stop.send(());
        let _ = self.server.await;
        let _ = self.handle.drain(Duration::from_secs(5)).await;
    }
}

fn gateway_yaml(
    options: &Options,
    cameras: usize,
    rate: f64,
    reference: &Reference,
    llm: bool,
) -> String {
    let period_ms = ((1_000.0 / rate).round() as u64).max(1);
    let mut models = String::new();
    for k in 0..cameras {
        let _ = write!(
            models,
            "\n  cam{k}:\n    class: protected\n    queue: {{ policy: latest, capacity: 1 }}\n    \
             contract: {{ period_ms: {period_ms}, deadline_ms: {period_ms}, max_age_ms: {} }}\n    \
             variants:\n      - id: v4\n        backend_model: {}\n        \
             quality: {{ value: 1.0, source: user_declared }}\n        \
             profile: {{ p50_us: {}, p95_us: {}, p99_us: {}, samples: {} }}",
            period_ms * 2,
            options.model,
            reference.p50_us,
            reference.p95_us.max(reference.p50_us),
            reference.p99_us.max(reference.p95_us),
            reference.samples.max(1),
        );
    }
    if llm && let Some(endpoint) = &options.llm {
        // Wie WP26: eigener Server, zerlegbar, Hintergrundlast.
        let _ = write!(
            models,
            "\n  {}:\n    decoupled: true\n    backend_endpoint: \"{endpoint}\"\n    \
             class: best_effort\n    queue: {{ policy: fifo, capacity: 2, overflow: backpressure_client }}\n    \
             contract: {{ deadline_ms: 20000, max_age_ms: 40000 }}\n    \
             cooperative: {{ tokens_per_second: 242, min_tokens: 4, max_total_tokens: 64, base_cost_us: 18000 }}\n    \
             variants:\n      - id: main\n        backend_model: {}\n        \
             quality: {{ value: 1.0, source: user_declared }}\n        \
             profile: {{ p50_us: 1100000, p95_us: 1400000, p99_us: 1600000, samples: 100 }}",
            options.llm_model, options.llm_model,
        );
    }
    format!(
        "version: 1\nbackend:\n  type: triton\n  grpc_endpoint: \"{}\"\n  slots: 1\n  \
         pipelining_depth: 0\n  safety_margin_percent: 110\nmodels:{models}\n",
        options.detector
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    Direct,
    Vigilant,
    VigilantNoReport,
}

impl Arm {
    const fn label(self) -> &'static str {
        match self {
            Self::Direct => "Triton direkt",
            Self::Vigilant => "Vigilant",
            Self::VigilantNoReport => "Vigilant ohne Bericht",
        }
    }
}

/// Die Kennzahlen eines Laufs, ueber alle Kameras zusammengefasst.
#[derive(Debug, Clone, Default)]
struct Summary {
    alarm_p50: u64,
    alarm_p95: u64,
    alarm_max: u64,
    missed_permille: u64,
    recall_permille: u64,
    ideal_permille: u64,
    false_alarm_permille: u64,
    coverage_permille: u64,
    longest_gap_ms: u64,
    reports_per_min: u64,
    report_ms_p50: u64,
    report_basis_age_ms_p50: u64,
}

/// Die Laeufe je Arm eines Lastpunkts.
type ArmResults = Vec<(Arm, Vec<Summary>)>;

fn summarise(report: &ArmReport, seconds: u64) -> Summary {
    let mut latencies = Vec::new();
    let (mut events, mut missed) = (0, 0);
    let (mut hits, mut ideal, mut total) = (0, 0, 0);
    let (mut negative, mut false_alarms) = (0, 0);
    let mut coverage = Vec::new();
    let mut gap = 0;
    for c in &report.cameras {
        latencies.extend(&c.alarm.latencies_ms);
        events += c.alarm.events;
        missed += c.alarm.missed;
        hits += c.recall.hits;
        ideal += c.recall.ideal;
        total += c.recall.total;
        negative += c.negative_deliveries;
        false_alarms += c.false_alarms;
        coverage.push(c.coverage.covered_permille());
        gap = gap.max(c.coverage.longest_gap_ns / 1_000_000);
    }
    Summary {
        alarm_p50: pilot::percentile(&latencies, 50),
        alarm_p95: pilot::percentile(&latencies, 95),
        alarm_max: latencies.iter().copied().max().unwrap_or(0),
        missed_permille: (missed * 1_000).checked_div(events).unwrap_or(0),
        recall_permille: (hits * 1_000).checked_div(total).unwrap_or(0),
        ideal_permille: (ideal * 1_000).checked_div(total).unwrap_or(0),
        false_alarm_permille: (false_alarms * 1_000).checked_div(negative).unwrap_or(0),
        coverage_permille: coverage.iter().sum::<u64>() / coverage.len().max(1) as u64,
        longest_gap_ms: gap,
        reports_per_min: report.llm.reports * 60 / seconds.max(1),
        report_ms_p50: pilot::percentile(&report.llm.latencies_ms, 50),
        report_basis_age_ms_p50: pilot::percentile(&report.llm.basis_age_ms, 50),
    }
}

/// Je Arm die bessere Puffertiefe: niedrigere p95-Alarmlatenz, bei Gleichstand
/// weniger verpasste Ereignisse (Spec 19.1).
fn better(a: Summary, b: Summary) -> Summary {
    if (b.alarm_p95, b.missed_permille) < (a.alarm_p95, a.missed_permille) {
        b
    } else {
        a
    }
}

fn median_of(values: &[Summary], f: impl Fn(&Summary) -> u64) -> (u64, u64, u64) {
    let v: Vec<u64> = values.iter().map(f).collect();
    (
        pilot::percentile(&v, 50),
        v.iter().copied().min().unwrap_or(0),
        v.iter().copied().max().unwrap_or(0),
    )
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
    let options = options();
    let (targets, iou) = match options.dataset {
        Dataset::Alarm => (ALARM_CLASSES.to_vec(), 0.3_f32),
        Dataset::Mot => (vec![PERSON_CLASS], pilot::IOU_THRESHOLD),
    };
    println!("edge-pilot: Vig-Edge-Pilot (docs/pilot/edge-pilot.md)");
    println!(
        "Detektor {} @ {} · Bericht {} · {:?} · Zielklassen {targets:?} · IoU {iou}",
        options.model,
        options.detector,
        options.llm.as_deref().unwrap_or("aus"),
        options.dataset,
    );

    let started = Instant::now();
    let loaded = load(&options);
    println!(
        "{} Kameras geladen in {:.1} s: {}",
        loaded.len(),
        started.elapsed().as_secs_f64(),
        loaded
            .iter()
            .map(|c| format!(
                "{} ({} Frames, {} Ankuenfte)",
                c.name,
                c.sequence.frames(),
                c.sequence.arrivals().len()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // --- Detektor: Metadaten und Shared-Memory-Regionen ---------------------
    let triton = TritonClient::new(options.detector.clone());
    let metadata = triton
        .raw()
        .await
        .expect("Detektor-Triton erreichbar")
        .model_metadata(ModelMetadataRequest {
            name: options.model.clone(),
            version: String::new(),
        })
        .await
        .expect("Modellmetadaten")
        .into_inner();
    let input = metadata.inputs.first().expect("Eingang");
    let input = (
        input.name.clone(),
        input.datatype.clone(),
        input.shape.clone(),
    );
    let classes = metadata
        .outputs
        .iter()
        .find(|o| o.name == "labels")
        .and_then(|o| o.shape.last().copied())
        .expect("labels-Ausgang") as usize;
    let side = input.2.last().copied().expect("Eingangsform") as usize;
    for camera in &loaded {
        assert_eq!(
            camera.size, side,
            "{}: Frames {}px, Modell {}px — mit SIZE={side} neu aufbereiten",
            camera.name, camera.size, side
        );
    }
    let slots_per_camera = options.caps.iter().copied().max().unwrap_or(1) + 1;
    let byte_size = (side * side * 12) as u64;
    let mut regions = Vec::new();
    let mut slots: Vec<Vec<(String, std::path::PathBuf)>> = Vec::new();
    for (k, _) in loaded.iter().enumerate() {
        let mut camera_slots = Vec::new();
        for s in 0..slots_per_camera {
            let region = Region::create(&format!("vig_pilot_{k}_{s}"), byte_size).expect("Region");
            let mut raw = triton.raw().await.expect("Detektor");
            let _ = raw
                .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
                    name: region.name.clone(),
                })
                .await;
            raw.system_shared_memory_register(SystemSharedMemoryRegisterRequest {
                name: region.name.clone(),
                key: region.key.clone(),
                offset: 0,
                byte_size,
            })
            .await
            .expect("Region registrieren");
            camera_slots.push((region.name.clone(), region.path().to_path_buf()));
            regions.push(region);
        }
        slots.push(camera_slots);
    }

    // --- 1. Referenz -------------------------------------------------------
    let started = Instant::now();
    let reference = Box::pin(reference(
        &loaded,
        &triton,
        &options.model,
        &input,
        classes,
        &targets,
        iou,
        &slots,
    ))
    .await;
    println!(
        "\nReferenz ({} Frames in {:.0} s): Detektor p50 {} us, p95 {} us, p99 {} us",
        reference.samples,
        started.elapsed().as_secs_f64(),
        reference.p50_us,
        reference.p95_us,
        reference.p99_us
    );
    let mut ranking: Vec<(usize, u64)> = reference
        .hits_by_class
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, n)| *n > 0)
        .collect();
    ranking.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    println!("  Treffer auf annotierten Objekten je Klasse (Index: Treffer): {ranking:?}");
    let p50 = Duration::from_micros(reference.p50_us);
    let mut reference_latencies = Vec::new();
    let (mut ref_events, mut ref_missed) = (0, 0);
    let (mut ref_negative, mut ref_false) = (0_u64, 0_u64);
    for (camera, ideal) in loaded.iter().zip(&reference.ideal) {
        let summary = reference_alarm(camera, ideal, p50, iou);
        reference_latencies.extend(summary.latencies_ms.iter().copied());
        ref_events += summary.events;
        ref_missed += summary.missed;
        for (frame, boxes) in ideal.iter().enumerate() {
            if camera.sequence.is_negative(frame) {
                ref_negative += 1;
                if !boxes.is_empty() {
                    ref_false += 1;
                }
            }
        }
    }
    let ref_missed_permille = (ref_missed * 1_000).checked_div(ref_events).unwrap_or(0);
    let ref_false_permille = (ref_false * 1_000).checked_div(ref_negative).unwrap_or(0);
    println!(
        "  Referenz-Alarm: {ref_events} Ereignisse, p50 {} ms, p95 {} ms, nie {} ‰ · Fehlalarme {} ‰ der Negativframes",
        pilot::percentile(&reference_latencies, 50),
        pilot::percentile(&reference_latencies, 95),
        ref_missed_permille,
        ref_false_permille,
    );

    // --- 2./3. Lastpunkte und Arme ------------------------------------------
    let duration = Duration::from_secs(options.seconds);
    let mut verdicts: Vec<String> = Vec::new();
    let mut by_scenario: Vec<(String, ArmResults)> = Vec::new();
    for &(name, cams, utilisation) in &SCENARIOS {
        if !options.scenarios.iter().any(|s| s == name) {
            continue;
        }
        let cams = cams.min(loaded.len());
        let rate = (utilisation / (cams as f64 * p50.as_secs_f64())).min(25.0);
        let effective = cams as f64 * rate * p50.as_secs_f64();
        println!(
            "\nLastpunkt {name}: {cams} Kameras × {rate:.1} Hz · serialisierte Auslastung {:.0} %",
            effective * 100.0
        );
        let arms: Vec<Arm> = if options.llm.is_some() {
            vec![Arm::Direct, Arm::Vigilant, Arm::VigilantNoReport]
        } else {
            vec![Arm::Direct, Arm::Vigilant]
        };
        let mut results: Vec<(Arm, Vec<Summary>)> = arms.iter().map(|a| (*a, Vec::new())).collect();
        for repeat in 0..options.repeats {
            for (arm, collected) in &mut results {
                let mut best: Option<Summary> = None;
                for &cap in &options.caps {
                    let with_report = *arm != Arm::VigilantNoReport && options.llm.is_some();
                    let gateway = if *arm == Arm::Direct {
                        None
                    } else {
                        let yaml = gateway_yaml(&options, cams, rate, &reference, with_report);
                        Some(Gateway::start(&yaml).await)
                    };
                    let endpoint = gateway
                        .as_ref()
                        .map_or_else(|| options.detector.clone(), |g| g.address.clone());
                    let cameras: Vec<CameraDef> = loaded
                        .iter()
                        .take(cams)
                        .enumerate()
                        .map(|(k, c)| CameraDef {
                            name: c.name.clone(),
                            model: if gateway.is_some() {
                                format!("cam{k}")
                            } else {
                                options.model.clone()
                            },
                            sequence: Arc::clone(&c.sequence),
                            frames: Arc::clone(&c.frames),
                            size: c.size,
                            rate_hz: rate,
                            regions: slots[k].clone(),
                            input: input.clone(),
                            target_classes: targets.clone(),
                            iou_threshold: iou,
                            classes,
                            ideal: Some(Arc::clone(&reference.ideal[k])),
                        })
                        .collect();
                    let llm = if with_report {
                        options.llm.as_ref().map(|llm_endpoint| LlmDef {
                            endpoint: gateway
                                .as_ref()
                                .map_or_else(|| llm_endpoint.clone(), |g| g.address.clone()),
                            model: options.llm_model.clone(),
                            max_tokens: 64,
                            min_interval: Duration::from_secs(2),
                        })
                    } else {
                        None
                    };
                    let report = Box::pin(pilot::run_arm(ArmConfig {
                        endpoint,
                        via_governor: gateway.is_some(),
                        cameras,
                        in_flight_cap: cap,
                        duration,
                        llm,
                    }))
                    .await
                    .expect("Arm laeuft");
                    if let Some(g) = gateway {
                        g.finish().await;
                    }
                    let summary = summarise(&report, options.seconds);
                    println!(
                        "  [{name} {} Wdh {} Tiefe {cap}] Alarm p95 {} ms, nie {} ‰, Treffer {} ‰ (ideal {} ‰), Berichte/min {}",
                        arm.label(),
                        repeat + 1,
                        summary.alarm_p95,
                        summary.missed_permille,
                        summary.recall_permille,
                        summary.ideal_permille,
                        summary.reports_per_min
                    );
                    best = Some(match best {
                        Some(b) => better(b, summary),
                        None => summary,
                    });
                }
                if let Some(b) = best {
                    collected.push(b);
                }
            }
        }
        by_scenario.push((name.to_owned(), results));
    }

    // --- Bericht --------------------------------------------------------------
    println!(
        "\nMedian ueber {} Wiederholungen, je Arm die bessere Puffertiefe {:?}.",
        options.repeats, options.caps
    );
    println!(
        "  Punkt | Arm                    | Alarm p50/p95/max ms  | nie ‰ | Treffer ‰ (ideal) | Fehlalarm ‰ | Abdeckung ‰ | Luecke ms | Berichte/min | Bericht ms | Basisalter ms"
    );
    println!(
        "  ------|------------------------|-----------------------|-------|-------------------|-------------|-------------|-----------|--------------|------------|--------------"
    );
    for (name, results) in &by_scenario {
        for (arm, runs) in results {
            let m = |f: fn(&Summary) -> u64| median_of(runs, f).0;
            let (p95, p95_min, p95_max) = median_of(runs, |s| s.alarm_p95);
            println!(
                "  {name:<5} | {:<22} | {:>5} / {:>5} [{p95_min}-{p95_max}] / {:>5} | {:>5} | {:>6} ({:>4}) | {:>11} | {:>11} | {:>9} | {:>12} | {:>10} | {:>12}",
                arm.label(),
                m(|s| s.alarm_p50),
                p95,
                m(|s| s.alarm_max),
                m(|s| s.missed_permille),
                m(|s| s.recall_permille),
                m(|s| s.ideal_permille),
                m(|s| s.false_alarm_permille),
                m(|s| s.coverage_permille),
                m(|s| s.longest_gap_ms),
                m(|s| s.reports_per_min),
                m(|s| s.report_ms_p50),
                m(|s| s.report_basis_age_ms_p50),
            );
        }
    }

    // --- 4. Urteil gegen K1–K7 ----------------------------------------------
    let get = |name: &str, arm: Arm, f: fn(&Summary) -> u64| -> Option<u64> {
        by_scenario
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, r)| r.iter().find(|(a, _)| *a == arm))
            .map(|(_, runs)| median_of(runs, f).0)
    };
    let mut verdict = |k: &str, text: String, ok: bool| {
        verdicts.push(format!(
            "  {k} {} — {text}",
            if ok { "bestanden" } else { "VERFEHLT" }
        ));
    };
    for name in ["C", "D"] {
        if let (Some(p95), Some(missed)) = (
            get(name, Arm::Vigilant, |s| s.alarm_p95),
            get(name, Arm::Vigilant, |s| s.missed_permille),
        ) {
            verdict(
                "K1",
                format!(
                    "{name}: p95 {p95} ms (≤ 300), nie {missed} ‰ (≤ Referenz {ref_missed_permille} + 20)"
                ),
                p95 <= 300 && missed <= ref_missed_permille + 20,
            );
            if let (Some(direct_p95), Some(direct_missed)) = (
                get(name, Arm::Direct, |s| s.alarm_p95),
                get(name, Arm::Direct, |s| s.missed_permille),
            ) {
                let direct_meets = direct_p95 <= 300 && direct_missed <= ref_missed_permille + 20;
                verdict(
                    "K2",
                    format!(
                        "{name}: Vigilant {p95} ms gegen Triton {direct_p95} ms (≥ 30 % kuerzer, oder Triton erfuellt K1: {direct_meets})"
                    ),
                    p95 * 10 <= direct_p95 * 7 || direct_meets,
                );
            }
            if let Some(recall) = get(name, Arm::Vigilant, |s| s.recall_permille)
                && let Some(ideal) = get(name, Arm::Vigilant, |s| s.ideal_permille)
            {
                verdict(
                    "K4",
                    format!("{name}: Trefferquote {recall} ‰ gegen Referenz {ideal} ‰ (≥ 90 %)"),
                    recall * 10 >= ideal * 9,
                );
            }
        }
    }
    if let (Some(v), Some(d)) = (
        get("A", Arm::Vigilant, |s| s.alarm_p95),
        get("A", Arm::Direct, |s| s.alarm_p95),
    ) {
        let allowed = (d * 105 / 100).max(d + 10);
        verdict(
            "K3",
            format!("A: Vigilant {v} ms gegen Triton {d} ms (≤ {allowed})"),
            v <= allowed,
        );
    }
    for (name, _) in &by_scenario {
        if let (Some(with), Some(without)) = (
            get(name, Arm::Vigilant, |s| s.alarm_p95),
            get(name, Arm::VigilantNoReport, |s| s.alarm_p95),
        ) {
            verdict(
                "K5",
                format!("{name}: mit Bericht {with} ms, ohne {without} ms (≤ +10 %)"),
                with * 10 <= without * 11 || with <= without + 5,
            );
        }
    }
    for name in ["A", "B"] {
        if let Some(per_min) = get(name, Arm::Vigilant, |s| s.reports_per_min)
            && options.llm.is_some()
        {
            verdict(
                "K6",
                format!("{name}: {per_min} Berichte/min (≥ 2)"),
                per_min >= 2,
            );
        }
    }
    for (name, _) in &by_scenario {
        if let Some(fa) = get(name, Arm::Vigilant, |s| s.false_alarm_permille) {
            verdict(
                "K7",
                format!("{name}: Fehlalarme {fa} ‰ (≤ Referenz {ref_false_permille} + 10)"),
                fa <= ref_false_permille + 10,
            );
        }
    }
    println!(
        "\nUrteil gegen die Abnahmekriterien (docs/pilot/edge-pilot.md; K8 ist das Datenpfad-Urteil von shm-latency):"
    );
    for line in &verdicts {
        println!("{line}");
    }
    drop(regions);
}
