//! `onetimer calibrate` — die Hardware ausmessen, statt sie zu raten (WP12).
//!
//! `onetimer profile` misst, wie lange ein Modell **allein** braucht. Das ist
//! der einfache Teil. Auf einem Geraet mit einer GPU laufen die Modelle aber
//! nicht allein, und wie stark sie sich gegenseitig bremsen, stand bisher in
//! keiner Konfiguration: der Governor plante unter Nebenlast mit dem
//! Alleinprofil und ueberliess die Korrektur dem Online Estimator
//! (ADR-0006).
//!
//! Dieses Werkzeug misst beides und schreibt eine fertige Konfiguration.
//!
//! ## Was gemessen werden kann und was nicht
//!
//! **Messbar sind Hardwaretatsachen:** wie lange eine Inferenz dauert, wie
//! sehr zwei Modelle einander bremsen, ab wann Nebenlaeufigkeit nichts mehr
//! bringt. Diese Zahlen gehoeren gemessen und nicht geschaetzt.
//!
//! **Nicht messbar sind Anforderungen:** wie frisch ein Ergebnis sein muss,
//! welcher Strom wichtiger ist, welche Deadline gilt. Das sind Aussagen
//! darueber, was der Roboter braucht, und die kann nur der Betreiber treffen.
//! Ein System, das sich seine Deadlines selbst ausdenkt, kann an ihnen nicht
//! mehr gemessen werden.
//!
//! Der Kalibrator fasst deshalb Vertraege nicht an. Er fuellt Profile,
//! schlaegt eine Slotzahl vor und nennt Modellpaare, die sich nicht vertragen.

use crate::profile::{WARMUP, build_request};
use onetimer_backend_triton::TritonClient;
use onetimer_config::schema::{Config, ProfileConfig};
use onetimer_protocol_oip::inference::{
    ModelInferRequest, ModelMetadataResponse, ServerMetadataRequest, ServerMetadataResponse,
};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Das Verhaeltnis zweier Laufzeiten in Prozent.
///
/// Ueber Ganzzahlen statt `f64`: Mikrosekundenwerte passen nicht verlustfrei
/// in eine 52-Bit-Mantisse, und ein Schwellwertvergleich, der von einer
/// Rundung abhaengen kann, ist kein Schwellwertvergleich.
fn slowdown_percent(under_load_us: u64, solo_us: u64) -> u64 {
    under_load_us
        .saturating_mul(100)
        .checked_div(solo_us.max(1))
        .unwrap_or(100)
}

/// Formatiert ein Prozentverhaeltnis als `N.MMx`.
fn as_factor(percent: u64) -> String {
    let whole = percent.checked_div(100).unwrap_or(0);
    let rest = percent.checked_rem(100).unwrap_or(0);
    format!("{whole}.{rest:02}x")
}

/// Ab diesem Verhaeltnis lohnt Nebenlaeufigkeit nicht mehr, in Prozent.
///
/// Bremst ein Modell ein anderes auf die doppelte Laufzeit, bringt paralleles
/// Ausfuehren keinen Durchsatz mehr — zwei Auftraege brauchen nebeneinander
/// genauso lange wie nacheinander — und kostet nur Latenz. Der Wert ist damit
/// keine Geschmacksfrage, sondern der Punkt, an dem sich das Vorzeichen des
/// Nutzens umdreht.
const NO_CORUN_SLOWDOWN_PERCENT: u64 = 200;

/// Eine Messreihe.
struct Measured {
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    samples: u32,
}

impl Measured {
    fn to_config(&self, fingerprint: Option<String>) -> ProfileConfig {
        ProfileConfig {
            p50_us: self.p50_us,
            p95_us: self.p95_us,
            p99_us: self.p99_us,
            samples: self.samples,
            fingerprint,
        }
    }
}

