//! `vig calibrate` — die Hardware ausmessen, statt sie zu raten (WP12).
//!
//! `vig profile` misst, wie lange ein Modell **allein** braucht. Das ist
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

use crate::identity::IdentityArgs;
use crate::profile::WARMUP;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use vig_backend_triton::TritonClient;
use vig_config::manifest::{ArtifactIdentity, ProfileManifest};
use vig_config::schema::{Config, ProfileConfig};
use vig_protocol_oip::inference::{
    ModelInferRequest, ModelMetadataResponse, ServerMetadataRequest, ServerMetadataResponse,
};

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

/// Ab diesem Verhaeltnis wird `no_corun` vorgeschlagen, in Prozent.
///
/// **Eine Heuristik mit einer Annahme, keine allgemeine Durchsatzaussage**
/// (NV-11). Sie stimmt fuer zwei Auftraege aehnlicher Laenge: bremst der
/// Nachbar auf die doppelte Laufzeit, brauchen zwei Auftraege nebeneinander
/// genauso lange wie nacheinander, und die Latenz ist obendrein schlechter.
///
/// Sie stimmt **nicht** allgemein. Bei sehr unterschiedlichen Laufzeiten kann
/// Nebenlaeufigkeit den Durchsatz erhoehen, obwohl der kurze Auftrag stark
/// leidet — ob das gut ist, entscheidet der Vertrag und nicht diese Zahl. Der
/// Kalibrator schlaegt deshalb vor und traegt nicht ungefragt fest ein; die
/// Ausgabe nennt beide Richtungen samt absoluter Zusatzzeit, damit der
/// Betreiber die Annahme pruefen kann.
const NO_CORUN_SLOWDOWN_PERCENT: u64 = 200;

/// Eine Messreihe.
struct Measured {
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    samples: u32,
}

impl Measured {
    fn to_config(
        &self,
        fingerprint: Option<String>,
        manifest: Option<ProfileManifest>,
    ) -> ProfileConfig {
        ProfileConfig {
            p50_us: self.p50_us,
            p95_us: self.p95_us,
            p99_us: self.p99_us,
            samples: self.samples,
            fingerprint,
            manifest,
        }
    }
}

/// Die Quantile einer Messreihe.
///
/// Aus dem [`CellRun`](vig_platform::measure::CellRun) und nicht aus einer
/// nackten Zahlenliste: nur so tragen Freigaben, Fehlschlaege und Ueberzuege
/// bis hierher, und nur so ist `samples` die Zahl, die auch im Runmanifest
/// steht (Review R08).
fn quantiles(run: &vig_platform::measure::CellRun) -> Measured {
    let pick = |percent: u32| -> u64 {
        run.quantile_ns(percent)
            .map_or(0, |ns| ns.checked_div(1_000).unwrap_or(0))
    };
    Measured {
        p50_us: pick(50),
        p95_us: pick(95),
        p99_us: pick(99),
        samples: u32::try_from(run.completed().saturating_add(run.overruns())).unwrap_or(u32::MAX),
    }
}

/// Faehrt eine Messreihe auf demselben Kern wie `vig profile` (Review R08).
///
/// Vorher mass dieser Befehl Ruecken an Ruecken und uebersprang
/// fehlgeschlagene Aufrufe stillschweigend — ausgerechnet der Befehl, der
/// Profile **automatisch in Konfigurationen schreibt**. Zwei Messpfade fuer
/// dieselbe Groesse, und der eine war systematisch guenstiger als der andere.
///
/// Gibt `None` zurueck, wenn die Reihe nicht qualifiziert — zu wenige
/// verwertbare Messwerte, zu viele Fehlschlaege oder ein Hardwarewechsel
/// mittendrin. Aus einer verworfenen Reihe darf kein Profil entstehen.
async fn measure(
    client: &TritonClient,
    model: &str,
    request: &ModelInferRequest,
    samples: usize,
    period_us: Option<u64>,
) -> Option<vig_platform::measure::CellRun> {
    let run = Box::pin(crate::runloop::run_cell(
        client,
        vig_platform::measure::CellId {
            model: model.to_owned(),
            concurrency: 1,
            batch: 1,
        },
        request,
        crate::runloop::RunOptions::qualified(samples, period_us, CALIBRATE_WARMUP),
    ))
    .await
    .ok()?;

    if let Some(reason) = crate::runloop::rejection(&run) {
        DISCARDED.fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "    Messreihe verworfen: {reason} \
             (Freigaben {}, Abschluesse {}, Ueberzuege {}, Fehlschlaege {}, \
             ausgelassen {})",
            run.releases(),
            run.completed(),
            run.overruns(),
            run.failures(),
            run.skipped()
        );
        return None;
    }
    QUALIFIED.fetch_add(1, Ordering::Relaxed);
    Some(run)
}

