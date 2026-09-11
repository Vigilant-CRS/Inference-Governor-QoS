//! `load-ramp` — ab welcher Auslastung lohnt sich der Governor? (Spec 19.4)
//!
//! Punktmessungen beantworten die Frage nicht, die ein Anwender zuerst stellt.
//! Er will nicht wissen, ob Vigilant bei 116 % Auslastung hilft — er will
//! wissen, **ab wann** es sich lohnt und ob es bei geringer Last schadet.
//!
//! Gemessen wird gegen echten Triton auf echter GPU, über System Shared
//! Memory, mit denselben Modellen auf beiden Seiten. Die Angebotslast wird
//! über die Sensorperioden variiert: die Kamera liefert schneller oder
//! langsamer, die Modelle bleiben dieselben. Das ist der realistische Fall —
//! nicht die Modelle werden teurer, die Bildrate steigt.
//!
//! ## Warum Wiederholungen
//!
//! Ein einzelner Lauf dieses Benchmarks lieferte einmal 52 % statt 98 % auf
//! **unverändertem Code**, nur weil andere Prozesse mitliefen. Jeder Punkt
//! wird deshalb mehrfach gefahren, berichtet werden Median und Spannweite, und
//! die Systemlast steht im Protokoll.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;
use vig_bench::shm::Region;
use vig_bench::workload::{Burst, InputSpec, StreamDef, StreamReport, drive};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::{
    ModelMetadataRequest, SystemSharedMemoryRegisterRequest, SystemSharedMemoryUnregisterRequest,
};

const SECONDS: u64 = 15;
const REPEATS: usize = 3;
/// Angebotslast in Prozent (Spec 19.4).
const LOADS: [u64; 7] = [50, 75, 90, 100, 110, 125, 150];
/// Client-Puffertiefen; je Strom zählt das bessere Ergebnis, auf beiden Seiten.
const CAPS: [usize; 2] = [1, 8];

/// Die Basiskonfiguration: Perioden bei 100 % Angebotslast.
///
/// Kalibriert gegen die **tatsaechliche** Laufzeit (p50), nicht gegen die
/// konservative Planungsgroesse. Der Unterschied ist erheblich und war in der
/// ersten Fassung dieses Benchmarks ein Fehler: mit p99 mal Marge kalibriert
/// lag die reale Auslastung bei nominell 150 % erst bei rund 95 %, und die
/// Rampe hat nie gesaettigt. Triton zeigte folgerichtig ueberall null
/// unabgedeckte Perioden — ein Ergebnis, das nur besagte, dass nichts
/// gemessen wurde.
///
/// Gemessene Mediane: RF-DETR 14,9 ms, Pose 4,0 ms, Tiefe 7,9 ms.
/// Mit Periode `P` fuer Detektor und Pose und `2P` fuer Tiefe gilt
/// `U = (14,9 + 4,0 + 3,95) / P`; fuer `U = 1` folgt `P = 23 ms`.
const BASE: [(&str, &str, u64, u64); 3] = [
    // (logisch, physisch, Periode ms bei 100 %, max_age ms)
    ("detector", "rfdetr", 23, 46),
    ("pose", "pose_main", 23, 46),
    ("depth", "depth_main", 46, 92),
];

fn scaled_period(base_ms: u64, load_percent: u64) -> u64 {
    // Höhere Last bedeutet kürzere Periode.
    base_ms
        .saturating_mul(100)
        .checked_div(load_percent)
        .unwrap_or(base_ms)
        .max(5)
}

