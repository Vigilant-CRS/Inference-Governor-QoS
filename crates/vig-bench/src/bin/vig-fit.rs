//! `vig-fit` — lohnt sich der Governor auf dieser Maschine, mit diesen Modellen?
//!
//! Die Frage, die jeder Interessent zuerst stellt, und die bisher nur
//! beantworten konnte, wer sich in unsere Benchmarks einarbeitet. Das Werkzeug
//! faehrt **seine** Modelle, **seine** Vertraege und **seine** Hardware, erst
//! direkt gegen das Backend und dann ueber den Governor, an mehreren
//! Lastpunkten — und sagt in einem Satz, was dabei herauskommt.
//!
//! ## Warum das kein Benchmark ist
//!
//! Ein Benchmark will eine Zahl verteidigen. Dieses Werkzeug will eine
//! Entscheidung ermoeglichen, und die haeufigste richtige Entscheidung ist
//! „brauchst du nicht": unterhalb der Saettigung kostet der Governor nur
//! seinen Aufwand. Es ist deshalb ausdruecklich so gebaut, dass ein negatives
//! Ergebnis genauso klar herauskommt wie ein positives.
//!
//! ## Was es nicht beantwortet
//!
//! * **Nicht, ob eure Erkennung gut genug ist.** Es misst Versorgung, nicht
//!   Qualitaet.
//! * **Nicht, was auf anderer Hardware passiert.** Eine Messung gilt fuer die
//!   Maschine, auf der sie lief.
//! * **Nicht, was ueber Stunden passiert.** Dafuer gibt es den Dauerlauf.
//!
//! Gemessen wird ueber den Kopierpfad, nicht ueber Shared Memory: beide Seiten
//! zahlen denselben Transport, und das Werkzeug laeuft damit auch dort, wo es
//! kein `/dev/shm` gibt. Der Vergleich bleibt einer des Schedulings.
//!
//! ## Nur der Governor-Arm
//!
//! `VIG_FIT_ARMS=governed` laesst den direkten Arm weg. Das ist **kein**
//! Urteil ueber den Governor, sondern die Bewertung einer Einstellung:
//! `vig autotune` vergleicht damit mehrere Einstellungen desselben Governors
//! gegeneinander (ADR-0045) und braucht dafuer den direkten Weg nicht jedes
//! Mal. Das JSON sagt es in `arms`, die direkten Felder stehen auf `null`,
//! und der Satz nennt sich Abstimmungslauf — damit niemand eine Bewertung
//! ohne Vergleich als „lohnt sich" liest.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
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
use vig_bench::workload::{InputSpec, StreamDef, connect, drive};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_protocol_oip::inference::ModelMetadataRequest;
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;

/// Die Lastpunkte, an denen gemessen wird.
///
/// Unterhalb, an und oberhalb der Saettigung. Der Knick liegt nach unseren
/// Messungen zwischen 100 und 110 % — deshalb liegen dort zwei Punkte.
const POINTS: [u64; 4] = [90, 100, 110, 125];

/// Ab hier gilt ein Strom als schlecht versorgt.
///
/// Fuenf Prozent verfehlter Takte sind fuer eine Regelung schon viel; als
/// Schwelle fuer „hier faengt es an wehzutun" ist das eine bewusst
/// konservative Wahl und keine gemessene Grenze.
const HURTS_PERMILLE: u64 = 50;

/// Welche Arme gefahren werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arms {
    /// Direkt und ueber den Governor — die Frage „lohnt es sich?".
    Both,
    /// Nur ueber den Governor — die Bewertung einer Einstellung, ohne
    /// Vergleich.
    Governed,
}

impl Arms {
    /// Aus `VIG_FIT_ARMS`; `None` bei einem unbekannten Wert.
    fn from_env() -> Option<Self> {
        match std::env::var("VIG_FIT_ARMS").ok().as_deref() {
            None | Some("both") => Some(Self::Both),
            Some("governed") => Some(Self::Governed),
            Some(_) => None,
        }
    }