/// Wie viele Messreihen verworfen wurden, und wie viele nicht.
///
/// Prozessweit, weil die Zahl am Ende **einmal** stehen soll. Ein Lauf, der
/// zwanzig Reihen verwirft und zwei behaelt, hat nichts kalibriert — und das
/// soll nicht zwischen zwanzig Einzelmeldungen untergehen.
static DISCARDED: AtomicU64 = AtomicU64::new(0);
static QUALIFIED: AtomicU64 = AtomicU64::new(0);

/// Sagt am Ende, was die Qualifikation ergeben hat.
///
/// Ohne diese Zeile liest sich ein Lauf mit lauter verworfenen Reihen wie ein
/// Lauf mit ein paar Warnungen. Er ist aber etwas anderes: er hat nichts
/// gemessen.
fn report_qualification() {
    let discarded = DISCARDED.load(Ordering::Relaxed);
    let qualified = QUALIFIED.load(Ordering::Relaxed);
    let total = qualified.saturating_add(discarded);
    if total == 0 {
        return;
    }
    eprintln!("\nQualifikation: {qualified} von {total} Messreihen verwertbar.");
    if discarded == 0 {
        return;
    }
    eprintln!(
        "  {discarded} verworfen. Der haeufigste Grund auf einem Laptop oder \n\
         \x20 einer Karte unter Leistungslimit ist ein wandernder Takt: die \n\
         \x20 Messung gilt dann fuer keinen Betriebspunkt. Abhilfe ist ein \n\
         \x20 festgehaltener Takt (`nvidia-smi -lgc`, braucht Rechte) oder \n\
         \x20 eine Maschine, die ihren Takt haelt. Die Schwelle wird dafuer \n\
         \x20 **nicht** gelockert — eine Messung, die jeden Betriebspunkt \n\
         \x20 mittelt, beschreibt keinen."
    );
}

/// Aufwaermlaeufe vor jeder Kalibrierreihe.
///
/// Dieselbe Zahl wie in `vig profile`: die erste Anfrage an ein Modell traegt
/// dessen Initialisierung, und sie in einer Messung zu fuehren macht die
/// Messung zu einer Aussage ueber den Start.
const CALIBRATE_WARMUP: usize = 20;

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
    /// Woher diese Zahlen stammen und wofuer sie gelten (NV-03).
    manifest: ProfileManifest,
    request: ModelInferRequest,
}