/// Die Stellgroessen eines Experiments, aus der Umgebung (ADR-0038).
///
/// Ohne gesetzte Variable ist jede davon die bisherige Voreinstellung, und die
/// Konfiguration des Governors ist bitgleich zu der vor diesen Schaltern. Der
/// Direktpfad zu Triton bleibt in jedem Fall unberuehrt.
///
/// * `VIG_RAMP_MARGIN` — `safety_margin_percent`, Voreinstellung 110.
/// * `VIG_RAMP_PREDICTION` — `shadow` oder `active`.
/// * `VIG_RAMP_MARGIN_LEARNING` — `an` oder `aus`: `margin_learning: {}`.
/// * `VIG_RAMP_PROFILE_SCALE` — die Profile des Governors mal diesem Faktor,
///   z. B. `2` oder `0.7`: ein absichtlich falsches Profil, damit die
///   Konvergenz der Kalibrierung auf echter Hardware sichtbar wird.
/// * `VIG_RAMP_POINTS` — die Lastpunkte der Rampe in Prozent, z. B.
///   `90,95,100,105,110,125`.
/// * `VIG_RAMP_PIPELINING` — `pipelining_depth`, Voreinstellung 0. Mit null
///   startet der Governor den naechsten Auftrag erst nach der Antwort auf den
///   vorigen; Triton direkt hat bis zu acht in der Schwebe. Die Luecke
///   dazwischen ist der Kandidat fuer den Verlust an der Kante (ADR-0038).
#[derive(Debug, Clone)]
struct Knobs {
    margin_percent: u32,
    pipelining: usize,
    active: bool,
    learning: bool,
    profile_permille: u64,
    points: Vec<u64>,
}

impl Knobs {
    fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let margin_percent = var("VIG_RAMP_MARGIN").map_or(110, |v| {
            v.trim()
                .parse()
                .expect("VIG_RAMP_MARGIN: eine ganze Zahl in Prozent")
        });
        let active = var("VIG_RAMP_PREDICTION").is_some_and(|v| match v.trim() {
            "active" => true,
            "shadow" => false,
            other => panic_on(&format!(
                "VIG_RAMP_PREDICTION: shadow oder active, nicht {other}"
            )),
        });
        let learning = var("VIG_RAMP_MARGIN_LEARNING").is_some_and(|v| {
            match v.trim().to_lowercase().as_str() {
                "an" | "on" | "ja" | "true" | "1" => true,
                "aus" | "off" | "nein" | "false" | "0" => false,
                other => panic_on(&format!(
                    "VIG_RAMP_MARGIN_LEARNING: an oder aus, nicht {other}"
                )),
            }
        });
        let profile_permille = var("VIG_RAMP_PROFILE_SCALE").map_or(1_000, |v| {
            permille(&v).expect("VIG_RAMP_PROFILE_SCALE: ein Faktor wie 2 oder 0.7")
        });
        let pipelining = var("VIG_RAMP_PIPELINING").map_or(0, |v| {
            v.trim()
                .parse()
                .expect("VIG_RAMP_PIPELINING: eine ganze Zahl")
        });
        let points = var("VIG_RAMP_POINTS").map_or_else(
            || LOADS.to_vec(),
            |v| {
                v.split(',')
                    .map(|p| p.trim().parse().expect("VIG_RAMP_POINTS: z. B. 90,100,110"))
                    .collect()
            },
        );
        Self {
            margin_percent,
            pipelining,
            active,
            learning,
            profile_permille,
            points,
        }
    }

    /// Eine Profillaufzeit, mal dem Profilfaktor.
    fn scale(&self, us: u64) -> u64 {
        us * self.profile_permille / 1_000
    }

    /// Was die Konfiguration des Governors ueber die Marge hinaus bekommt.
    fn backend_extra(&self) -> String {
        let mut extra = String::new();
        if self.active {
            extra.push_str("\n  prediction: active");
        }
        if self.learning {
            extra.push_str("\n  margin_learning: {}");
        }
        extra
    }

    /// Eine Kopfzeile, damit jede Ergebnisdatei ihre Einstellungen nennt.
    fn describe(&self) -> String {
        format!(
            "Governor: Marge {} %, Pipelining {}, Prognose {}, Margenlernen {}, Profil x{}.{:03}",
            self.margin_percent,
            self.pipelining,
            if self.active { "active" } else { "shadow" },
            if self.learning { "an" } else { "aus" },
            self.profile_permille / 1_000,
            self.profile_permille % 1_000,
        )
    }
}