    /// Das Wort im JSON.
    const fn label(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Governed => "governed",
        }
    }
}

/// Eine Zeile des Ergebnisses.
#[derive(Debug, Clone)]
struct Row {
    load: u64,
    stream: String,
    protected: bool,
    /// Unabgedeckte Abtastungen aus Verbrauchersicht, direkt am Backend;
    /// `None`, wenn der direkte Arm nicht gefahren wurde.
    direct: Option<u64>,
    /// Dasselbe ueber den Governor.
    governed: u64,
    direct_gap_ms: Option<u64>,
    governed_gap_ms: u64,
    /// Abtastungen (Takte) hinter dem Promillewert; `None` ohne direkten Arm.
    ///
    /// Ohne sie ist 37 ‰ nicht von 37 ‰ zu unterscheiden: einer von 27
    /// Takten oder 7 von 200. `vig autotune` bemisst daran seine Rauschschwelle.
    direct_samples: Option<u64>,
    governed_samples: u64,
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
    let Some(path) = std::env::args().nth(1) else {
        eprintln!(
            "Aufruf: vig-fit <vig.yaml>\n\n\
             Faehrt die Modelle dieser Konfiguration erst direkt gegen das\n\
             Backend und dann ueber den Governor, an mehreren Lastpunkten.\n\n\
             Umgebung:\n  \
             VIG_FIT_SECONDS  Messdauer je Arm und Punkt (Vorgabe: {} Takte des\n                   \
             langsamsten geschuetzten Stroms, {} bis {} s)\n  \
             VIG_FIT_POINTS   Lastpunkte in Prozent (Vorgabe {POINTS:?})\n  \
             VIG_FIT_ARMS     both (Vorgabe) oder governed: nur der Governor-Arm,\n                   \
             eine Bewertung ohne Vergleich (vig autotune, Schritt tune)\n  \
             VIG_FIT_JSON     Ergebnis zusaetzlich als JSON in diese Datei",
            vig_config::window::CYCLES,
            vig_config::window::FLOOR_SECONDS,
            vig_config::window::CAP_SECONDS
        );
        std::process::exit(2);
    };
    let Some(arms) = Arms::from_env() else {
        eprintln!("VIG_FIT_ARMS kennt nur `both` und `governed`.");
        std::process::exit(2);
    };

    let text = std::fs::read_to_string(&path).expect("Konfiguration lesbar");
    let config = Config::from_yaml(&text).expect("Konfiguration gueltig");
    let findings = config.diagnose();
    if !findings.is_empty() {
        eprintln!("Die Konfiguration hat offene Befunde — erst `vig doctor` gruen bekommen:");
        for finding in &findings {
            eprintln!("  {finding}");
        }
        std::process::exit(2);
    }
    let base = Arc::new(config.resolve().expect("aufloesbar"));

    let points = std::env::var("VIG_FIT_POINTS").ok().map_or_else(
        || POINTS.to_vec(),
        |raw| {
            raw.split(',')
                .filter_map(|p| p.trim().parse::<u64>().ok())
                .collect()
        },
    );
    // Das Fenster in Takten, nicht in Sekunden (`vig_config::window`): zehn
    // Sekunden sind bei einer 370-ms-Periode 27 Takte, und ein verfehlter Takt
    // ist dann schon 37 ‰.
    let lowest = points.iter().copied().min().unwrap_or(100);
    let seconds = env_u64("VIG_FIT_SECONDS")
        .unwrap_or_else(|| vig_config::window::seconds(&config, lowest, false));
    let slowest = vig_config::window::slowest_protected_period_ms(&config);
    let duration = Duration::from_secs(seconds);

