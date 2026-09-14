//! `gate-m3` — der Produktvergleich gegen echten Triton auf echter GPU.
//!
//! Derselbe Workload zweimal: einmal direkt zu Triton, einmal ueber Vigilant.
//! Gleiche Modelle, gleiche Hardware, gleiche Frames, gleicher Client,
//! gleicher Transport.
//!
//! ## Warum Shared Memory
//!
//! Die Tensordaten reisen als Referenz, nicht im Request. Auf dem Copy-Pfad
//! kostet ein 3-MB-Frame den Governor rund das Doppelte der Uebertragungszeit
//! (ADR-0003, `docs/benchmark/data-plane.md`) — der Vergleich wuerde dann den
//! Transport messen und nicht das Scheduling. Shared Memory ist zugleich die
//! Konfiguration, die ein reales Deployment ohnehin verwendet.
//!
//! ## Was hier die Baseline ist
//!
//! Triton selbst, unveraendert, mit denselben Modellen und derselben
//! Instance-Group-Konfiguration. Kein Strohmann: dynamisches Batching ist auf
//! beiden Seiten aus, weil die Warteschlange vor den Governor gehoert und
//! nicht dahinter (ADR-0002) — und weil ein Batcher die Baseline bei
//! periodischer Einzelbildlast nicht schneller, sondern nur traeger machen
//! wuerde.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::integer_division,
    clippy::too_many_lines
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use vig_bench::shm::Region;
use vig_bench::workload::{InputSpec, StreamDef, StreamReport, connect, drive};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use vig_protocol_oip::inference::{
    ModelMetadataRequest, SystemSharedMemoryRegisterRequest, SystemSharedMemoryUnregisterRequest,
};

const RUN_SECONDS: u64 = 30;
const CAPS: [usize; 2] = [1, 8];

/// Was ein Strom fuer den Lauf braucht.
struct Prepared {
    name: String,
    physical: String,
    /// Der Triton-Prozess, der dieses Modell bedient.
    ///
    /// Meist fuer alle derselbe. Zwei Prozesse auf einer GPU — etwa Vision
    /// und Sprache getrennt, oder unter XSched mit verschiedenen Prioritaeten
    /// (NV-15) — sind trotzdem **ein** Lauf und werden gleichzeitig gefahren.
    endpoint: String,
    period: Duration,
    max_age: Duration,
    input: InputSpec,
    /// `None` auf dem Kopierpfad (`VIG_GATE_COPY=1`).
    _region: Option<Region>,
}

/// Wahr, wenn die Umgebungsvariable auf `1` steht.
fn flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == "1")
}

fn to_std(d: vig_core::Duration) -> Duration {
    Duration::from_nanos(d.as_nanos())
}