/// Bricht mit einer Meldung ab: ein Experiment mit einer unverstandenen
/// Einstellung soll nicht stillschweigend etwas anderes messen.
#[allow(clippy::panic)]
fn panic_on(message: &str) -> bool {
    panic!("{message}")
}

/// `2` -> 2000, `0.7` -> 700, `1,25` -> 1250; hoechstens drei Nachkommastellen.
fn permille(text: &str) -> Option<u64> {
    let text = text.trim().replace(',', ".");
    let (int, frac) = text.split_once('.').unwrap_or((text.as_str(), ""));
    if frac.len() > 3 {
        return None;
    }
    let int: u64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let frac: u64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<3}").parse().ok()?
    };
    Some(int * 1_000 + frac).filter(|v| *v > 0)
}

fn config_yaml(load: u64, endpoint: &str, knobs: &Knobs) -> String {
    let mut models = String::new();
    for (logical, physical, base_period, base_age) in BASE {
        let period = scaled_period(base_period, load);
        let max_age = scaled_period(base_age, load);
        let class = if logical == "detector" {
            "protected"
        } else {
            "high"
        };
        let (p50, p95, p99) = match physical {
            "rfdetr" => (14916, 17081, 17470),
            "pose_main" => (3987, 4630, 5506),
            _ => (7908, 9367, 9534),
        };
        let (p50, p95, p99) = (knobs.scale(p50), knobs.scale(p95), knobs.scale(p99));
        let _ = write!(
            models,
            "\n  {logical}:\n    class: {class}\n    \
             queue: {{ policy: latest, capacity: 1 }}\n    \
             contract: {{ period_ms: {period}, deadline_ms: {}, max_age_ms: {max_age} }}\n    \
             variants:\n      - id: main\n        backend_model: {physical}\n        \
             quality: {{ value: 1.0, source: user_declared }}\n        \
             profile: {{ p50_us: {p50}, p95_us: {p95}, p99_us: {p99}, samples: 120 }}",
            period.saturating_mul(3).checked_div(2).unwrap_or(period),
        );
    }
    format!(
        "version: 1\nbackend:\n  type: triton\n  grpc_endpoint: {endpoint}\n  \
         slots: 1\n  pipelining_depth: {}\n  safety_margin_percent: {}{}\nmodels:{models}\n",
        knobs.pipelining,
        knobs.margin_percent,
        knobs.backend_extra(),
    )
}

/// Eine Lastspitze: Spitzenlast in Prozent, Dauer, Abstand.
type Peak = (u64, Duration, Duration);

fn streams(
    load: u64,
    cap: usize,
    logical: bool,
    specs: &HashMap<String, InputSpec>,
    peak: Option<Peak>,
) -> Vec<StreamDef> {
    BASE.iter()
        .map(|(name, physical, base_period, base_age)| StreamDef {
            name,
            model: if logical { name } else { physical },
            period: Duration::from_millis(scaled_period(*base_period, load)),
            max_age: Duration::from_millis(scaled_period(*base_age, load)),
            in_flight_cap: cap,
            input: specs.get(*name).cloned(),
            pump: false,
            burst: peak.map(|(peak_load, length, every)| Burst {
                every,
                length,
                period: Duration::from_millis(scaled_period(*base_period, peak_load)),
            }),
        })
        .collect()
}

/// Der Faktor zwischen zwei Promillewerten, lesbar in beide Richtungen.
fn factor(direct: u64, governed: u64) -> String {
    if governed == 0 {
        if direct == 0 {
            "—".to_owned()
        } else {
            "besser".to_owned()
        }
    } else if direct >= governed {
        format!("{:.1}x", direct as f64 / governed as f64)
    } else {
        format!("-{:.1}x", governed as f64 / direct.max(1) as f64)
    }
}