    match arms {
        Arms::Both => println!("vig-fit: lohnt sich der Governor hier?"),
        Arms::Governed => println!(
            "vig-fit: Bewertung einer Governor-Einstellung (nur Governor-Arm, kein Vergleich)"
        ),
    }
    println!(
        "Backend {} · {} Modelle · geschuetzte serialisierte Auslastung {} % bei den \
         Vertragsperioden",
        base.backend_endpoint,
        base.model_names.len(),
        base.protected_utilization_permille() / 10
    );
    let cycles = slowest.map_or_else(String::new, |period| {
        format!(
            " ({} Takte des langsamsten geschuetzten Stroms bei {lowest} %)",
            vig_config::window::cycles_in(seconds, period * 100 / lowest.max(1))
        )
    });
    println!(
        "Lastpunkte {points:?} % · {seconds} s je Arm und Punkt{cycles} · Datenpfad Kopie im \
         Request\n"
    );

    // Fremde Rechenzeit statt `loadavg`: Ein Pixel 2 steht im Leerlauf bei
    // 3,5 und verbraucht dabei 0,04 Kerne, und der alte Hinweis nannte jede
    // Messung auf einem Telefon unruhig (15.09.2026). Gemessen wird vor dem
    // Lauf, solange das Backend ruht.
    let load_before = foreign_load().await;
    if load_before.is_none_or(|centi| centi >= FOREIGN_CORES_CENTI) {
        println!(
            "  HINWEIS Fremde Rechenzeit vor dem Lauf: {}. Auf einer unruhigen \
             Maschine\n          misst dieses Werkzeug teilweise die andere Arbeit. \
             Fuer eine belastbare\n          Zahl erst die Maschine frei machen.\n",
            cores(load_before)
        );
    }

    // --- Eingaben vorbereiten: eine Nullnutzlast je Modell, aus den Metadaten
    let client = vig_backend_triton::TritonClient::new(&base.backend_endpoint);
    let health = client.health().await.expect("Backend erreichbar");
    assert!(health.ready, "das Backend meldet sich als nicht bereit");