fn quantiles(mut v: Vec<u64>) -> Measured {
    v.sort_unstable();
    let pick = |percent: usize| -> u64 {
        if v.is_empty() {
            return 0;
        }
        let index = v
            .len()
            .saturating_mul(percent)
            .checked_div(100)
            .unwrap_or(0)
            .min(v.len().saturating_sub(1));
        v.get(index).copied().unwrap_or(0)
    };
    Measured {
        p50_us: pick(50),
        p95_us: pick(95),
        p99_us: pick(99),
        samples: u32::try_from(v.len()).unwrap_or(u32::MAX),
    }
}

/// Misst `samples` Inferenzen nacheinander.
async fn measure(client: &TritonClient, request: &ModelInferRequest, samples: usize) -> Vec<u64> {
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        if client.infer(request.clone()).await.is_err() {
            continue;
        }
        out.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
    out
}

/// Haelt im Hintergrund dauerhaft `count` Auftraege in Flug.
///
/// Damit entsteht der Belegungsgrad, unter dem gemessen werden soll. Der
/// Schalter beendet die Schleifen; ohne ihn liefen sie weiter und wuerden die
/// naechste Messreihe verfaelschen.
fn spawn_load(
    client: &Arc<TritonClient>,
    request: &ModelInferRequest,
    count: usize,
    stop: &Arc<AtomicBool>,
) -> Vec<tokio::task::JoinHandle<()>> {
    (0..count)
        .map(|_| {
            let client = Arc::clone(client);
            let request = request.clone();
            let stop = Arc::clone(stop);
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    if client.infer(request.clone()).await.is_err() {
                        break;
                    }
                }
            })
        })
        .collect()
}

async fn stop_load(stop: &Arc<AtomicBool>, tasks: Vec<tokio::task::JoinHandle<()>>) {
    stop.store(true, Ordering::Relaxed);
    for task in tasks {
        let _ = task.await;
    }
}

/// Alles, was zu einem Backendmodell gemessen wurde.
struct VariantMeasurement {
    logical: String,
    backend_model: String,
    solo: Measured,
    under_load: Vec<Measured>,
    fingerprint: String,
    request: ModelInferRequest,
}

