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

/// Messdauer je Arm und Lastpunkt.
const SECONDS: u64 = 10;

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

/// Eine Zeile des Ergebnisses.
#[derive(Debug, Clone)]
struct Row {
    load: u64,
    stream: String,
    protected: bool,
    /// Unabgedeckte Abtastungen aus Verbrauchersicht, direkt am Backend.
    direct: u64,
    /// Dasselbe ueber den Governor.
    governed: u64,
    direct_gap_ms: u64,
    governed_gap_ms: u64,
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
             VIG_FIT_SECONDS  Messdauer je Arm und Punkt (Vorgabe {SECONDS})\n  \
             VIG_FIT_POINTS   Lastpunkte in Prozent (Vorgabe {POINTS:?})\n  \
             VIG_FIT_JSON     Ergebnis zusaetzlich als JSON in diese Datei"
        );
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

    let seconds = env_u64("VIG_FIT_SECONDS").unwrap_or(SECONDS);
    let points = std::env::var("VIG_FIT_POINTS").ok().map_or_else(
        || POINTS.to_vec(),
        |raw| {
            raw.split(',')
                .filter_map(|p| p.trim().parse::<u64>().ok())
                .collect()
        },
    );
    let duration = Duration::from_secs(seconds);

    println!("vig-fit: lohnt sich der Governor hier?");
    println!(
        "Backend {} · {} Modelle · geschuetzte serialisierte Auslastung {} % bei den \
         Vertragsperioden",
        base.backend_endpoint,
        base.model_names.len(),
        base.protected_utilization_permille() / 10
    );
    println!(
        "Lastpunkte {points:?} % · {seconds} s je Arm und Punkt · Datenpfad Kopie im Request\n"
    );

    let load_before = loadavg();
    if load_before > 1.5 {
        println!(
            "  HINWEIS Die Systemlast liegt bei {load_before:.1}. Auf einer unruhigen \
             Maschine\n          misst dieses Werkzeug teilweise die andere Arbeit. \
             Fuer eine belastbare\n          Zahl erst die Maschine frei machen.\n"
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

        print!("  {load:>3} % Last  direkt …");
        let direct = drive(&base.backend_endpoint, &defs(false), duration, false).await;

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
            let (Some(a), Some(b)) = (
                direct.iter().find(|r| r.name == logical.as_str()),
                governed.iter().find(|r| r.name == logical.as_str()),
            ) else {
                continue;
            };
            let protected = config
                .models
                .get(logical)
                .is_some_and(|m| m.class == "protected");
            rows.push(Row {
                load: *load,
                stream: logical.clone(),
                protected,
                direct: a.coverage.consumer_uncovered_permille(),
                governed: b.coverage.consumer_uncovered_permille(),
                direct_gap_ms: a.coverage.longest_gap_ns / 1_000_000,
                governed_gap_ms: b.coverage.longest_gap_ns / 1_000_000,
            });
        }
    }

    // --- Bericht -----------------------------------------------------------
    println!("\n  Unabgedeckte Abtastungen aus Verbrauchersicht, je Promille.\n");
    println!("  Last | Strom            | Klasse      | direkt | Governor | laengste Luecke d/G");
    println!("  -----|------------------|-------------|--------|----------|--------------------");
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
            row.direct,
            row.governed,
            row.direct_gap_ms,
            row.governed_gap_ms,
        );
    }

    let load_after = loadavg();
    println!("\n{}", verdict(&rows, &points));
    println!(
        "\n  Systemlast {load_before:.1} vor, {load_after:.1} nach dem Lauf. Gemessen wurde die\n  \
         Versorgung, nicht die Erkennungsqualitaet; die Zahlen gelten fuer diese\n  \
         Maschine und diese Vertraege."
    );

    if let Ok(path) = std::env::var("VIG_FIT_JSON") {
        let json = as_json(&rows, &points, seconds, load_before, load_after);
        match std::fs::write(&path, json) {
            Ok(()) => println!("  JSON: {path}"),
            Err(error) => eprintln!("  JSON nicht schreibbar ({error})"),
        }
    }
}

