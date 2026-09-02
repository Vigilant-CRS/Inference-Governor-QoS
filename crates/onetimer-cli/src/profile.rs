//! `onetimer profile` — misst echte Laufzeitprofile am Backend (WP10, Spec 13).
//!
//! Bis hierhin mussten Laufzeitprofile von Hand in die Konfiguration
//! geschrieben werden. Damit steht und faellt aber die gesamte Planung: der
//! Scheduler entscheidet ueber Zulassung, Variantenwahl und Look-ahead auf
//! Grundlage dieser Zahlen. Geratene Profile ergeben geratene Entscheidungen.
//!
//! ## Was gemessen wird
//!
//! Die **Backend-Antwortzeit** einer einzelnen Inferenz bei sonst leerem
//! System. Das ist bewusst nicht die Ende-zu-Ende-Latenz und nicht die Zeit
//! unter Nebenlast:
//!
//! * Ende-zu-Ende enthielte Transport und Warteschlange — Groessen, die der
//!   Scheduler selbst beeinflusst. Ein Profil, das sie enthaelt, waere von
//!   den eigenen Entscheidungen abhaengig.
//! * Nebenlast gehoert in die Belegungsgrad-Zellen des Online-Schaetzers
//!   (ADR-0006). Sie hier hineinzumischen wuerde beide Groessen unbrauchbar
//!   machen.
//!
//! ## Warum das Ergebnis nicht in die Datei geschrieben wird
//!
//! Die Konfiguration enthaelt Kommentare, und ein YAML-Serialisierer verliert
//! sie. Der Profiler gibt deshalb einen einfuegefertigen Block aus, statt die
//! Datei des Nutzers umzuschreiben.

#![allow(clippy::print_stdout)]

use onetimer_backend_triton::{BackendError, TritonClient};
use onetimer_config::Config;
use onetimer_protocol_oip::inference::model_infer_request::InferInputTensor;
use onetimer_protocol_oip::inference::{
    ModelInferRequest, ModelMetadataResponse, ServerMetadataResponse,
};
use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

/// Aufwaermlaeufe je Variante.
///
/// Die ersten Inferenzen eines Modells sind nicht repraesentativ: Gewichte
/// wandern in den Speicher, Kernel werden ausgewaehlt, Caches fuellen sich.
/// Sie mitzumessen wuerde das Profil systematisch verschlechtern.
pub(crate) const WARMUP: usize = 20;

/// Messlaeufe je Variante.
///
/// Fuer ein p99 sind 100 Messwerte das Minimum, unterhalb dessen der Wert das
/// Maximum ist und kein Quantil (siehe `RuntimeProfile::MIN_SAMPLES`).
const SAMPLES: usize = 200;