// Die Funktion beschreibt einen zusammenhaengenden Messablauf: Metadaten,
// Solo, Nebenlast, Paare, Textmodelle, schreiben. Sie aufzuteilen verteilte den
// Ablauf auf Funktionen, die einzeln nichts bedeuten — und der Betreiber liest
// hier nach, was in welcher Reihenfolge gemessen wird.
#[expect(clippy::too_many_lines, reason = "ein zusammenhaengender Messablauf")]
pub(crate) async fn run(
    path: &Path,
    samples: usize,
    period_us: Option<u64>,
    out: Option<&Path>,
    identity: &IdentityArgs,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut config = Config::from_yaml(&text)?;
    let identity = &crate::profile::probe_hardware(identity.clone());
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
        "vig calibrate — Backend {endpoint}, Server {} {}",
        server.name, server.version
    );
    eprintln!("Slots laut Konfiguration: {slots}; {samples} Messungen je Stufe\n");

    let mut measurements = Vec::new();
    for (logical, backend_models) in &models {
        for backend_model in backend_models {
            eprintln!("  {logical} -> {backend_model}");
            // Ein Modell, das sich nicht mit Nulltensoren vermessen laesst —
            // Texteingaben, ein anderes Backend, ein Stream-Endpunkt — darf
            // den ganzen Lauf nicht beenden. Sonst erreicht die
            // Textkalibrierung weiter unten nie ihr Modell, und der
            // Betreiber traegt Sockel und Rate wieder von Hand ein.
            let metadata: ModelMetadataResponse = match client.model_metadata(backend_model).await {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("    uebersprungen: Metadaten nicht abrufbar ({e})");
                    continue;
                }
            };
            let request = match vig_backend_triton::zero_request(backend_model, &metadata) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "    uebersprungen: kein Nulltensor-Request baubar ({e}). \
                         Generative Modelle werden weiter unten textuell vermessen."
                    );
                    continue;
                }
            };
            let fingerprint = vig_backend_triton::fingerprint(&server, &metadata);
            let observation = vig_backend_triton::observe(&server, &metadata);
            let artifact = match crate::identity::artifact_of(
                identity,
                backend_model,
                &observation.versions,
            ) {
                crate::identity::ArtifactLookup::Found(found) => found,
                crate::identity::ArtifactLookup::NotRequested => ArtifactIdentity::default(),
                crate::identity::ArtifactLookup::Failed(reason) => {
                    eprintln!("    Artefakt-Digest nicht bildbar ({reason}); Feld bleibt unknown");
                    ArtifactIdentity::default()
                }
            };
            let manifest = crate::identity::assemble(
                &observation,
                artifact,
                identity,
                crate::identity::MeasuredUnder {
                    warmup: u32::try_from(WARMUP).unwrap_or(u32::MAX),
                    concurrency: 1,
                    batch_size: 1,
                },
                crate::identity::now_rfc3339(),
            );

            // Der Warmlauf steckt im Messkern; hier keiner mehr.
            let Some(solo_run) = Box::pin(measure(
                &client,
                backend_model,
                &request,
                samples,
                period_us,
            ))
            .await
            else {
                eprintln!(
                    "    {logical} -> {backend_model}: keine verwertbare Messreihe; \
                     die vorhandenen Werte bleiben stehen."
                );
                continue;
            };
            let solo = quantiles(&solo_run);
            eprintln!("    allein            p50 {:>7} us", solo.p50_us);

            // Je zusaetzlich belegtem Slot eine Stufe. Bei einem Slot gibt es
            // keine Nebenlast und damit nichts zu messen.
            let mut under_load = Vec::new();
            for busy in 1..slots {
                let stop = Arc::new(AtomicBool::new(false));
                let tasks = spawn_load(&client, &request, busy, &stop);
                let level = Box::pin(measure(
                    &client,
                    backend_model,
                    &request,
                    samples,
                    period_us,
                ))
                .await;
                stop_load(&stop, tasks).await;
                let Some(level) = level.as_ref().map(quantiles) else {
                    eprintln!("    {busy} belegt: keine verwertbare Messreihe; Stufe faellt aus");
                    continue;
                };
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
                manifest,
                request,
            });
        }
    }

    let (pairs, directed) = if slots > 1 {
        Box::pin(measure_pairs(&client, &measurements, samples, period_us)).await
    } else {
        eprintln!("\nEin Slot: Modellpaare koennen sich nicht behindern, keine Paarmessung.");
        (Vec::new(), Vec::new())
    };

    let cooperative = Box::pin(measure_all_cooperative(&config)).await;

    apply(&mut config, &measurements, &pairs, &directed);
    report_qualification();
    apply_cooperative(&mut config, &cooperative);
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

