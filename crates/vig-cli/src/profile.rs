//! `vig profile` — misst echte Laufzeitprofile am Backend (WP10, Spec 13).
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

use crate::identity::{ArtifactLookup, IdentityArgs, MeasuredUnder, artifact_of, assemble};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;
use vig_backend_triton::{BackendError, TritonClient, zero_request};
use vig_config::Config;
use vig_protocol_oip::inference::ServerMetadataResponse;

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
    identity: &IdentityArgs,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let config = Config::from_yaml(&text)?;
    let identity = &probe_hardware(identity.clone());
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
    println!("# Erzeugt von `vig profile`");
    println!("# Backend: {endpoint}");
    let server_meta = if let Ok(response) = client
        .raw()
        .await?
        .server_metadata(vig_protocol_oip::inference::ServerMetadataRequest {})
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
            match Box::pin(profile_variant(
                &client,
                physical,
                samples,
                &server_meta,
                identity,
            ))
            .await
            {
                Ok(measured) => {
                    println!("      - id: <unveraendert lassen>");
                    println!("        backend_model: {physical}");
                    // Blockform statt Flow-Mapping: das Manifest ist ein
                    // verschachtelter Block und passt nicht in eine Zeile.
                    println!("        profile:");
                    println!("          p50_us: {}", measured.p50_us);
                    println!("          p95_us: {}", measured.p95_us);
                    println!("          p99_us: {}", measured.p99_us);
                    println!("          samples: {}", measured.samples);
                    println!("          fingerprint: \"{}\"", measured.fingerprint);
                    match vig_config::manifest::to_yaml_block(&measured.manifest, 12) {
                        Ok(block) => {
                            println!("          manifest:");
                            print!("{block}");
                        }
                        Err(e) => {
                            println!("          # Manifest nicht darstellbar: {e}");
                        }
                    }
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

/// Belegt fehlende Geraetefelder aus der Hardwarebeobachtung vor (NV-04).
///
/// Was der Betreiber angegeben hat, bleibt stehen. Was er nicht angegeben hat
/// und was sich beobachten laesst, wird ergaenzt und genannt. Was sich nicht
/// beobachten laesst, bleibt `unknown`.
pub(crate) fn probe_hardware(mut identity: IdentityArgs) -> IdentityArgs {
    if identity.no_hardware_probe {
        return identity;
    }
    let gpu_index = identity.gpu_index;
    let filled = crate::identity::prefill_from_hardware(&mut identity, gpu_index);
    if !filled.is_empty() {
        println!("# Aus der Hardware ergaenzt: {}", filled.join(", "));
    }
    identity
}

/// Das Messergebnis einer Variante.
struct Measured {
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    samples: u32,
    /// G-010: unter welcher Umgebung diese Zahlen entstanden sind.
    fingerprint: String,
    /// NV-03: woher sie stammen und wofuer sie gelten.
    manifest: vig_config::manifest::ProfileManifest,
}

async fn profile_variant(
    client: &TritonClient,
    model: &str,
    samples: usize,
    server: &ServerMetadataResponse,
    identity: &IdentityArgs,
) -> Result<Measured, BackendError> {
    let metadata = client.model_metadata(model).await?;
    let request = zero_request(model, &metadata)?;

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

    let observation = vig_backend_triton::observe(server, &metadata);
    let artifact = match artifact_of(identity, model, &observation.versions) {
        ArtifactLookup::Found(found) => found,
        ArtifactLookup::NotRequested => vig_config::manifest::ArtifactIdentity::default(),
        ArtifactLookup::Failed(reason) => {
            // Kein Abbruch: ein Profil ohne Digest ist ein unbelegtes Profil,
            // kein falsches. Der Betreiber soll aber wissen, warum.
            eprintln!("    Artefakt-Digest nicht bildbar ({reason}); Feld bleibt unknown");
            vig_config::manifest::ArtifactIdentity::default()
        }
    };

    Ok(Measured {
        p50_us: pick(50),
        p95_us: pick(95),
        p99_us: pick(99),
        samples: u32::try_from(measurements.len()).unwrap_or(u32::MAX),
        fingerprint: vig_backend_triton::fingerprint(server, &metadata),
        manifest: assemble(
            &observation,
            artifact,
            identity,
            MeasuredUnder {
                warmup: u32::try_from(WARMUP).unwrap_or(u32::MAX),
                concurrency: 1,
                batch_size: 1,
            },
            crate::identity::now_rfc3339(),
        ),
    })
}

/// Die Voreinstellung fuer die Messlaufzahl.
pub(crate) const DEFAULT_SAMPLES: usize = SAMPLES;