/// Der Median einer Messreihe.
fn median(mut values: Vec<u64>) -> u64 {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

fn spread(values: &[u64]) -> (u64, u64) {
    (
        values.iter().copied().min().unwrap_or(0),
        values.iter().copied().max().unwrap_or(0),
    )
}

fn load_average() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(ToOwned::to_owned))
        .unwrap_or_else(|| "?".to_owned())
}

async fn start_gateway(yaml: &str) -> String {
    let config = Config::from_yaml(yaml).expect("Konfiguration gueltig");
    let findings = config.diagnose();
    assert!(
        findings.is_empty(),
        "Konfiguration hat Befunde: {findings:?}"
    );
    let resolved = Arc::new(config.resolve().expect("aufloesbar"));
    let clock = MonotonicClock::start();
    let triton = Arc::new(vig_backend_triton::TritonClient::new(
        &resolved.backend_endpoint,
    ));
    let handle = actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler");
    // Wie `vig serve`: der Governor beobachtet die Karte.
    handle.observe_hardware();
    let service = GatewayService::new(resolved, triton, handle, clock);
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
    tokio::time::sleep(Duration::from_millis(150)).await;
    address
}

/// Die unabgedeckten Perioden des **geschuetzten** Stroms.
///
/// Fuer die Produktaussage zaehlt dieser. Dass `high`-Stroeme hinter
/// `protected` zuruecktreten, ist die konfigurierte Absicht und kein Defekt;
/// sie mit in ein Maximum zu werfen wuerde beides vermengen.
fn protected_uncovered(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .find(|r| r.name == "detector")
        .map_or(0, |r| r.coverage.uncovered_permille())
}

/// Die Verbrauchersicht des geschuetzten Stroms: Abtastzeitpunkte ohne
/// brauchbares Ergebnis, in Promille.
///
/// Neben der Fenstersicht, weil die unter Lastspitzen die Phase misst statt
/// die Versorgung: nach einer Spitze liegen Aufnahmen und Fenstergrenzen
/// gegeneinander verschoben, eine Lieferung faellt mal vor, mal hinter die
/// Grenze, und leere Fenster entstehen, ohne dass dem Verbraucher etwas
/// fehlte (docs/analysis/bursts-and-frontier.md).
fn protected_consumer(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .find(|r| r.name == "detector")
        .map_or(0, |r| r.coverage.consumer_uncovered_permille())
}

/// Die schlechteste Verbrauchersicht über alle Ströme, in Promille.
fn worst_consumer(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .map(|r| r.coverage.consumer_uncovered_permille())
        .max()
        .unwrap_or(0)
}

/// Die schlechteste Abdeckung über alle Ströme, in Promille unabgedeckt.
fn worst_uncovered(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .map(|r| r.coverage.uncovered_permille())
        .max()
        .unwrap_or(0)
}

/// Die laengste Versorgungsluecke des geschuetzten Stroms, in Millisekunden.
///
/// Unter Lastspitzen die entscheidende Groesse: eine Abdeckung von 97 % kann
/// verstreute Ausfaelle bedeuten oder einen Block, in dem der Regler eine
/// halbe Sekunde blind ist. Fuer eine Regelung ist das der ganze Unterschied.
fn protected_gap_ms(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .find(|r| r.name == "detector")
        .map_or(0, |r| r.coverage.longest_gap_ns / 1_000_000)
}

fn worst_response_age_ms(reports: &[StreamReport]) -> u64 {
    reports
        .iter()
        .map(|r| r.coverage.response_age_p95_ns / 1_000_000)
        .max()
        .unwrap_or(0)
}