    let mut inputs: HashMap<String, InputSpec> = HashMap::new();
    let mut physical: HashMap<String, String> = HashMap::new();
    for (index, logical) in base.model_names.iter().enumerate() {
        let model = vig_core::ModelIdx(u16::try_from(index).unwrap_or(0));
        let name = base
            .backend_model(model, 0)
            .expect("Variante vorhanden")
            .to_owned();
        let endpoint = base.endpoint_of(model).to_owned();
        let per_model = vig_backend_triton::TritonClient::new(endpoint.as_str());
        let metadata = per_model
            .raw()
            .await
            .expect("Backend erreichbar")
            .model_metadata(ModelMetadataRequest {
                name: name.clone(),
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
        let byte_size = u64::try_from(elements).unwrap_or(0) * element_size(&input.datatype);
        inputs.insert(
            logical.clone(),
            InputSpec {
                name: input.name.clone(),
                datatype: input.datatype.clone(),
                shape,
                region: None,
                byte_size,
                payload: None,
            },
        );
        physical.insert(logical.clone(), name);
    }

    // --- Messen ------------------------------------------------------------
    let mut rows: Vec<Row> = Vec::new();
    for load in &points {
        let scaled = scale(&config, *load);
        let resolved = Arc::new(
            scaled
                .resolve()
                .expect("skalierte Konfiguration aufloesbar"),
        );
        let defs = |use_logical: bool| -> Vec<StreamDef> {
            resolved
                .model_names
                .iter()
                .enumerate()
                .filter_map(|(index, logical)| {
                    let contract = resolved.contracts.get(index)?;
                    Some(StreamDef {
                        text: None,
                        name: Box::leak(logical.clone().into_boxed_str()),
                        model: Box::leak(
                            if use_logical {
                                logical.clone()
                            } else {
                                physical.get(logical).cloned()?
                            }
                            .into_boxed_str(),
                        ),
                        period: contract.period.map_or(Duration::from_millis(100), to_std),
                        max_age: contract.max_age.map_or(Duration::from_secs(1), to_std),
                        in_flight_cap: 4,
                        input: inputs.get(logical).cloned(),
                        pump: false,
                        burst: None,
                    })
                })
                .collect()
        };

        print!("  {load:>3} % Last ");
        let direct = match arms {
            Arms::Both => {
                print!(" direkt …");
                Some(drive(&base.backend_endpoint, &defs(false), duration, false).await)
            }
            Arms::Governed => None,
        };

        // Der Governor bekommt fuer jeden Punkt seine eigene Instanz: die
        // Vertraege unterscheiden sich, und ein Scheduler, der mit den
        // Perioden des vorigen Punktes plant, misst etwas anderes als das,
        // was hier steht.
        let triton = Arc::new(vig_backend_triton::TritonClient::new(
            &resolved.backend_endpoint,
        ));
        let clock = MonotonicClock::start();
        let handle =
            actor::spawn(Arc::clone(&resolved), &triton, clock, &[]).expect("Scheduler startet");
        let service = GatewayService::new(Arc::clone(&resolved), triton, handle.clone(), clock);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Port");
        let gateway = listener.local_addr().expect("Adresse").to_string();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
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
        let _warm = connect(&gateway).await;

        print!(" Governor …");
        let governed = drive(&gateway, &defs(true), duration, true).await;
        let _ = stop.send(());
        let _ = server.await;
        println!(" fertig");

        for logical in &resolved.model_names {
            let Some(b) = governed.iter().find(|r| r.name == logical.as_str()) else {
                continue;
            };
            // Mit beiden Armen gilt eine Zeile nur, wenn beide geliefert
            // haben — wie bisher. Ohne direkten Arm gibt es nichts zu suchen.
            let a = match &direct {
                Some(reports) => match reports.iter().find(|r| r.name == logical.as_str()) {
                    Some(a) => Some(a),
                    None => continue,
                },
                None => None,
            };
            let protected = config
                .models
                .get(logical)
                .is_some_and(|m| m.class == "protected");
            rows.push(Row {
                load: *load,
                stream: logical.clone(),
                protected,
                direct: a.map(|a| a.coverage.consumer_uncovered_permille()),
                governed: b.coverage.consumer_uncovered_permille(),
                direct_gap_ms: a.map(|a| a.coverage.longest_gap_ns / 1_000_000),
                governed_gap_ms: b.coverage.longest_gap_ns / 1_000_000,
                direct_samples: a.map(|a| a.coverage.total),
                governed_samples: b.coverage.total,
            });
        }
    }

    // --- Bericht -----------------------------------------------------------
    println!("\n  Unabgedeckte Abtastungen aus Verbrauchersicht, je Promille.\n");
    println!("  Last | Strom            | Klasse      | direkt | Governor | laengste Luecke d/G");
    println!("  -----|------------------|-------------|--------|----------|--------------------");
    let dash = || "—".to_owned();
    for row in &rows {
        println!(
            "  {:>3} % | {:<16} | {:<11} | {:>5} ‰ | {:>6} ‰ | {:>6} / {:<6} ms",
            row.load,
            row.stream,
            if row.protected {
                "geschuetzt"
            } else {
                "nachrangig"
            },
            row.direct.map_or_else(dash, |d| d.to_string()),
            row.governed,
            row.direct_gap_ms.map_or_else(dash, |d| d.to_string()),
            row.governed_gap_ms,
        );
    }

    let load_after = foreign_load().await;
    println!("\n{}", verdict(&rows, &points, arms));
    println!(
        "\n  Fremde Rechenzeit {} vor, {} nach dem Lauf. Gemessen wurde die\n  \
         Versorgung, nicht die Erkennungsqualitaet; die Zahlen gelten fuer diese\n  \
         Maschine und diese Vertraege.",
        cores(load_before),
        cores(load_after)
    );

    if let Ok(path) = std::env::var("VIG_FIT_JSON") {
        let json = as_json(&rows, &points, seconds, load_before, load_after, arms);
        match std::fs::write(&path, json) {
            Ok(()) => println!("  JSON: {path}"),
            Err(error) => {
                eprintln!("  JSON nicht schreibbar ({error})");
                std::process::exit(1);
            }
        }
    }
    // Ohne Lieferung gibt es kein Urteil, und ein Aufrufer darf das nicht an
    // einem Exitcode 0 vorbeilesen.
    if rows.is_empty() {
        std::process::exit(1);
    }
}

/// Das Urteil in einem Satz — auch, wenn es negativ ausfaellt.
fn verdict(rows: &[Row], points: &[u64], arms: Arms) -> String {
    if rows.is_empty() {
        return "  Kein Ergebnis: kein Strom hat geliefert. Laeuft das Backend, und \
                passen die Modellnamen?"
            .to_owned();
    }
    if arms == Arms::Governed {
        let tuning = governed_only(rows);
        return format!(
            "  BEWERTUNG Abstimmungslauf ohne direkten Vergleich: Unter dem Governor\n  \
             verfehlen die geschuetzten Stroeme hoechstens {} ‰ (bei {} % Last), die\n  \
             nachrangigen hoechstens {} ‰. Ob sich der Governor hier lohnt, sagt nur\n  \
             ein Lauf mit beiden Armen.",
            tuning.protected_worst, tuning.protected_load, tuning.background_worst
        );
    }
    // Der erste Punkt, an dem der direkte Weg den geschuetzten Strom verliert.
    let Some(finding) = finding(rows, points) else {
        return "  Kein Ergebnis: kein Strom hat geliefert. Laeuft das Backend, und \
                passen die Modellnamen?"
            .to_owned();
    };
    let Finding::Breaks {
        load,
        direct,
        governed,
        price_direct,
        price_governed,
    } = finding
    else {
        let highest = points.iter().copied().max().unwrap_or(0);
        return format!(
            "  URTEIL Bis {highest} % Angebotslast verliert auch der direkte Weg nichts.\n  \
             Auf dieser Maschine, mit diesen Modellen und Vertraegen lohnt sich der\n  \
             Governor nicht — er kostet dann nur seinen eigenen Aufwand. Interessant\n  \
             wird es erst, wenn die Last ueber die Saettigung geht oder ein langer,\n  \
             nicht unterbrechbarer Auftrag dazwischenkommt."
        );
    };

    let mut out = if governed >= direct {
        format!(
            "  URTEIL Ab {load} % Last verliert der direkte Weg {direct} ‰ der Takte des\n  \
             geschuetzten Stroms — der Governor {governed} ‰, also **nicht weniger**.\n  \
             Das ist ein Ergebnis gegen uns: auf dieser Last bringt er hier nichts."
        )
    } else {
        format!(
            "  URTEIL Ab {load} % Last verfehlt der direkte Weg {direct} ‰ der Takte des\n  \
             geschuetzten Stroms, der Governor {governed} ‰."
        )
    };
    if price_direct > 0 || price_governed > 0 {
        let _ = write!(
            out,
            "\n  Der Preis steht daneben: die nachrangigen Stroeme verlieren direkt\n  \
             {price_direct} ‰, unter dem Governor {price_governed} ‰. Wer sie braucht, muss das\n  \
             gegeneinander abwaegen."
        );
    }
    out
}

/// Skaliert die Perioden auf einen Lastpunkt.
///
/// Nur die Periode, nicht die Frist: Hoehere Last heisst, dass die Quelle
/// haeufiger liefert. Was ein einzelnes Ergebnis wert ist und wann es zu spaet
/// kommt, aendert sich dadurch nicht.
fn scale(config: &Config, load_percent: u64) -> Config {
    let mut scaled = config.clone();
    for model in scaled.models.values_mut() {
        if let Some(period) = model.contract.period_ms {
            model.contract.period_ms = Some((period * 100 / load_percent.max(1)).max(1));
        }
    }
    scaled
}

fn as_json(
    rows: &[Row],
    points: &[u64],
    seconds: u64,
    before: Option<u64>,
    after: Option<u64>,
    arms: Arms,
) -> String {
    let cells: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "load_percent": r.load,
                "stream": r.stream,
                "protected": r.protected,
                "direct_uncovered_permille": r.direct,
                "governed_uncovered_permille": r.governed,
                "direct_longest_gap_ms": r.direct_gap_ms,
                "governed_longest_gap_ms": r.governed_gap_ms,
                // Takte hinter dem Promillewert; `vig autotune` rechnet damit
                // seine Rauschschwelle.
                "direct_samples": r.direct_samples,
                "governed_samples": r.governed_samples,
            })
        })
        .collect();
    serde_json::json!({
        "tool": "vig-fit",
        // `governed`: nur der Governor-Arm lief. Dann sind die direkten
        // Felder `null` — nicht gemessen, nicht null Promille.
        "arms": arms.label(),
        "seconds_per_arm": seconds,
        "load_points_percent": points,
        // Hundertstel Kerne fremder Rechenzeit; `null`, wo nicht beobachtbar.
        "foreign_cores_centi_before": before,
        "foreign_cores_centi_after": after,
        "view": "consumer",
        "cells": cells,
        "verdict": verdict(rows, points, arms).trim().to_owned(),
        // Dieselbe Aussage fuer den englischen Qualifikationsbericht von
        // `vig autotune`. Vorher stand dort der deutsche Satz mitten in einem
        // englischen Dokument — ein Urteil, das der Leser nicht lesen kann,
        // ist keines (validierung-autotune.md, Befund 5).
        "verdict_en": verdict_en(rows, points, arms),
        // Ohne Lieferung gibt es kein Urteil. Das Feld sagt es maschinenlesbar,
        // damit niemand „kein Strom hat geliefert" als Ergebnis uebernimmt.
        "conclusive": !rows.is_empty(),
    })
    .to_string()
}