fn element_size(datatype: &str) -> usize {
    match datatype {
        "BOOL" | "INT8" | "UINT8" => 1,
        "INT16" | "UINT16" | "FP16" | "BF16" => 2,
        "INT64" | "UINT64" | "FP64" => 8,
        _ => 4,
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Echte Bilder statt Nullen, fuer den Kopierpfad (`VIG_GATE_FRAMES`).
///
/// Die Rechenzeit eines Faltungsnetzes haengt kaum am Inhalt, die einer
/// Nachbearbeitung im Graphen (NMS) sehr wohl: auf Nullen findet ein Detektor
/// nichts und sortiert nichts. Die Datei ist RGB24 aus quadratischen Bildern
/// mit `VIG_GATE_FRAME_SIZE` Pixeln Kantenlaenge (Voreinstellung 512, wie die
/// MOT16-Aufbereitung unter `tools/pilot/prepare-data.sh`). Gleichmaessig
/// ueber die Datei verteilt werden `VIG_GATE_FRAME_COUNT` Bilder (16)
/// genommen und reihum geschickt.
fn load_frames(path: &str) -> (Vec<Vec<u8>>, usize) {
    let size = env_usize("VIG_GATE_FRAME_SIZE", 512);
    let count = env_usize("VIG_GATE_FRAME_COUNT", 16).max(1);
    let bytes = std::fs::read(path).expect("Bilddatei lesbar");
    let frame_bytes = size.saturating_mul(size).saturating_mul(3);
    let available = bytes.len().checked_div(frame_bytes).unwrap_or(0);
    assert!(
        available > 0,
        "{path}: kein ganzes Bild mit {size}x{size} RGB24"
    );
    let step = available.checked_div(count).unwrap_or(1).max(1);
    let frames = (0..available)
        .step_by(step)
        .take(count)
        .filter_map(|i| {
            let start = i.saturating_mul(frame_bytes);
            bytes
                .get(start..start.saturating_add(frame_bytes))
                .map(<[u8]>::to_vec)
        })
        .collect();
    (frames, size)
}

/// Bringt ein quadratisches RGB24-Bild in die Eingabeform eines Modells:
/// naechster Nachbar, NHWC oder NCHW, `UINT8` roh oder `FP32` in [0, 1].
/// `None` fuer jede andere Form — dann bleibt es bei Nullen.
fn tensor_from_frame(rgb: &[u8], size: usize, shape: &[i64], datatype: &str) -> Option<Vec<u8>> {
    let dims: Vec<usize> = shape
        .iter()
        .map(|d| usize::try_from(*d).ok())
        .collect::<Option<_>>()?;
    let (nhwc, height, width) = match dims.as_slice() {
        [1, h, w, 3] => (true, *h, *w),
        [1, 3, h, w] => (false, *h, *w),
        _ => return None,
    };
    let float = match datatype {
        "UINT8" => false,
        "FP32" => true,
        _ => return None,
    };
    let pixel = |y: usize, x: usize, c: usize| -> u8 {
        let sy = y.saturating_mul(size).checked_div(height).unwrap_or(0);
        let sx = x.saturating_mul(size).checked_div(width).unwrap_or(0);
        let offset = sy
            .saturating_mul(size)
            .saturating_add(sx)
            .saturating_mul(3)
            .saturating_add(c);
        rgb.get(offset).copied().unwrap_or(0)
    };
    let mut out = Vec::new();
    let mut push = |value: u8| {
        if float {
            out.extend_from_slice(&(f32::from(value) / 255.0).to_le_bytes());
        } else {
            out.push(value);
        }
    };
    if nhwc {
        for y in 0..height {
            for x in 0..width {
                for c in 0..3 {
                    push(pixel(y, x, c));
                }
            }
        }
    } else {
        for c in 0..3 {
            for y in 0..height {
                for x in 0..width {
                    push(pixel(y, x, c));
                }
            }
        }
    }
    Some(out)
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
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "examples/gate_m3/vig.yaml".to_owned());
    let text = std::fs::read_to_string(&path).expect("Konfiguration lesbar");
    let config = Config::from_yaml(&text).expect("Konfiguration gueltig");
    let findings = config.diagnose();
    assert!(
        findings.is_empty(),
        "Konfiguration hat Befunde: {findings:?}"
    );
    let resolved = Arc::new(config.resolve().expect("aufloesbar"));

    let triton = Arc::new(vig_backend_triton::TritonClient::new(
        &resolved.backend_endpoint,
    ));

    println!("gate-m3: Vigilant gegen Triton, gleiche Modelle, gleiche GPU");
    println!(
        "Backend {} · geschuetzte serialisierte Auslastung {} %",
        resolved.backend_endpoint,
        resolved.protected_utilization_permille() / 10
    );
    // Ein Backend ohne Shared Memory — etwa TFLite auf Android, das kein
    // `/dev/shm` hat (ADR-0039) — bekommt die Nutzlast im Request. Das
    // kostet den Governor die Kopie (ADR-0003); beide Seiten zahlen denselben
    // Transport, der Vergleich bleibt einer des Schedulings.
    let copy = flag("VIG_GATE_COPY");
    let run_seconds = std::env::var("VIG_GATE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(RUN_SECONDS);
    let frames = std::env::var("VIG_GATE_FRAMES")
        .ok()
        .filter(|_| copy)
        .map(|path| (load_frames(&path), path));
    println!(
        "Messdauer {run_seconds} s je Lauf und Puffertiefe {CAPS:?} · Datenpfad {} · Eingaben {}\n",
        if copy {
            "Kopie im Request"
        } else {
            "Shared Memory"
        },
        frames.as_ref().map_or_else(
            || "Nullen".to_owned(),
            |((f, size), path)| format!("{} Bilder {size}x{size} aus {path}", f.len())
        )
    );

    // --- Vorbereiten: Metadaten holen, Shm-Regionen anlegen und registrieren
    let mut prepared = Vec::new();
    let mut clients: HashMap<String, Arc<vig_backend_triton::TritonClient>> = HashMap::new();
    for (index, logical) in resolved.model_names.iter().enumerate() {
        let model = vig_core::ModelIdx(u16::try_from(index).unwrap_or(0));
        let physical = resolved
            .backend_model(model, 0)
            .expect("Variante vorhanden")
            .to_owned();
        let contract = resolved.contracts.get(index).expect("Vertrag vorhanden");
        // Die Region muss bei **dem** Prozess registriert sein, der das Modell
        // rechnet — sonst faellt der Vergleich still auf den Copy-Pfad zurueck.
        let endpoint = resolved.endpoint_of(model).to_owned();
        let client =
            Arc::clone(clients.entry(endpoint.clone()).or_insert_with(|| {
                Arc::new(vig_backend_triton::TritonClient::new(endpoint.as_str()))
            }));

        let metadata = client
            .raw()
            .await
            .expect("Backend erreichbar")
            .model_metadata(ModelMetadataRequest {
                name: physical.clone(),
                version: String::new(),
            })
            .await
            .expect("Modellmetadaten")
            .into_inner();
        let input = metadata.inputs.first().expect("Modell hat eine Eingabe");

        let shape: Vec<i64> = input
            .shape
            .iter()
            .enumerate()
            .map(|(position, d)| if *d < 0 && position == 0 { 1 } else { *d })
            .collect();
        let elements: i64 = shape.iter().copied().product();
        let byte_size = u64::try_from(elements).unwrap_or(0) * element_size(&input.datatype) as u64;

        let region = if copy {
            None
        } else {
            let region_name = format!("vig_{logical}");
            let region = Region::create(&region_name, byte_size).expect("Shm-Region anlegen");

            // Aufraeumen, falls ein frueherer Lauf abgebrochen ist.
            let _ = client
                .raw()
                .await
                .expect("Backend erreichbar")
                .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
                    name: region.name.clone(),
                })
                .await;
            client
                .raw()
                .await
                .expect("Backend erreichbar")
                .system_shared_memory_register(SystemSharedMemoryRegisterRequest {
                    name: region.name.clone(),
                    key: region.key.clone(),
                    offset: 0,
                    byte_size,
                })
                .await
                .expect("Shm-Region registrieren");
            Some(region)
        };

        let payload = frames.as_ref().and_then(|((images, size), _)| {
            let tensors: Option<Vec<Vec<u8>>> = images
                .iter()
                .map(|image| tensor_from_frame(image, *size, &shape, &input.datatype))
                .collect();
            tensors
                .filter(|t| {
                    t.first()
                        .is_some_and(|b| u64::try_from(b.len()).ok() == Some(byte_size))
                })
                .map(Arc::new)
        });

        println!(
            "  {logical:<9} -> {physical:<15} {:>6} KB  Periode {:>4} ms{}{}",
            byte_size / 1024,
            to_std(contract.deadline).as_millis(),
            if endpoint == resolved.backend_endpoint {
                String::new()
            } else {
                format!("  @ {endpoint}")
            },
            if frames.is_some() && payload.is_none() {
                format!(
                    "  (Form {shape:?} {} nicht aus Bildern baubar: Nullen)",
                    input.datatype
                )
            } else {
                String::new()
            }
        );

        prepared.push(Prepared {
            name: logical.clone(),
            physical,
            endpoint,
            period: contract.period.map_or(Duration::from_millis(500), to_std),
            max_age: contract.max_age.map_or(Duration::from_secs(1), to_std),
            input: InputSpec {
                name: input.name.clone(),
                datatype: input.datatype.clone(),
                shape,
                region: region.as_ref().map(|r| r.name.clone()),
                byte_size,
                payload,
            },
            _region: region,
        });
    }

    let streams = |use_logical: bool, cap: usize, only: Option<&str>| -> Vec<StreamDef> {
        prepared
            .iter()
            .filter(|p| only.is_none_or(|endpoint| p.endpoint == endpoint))
            .map(|p| StreamDef {
                text: None,
                name: Box::leak(p.name.clone().into_boxed_str()),
                model: Box::leak(
                    if use_logical {
                        p.name.clone()
                    } else {
                        p.physical.clone()
                    }
                    .into_boxed_str(),
                ),
                period: p.period,
                max_age: p.max_age,
                in_flight_cap: cap,
                input: Some(p.input.clone()),
                pump: false,
                burst: None,
            })
            .collect()
    };

    let duration = Duration::from_secs(run_seconds);

    // --- Baseline: direkt zu Triton --------------------------------------
    let mut endpoints: Vec<String> = prepared.iter().map(|p| p.endpoint.clone()).collect();
    endpoints.sort();
    endpoints.dedup();
    let mut baseline: HashMap<String, StreamReport> = HashMap::new();
    for cap in CAPS {
        // Je Prozess ein Treiber, alle gleichzeitig. Bei einem einzigen
        // Endpunkt ist das genau der eine Aufruf von frueher.
        let mut runs = Vec::new();
        for endpoint in &endpoints {
            let defs = streams(false, cap, Some(endpoint));
            let endpoint = endpoint.clone();
            runs.push(tokio::spawn(async move {
                drive(&endpoint, &defs, duration, false).await
            }));
        }
        let mut reports = Vec::new();
        for run in runs {
            reports.extend(run.await.expect("Treiber beendet"));
        }
        for report in reports {
            let entry = baseline
                .entry(report.name.to_owned())
                .or_insert_with(|| report.clone());
            if report.coverage.covered_permille() > entry.coverage.covered_permille() {
                *entry = report;
            }
        }
    }

    // --- Mit Vigilant -----------------------------------------------------
    let clock = MonotonicClock::start();
    let handle =
        actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
    // Wie `vig serve`: der Governor beobachtet die Karte. Ohne das plant er
    // ohne Geraetezustand, und die Prognose (NV-06) haette nie eine Zelle.
    // Auf einem Geraet ohne `nvidia-smi` gibt es nichts zu beobachten
    // (`VIG_GATE_NO_HARDWARE=1`); der Governor plant dann mit dem Profil,
    // wie ADR-0022 es fuer diesen Fall vorsieht.
    if !flag("VIG_GATE_NO_HARDWARE") {
        handle.observe_hardware();
    }
    let service = GatewayService::new(Arc::clone(&resolved), triton, handle.clone(), clock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Port");
    let gateway = listener.local_addr().expect("Adresse").to_string();
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
    let _warm = connect(&gateway).await;

    let mut governed: HashMap<String, StreamReport> = HashMap::new();
    for cap in CAPS {
        let reports = drive(&gateway, &streams(true, cap, None), duration, true).await;
        for report in reports {
            let entry = governed
                .entry(report.name.to_owned())
                .or_insert_with(|| report.clone());
            if report.coverage.covered_permille() > entry.coverage.covered_permille() {
                *entry = report;
            }
        }
    }

    // --- Bericht -----------------------------------------------------------
    println!("\n  Strom     | Abdeckung Triton | Vigilant | Antwortalter p95 T | O | Faktor");
    println!("  ----------|------------------|----------|----------------|----------|-------");
    for p in &prepared {
        let (Some(a), Some(b)) = (baseline.get(&p.name), governed.get(&p.name)) else {
            continue;
        };
        let without = a.coverage.uncovered_permille();
        let with = b.coverage.uncovered_permille();
        let factor = if with == 0 {
            if without == 0 {
                "—".to_owned()
            } else {
                "besser".to_owned()
            }
        } else if without >= with {
            format!("{:.1}x", without as f64 / with as f64)
        } else {
            format!("-{:.1}x", with as f64 / without.max(1) as f64)
        };
        println!(
            "  {:<9} | {:>14} % | {:>6} % | {:>11} ms | {:>5} ms | {factor:>6}",
            p.name,
            a.coverage.covered_permille() / 10,
            b.coverage.covered_permille() / 10,
            a.coverage.response_age_p95_ns / 1_000_000,
            b.coverage.response_age_p95_ns / 1_000_000,
        );
    }

    // Die Verbrauchersicht daneben: was der Regler zum Abtastzeitpunkt
    // tatsaechlich vorliegen hatte, und wie lange er am Stueck ohne
    // brauchbares Ergebnis war. Eine Abdeckungszahl allein unterscheidet
    // verstreute Ausfaelle nicht von einem Block — fuer eine Regelung ist das
    // der ganze Unterschied.
    println!(
        "\n  Verbrauchersicht  | Abdeckung T | O | laengste Luecke T | O | mittlere AoI T | O"
    );
    println!(
        "  ------------------|-------------|------|-------------------|------|----------------|------"
    );
    for p in &prepared {
        let (Some(a), Some(b)) = (baseline.get(&p.name), governed.get(&p.name)) else {
            continue;
        };
        let share = |c: &vig_sim::coverage::Coverage| {
            c.consumer_covered
                .saturating_mul(100)
                .checked_div(c.total)
                .unwrap_or(0)
        };
        println!(
            "  {:<17} | {:>9} % | {:>3} % | {:>14} ms | {:>3} ms | {:>11} ms | {:>3} ms",
            p.name,
            share(&a.coverage),
            share(&b.coverage),
            a.coverage.longest_gap_ns / 1_000_000,
            b.coverage.longest_gap_ns / 1_000_000,
            a.coverage.mean_aoi_ns / 1_000_000,
            b.coverage.mean_aoi_ns / 1_000_000,
        );
    }

    if let Ok(m) = handle.metrics().await {
        println!(
            "\n  Governor: angenommen {} weitergereicht {} supersediert {} stale {} \
             unmachbar {} verspaetet {} zurueckgestellt {} best-effort ausgehungert {}",
            m.received,
            m.forwarded,
            m.superseded,
            m.stale,
            m.rejected_infeasible,
            m.dispatched_late,
            m.deferred_for_protected,
            m.best_effort_starved,
        );
        // NV-06: ob die Prognose mutiger oder nur vorsichtiger war. Eine
        // Policy, die nur mehr ablehnt, haelt jede Zusage ein und ist
        // trotzdem wertlos — deshalb beide Richtungen getrennt.
        println!(
            "  Prognose ({}): verglichen {} ohne Zelle {} vorsichtiger {} mutiger {}",
            if m.predictor_active == 1 {
                "scharf"
            } else {
                "Schatten"
            },
            m.predictor_comparisons,
            m.predictor_fallbacks,
            m.predictor_more_conservative,
            m.predictor_more_optimistic,
        );
    }
    println!(
        "\nAbdeckung nach ADR-0005: Anteil der Perioden mit einem gelieferten Ergebnis,\n\
         dessen Alter unter max_age lag."
    );
}

#[cfg(test)]
mod tests {
    use super::tensor_from_frame;

    /// Ein 2x2-Bild, jeder Pixel mit eigenem Rotwert.
    fn image() -> Vec<u8> {
        vec![10, 0, 0, 20, 0, 0, 30, 0, 0, 40, 0, 0]
    }

    #[test]
    fn nhwc_uint8_is_the_resized_image_itself() {
        let tensor = tensor_from_frame(&image(), 2, &[1, 2, 2, 3], "UINT8");
        assert_eq!(tensor, Some(image()));
    }

    #[test]
    fn nchw_float_puts_each_channel_in_its_own_plane_scaled_to_one() {
        let tensor = tensor_from_frame(&image(), 2, &[1, 3, 1, 1], "FP32");
        let expected: Vec<u8> = [10.0_f32 / 255.0, 0.0, 0.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(tensor, Some(expected));
    }

    #[test]
    fn other_shapes_and_types_stay_zeros() {
        assert_eq!(tensor_from_frame(&image(), 2, &[1, 4], "UINT8"), None);
        assert_eq!(tensor_from_frame(&image(), 2, &[1, 2, 2, 3], "INT64"), None);
    }
}