/// Lastspitzen ueber einer Grundlast (Spec 19.4: Bursts).
///
/// Die stationaere Rampe beantwortet, ab welcher Dauerlast sich der Governor
/// lohnt. Ihr eigenes Grenzenkapitel nennt den Fall, den sie nicht abbildet —
/// und der der realistische ist: ein System, das im Mittel unter der
/// Saettigung laeuft und Spitzen darueber hat. Eine Mittelwertrechnung sagt
/// dort „passt". Ob der Regler waehrend der Spitze blind wird, sagt sie nicht.
///
/// Der Vertrag bleibt der der Grundlast: der Betreiber hat fuer den
/// Normalbetrieb konfiguriert, nicht fuer die Spitze. Waehrend der Spitze
/// liefert die Kamera schneller; der Verbraucher tastet weiter im Takt der
/// Grundlast ab.
///
/// (Grundlast %, Spitzenlast %, Spitzendauer ms, Abstand ms)
const BURSTS: [(u64, u64, u64, u64); 3] = [
    (90, 150, 200, 2_000),
    (90, 150, 500, 2_000),
    (75, 150, 1_000, 4_000),
];

/// Laenger als die stationaere Rampe: bei 4 s Abstand sollen es noch
/// fuenf Spitzen je Lauf sein.
const BURST_SECONDS: u64 = 20;