/// Das Urteil auf Englisch, mit denselben Zahlen wie [`verdict`].
fn verdict_en(rows: &[Row], points: &[u64], arms: Arms) -> String {
    if rows.is_empty() {
        return "No result: no stream delivered. Is the backend running, and do the model \
                names match?"
            .to_owned();
    }
    if arms == Arms::Governed {
        let tuning = governed_only(rows);
        return format!(
            "Tuning evaluation without a direct comparison: under the governor the protected \
             streams miss at worst {} ‰ (at {} % load), the lower-priority streams at worst \
             {} ‰. Whether the governor pays off here only a run with both arms can say.",
            tuning.protected_worst, tuning.protected_load, tuning.background_worst
        );
    }
    let Some(finding) = finding(rows, points) else {
        return "No result: no stream delivered. Is the backend running, and do the model \
                names match?"
            .to_owned();
    };
    match finding {
        Finding::NoGain { highest } => format!(
            "Up to {highest} % offered load the direct path loses nothing either. On this \
             machine, with these models and contracts, the governor is not worth it — it only \
             costs its own overhead. It becomes interesting once load goes past saturation or \
             a long, non-interruptible job gets in the way."
        ),
        Finding::Breaks {
            load,
            direct,
            governed,
            price_direct,
            price_governed,
        } => {
            let mut out = if governed >= direct {
                format!(
                    "From {load} % load the direct path misses {direct} ‰ of the protected \
                     stream's cycles — the governor {governed} ‰, so no fewer. That is a result \
                     against us: on this load it brings nothing here."
                )
            } else {
                format!(
                    "From {load} % load the direct path misses {direct} ‰ of the protected \
                     stream's cycles, the governor {governed} ‰."
                )
            };
            if price_direct > 0 || price_governed > 0 {
                let _ = write!(
                    out,
                    " The price is right beside it: the lower-priority streams miss \
                     {price_direct} ‰ directly and {price_governed} ‰ under the governor. \
                     Whoever needs them has to weigh one against the other."
                );
            }
            out
        }
    }
}