/// Fuehrt die Profilierung aus.
///
/// # Errors
///
/// Wenn die Konfiguration nicht gelesen werden kann.
pub(crate) async fn run(
    path: &Path,
    samples: usize,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let config = Config::from_yaml(&text)?;
    // Bewusst ohne `resolve()`: das Werkzeug soll die Profile erst erzeugen
    // und darf sie deshalb nicht voraussetzen.
    let (endpoint, models) = config.profiling_targets();
    let client = TritonClient::new(&endpoint);

    let health = client.health().await?;
    if !health.ready {
        eprintln!("FEHLER das Backend ist nicht bereit");
        return Ok(ExitCode::FAILURE);
    }

    // Spec 13.5: ein Profil ist nur zusammen mit seiner Umgebung gueltig.
    // Was davon ueber das Protokoll erreichbar ist, wird mitgeschrieben; GPU
    // und Treiber kennt der Server nicht und muessen aus dem
    // Deploymentkontext kommen.
    println!("# Erzeugt von `onetimer profile`");
    println!("# Backend: {endpoint}");
    let server_meta = if let Ok(response) = client
        .raw()
        .await?
        .server_metadata(onetimer_protocol_oip::inference::ServerMetadataRequest {})
        .await
    {
        let meta = response.into_inner();
        println!("# Server: {} {}", meta.name, meta.version);
        meta
    } else {
        // Ohne Servermetadaten laesst sich kein Fingerabdruck bilden, der die
        // Serverversion einschliesst. Das Profil entsteht trotzdem — es ist
        // dann nur nicht pruefbar, und `doctor` sagt das.
        println!("# Server: Version nicht abfragbar");
        ServerMetadataResponse::default()
    };

    println!("# Aufwaermlaeufe: {WARMUP}, Messlaeufe: {samples}");
    println!(
        "# ACHTUNG Ein Profil gilt nur fuer diese Umgebung. Aendern sich GPU,\n\
         # Treiber, Backend-Version oder das Modell selbst, muss neu gemessen\n\
         # werden (Spec 13.5)."
    );

    let mut failures = 0_u32;
    for (logical, names) in &models {
        println!("\n  {logical}:\n    variants:");

        for physical in names {
            // Die generierten OIP-Typen sind gross; ungeboxt landet das
            // Future auf dem Stack des Aufrufers.
            match Box::pin(profile_variant(&client, physical, samples, &server_meta)).await {
                Ok(measured) => {
                    println!("      - id: <unveraendert lassen>");
                    println!("        backend_model: {physical}");
                    println!(
                        "        profile: {{ p50_us: {}, p95_us: {}, p99_us: {}, samples: {}, \
                         fingerprint: \"{}\" }}",
                        measured.p50_us,
                        measured.p95_us,
                        measured.p99_us,
                        measured.samples,
                        measured.fingerprint
                    );
                    if measured.p99_us > measured.p50_us.saturating_mul(3) {
                        println!(
                            "        # WARNUNG p99 liegt beim {}-fachen des Medians. Eine so\n\
                             \x20       # breite Verteilung macht konservative Planung teuer;\n\
                             \x20       # meist steckt eine Hintergrundlast oder Taktabsenkung\n\
                             \x20       # dahinter (Spec 13.1).",
                            measured
                                .p99_us
                                .checked_div(measured.p50_us.max(1))
                                .unwrap_or(0)
                        );
                    }
                }
                Err(e) => {
                    eprintln!("FEHLER {physical}: {e}");
                    failures = failures.saturating_add(1);
                }
            }
        }
    }

    if failures > 0 {
        eprintln!("\n{failures} Variante(n) konnten nicht profiliert werden");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// Das Messergebnis einer Variante.
struct Measured {
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    samples: u32,
    /// G-010: unter welcher Umgebung diese Zahlen entstanden sind.
    fingerprint: String,
}

async fn profile_variant(
    client: &TritonClient,
    model: &str,
    samples: usize,
    server: &ServerMetadataResponse,
) -> Result<Measured, BackendError> {
    let metadata = client.model_metadata(model).await?;
    let request = build_request(model, &metadata)?;

    for _ in 0..WARMUP {
        client.infer(request.clone()).await?;
    }

    let mut measurements = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        client.infer(request.clone()).await?;
        measurements.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
    measurements.sort_unstable();

    let pick = |percent: usize| -> u64 {
        if measurements.is_empty() {
            return 0;
        }
        let index = measurements
            .len()
            .saturating_mul(percent)
            .checked_div(100)
            .unwrap_or(0)
            .min(measurements.len().saturating_sub(1));
        measurements.get(index).copied().unwrap_or(0)
    };

    Ok(Measured {
        p50_us: pick(50),
        p95_us: pick(95),
        p99_us: pick(99),
        samples: u32::try_from(measurements.len()).unwrap_or(u32::MAX),
        fingerprint: onetimer_backend_triton::fingerprint(server, &metadata),
    })
}

/// Baut einen Request mit nullgefuellten Eingaben passend zu den Metadaten.
///
/// Nullen und keine Zufallsdaten: die Laufzeit eines Inferenzkernels haengt bei
/// den hier betrachteten Modellen nicht vom Inhalt ab, und reproduzierbare
/// Eingaben machen zwei Profilierungslaeufe vergleichbar.
pub(crate) fn build_request(
    model: &str,
    metadata: &ModelMetadataResponse,
) -> Result<ModelInferRequest, BackendError> {
    let mut inputs = Vec::new();
    let mut contents = Vec::new();

    for input in &metadata.inputs {
        let mut shape = Vec::with_capacity(input.shape.len());
        for (position, dimension) in input.shape.iter().enumerate() {
            match *dimension {
                // Dynamische Dimensionen: die fuehrende gilt als Batch und
                // wird auf 1 gesetzt. Jede weitere waere geraten, und ein
                // geratenes Profil ist schlechter als keines.
                -1 if position == 0 => shape.push(1),
                -1 => {
                    return Err(BackendError::Malformed {
                        detail: format!(
                            "{model}: Eingabe {:?} hat die dynamische Dimension {position}; \
                             sie muss im Modellrepository festgelegt werden, sonst ist die \
                             gemessene Laufzeit nicht reproduzierbar",
                            input.name
                        ),
                    });
                }
                value => shape.push(value),
            }
        }

        let elements: i64 = shape.iter().copied().product();
        let width = element_size(&input.datatype).ok_or_else(|| BackendError::Malformed {
            detail: format!("{model}: unbekannter Datentyp {:?}", input.datatype),
        })?;
        let bytes = usize::try_from(elements)
            .ok()
            .and_then(|e| e.checked_mul(width))
            .ok_or_else(|| BackendError::Malformed {
                detail: format!("{model}: Eingabe {:?} ist zu gross", input.name),
            })?;

        inputs.push(InferInputTensor {
            name: input.name.clone(),
            datatype: input.datatype.clone(),
            shape,
            parameters: HashMap::new(),
            contents: None,
        });
        contents.push(vec![0_u8; bytes]);
    }

    Ok(ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: "onetimer-profile".to_owned(),
        parameters: HashMap::new(),
        inputs,
        outputs: Vec::new(),
        raw_input_contents: contents,
    })
}

/// Die Groesse eines Elements des angegebenen OIP-Datentyps in Bytes.
const fn element_size(datatype: &str) -> Option<usize> {
    Some(match datatype.as_bytes() {
        b"BOOL" | b"INT8" | b"UINT8" => 1,
        b"INT16" | b"UINT16" | b"FP16" | b"BF16" => 2,
        b"INT32" | b"UINT32" | b"FP32" => 4,
        b"INT64" | b"UINT64" | b"FP64" => 8,
        // BYTES ist laengenpraefigiert und ohne Modellwissen nicht
        // konstruierbar; das muss der Nutzer erfahren, statt eine falsche
        // Groesse zu bekommen.
        _ => return None,
    })
}

/// Die Voreinstellung fuer die Messlaufzahl.
pub(crate) const DEFAULT_SAMPLES: usize = SAMPLES;