#[allow(clippy::similar_names)]
async fn bursts(triton_endpoint: &str, specs: &HashMap<String, InputSpec>, knobs: &Knobs) {
    println!("load-ramp bursts: Lastspitzen ueber einer Grundlast (Spec 19.4)");
    println!(
        "Triton {triton_endpoint} · RF-DETR, Pose, Tiefe · ein Slot · \
         Shared Memory auf beiden Seiten"
    );
    println!("{}", knobs.describe());
    println!(
        "{BURST_SECONDS} s je Lauf, {REPEATS} Wiederholungen je Profil, \
         Puffertiefen {CAPS:?} auf beiden Seiten\n"
    );
    println!(
        "  Profil                  | Mittel | Detektor Triton | Vigilant | Faktor | \
         laengste Luecke T/O | alle Stroeme T/O | Verbraucher Det. T/O | alle T/O | Last-Ø"
    );
    println!(
        "  ------------------------|--------|-----------------|----------|--------|\
         ---------------------|------------------|----------------------|----------|-------"
    );

    let duration = Duration::from_secs(BURST_SECONDS);
    for (base, peak, length_ms, every_ms) in BURSTS {
        let yaml = config_yaml(base, triton_endpoint, knobs);
        let gateway = start_gateway(&yaml).await;
        let burst = Some((
            peak,
            Duration::from_millis(length_ms),
            Duration::from_millis(every_ms),
        ));
        let mean = base + (peak - base) * length_ms / every_ms;

        let mut direct_unc = Vec::new();
        let mut gov_unc = Vec::new();
        let mut direct_gap = Vec::new();
        let mut gov_gap = Vec::new();
        let mut direct_all = Vec::new();
        let mut gov_all = Vec::new();
        let mut direct_cons = Vec::new();
        let mut gov_cons = Vec::new();
        let mut direct_cons_all = Vec::new();
        let mut gov_cons_all = Vec::new();
        let mut loads = Vec::new();

        for _ in 0..REPEATS {
            loads.push(load_average());
            let (mut bd, mut bg) = (u64::MAX, u64::MAX);
            let (mut bd_gap, mut bg_gap) = (u64::MAX, u64::MAX);
            let (mut bd_all, mut bg_all) = (u64::MAX, u64::MAX);
            let (mut bd_cons, mut bg_cons) = (u64::MAX, u64::MAX);
            let (mut bd_cons_all, mut bg_cons_all) = (u64::MAX, u64::MAX);
            for cap in CAPS {
                let d = drive(
                    triton_endpoint,
                    &streams(base, cap, false, specs, burst),
                    duration,
                    false,
                )
                .await;
                bd = bd.min(protected_uncovered(&d));
                bd_gap = bd_gap.min(protected_gap_ms(&d));
                bd_all = bd_all.min(worst_uncovered(&d));
                bd_cons = bd_cons.min(protected_consumer(&d));
                bd_cons_all = bd_cons_all.min(worst_consumer(&d));

                let g = drive(
                    &gateway,
                    &streams(base, cap, true, specs, burst),
                    duration,
                    true,
                )
                .await;
                bg = bg.min(protected_uncovered(&g));
                bg_gap = bg_gap.min(protected_gap_ms(&g));
                bg_all = bg_all.min(worst_uncovered(&g));
                bg_cons = bg_cons.min(protected_consumer(&g));
                bg_cons_all = bg_cons_all.min(worst_consumer(&g));
            }
            direct_unc.push(bd);
            gov_unc.push(bg);
            direct_gap.push(bd_gap);
            gov_gap.push(bg_gap);
            direct_all.push(bd_all);
            gov_all.push(bg_all);
            direct_cons.push(bd_cons);
            gov_cons.push(bg_cons);
            direct_cons_all.push(bd_cons_all);
            gov_cons_all.push(bg_cons_all);
        }

        let d = median(direct_unc.clone());
        let g = median(gov_unc.clone());
        let (dmin, dmax) = spread(&direct_unc);
        let (gmin, gmax) = spread(&gov_unc);
        println!(
            "  {base:>3} → {peak:>3} %, {length_ms:>4}/{every_ms:>4} ms | {mean:>4} % | \
             {d:>6} ‰ [{dmin}-{dmax}] | {g:>3} ‰ [{gmin}-{gmax}] | {:>6} | \
             {:>7} / {:>4} ms | {:>5} / {:>5} ‰ | {:>9} / {:>5} ‰ | {:>3} / {:>3} ‰ | {}",
            factor(d, g),
            median(direct_gap),
            median(gov_gap),
            median(direct_all),
            median(gov_all),
            median(direct_cons),
            median(gov_cons),
            median(direct_cons_all),
            median(gov_cons_all),
            loads.join(" "),
        );
    }

    println!(
        "\nUnabgedeckte Lieferfenster des geschuetzten Stroms nach ADR-0005, Median\n\
         und Spannweite ueber {REPEATS} Wiederholungen. Mittel ist die zeitgewichtete\n\
         Angebotslast. Der Vertrag ist der der Grundlast; waehrend einer Spitze\n\
         liefert die Kamera schneller, der Verbraucher tastet im Grundtakt ab.\n\
         Verbraucher: Anteil der Periodenenden ohne Ergebnis unter max_age (NV-01).\n\
         Unter Spitzen weichen Fenster- und Verbrauchersicht stark voneinander ab;\n\
         die Fenstersicht misst dort vor allem die Phase zwischen Aufnahme und\n\
         Fenstergrenze (docs/analysis/bursts-and-frontier.md).\n\
         Die laengste Luecke ist der Median der Maxima — ein einzelnes Maximum\n\
         ist keine Aussage (gate-m3-r03.md)."
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
    let triton_endpoint = "127.0.0.1:8001";
    let triton = vig_backend_triton::TritonClient::new(triton_endpoint);

    // Shared-Memory-Regionen je Modell, einmal fuer den ganzen Lauf.
    let mut specs = HashMap::new();
    let mut regions = Vec::new();
    for (logical, physical, _, _) in BASE {
        let metadata = triton
            .raw()
            .await
            .expect("Backend erreichbar")
            .model_metadata(ModelMetadataRequest {
                name: physical.to_owned(),
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
        let region = Region::create(&format!("vig_ramp_{logical}"), byte_size).expect("Shm-Region");
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
        specs.insert(
            logical.to_owned(),
            InputSpec {
                name: input.name.clone(),
                datatype: input.datatype.clone(),
                shape,
                region: Some(region.name.clone()),
                byte_size,
            },
        );
        regions.push(region);
    }

    let knobs = Knobs::from_env();
    if std::env::args().nth(1).as_deref() == Some("bursts") {
        Box::pin(bursts(triton_endpoint, &specs, &knobs)).await;
        return;
    }

    println!("load-ramp: ab welcher Auslastung lohnt sich der Governor?");
    println!(
        "Triton {triton_endpoint} · RF-DETR, Pose, Tiefe · ein Slot · \
         Shared Memory auf beiden Seiten"
    );
    println!("{}", knobs.describe());
    println!(
        "{SECONDS} s je Lauf, {REPEATS} Wiederholungen je Punkt, \
         Puffertiefen {CAPS:?} auf beiden Seiten\n"
    );
    println!(
        "  Last | Detektor Triton | Vigilant | Faktor | alle Stroeme T/O | Antwortalter p95 T/O | Last-Ø"
    );
    println!(
        "  -----|-----------------|----------|--------|------------------|-------------|-------"
    );

    let duration = Duration::from_secs(SECONDS);
    for load in knobs.points.iter().copied() {
        let yaml = config_yaml(load, triton_endpoint, &knobs);
        let gateway = start_gateway(&yaml).await;

        let mut direct_unc = Vec::new();
        let mut direct_response_age = Vec::new();
        let mut gov_unc = Vec::new();
        let mut gov_response_age = Vec::new();
        let mut direct_all = Vec::new();
        let mut gov_all = Vec::new();
        let mut loads = Vec::new();

        for _ in 0..REPEATS {
            loads.push(load_average());

            let mut best_direct = u64::MAX;
            let mut best_direct_response_age = u64::MAX;
            let mut best_gov = u64::MAX;
            let mut best_gov_response_age = u64::MAX;
            let mut best_direct_all = u64::MAX;
            let mut best_gov_all = u64::MAX;
            for cap in CAPS {
                let d = drive(
                    triton_endpoint,
                    &streams(load, cap, false, &specs, None),
                    duration,
                    false,
                )
                .await;
                best_direct = best_direct.min(protected_uncovered(&d));
                best_direct_all = best_direct_all.min(worst_uncovered(&d));
                best_direct_response_age = best_direct_response_age.min(worst_response_age_ms(&d));

                let g = drive(
                    &gateway,
                    &streams(load, cap, true, &specs, None),
                    duration,
                    true,
                )
                .await;
                best_gov = best_gov.min(protected_uncovered(&g));
                best_gov_all = best_gov_all.min(worst_uncovered(&g));
                best_gov_response_age = best_gov_response_age.min(worst_response_age_ms(&g));
            }
            direct_unc.push(best_direct);
            direct_response_age.push(best_direct_response_age);
            gov_unc.push(best_gov);
            gov_response_age.push(best_gov_response_age);
            direct_all.push(best_direct_all);
            gov_all.push(best_gov_all);
        }

        let d = median(direct_unc.clone());
        let g = median(gov_unc.clone());
        let (dmin, dmax) = spread(&direct_unc);
        let (gmin, gmax) = spread(&gov_unc);
        let factor = factor(d, g);
        println!(
            "  {load:>3} % | {d:>6} ‰ [{dmin}-{dmax}] | {g:>3} ‰ [{gmin}-{gmax}] | {factor:>6} | \
             {:>5} / {:>5} ‰ | {:>3} / {:>3} ms | {}",
            median(direct_all),
            median(gov_all),
            median(direct_response_age),
            median(gov_response_age),
            loads.join(" "),
        );
    }

    println!(
        "\nUnabgedeckte Perioden nach ADR-0005, schlechtester Strom je Lauf.\n\
         In eckigen Klammern die Spannweite ueber {REPEATS} Wiederholungen.\n\
         Last-Ø ist die Systemlast beim Start jeder Wiederholung — eine\n\
         Latenzmessung neben anderer Arbeit misst die andere Arbeit."
    );
}