/// Was das Urteil feststellt, ohne Sprache.
enum Finding {
    /// Auch der direkte Weg verliert bis zum hoechsten Lastpunkt nichts.
    NoGain { highest: u64 },
    /// Ab `load` verliert der direkte Weg den geschuetzten Strom.
    Breaks {
        load: u64,
        direct: u64,
        governed: u64,
        price_direct: u64,
        price_governed: u64,
    },
}

/// Die Feststellung hinter beiden Fassungen des Urteils; `None` ohne Lieferung.
fn finding(rows: &[Row], points: &[u64]) -> Option<Finding> {
    if rows.is_empty() {
        return None;
    }
    let worst = |load: u64, protected: bool, governed: bool| -> u64 {
        rows.iter()
            .filter(|r| r.load == load && r.protected == protected)
            .filter_map(|r| if governed { Some(r.governed) } else { r.direct })
            .max()
            .unwrap_or(0)
    };
    let highest = points.iter().copied().max().unwrap_or(0);
    let Some(load) = points
        .iter()
        .copied()
        .find(|load| worst(*load, true, false) > HURTS_PERMILLE)
    else {
        return Some(Finding::NoGain { highest });
    };
    Some(Finding::Breaks {
        load,
        direct: worst(load, true, false),
        governed: worst(load, true, true),
        price_direct: worst(load, false, false),
        price_governed: worst(load, false, true),
    })
}

