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
    periodic_us: Option<u64>,
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

    if !announce_method(periodic_us, samples) {
        return Ok(ExitCode::FAILURE);
    }
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
                periodic_us,
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

/// Prueft die Uhr und schreibt in den Kopf, **wie** gemessen wird (NV-05).
///
/// Gibt `false`, wenn die Uhr fuer diese Groessenordnung nicht taugt. Eine
/// Zahl, die nur gerundet ist, ist keine Messung — und der Betreiber soll das
/// erfahren, bevor er sie in seine Konfiguration schreibt.
fn announce_method(periodic_us: Option<u64>, samples: usize) -> bool {
    // Der kuerzeste erwartete Wert ist eine Millisekunde; darunter waere die
    // Aussage ohnehin eine ueber die Uhr und nicht ueber das Modell.
    match vig_platform::verify_clock(1_000_000) {
        vig_platform::ClockVerdict::Usable { resolution_ns } => {
            println!("# Uhraufloesung: {resolution_ns} ns");
        }
        vig_platform::ClockVerdict::TooCoarse {
            resolution_ns,
            needed_ns,
        } => {
            eprintln!(
                "FEHLER Die monotone Uhr loest {resolution_ns} ns auf; fuer diese \
                 Messung waeren {needed_ns} ns noetig. Gemessene Zahlen waeren \
                 gerundete Zahlen."
            );
            return false;
        }
    }

    match periodic_us {
        Some(us) => println!(
            "# Freigabe auf absolutem Raster alle {us} us — die Zahl, die zu einem\n\
             # Vertrag gehoert. Ein Ueberzug verschiebt das Raster nicht."
        ),
        None => println!(
            "# Saettigungsmessung: Ruecken an Ruecken. Das ist eine Aussage ueber\n\
             # die Kapazitaet, nicht ueber das Verhalten unter Takt."
        ),
    }
    println!("# Aufwaermlaeufe: {WARMUP}, Messlaeufe: {samples}");
    true
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

/// Misst eine Variante und fuehrt dabei Buch (NV-05).
///
/// Drei Dinge unterscheiden das vom naiven „schicken, Zeit nehmen,
/// wiederholen":
///
/// * **Der Hardwarezustand wird vorher und nachher gelesen.** Faellt der Takt
///   oder kommt ein thermisches Limit hinzu, beschreiben die Zahlen davor und
///   danach zwei verschiedene Maschinen. Die Zelle wird dann verworfen — mit
///   Grund.
/// * **Fehlschlaege sind Daten.** Ein abgebrochener Aufruf beendet die Reihe
///   nicht und faellt auch nicht heraus; er wird gezaehlt. Ein Fehlschlag,
///   der aus der Messreihe faellt, verbessert das Quantil.
/// * **Der Puffer steht vor der Schleife.** Eine Nachbelegung mitten in der
///   Messung waere ein Ausreisser, den niemand als solchen erkennt.
///
/// Bei `period_us` wird auf einem **absoluten** Raster freigegeben statt
/// Rueckn-an-Ruecken. Das ist die Zahl, die zu einem Vertrag gehoert: wer nach
/// jeder Antwort eine Periode wartet, misst bei langsamen Antworten seltener
/// und macht die Messung genau dann gnaedig, wenn sie hart wuerde.
async fn profile_variant(
    client: &TritonClient,
    model: &str,
    samples: usize,
    server: &ServerMetadataResponse,
    identity: &IdentityArgs,
    period_us: Option<u64>,
) -> Result<Measured, BackendError> {
    use vig_platform::measure::{CellId, CellRun, ReleaseOutcome, ReleaseSchedule};
    use vig_platform::{Collector as _, measure};

    let metadata = client.model_metadata(model).await?;
    let request = zero_request(model, &metadata)?;

    for _ in 0..WARMUP {
        client.infer(request.clone()).await?;
    }

    // Vor der Reihe: Zustand merken, damit ein Wechsel hinterher auffaellt.
    let mut collector = vig_platform::NvidiaSmi::default();
    let before = collector.snapshot().ok();

    let mut run = CellRun::with_capacity(
        CellId {
            model: model.to_owned(),
            concurrency: 1,
            batch: 1,
        },
        samples,
    );

    let origin = Instant::now();
    let mut schedule = period_us.map(|us| ReleaseSchedule::new(0, us.saturating_mul(1_000).max(1)));

    for _ in 0..samples {
        // Auf dem Raster warten, falls periodisch gemessen wird.
        if let Some(schedule) = schedule.as_mut() {
            let due_ns = schedule.take();
            let now_ns = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
            if due_ns > now_ns {
                tokio::time::sleep(std::time::Duration::from_nanos(
                    due_ns.saturating_sub(now_ns),
                ))
                .await;
            }
        }

        let started = Instant::now();
        let outcome = match client.infer(request.clone()).await {
            Ok(_) => {
                let latency_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                let overrun = schedule.as_ref().is_some_and(|s| {
                    let now_ns = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    now_ns > s.next_release_ns()
                });
                if overrun {
                    ReleaseOutcome::Overrun { latency_ns }
                } else {
                    ReleaseOutcome::Completed { latency_ns }
                }
            }
            Err(e) => ReleaseOutcome::Failed {
                reason: e.to_string(),
            },
        };
        run.record(outcome);

        // Nach einem Ueberzug auf das Raster aufschliessen: die
        // ausgelassenen Punkte werden gebucht, der naechste Freigabepunkt
        // wird **nicht** nach hinten geschoben.
        if let Some(schedule) = schedule.as_mut() {
            let now_ns = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
            run.record_skipped(schedule.catch_up(now_ns));
        }
    }

    // Mindestens hundert verwertbare Messwerte und hoechstens fuenf Prozent
    // Fehlschlaege; darunter ist das p99 kein Quantil, sondern das Maximum.
    run.qualify(100.min(samples), 50);

    if let (Some(before), Ok(after)) = (before.as_ref(), collector.snapshot())
        && let Some(reason) = measure::hardware_invalidates(before, &after)
    {
        run.discard(reason);
    }

    if let Some(reason) = run.discarded() {
        return Err(BackendError::Malformed {
            detail: format!("Messung verworfen: {reason}"),
        });
    }

    let pick = |percent: u32| -> u64 {
        run.quantile_ns(percent)
            .map_or(0, |ns| ns.checked_div(1_000).unwrap_or(0))
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
        samples: u32::try_from(run.completed().saturating_add(run.overruns())).unwrap_or(u32::MAX),
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
