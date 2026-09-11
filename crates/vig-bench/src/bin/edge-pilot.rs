//! `edge-pilot` — der Vig-Edge-Pilot (`docs/pilot/edge-pilot.md`).
//!
//! Vier Kameras spielen annotierte Clips mit Alarmobjekten in Echtzeit ab, der
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
//! ## Ablage, Wiederaufnahme, Exitcode
//!
//! Jede Zelle — Lastpunkt, Wiederholung, Arm, Puffertiefe — steht als eine
//! JSON-Zeile in `<out>/cells.jsonl`, sobald sie gemessen ist. Ein erneuter
//! Start mit demselben `--out` ueberspringt gueltige Zellen; Raten und
//! Profile kommen dann aus dem ersten Referenzdurchlauf (`manifest.json`),
//! damit spaetere Zellen mit frueheren vergleichbar bleiben. Vor dem Start
//! steht die geschaetzte Dauer im Protokoll, am Ende `summary.json`.
//!
//! Exitcode: 0 alle Kriterien erfuellt (oder keines auswertbar), 1 sauber
//! gelaufen und mindestens ein Kriterium verfehlt, 2 Aufbau oder Lauf
//! kaputt. Die Funktionsprobe (`--smoke`) kennt nur 0 und 2: zwoelf Sekunden
//! sind keine Abnahme.
//!
//! ## Was dieses Werkzeug nicht tut
//!
//! Es liest nur vorbereitete Daten: sie kommen aus
//! einem lokalen Aufbereitungsskript (nicht im Repository) bzw.
//! `tools/pilot/prepare-data.sh`, der Detektor aus
//! einer Kopie in einem eigenen Triton-Modellverzeichnis.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
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