/// Was eine Bewertung ohne direkten Arm feststellt.
struct GovernedOnly {
    /// Der schlechteste geschuetzte Strom ueber alle Lastpunkte.
    protected_worst: u64,
    /// Der Lastpunkt, an dem er auftrat (der erste bei Gleichstand).
    protected_load: u64,
    /// Der schlechteste nachrangige Strom ueber alle Lastpunkte.
    background_worst: u64,
}

/// Die Feststellung hinter dem Satz eines Abstimmungslaufs.
///
/// Bewusst nur Maxima und kein Urteil: Ohne direkten Arm gibt es nichts, wogegen
/// ein „lohnt sich" stehen koennte. Die Zielgroesse, nach der `vig autotune`
/// Einstellungen vergleicht, rechnet `autotune` selbst aus den Zellen.
fn governed_only(rows: &[Row]) -> GovernedOnly {
    let mut out = GovernedOnly {
        protected_worst: 0,
        protected_load: rows.first().map_or(0, |r| r.load),
        background_worst: 0,
    };
    for row in rows {
        if row.protected {
            if row.governed > out.protected_worst {
                out.protected_worst = row.governed;
                out.protected_load = row.load;
            }
        } else {
            out.background_worst = out.background_worst.max(row.governed);
        }
    }
    out
}

fn element_size(datatype: &str) -> u64 {
    match datatype {
        "FP32" | "INT32" | "UINT32" => 4,
        "FP16" | "INT16" | "UINT16" => 2,
        "FP64" | "INT64" | "UINT64" => 8,
        _ => 1,
    }
}