pub(crate) async fn run(
    path: &Path,
    samples: usize,
    out: Option<&Path>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut config = Config::from_yaml(&text)?;
    let (endpoint, models) = config.profiling_targets();
    let slots = config.backend.slots.max(1);
    let client = Arc::new(TritonClient::new(&endpoint));

    if !client.health().await?.ready {
        eprintln!("FEHLER das Backend ist nicht bereit");
        return Ok(ExitCode::FAILURE);
    }

    let server = match client
        .raw()
        .await?
        .server_metadata(ServerMetadataRequest {})
        .await
    {
        Ok(r) => r.into_inner(),
        Err(_) => ServerMetadataResponse::default(),
    };

    eprintln!(
        "onetimer calibrate — Backend {endpoint}, Server {} {}",
        server.name, server.version
    );
    eprintln!("Slots laut Konfiguration: {slots}; {samples} Messungen je Stufe\n");

    let mut measurements = Vec::new();
    for (logical, backend_models) in &models {
        for backend_model in backend_models {
            eprintln!("  {logical} -> {backend_model}");
            let metadata: ModelMetadataResponse = client.model_metadata(backend_model).await?;
            let request = build_request(backend_model, &metadata)?;
            let fingerprint = onetimer_backend_triton::fingerprint(&server, &metadata);

            for _ in 0..WARMUP {
                let _ = client.infer(request.clone()).await;
            }
            let solo = quantiles(measure(&client, &request, samples).await);
            eprintln!("    allein            p50 {:>7} us", solo.p50_us);

            // Je zusaetzlich belegtem Slot eine Stufe. Bei einem Slot gibt es
            // keine Nebenlast und damit nichts zu messen.
            let mut under_load = Vec::new();
            for busy in 1..slots {
                let stop = Arc::new(AtomicBool::new(false));
                let tasks = spawn_load(&client, &request, busy, &stop);
                let level = quantiles(measure(&client, &request, samples).await);
                stop_load(&stop, tasks).await;
                eprintln!(
                    "    {busy} weitere{}  p50 {:>7} us  ({})",
                    if busy == 1 {
                        " Slot belegt "
                    } else {
                        " Slots belegt"
                    },
                    level.p50_us,
                    as_factor(slowdown_percent(level.p50_us, solo.p50_us))
                );
                under_load.push(level);
            }

            measurements.push(VariantMeasurement {
                logical: logical.clone(),
                backend_model: backend_model.clone(),
                solo,
                under_load,
                fingerprint,
                request,
            });
        }
    }

    let pairs = if slots > 1 {
        measure_pairs(&client, &measurements, samples).await
    } else {
        eprintln!("\nEin Slot: Modellpaare koennen sich nicht behindern, keine Paarmessung.");
        Vec::new()
    };

    apply(&mut config, &measurements, &pairs);
    report(&pairs);

    let yaml = config.to_yaml()?;
    match out {
        Some(target) => {
            std::fs::write(target, &yaml)?;
            eprintln!("\nGeschrieben nach {}", target.display());
            eprintln!(
                "Kommentare der Vorlage sind dabei verloren gegangen — die Vertraege \
                 selbst\nsind unveraendert uebernommen."
            );
        }
        None => print!("{yaml}"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Ein Paar, das sich gegenseitig zu stark bremst.
struct Pair {
    a: String,
    b: String,
    /// Verlangsamung in Prozent; 200 bedeutet die doppelte Laufzeit.
    slowdown: u64,
}

/// Misst fuer jedes Modellpaar, wie stark das eine das andere bremst.
async fn measure_pairs(
    client: &Arc<TritonClient>,
    measurements: &[VariantMeasurement],
    samples: usize,
) -> Vec<Pair> {
    let mut pairs = Vec::new();
    eprintln!("\nPaarmessung:");
    for (i, a) in measurements.iter().enumerate() {
        for b in measurements.iter().skip(i.saturating_add(1)) {
            if a.logical == b.logical {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let tasks = spawn_load(client, &b.request, 1, &stop);
            let under = quantiles(measure(client, &a.request, samples).await);
            stop_load(&stop, tasks).await;

            let slowdown = slowdown_percent(under.p50_us, a.solo.p50_us);
            eprintln!(
                "  {:<10} neben {:<10} {}",
                a.logical,
                b.logical,
                as_factor(slowdown)
            );
            if slowdown >= NO_CORUN_SLOWDOWN_PERCENT {
                pairs.push(Pair {
                    a: a.logical.clone(),
                    b: b.logical.clone(),
                    slowdown,
                });
            }
        }
    }
    pairs
}

/// Traegt die Messungen in die Konfiguration ein.
///
/// Vertraege, Klassen und Qualitaeten bleiben unberuehrt: sie sind
/// Anforderungen und keine Messwerte.
fn apply(config: &mut Config, measurements: &[VariantMeasurement], pairs: &[Pair]) {
    for (name, model) in &mut config.models {
        for variant in &mut model.variants {
            let Some(m) = measurements
                .iter()
                .find(|m| m.logical == *name && m.backend_model == variant.backend_model)
            else {
                continue;
            };
            variant.profile = Some(m.solo.to_config(Some(m.fingerprint.clone())));
            variant.under_load = m.under_load.iter().map(|l| l.to_config(None)).collect();
        }
    }
    for pair in pairs {
        let entry = [pair.a.clone(), pair.b.clone()];
        if !config.backend.no_corun.contains(&entry) {
            config.backend.no_corun.push(entry);
        }
    }
}

fn report(pairs: &[Pair]) {
    if pairs.is_empty() {
        return;
    }
    eprintln!("\nAls `no_corun` eingetragen:");
    for pair in pairs {
        eprintln!(
            "  [{}, {}] — {} Verlangsamung. Ab dem Doppelten bringt \
             Nebenlaeufigkeit
    keinen Durchsatz mehr und kostet nur Latenz.",
            pair.a,
            pair.b,
            as_factor(pair.slowdown),
        );
    }
}