use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
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
/// Die Alarmklassen des internen 23-Klassen-Detektors.
const ALARM_CLASSES: [usize; 3] = [13, 14, 15];
/// Person im selben Detektor.
const PERSON_CLASS: usize = 0;
/// Lastpunkte: Name, Kameras, serialisierte Auslastung des Detektors.
const SCENARIOS: [(&str, usize, f64); 4] =
    [("A", 2, 0.5), ("B", 4, 0.9), ("C", 4, 1.25), ("D", 4, 2.0)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dataset {
    Alarm,
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
    /// Die Ablage; ohne Angabe unter `VIG_PILOT_DIR/results`.
    out: Option<String>,
    /// Eine vorhandene Ablage verwerfen statt fortzusetzen.
    fresh: bool,
    /// Die Funktionsprobe: kurz, und ohne fachliches Urteil im Exitcode.
    smoke: bool,
}

impl Options {
    fn out_dir(&self) -> PathBuf {
        self.out.as_ref().map_or_else(
            || {
                let leaf = if self.smoke {
                    "edge-pilot-smoke"
                } else {
                    "edge-pilot"
                };
                PathBuf::from(format!("{}/results/{leaf}", pilot_dir()))
            },
            PathBuf::from,
        )
    }

    /// Was eine Zelle bestimmt: ein Lauf darf nur mit demselben Aufbau
    /// fortgesetzt werden. Lastpunkte, Wiederholungen und Puffertiefen
    /// duerfen sich aendern — jede Zelle steht fuer sich.
    fn setup(&self) -> Value {
        json!({
            "dataset": format!("{:?}", self.dataset),
            "data": self.data,
            "seconds": self.seconds,
            "detector": self.detector,
            "model": self.model,
            "llm": self.llm,
            "llm_model": self.llm_model,
            "max_cameras": self.max_cameras,
        })
    }
}

fn options() -> Options {
    let mut o = Options {
        dataset: Dataset::Alarm,
        data: format!("{}/alarm", pilot_dir()),
        seconds: 60,
        repeats: 3,
        scenarios: SCENARIOS.iter().map(|s| s.0.to_owned()).collect(),
        caps: vec![1, 4],
        detector: "127.0.0.1:8202".to_owned(),
        model: "edge_detector".to_owned(),
        llm: Some("127.0.0.1:8011".to_owned()),
        llm_model: "qwen".to_owned(),
        max_cameras: 4,
        out: None,
        fresh: false,
        smoke: false,
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
            "--out" => {
                o.out = Some(value);
                i += 1;
            }
            "--fresh" => o.fresh = true,
            "--smoke" => {
                o.smoke = true;
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

    /// Der Name in der Ablage.
    const fn id(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Vigilant => "vigilant",
            Self::VigilantNoReport => "vigilant_no_report",
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

impl Summary {
    fn to_json(&self) -> Value {
        json!({
            "alarm_p50": self.alarm_p50,
            "alarm_p95": self.alarm_p95,
            "alarm_max": self.alarm_max,
            "missed_permille": self.missed_permille,
            "recall_permille": self.recall_permille,
            "ideal_permille": self.ideal_permille,
            "false_alarm_permille": self.false_alarm_permille,
            "coverage_permille": self.coverage_permille,
            "longest_gap_ms": self.longest_gap_ms,
            "reports_per_min": self.reports_per_min,
            "report_ms_p50": self.report_ms_p50,
            "report_basis_age_ms_p50": self.report_basis_age_ms_p50,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let get = |key: &str| value.get(key).and_then(Value::as_u64);
        Some(Self {
            alarm_p50: get("alarm_p50")?,
            alarm_p95: get("alarm_p95")?,
            alarm_max: get("alarm_max")?,
            missed_permille: get("missed_permille")?,
            recall_permille: get("recall_permille")?,
            ideal_permille: get("ideal_permille")?,
            false_alarm_permille: get("false_alarm_permille")?,
            coverage_permille: get("coverage_permille")?,
            longest_gap_ms: get("longest_gap_ms")?,
            reports_per_min: get("reports_per_min")?,
            report_ms_p50: get("report_ms_p50")?,
            report_basis_age_ms_p50: get("report_basis_age_ms_p50")?,
        })
    }
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

/// Geschaetzter Aufwand je Zelle neben der Messdauer: Gateway starten,
/// Nachlauf offener Auftraege, Drain.
const CELL_OVERHEAD_S: u64 = 3;

fn cell_key(scenario: &str, repeat: usize, arm: Arm, cap: usize) -> String {
    format!("{scenario}/{}/{}/{cap}", repeat + 1, arm.id())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn arms_for(options: &Options) -> Vec<Arm> {
    if options.llm.is_some() {
        vec![Arm::Direct, Arm::Vigilant, Arm::VigilantNoReport]
    } else {
        vec![Arm::Direct, Arm::Vigilant]
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
}

/// Die Ablage eines Laufs (siehe Moduldoku).
struct Store {
    dir: PathBuf,
    manifest: Value,
    /// Gueltige, schon gemessene Zellen.
    done: HashMap<String, Summary>,
}

impl Store {
    fn open(options: &Options) -> Result<Self, String> {
        let dir = options.out_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if options.fresh {
            for name in ["manifest.json", "cells.jsonl", "summary.json"] {
                match std::fs::remove_file(dir.join(name)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("{name}: {e}")),
                }
            }
        }
        let setup = options.setup();
        let manifest = match std::fs::read_to_string(dir.join("manifest.json")) {
            Ok(text) => {
                let manifest: Value =
                    serde_json::from_str(&text).map_err(|e| format!("manifest.json: {e}"))?;
                if manifest.get("setup") != Some(&setup) {
                    return Err(format!(
                        "{}: angefangener Lauf mit anderem Aufbau ({} gegen {setup}); \
                         --fresh verwirft ihn, --out waehlt eine andere Ablage",
                        dir.display(),
                        manifest.get("setup").unwrap_or(&Value::Null)
                    ));
                }
                manifest
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let manifest = json!({ "setup": setup, "created_unix": unix_now() });
                write_json(&dir.join("manifest.json"), &manifest)?;
                manifest
            }
            Err(e) => return Err(format!("manifest.json: {e}")),
        };
        let mut done = HashMap::new();
        let mut unreadable = 0_usize;
        if let Ok(text) = std::fs::read_to_string(dir.join("cells.jsonl")) {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                // Eine halbe letzte Zeile nach einem Abbruch ist kein Ergebnis.
                let Ok(cell) = serde_json::from_str::<Value>(line) else {
                    unreadable += 1;
                    continue;
                };
                if cell.get("valid") != Some(&Value::Bool(true)) {
                    continue;
                }
                if let (Some(key), Some(summary)) = (
                    cell.get("key").and_then(Value::as_str),
                    cell.get("summary").and_then(Summary::from_json),
                ) {
                    done.insert(key.to_owned(), summary);
                }
            }
        }
        if unreadable > 0 {
            println!("  {unreadable} unlesbare Zeile(n) in cells.jsonl uebersprungen");
        }
        Ok(Self {
            dir,
            manifest,
            done,
        })
    }

    /// Die Referenzwerte, mit denen Raten und Profile dieser Ablage rechnen:
    /// die des ersten Starts, sonst die gerade gemessenen. Eine Zelle aus
    /// einem spaeteren Start faehrt so dieselbe Rate wie ihre Nachbarn.
    fn reference(&mut self, measured: [u64; 4]) -> Result<[u64; 4], String> {
        let stored = self.manifest.get("reference").and_then(|r| {
            let get = |key: &str| r.get(key).and_then(Value::as_u64);
            Some([
                get("p50_us")?,
                get("p95_us")?,
                get("p99_us")?,
                get("samples")?,
            ])
        });
        if let Some(stored) = stored {
            return Ok(stored);
        }
        let [p50, p95, p99, samples] = measured;
        if let Some(object) = self.manifest.as_object_mut() {
            object.insert(
                "reference".to_owned(),
                json!({ "p50_us": p50, "p95_us": p95, "p99_us": p99, "samples": samples }),
            );
        }
        write_json(&self.dir.join("manifest.json"), &self.manifest)?;
        Ok(measured)
    }

    /// Haengt eine Zelle an, sofort und bis auf die Platte.
    fn record(&self, cell: &Value) -> Result<(), String> {
        let path = self.dir.join("cells.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        writeln!(file, "{cell}")
            .and_then(|()| file.sync_data())
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Eine Zelle fuer die Ablage.
fn cell_json(
    key: &str,
    scenario: &str,
    repeat: usize,
    arm: Arm,
    cap: usize,
    seconds: u64,
    measured: Option<(&ArmReport, &Summary)>,
    problem: Option<&str>,
) -> Value {
    let total = |f: fn(&pilot::CameraReport) -> u64| {
        measured.map(|(report, _)| report.cameras.iter().map(f).sum::<u64>())
    };
    json!({
        "key": key,
        "scenario": scenario,
        "repeat": repeat + 1,
        "arm": arm.id(),
        "cap": cap,
        "seconds": seconds,
        "valid": problem.is_none(),
        "problem": problem,
        "summary": measured.map(|(_, summary)| summary.to_json()),
        "sent": total(|c| c.sent),
        "delivered": total(|c| c.delivered),
        "rejected": total(|c| c.rejected),
        "buffers_exhausted": total(|c| c.buffers_exhausted),
        "buffers_quarantined": total(|c| c.buffers_quarantined),
        "llm_reports": measured.map(|(report, _)| report.llm.reports),
        "llm_failed": measured.map(|(report, _)| report.llm.failed),
        "finished_unix": unix_now(),
    })
}

/// Ein Abnahmekriterium und sein Ergebnis.
struct Criterion {
    k: &'static str,
    text: String,
    ok: bool,
}

/// Wie ein Lauf endete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunStatus {
    /// Jedes ausgewertete Kriterium erfuellt.
    Met,
    /// Sauber gelaufen, mindestens ein Kriterium verfehlt.
    Missed,
    /// Sauber gelaufen, aber kein Kriterium auswertbar (Teilmatrix).
    NoVerdict,
    /// Aufbau oder Lauf kaputt: das Ergebnis sagt nichts ueber den Governor.
    Broken,
}

impl RunStatus {
    const fn label(self) -> &'static str {
        match self {
            Self::Met => "bestanden",
            Self::Missed => "verfehlt",
            Self::NoVerdict => "ohne_urteil",
            Self::Broken => "kaputt",
        }
    }

    /// 0, 1 oder 2; die Funktionsprobe faellt fachlich nie durch.
    const fn exit(self, smoke: bool) -> u8 {
        match self {
            Self::Met | Self::NoVerdict => 0,
            Self::Missed => {
                if smoke {
                    0
                } else {
                    1
                }
            }
            Self::Broken => 2,
        }
    }
}

fn write_summary(
    dir: &Path,
    status: RunStatus,
    smoke: bool,
    criteria: &[Criterion],
    problem: Option<&str>,
    run: &Value,
) {
    let summary = json!({
        "status": status.label(),
        "exit": status.exit(smoke),
        "smoke": smoke,
        "problem": problem,
        "criteria": criteria
            .iter()
            .map(|c| json!({ "k": c.k, "text": c.text, "ok": c.ok }))
            .collect::<Vec<_>>(),
        "run": run,
        "finished_unix": unix_now(),
    });
    if let Err(e) = write_json(&dir.join("summary.json"), &summary) {
        eprintln!("summary.json: {e}");
    }
}

fn main() -> ExitCode {
    // Jede Panik ist ein kaputter Aufbau oder Lauf und nie ein fachliches
    // Ergebnis: Exitcode 2, und die Ablage sagt es auch.
    let Ok(options) = std::panic::catch_unwind(options) else {
        return ExitCode::from(RunStatus::Broken.exit(false));
    };
    let dir = options.out_dir();
    let smoke = options.smoke;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("Tokio-Runtime");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(run(&options))
    }));
    outcome.map_or_else(
        |_| {
            write_summary(
                &dir,
                RunStatus::Broken,
                smoke,
                &[],
                Some("Abbruch durch eine Panik, siehe Protokoll"),
                &Value::Null,
            );
            ExitCode::from(RunStatus::Broken.exit(smoke))
        },
        ExitCode::from,
    )
}

async fn run(options: &Options) -> u8 {
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

    let mut store = match Store::open(options) {
        Ok(store) => store,
        Err(problem) => {
            eprintln!("Ablage: {problem}");
            return RunStatus::Broken.exit(options.smoke);
        }
    };
    let arms = arms_for(options);
    let selected: Vec<&str> = SCENARIOS
        .iter()
        .map(|s| s.0)
        .filter(|name| options.scenarios.iter().any(|s| s == name))
        .collect();
    let (mut total_cells, mut open_cells) = (0_u64, 0_u64);
    for name in &selected {
        for repeat in 0..options.repeats {
            for &arm in &arms {
                for &cap in &options.caps {
                    total_cells += 1;
                    if !store.done.contains_key(&cell_key(name, repeat, arm, cap)) {
                        open_cells += 1;
                    }
                }
            }
        }
    }
    let per_cell = options.seconds + CELL_OVERHEAD_S;
    println!(
        "Ablage {} · Matrix {} Lastpunkt(e) × {} Wdh × {} Arme × {} Tiefe(n) = {total_cells} Zellen à {} s · \
         {} schon gemessen, {open_cells} offen · geschaetzt {} min zuzueglich Referenzdurchlauf",
        store.dir.display(),
        selected.len(),
        options.repeats,
        arms.len(),
        options.caps.len(),
        options.seconds,
        total_cells - open_cells,
        (open_cells * per_cell).div_ceil(60),
    );

    let started = Instant::now();
    let loaded = load(options);
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
    let mut reference = Box::pin(reference(
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
    let measured = [
        reference.p50_us,
        reference.p95_us,
        reference.p99_us,
        reference.samples as u64,
    ];
    match store.reference(measured) {
        Ok(used) => {
            if used != measured {
                println!(
                    "  Raten und Profile aus dem ersten Start dieser Ablage: p50 {} us, p95 {} us, p99 {} us",
                    used[0], used[1], used[2]
                );
            }
            reference.p50_us = used[0];
            reference.p95_us = used[1];
            reference.p99_us = used[2];
            reference.samples = used[3] as usize;
        }
        Err(problem) => {
            eprintln!("Ablage: {problem}");
            write_summary(
                &store.dir,
                RunStatus::Broken,
                options.smoke,
                &[],
                Some(&problem),
                &Value::Null,
            );
            return RunStatus::Broken.exit(options.smoke);
        }
    }
    println!(
        "  Rest geschaetzt {} min fuer {open_cells} offene Zellen",
        (open_cells * per_cell).div_ceil(60)
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
    let mut by_scenario: Vec<(String, ArmResults)> = Vec::new();
    let mut broken: Option<String> = None;
    'matrix: for &(name, cams, utilisation) in &SCENARIOS {
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
        let mut results: Vec<(Arm, Vec<Summary>)> = arms.iter().map(|a| (*a, Vec::new())).collect();
        for repeat in 0..options.repeats {
            for (arm, collected) in &mut results {
                let mut best: Option<Summary> = None;
                for &cap in &options.caps {
                    let key = cell_key(name, repeat, *arm, cap);
                    if let Some(summary) = store.done.get(&key) {
                        println!(
                            "  [{name} {} Wdh {} Tiefe {cap}] aus cells.jsonl",
                            arm.label(),
                            repeat + 1
                        );
                        best = Some(match best {
                            Some(b) => better(b, summary.clone()),
                            None => summary.clone(),
                        });
                        continue;
                    }
                    let with_report = *arm != Arm::VigilantNoReport && options.llm.is_some();
                    let gateway = if *arm == Arm::Direct {
                        None
                    } else {
                        let yaml = gateway_yaml(options, cams, rate, &reference, with_report);
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
                    let outcome = Box::pin(pilot::run_arm(ArmConfig {
                        endpoint,
                        via_governor: gateway.is_some(),
                        cameras,
                        in_flight_cap: cap,
                        duration,
                        llm,
                    }))
                    .await;
                    if let Some(g) = gateway {
                        g.finish().await;
                    }
                    let report = match outcome {
                        Ok(report) => report,
                        Err(problem) => {
                            let cell = cell_json(
                                &key,
                                name,
                                repeat,
                                *arm,
                                cap,
                                options.seconds,
                                None,
                                Some(&problem),
                            );
                            let _ = store.record(&cell);
                            broken = Some(format!("{key}: {problem}"));
                            break 'matrix;
                        }
                    };
                    let summary = summarise(&report, options.seconds);
                    // Nichts geliefert heisst: der Aufbau ist kaputt, nicht der
                    // Governor schlecht. Die Zelle zaehlt nicht, der Lauf endet.
                    let delivered: u64 = report.cameras.iter().map(|c| c.delivered).sum();
                    let problem = (delivered == 0)
                        .then(|| "keine einzige Lieferung, Aufbau pruefen".to_owned());
                    let cell = cell_json(
                        &key,
                        name,
                        repeat,
                        *arm,
                        cap,
                        options.seconds,
                        Some((&report, &summary)),
                        problem.as_deref(),
                    );
                    if let Err(e) = store.record(&cell) {
                        broken = Some(format!("Ablage: {e}"));
                        break 'matrix;
                    }
                    if let Some(problem) = problem {
                        broken = Some(format!("{key}: {problem}"));
                        break 'matrix;
                    }
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

    let run_facts = json!({
        "cells_total": total_cells,
        "cells_measured_before": total_cells - open_cells,
        "reference": { "p50_us": reference.p50_us, "p95_us": reference.p95_us, "p99_us": reference.p99_us },
    });
    if let Some(problem) = broken {
        eprintln!("\nLauf abgebrochen, kein Urteil: {problem}");
        eprintln!("Gueltige Zellen stehen in cells.jsonl; ein erneuter Start setzt dort fort.");
        write_summary(
            &store.dir,
            RunStatus::Broken,
            options.smoke,
            &[],
            Some(&problem),
            &run_facts,
        );
        drop(regions);
        return RunStatus::Broken.exit(options.smoke);
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
    let mut criteria: Vec<Criterion> = Vec::new();
    let mut verdict = |k: &'static str, text: String, ok: bool| {
        criteria.push(Criterion { k, text, ok });
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
    for c in &criteria {
        println!(
            "  {} {} — {}",
            c.k,
            if c.ok { "bestanden" } else { "VERFEHLT" },
            c.text
        );
    }
    let status = if criteria.is_empty() {
        RunStatus::NoVerdict
    } else if criteria.iter().all(|c| c.ok) {
        RunStatus::Met
    } else {
        RunStatus::Missed
    };
    println!(
        "\nErgebnis: {} (Exitcode {}{}), Ablage {}",
        status.label(),
        status.exit(options.smoke),
        if options.smoke {
            ", Funktionsprobe: nur 2 heisst kaputt"
        } else {
            ""
        },
        store.dir.display()
    );
    write_summary(
        &store.dir,
        status,
        options.smoke,
        &criteria,
        None,
        &run_facts,
    );
    drop(regions);
    status.exit(options.smoke)
}