/// Das Urteil in einem Satz — auch, wenn es negativ ausfaellt.
fn verdict(rows: &[Row], points: &[u64]) -> String {
    if rows.is_empty() {
        return "  Kein Ergebnis: kein Strom hat geliefert. Laeuft das Backend, und \
                passen die Modellnamen?"
            .to_owned();
    }
    let worst = |load: u64, protected: bool, governed: bool| -> u64 {
        rows.iter()
            .filter(|r| r.load == load && r.protected == protected)
            .map(|r| if governed { r.governed } else { r.direct })
            .max()
            .unwrap_or(0)
    };
    let highest = points.iter().copied().max().unwrap_or(0);

    // Der erste Punkt, an dem der direkte Weg den geschuetzten Strom verliert.
    let breaking = points
        .iter()
        .copied()
        .find(|load| worst(*load, true, false) > HURTS_PERMILLE);

    let Some(load) = breaking else {
        return format!(
            "  URTEIL Bis {highest} % Angebotslast verliert auch der direkte Weg nichts.\n  \
             Auf dieser Maschine, mit diesen Modellen und Vertraegen lohnt sich der\n  \
             Governor nicht — er kostet dann nur seinen eigenen Aufwand. Interessant\n  \
             wird es erst, wenn die Last ueber die Saettigung geht oder ein langer,\n  \
             nicht unterbrechbarer Auftrag dazwischenkommt."
        );
    };

    let direct = worst(load, true, false);
    let governed = worst(load, true, true);
    let price_direct = worst(load, false, false);
    let price_governed = worst(load, false, true);

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

fn as_json(rows: &[Row], points: &[u64], seconds: u64, before: f64, after: f64) -> String {
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
            })
        })
        .collect();
    serde_json::json!({
        "tool": "vig-fit",
        "seconds_per_arm": seconds,
        "load_points_percent": points,
        "loadavg_before": before,
        "loadavg_after": after,
        "view": "consumer",
        "cells": cells,
        "verdict": verdict(rows, points).trim().to_owned(),
    })
    .to_string()
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

/// Die Systemlast der letzten Minute, oder 0, wo es sie nicht gibt.
fn loadavg() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::{Row, verdict};

    fn row(load: u64, protected: bool, direct: u64, governed: u64) -> Row {
        Row {
            load,
            stream: if protected { "det" } else { "bg" }.to_owned(),
            protected,
            direct,
            governed,
            direct_gap_ms: 0,
            governed_gap_ms: 0,
        }
    }

    /// Unterhalb der Saettigung ist „brauchst du nicht" die richtige Antwort,
    /// und sie muss genauso deutlich dastehen wie ein Erfolg.
    #[test]
    fn below_saturation_the_verdict_says_no() {
        let rows = vec![row(90, true, 0, 0), row(100, true, 3, 2)];
        let text = verdict(&rows, &[90, 100]);
        assert!(text.contains("lohnt sich der\n  Governor nicht"), "{text}");
    }

    /// Ueber der Saettigung nennt das Urteil den Punkt und beide Zahlen.
    #[test]
    fn above_saturation_it_names_the_point_and_both_numbers() {
        let rows = vec![
            row(100, true, 10, 8),
            row(110, true, 340, 12),
            row(110, false, 20, 300),
        ];
        let text = verdict(&rows, &[100, 110]);
        assert!(text.contains("Ab 110 % Last"), "{text}");
        assert!(text.contains("340 ‰"), "{text}");
        assert!(text.contains("12 ‰"), "{text}");
        assert!(text.contains("300 ‰"), "Preis fehlt: {text}");
    }

    /// Ist der Governor nicht besser, sagt das Werkzeug genau das.
    #[test]
    fn a_result_against_us_is_reported_as_such() {
        let rows = vec![row(110, true, 200, 260)];
        let text = verdict(&rows, &[110]);
        assert!(text.contains("nicht weniger"), "{text}");
        assert!(text.contains("gegen uns"), "{text}");
    }

    /// Ohne Lieferung gibt es kein Urteil, sondern einen Hinweis.
    #[test]
    fn no_delivery_is_not_a_verdict() {
        assert!(verdict(&[], &[100]).contains("Kein Ergebnis"));
    }
}