/// Eine gerichtete Paarmessung (NV-11).
///
/// `victim` leidet, `co_tenant` stoert. Die Umkehrung ist eine eigene
/// Messung und kann sehr andere Zahlen ergeben: ein 95-ms-VLM verlaengert
/// einen 5-ms-Detektor um ein Vielfaches seiner eigenen Laufzeit, der
/// Detektor den VLM um wenige Prozent.
struct Directed {
    victim: String,
    co_tenant: String,
    /// Verlangsamung in Prozent; 200 bedeutet die doppelte Laufzeit.
    slowdown: u64,
    /// Die zusaetzliche Laufzeit in Mikrosekunden.
    ///
    /// Die Groesse, mit der geplant wird. Ein Verhaeltnis heisst bei 5 ms
    /// etwas anderes als bei 95 ms.
    added_us: u64,
}

/// Ein Paar, fuer das `no_corun` vorgeschlagen wird.
struct Pair {
    a: String,
    b: String,
    /// Die staerker leidende Richtung.
    worst: Directed,
}

/// Misst fuer jedes Modellpaar, wie stark das eine das andere bremst.
async fn measure_pairs(
    client: &Arc<TritonClient>,
    measurements: &[VariantMeasurement],
    samples: usize,
    period_us: Option<u64>,
) -> (Vec<Pair>, Vec<Directed>) {
    // Beide Richtungen, und zwar getrennt (NV-11). Bis hierhin wurde nur eine
    // gemessen und das Ergebnis symmetrisch angewandt — genau der Fehler, um
    // den es geht.
    let mut directed: Vec<Directed> = Vec::new();
    eprintln!("\nPaarmessung (gerichtet, beide Richtungen):");
    for victim in measurements {
        for co_tenant in measurements {
            if victim.logical == co_tenant.logical {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let tasks = spawn_load(client, &co_tenant.request, 1, &stop);
            let under = Box::pin(measure(
                client,
                &victim.backend_model,
                &victim.request,
                samples,
                period_us,
            ))
            .await;
            let Some(under) = under.as_ref().map(quantiles) else {
                eprintln!("    Paarmessung ohne verwertbare Reihe; das Paar faellt aus");
                continue;
            };
            stop_load(&stop, tasks).await;

            let slowdown = slowdown_percent(under.p50_us, victim.solo.p50_us);
            let added_us = under.p50_us.saturating_sub(victim.solo.p50_us);
            eprintln!(
                "  {:<10} neben {:<10} {:>8}  +{} us",
                victim.logical,
                co_tenant.logical,
                as_factor(slowdown),
                added_us
            );
            directed.push(Directed {
                victim: victim.logical.clone(),
                co_tenant: co_tenant.logical.clone(),
                slowdown,
                added_us,
            });
        }
    }

    // `no_corun` ist symmetrisch — der Slot verbietet das gleichzeitige
    // Laufen, nicht eine Richtung. Vorgeschlagen wird ein Paar deshalb, wenn
    // **eine** Richtung die Heuristik reisst; genannt wird, welche.
    let mut pairs: Vec<Pair> = Vec::new();
    for entry in &directed {
        if entry.slowdown < NO_CORUN_SLOWDOWN_PERCENT {
            continue;
        }
        let already = pairs.iter().any(|p| {
            (p.a == entry.victim && p.b == entry.co_tenant)
                || (p.a == entry.co_tenant && p.b == entry.victim)
        });
        if already {
            continue;
        }
        pairs.push(Pair {
            a: entry.victim.clone(),
            b: entry.co_tenant.clone(),
            worst: Directed {
                victim: entry.victim.clone(),
                co_tenant: entry.co_tenant.clone(),
                slowdown: entry.slowdown,
                added_us: entry.added_us,
            },
        });
    }
    report_asymmetry(&directed);
    (pairs, directed)
}

/// Nennt die Paare, deren Richtungen deutlich auseinanderliegen (NV-11).
///
/// Der Betreiber soll sehen, dass `no_corun` eine symmetrische Regel auf eine
/// unsymmetrische Wirklichkeit legt. Wo die beiden Richtungen weit
/// auseinanderliegen, ist die Serialisierung eine Entscheidung und keine
/// Ableitung.
fn report_asymmetry(directed: &[Directed]) {
    let mut named: Vec<(String, String)> = Vec::new();
    for forward in directed {
        let Some(backward) = directed
            .iter()
            .find(|d| d.victim == forward.co_tenant && d.co_tenant == forward.victim)
        else {
            continue;
        };
        let (high, low) = (
            forward.slowdown.max(backward.slowdown),
            forward.slowdown.min(backward.slowdown),
        );
        // Faktor zwei zwischen den Richtungen: darunter ist der Unterschied
        // Messrauschen, darueber eine Eigenschaft der Paarung.
        if high < low.saturating_mul(2) {
            continue;
        }
        let key = if forward.victim < forward.co_tenant {
            (forward.victim.clone(), forward.co_tenant.clone())
        } else {
            (forward.co_tenant.clone(), forward.victim.clone())
        };
        if named.contains(&key) {
            continue;
        }
        named.push(key);
        eprintln!(
            "  HINWEIS {} leidet {} unter {}, umgekehrt nur {}. \n\
             \x20        `no_corun` ist symmetrisch; ob die Serialisierung den \n\
             \x20        Durchsatzverlust wert ist, entscheidet der Vertrag.",
            forward.victim,
            as_factor(forward.slowdown),
            forward.co_tenant,
            as_factor(backward.slowdown),
        );
    }
}

/// Traegt die Messungen in die Konfiguration ein.
///
/// Vertraege, Klassen und Qualitaeten bleiben unberuehrt: sie sind
/// Anforderungen und keine Messwerte.
/// Misst das Kostenmodell aller zerlegbaren Modelle der Konfiguration.
///
/// Sockel und Rate veralten mit jeder Treiber- und Backendversion. In der
/// Messkonfiguration stand einmal eine Rate, die um Faktor 4,4 danebenlag —
/// der Look-ahead rechnete entsprechend falsch, und niemand sah es.
async fn measure_all_cooperative(config: &Config) -> Vec<CooperativeMeasurement> {
    let mut out = Vec::new();
    for (logical, model) in &config.models {
        if model.cooperative.is_none() {
            continue;
        }
        let Some(variant) = model.variants.first() else {
            continue;
        };
        let endpoint = model
            .backend_endpoint
            .clone()
            .unwrap_or_else(|| config.backend.grpc_endpoint.clone());
        let client = TritonClient::new(&endpoint);
        match Box::pin(measure_cooperative(
            &client,
            &variant.backend_model,
            logical,
            model.decoupled,
        ))
        .await
        {
            Some(m) => {
                let context = if m.prefill_per_token_us == 0 {
                    "Kontext kostenlos (wirksamer Prefix-Cache)".to_owned()
                } else {
                    format!("{} us je Kontexttoken", m.prefill_per_token_us)
                };
                eprintln!(
                    "\n  {logical}: Sockel {} us je Auftrag, {} Token/s, {context}",
                    m.base_cost_us, m.tokens_per_second
                );
                out.push(m);
            }
            None => eprintln!(
                "\n  {logical}: Kostenmodell nicht messbar (kein Texteingang oder keine \
                 Antwort); die vorhandenen Werte bleiben stehen."
            ),
        }
    }
    out
}

/// Das gemessene Kostenmodell eines zerlegbaren Modells.
///
/// Affin, nicht proportional, und in zwei Groessen statt einer:
///
///     Dauer = Sockel + Kontext / Kontextrate + Token / Erzeugungsrate
///
/// Der Sockel — Round-Trip und Scheduling im Backend — ist auf der
/// Messmaschine so gross wie der Slack zwischen zwei geschuetzten Ankuenften.
/// Wer ihn nicht misst, plant Quanten, die ihre Luecke sicher ueberziehen.
///
/// Der Kontextterm kam mit NV-16 dazu. Bis dahin steckte die erneute
/// Prefill-Berechnung im Sockel und galt damit als konstant — waehrend der
/// Kontext mit jedem Quantum waechst. Jetzt steht sie getrennt und wird
/// getrennt gemessen.
pub(crate) struct CooperativeMeasurement {
    /// Logischer Modellname.
    pub logical: String,
    /// Feste Kosten je Auftrag in Mikrosekunden.
    pub base_cost_us: u64,
    /// Erzeugungsrate in Token je Sekunde.
    pub tokens_per_second: u32,
    /// Aufwand je Token bereits vorhandenen Kontexts, in Mikrosekunden (NV-16).
    ///
    /// Null heisst hier **gemessen wirkungslos**: das Backend faehrt einen
    /// Prefix-Cache, der greift. Das ist eine Aussage und keine Annahme —
    /// genau der Unterschied, um den es bei dieser Zahl geht.
    pub prefill_per_token_us: u64,
}

/// Misst Sockel, Erzeugungsrate und Kontextkosten eines zerlegbaren Modells.
///
/// Zwei Punkte genuegen fuer eine Gerade, aber nicht fuer Vertrauen: gemessen
/// wird ueber mehrere Tokenzahlen, und die Rate ergibt sich aus der Differenz
/// zwischen der kleinsten und der groessten.
///
/// Es sind **zwei** Geraden, nicht eine. Die eine variiert die Zahl erzeugter
/// Token bei festem Prompt und liefert die Erzeugungsrate. Die andere variiert
/// die **Promptlaenge** bei fester Tokenzahl und liefert die Kontextkosten
/// (NV-16). Ohne die zweite Messung kuerzt sich der Prefill-Anteil aus der
/// Differenz definitionsgemaess heraus, und `prefill_per_token_us` bliebe in
/// jeder erzeugten Konfiguration auf null — die Zerlegung waere im Feld
/// wirkungslos, obwohl das Kostenmodell sie kennt.
///
/// Was dabei herauskommt, ist der **marginale** Aufwand je Kontexttoken, so
/// wie das Backend ihn liefert: mit wirksamem Prefix-Cache ist er nahe null,
/// und dann ist null die richtige Antwort. Der Sockel wird anschliessend um
/// den Prefill des Kalibrierprompts bereinigt, sonst stuende dieser Anteil
/// zweimal im Modell.
///
/// Gibt `None` zurueck, wenn das Modell keinen Texteingang hat oder das
/// Backend nicht antwortet — dann ist nichts gemessen, und geraten wird hier
/// nicht.
async fn measure_cooperative(
    client: &TritonClient,
    backend_model: &str,
    logical: &str,
    decoupled: bool,
) -> Option<CooperativeMeasurement> {
    const PROMPT: &str = "Beschreibe kurz, was auf dem Bild zu sehen ist.";
    const SMALL: u32 = 4;
    const LARGE: u32 = 32;
    /// Messrunden je Punkt, **nach** dem Warmlauf.
    ///
    /// Ungerade, damit der Median eine gemessene Stichprobe ist und kein
    /// Mittelwert zweier.
    const ROUNDS: u32 = 5;
    /// Wie oft der Kalibrierprompt fuer die lange Messung wiederholt wird.
    ///
    /// Ein deutlich laengerer Kontext, aber kein anderer Inhalt: gemessen
    /// werden soll die Laenge und nicht das Thema.
    ///
    /// 24 Wiederholungen sind rund 276 Token. Die daraus bestimmte Gerade auf
    /// mehrere tausend Kontexttoken zu verlaengern unterschaetzt den Aufwand:
    /// Attention waechst superlinear im Kontext. Der gemessene Wert ist damit
    /// eine **Untergrenze** der Grenzkosten bei langem Kontext — und die
    /// Zuschneidung faellt in dieser Richtung zu grosszuegig aus, nicht zu
    /// knapp. Das ist bekannt und steht in ADR-0031.
    const LONG_REPEATS: usize = 24;

    let long_prompt = PROMPT.repeat(LONG_REPEATS);
    // Dieselbe grobe Schaetzung wie im Gateway: rund vier Zeichen je Token.
    let prompt_tokens = |p: &str| u64::try_from(p.len().div_ceil(4)).unwrap_or(u64::MAX);

    // Median statt Mittelwert, und ein verworfener Warmlauf davor.
    //
    // Der Mittelwert ueber fuenf Runden traegt jeden Ausreisser mit einem
    // Fuenftel weiter: ein einziger Stall im Sekundenbereich verschiebt die
    // Differenz `long - small` um Hunderte Millisekunden, und der daraus
    // errechnete Kontextterm frisst dann den ganzen Sockel auf. Der Median
    // ist dagegen unempfindlich.
    //
    // Der Warmlauf muss weg, weil die allererste Anfrage an ein Modell dessen
    // Initialisierung traegt. Faellt sie in `small`, ist `small` zu gross,
    // `long - small` zu klein — und die CLI meldet „Kontext kostenlos", wo sie
    // in Wahrheit einen Warmlauf gemessen hat.
    let median_us = |prompt: &str, tokens: u32| {
        let prompt = prompt.to_owned();
        async move {
            let mut samples: Vec<u64> = Vec::new();
            for round in 0..ROUNDS.saturating_add(1) {
                let request = vig_backend_triton::text_request(backend_model, &prompt, tokens);
                let started = std::time::Instant::now();
                let result = if decoupled {
                    client.infer_decoupled(request).await
                } else {
                    client.infer(request).await
                };
                if round == 0 {
                    // Warmlauf: gefahren, nicht gezaehlt.
                    continue;
                }
                if result.is_ok() {
                    samples.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
                }
            }
            samples.sort_unstable();
            samples
                .get(samples.len().checked_div(2).unwrap_or(0))
                .copied()
        }
    };

    let small = Box::pin(median_us(PROMPT, SMALL)).await?;
    let large = Box::pin(median_us(PROMPT, LARGE)).await?;

    // Steigung aus der Differenz; sie ist gegen einen konstanten Sockel immun.
    let delta_us = large.saturating_sub(small);
    let delta_tokens = u64::from(LARGE.saturating_sub(SMALL));
    if delta_us == 0 || delta_tokens == 0 {
        return None;
    }
    let us_per_token = delta_us.checked_div(delta_tokens).unwrap_or(0).max(1);
    let tokens_per_second =
        u32::try_from(1_000_000_u64.checked_div(us_per_token).unwrap_or(0)).unwrap_or(u32::MAX);

    // Der Sockel ohne Kontextabzug: er steht auch dann, wenn die dritte
    // Messreihe scheitert. Vor NV-16 war das das ganze Ergebnis, und es waere
    // falsch, es an einer zusaetzlichen Messung aufzuhaengen.
    let generating = us_per_token.saturating_mul(u64::from(SMALL));
    let base_without_context = small.saturating_sub(generating);

    // Zweite Gerade: derselbe Auftrag, laengerer Prompt. Was hier an Zeit
    // dazukommt, ist Kontext und nicht Erzeugung.
    let short_context = prompt_tokens(PROMPT);
    let long_context = prompt_tokens(&long_prompt);
    let context_delta = long_context.saturating_sub(short_context).max(1);
    let measured_prefill = Box::pin(median_us(&long_prompt, SMALL)).await.map(|long| {
        long.saturating_sub(small)
            .checked_div(context_delta)
            .unwrap_or(0)
    });

    // Der Kontextabzug am Sockel gilt nur, wenn er ihn nicht aufzehrt. Ein
    // Sockel von null waere genau der Zustand, gegen den ADR-0015 geschrieben
    // wurde: die Zuschneidung meldete dann eine Dauer, die das Quantum nie
    // einhaelt. Bleibt vom Sockel nichts uebrig, ist die Messung
    // widerspruechlich — dann gilt der unbereinigte Sockel und der
    // Kontextterm wird verworfen, statt beide gemeinsam unbrauchbar zu
    // machen.
    let (base_cost_us, prefill_per_token_us) = match measured_prefill {
        Some(prefill) => {
            let deduction = prefill.saturating_mul(short_context);
            if deduction >= base_without_context {
                eprintln!(
                    "\n  {logical}: Kontextmessung verworfen — der errechnete \
                     Prefill ({deduction} us fuer {short_context} Token) \
                     uebersteigt den Sockel ({base_without_context} us). Das \
                     ist ein Widerspruch, kein Messwert."
                );
                (base_without_context, 0)
            } else {
                (base_without_context.saturating_sub(deduction), prefill)
            }
        }
        None => (base_without_context, 0),
    };

    Some(CooperativeMeasurement {
        logical: logical.to_owned(),
        base_cost_us,
        tokens_per_second,
        prefill_per_token_us,
    })
}

fn apply_cooperative(config: &mut Config, measured: &[CooperativeMeasurement]) {
    for (name, model) in &mut config.models {
        let Some(m) = measured.iter().find(|m| m.logical == *name) else {
            continue;
        };
        if let Some(cooperative) = model.cooperative.as_mut() {
            cooperative.base_cost_us = m.base_cost_us;
            cooperative.tokens_per_second = m.tokens_per_second;
            cooperative.prefill_per_token_us = m.prefill_per_token_us;
        }
    }
}

fn apply(
    config: &mut Config,
    measurements: &[VariantMeasurement],
    pairs: &[Pair],
    directed: &[Directed],
) {
    for (name, model) in &mut config.models {
        for variant in &mut model.variants {
            let Some(m) = measurements
                .iter()
                .find(|m| m.logical == *name && m.backend_model == variant.backend_model)
            else {
                continue;
            };
            variant.profile = Some(
                m.solo
                    .to_config(Some(m.fingerprint.clone()), Some(m.manifest.clone())),
            );
            // Die Laststufen entstehen in derselben Umgebung wie das
            // Sologprofil. Das Manifest ein zweites Mal danebenzuschreiben
            // wuerde die Datei aufblaehen, ohne eine Frage zu beantworten.
            variant.under_load = m
                .under_load
                .iter()
                .map(|l| l.to_config(None, None))
                .collect();
        }
    }
    for pair in pairs {
        let entry = [pair.a.clone(), pair.b.clone()];
        if !config.backend.no_corun.contains(&entry) {
            config.backend.no_corun.push(entry);
        }
    }

    // Die gerichtete Tabelle selbst (NV-11). Bis hierher wurden diese Zahlen
    // gemessen, berichtet und **weggeworfen** — `no_corun` allein sagt nur
    // „gar nicht zusammen" und nie „so viel kostet es".
    //
    // Ein Paar, das ohnehin serialisiert wird, braucht keinen Aufschlag: die
    // beiden laufen nie gleichzeitig.
    config.backend.interference.clear();
    for entry in directed {
        let serialised = pairs.iter().any(|p| {
            (p.a == entry.victim && p.b == entry.co_tenant)
                || (p.b == entry.victim && p.a == entry.co_tenant)
        });
        if serialised || entry.added_us == 0 {
            continue;
        }
        config
            .backend
            .interference
            .push(vig_config::schema::InterferencePair {
                victim: entry.victim.clone(),
                co_tenant: entry.co_tenant.clone(),
                added_us: entry.added_us,
            });
    }
}

fn report(pairs: &[Pair]) {
    if pairs.is_empty() {
        return;
    }
    eprintln!("\nAls `no_corun` vorgeschlagen:");
    for pair in pairs {
        eprintln!(
            "  [{}, {}] — {} leidet {} unter {} (+{} us). \n\
             \x20   Heuristik: ab der doppelten Laufzeit brauchen zwei \n\
             \x20   Auftraege **aehnlicher Laenge** nebeneinander so lange wie \n\
             \x20   nacheinander. Bei ungleichen Laengen kann Nebenlaeufigkeit \n\
             \x20   trotzdem Durchsatz bringen — das entscheidet der Vertrag.",
            pair.a,
            pair.b,
            pair.worst.victim,
            as_factor(pair.worst.slowdown),
            pair.worst.co_tenant,
            pair.worst.added_us,
        );
    }
}