fn to_std(d: vig_core::Duration) -> Duration {
    Duration::from_nanos(d.as_nanos())
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

/// Ab so viel fremder Rechenzeit gilt die Maschine als unruhig, in Hundertstel
/// Kernen — dieselbe Grenze wie in `vig autotune`.
const FOREIGN_CORES_CENTI: u64 = 100;

/// Fremde Rechenzeit ueber drei Sekunden, in Hundertstel Kernen; `None`, wo
/// sie nicht beobachtbar ist.
async fn foreign_load() -> Option<u64> {
    let before = vig_platform::cpu::CpuSample::now()?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    before.foreign_cores_centi(vig_platform::cpu::CpuSample::now()?)
}

/// `1.23 Kerne` oder `nicht beobachtbar`.
fn cores(centi: Option<u64>) -> String {
    centi.map_or_else(
        || "nicht beobachtbar".to_owned(),
        |c| {
            format!(
                "{}.{:02} Kerne",
                c.checked_div(100).unwrap_or(0),
                c.checked_rem(100).unwrap_or(0)
            )
        },
    )
}

#[cfg(test)]
// `parsed["arms"]` ist hier die Zusicherung, dass das Feld existiert.
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::{Arms, Row, as_json, verdict};

    fn row(load: u64, protected: bool, direct: u64, governed: u64) -> Row {
        Row {
            load,
            stream: if protected { "det" } else { "bg" }.to_owned(),
            protected,
            direct: Some(direct),
            governed,
            direct_gap_ms: Some(0),
            governed_gap_ms: 0,
            direct_samples: Some(200),
            governed_samples: 200,
        }
    }

    /// Unterhalb der Saettigung ist „brauchst du nicht" die richtige Antwort,
    /// und sie muss genauso deutlich dastehen wie ein Erfolg.
    #[test]
    fn below_saturation_the_verdict_says_no() {
        let rows = vec![row(90, true, 0, 0), row(100, true, 3, 2)];
        let text = verdict(&rows, &[90, 100], Arms::Both);
        assert!(text.contains("lohnt sich der\n  Governor nicht"), "{text}");
    }

    /// Ohne direkten Arm stehen die direkten Felder auf `null`, und der Satz
    /// nennt sich Bewertung — nie ein Urteil, das es ohne Vergleich nicht gibt.
    #[test]
    fn a_governed_only_run_is_a_tuning_evaluation_not_a_verdict() {
        let governed_only = |load: u64, protected: bool, governed: u64| Row {
            direct: None,
            direct_gap_ms: None,
            direct_samples: None,
            ..row(load, protected, 0, governed)
        };
        let rows = vec![
            governed_only(110, true, 12),
            governed_only(110, false, 40),
            governed_only(125, true, 30),
            governed_only(125, false, 90),
        ];
        let text = as_json(&rows, &[110, 125], 10, Some(3), None, Arms::Governed);
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["arms"], "governed");
        assert_eq!(parsed["conclusive"], true);
        let cells = parsed["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 4);
        for cell in cells {
            assert!(cell["direct_uncovered_permille"].is_null(), "{cell}");
            assert!(cell["direct_longest_gap_ms"].is_null(), "{cell}");
            assert!(cell["governed_uncovered_permille"].is_u64(), "{cell}");
            assert!(cell["direct_samples"].is_null(), "{cell}");
            assert_eq!(cell["governed_samples"], 200, "{cell}");
        }
        let english = parsed["verdict_en"].as_str().unwrap();
        assert!(english.starts_with("Tuning evaluation"), "{english}");
        assert!(english.contains("30 ‰ (at 125 % load)"), "{english}");
        assert!(english.contains("90 ‰"), "{english}");
        assert!(!english.contains("not worth it"), "{english}");
        assert!(
            parsed["verdict"]
                .as_str()
                .unwrap()
                .contains("ohne direkten Vergleich")
        );

        // Mit beiden Armen bleibt alles, wie es war.
        let both = as_json(
            &[row(110, true, 300, 12)],
            &[110],
            10,
            None,
            None,
            Arms::Both,
        );
        let parsed: serde_json::Value = serde_json::from_str(&both).unwrap();
        assert_eq!(parsed["arms"], "both");
        assert_eq!(parsed["cells"][0]["direct_uncovered_permille"], 300);
    }

    /// Ueber der Saettigung nennt das Urteil den Punkt und beide Zahlen.
    #[test]
    fn above_saturation_it_names_the_point_and_both_numbers() {
        let rows = vec![
            row(100, true, 10, 8),
            row(110, true, 340, 12),
            row(110, false, 20, 300),
        ];
        let text = verdict(&rows, &[100, 110], Arms::Both);
        assert!(text.contains("Ab 110 % Last"), "{text}");
        assert!(text.contains("340 ‰"), "{text}");
        assert!(text.contains("12 ‰"), "{text}");
        assert!(text.contains("300 ‰"), "Preis fehlt: {text}");
    }

    /// Ist der Governor nicht besser, sagt das Werkzeug genau das.
    #[test]
    fn a_result_against_us_is_reported_as_such() {
        let rows = vec![row(110, true, 200, 260)];
        let text = verdict(&rows, &[110], Arms::Both);
        assert!(text.contains("nicht weniger"), "{text}");
        assert!(text.contains("gegen uns"), "{text}");
    }

    /// Ohne Lieferung gibt es kein Urteil, sondern einen Hinweis.
    #[test]
    fn no_delivery_is_not_a_verdict() {
        assert!(verdict(&[], &[100], Arms::Both).contains("Kein Ergebnis"));
    }
}
