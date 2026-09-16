//! Lange nachrangige Arbeit gegen die Zusage bewachter Modelle (ADR-0035).
//!
//! Der Anlass ist gemessen: zwei Sprachmodelle mit 195 ms p99 auf zwei Slots
//! rissen eine geschuetzte Kamera mit 100 ms Hoechstalter auf 245 ms Luecke.
//! Ohne die beiden blieb dieselbe Kamera bei 86 ms. Die Konfiguration konnte
//! das vorher nicht sagen — jetzt sagt sie es.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use core::fmt::Write as _;
use vig_config::Config;

const DEMO: &str = include_str!("../../../examples/demo/krakow-cams4.yaml");

/// Eine geschuetzte Kamera (Hoechstalter 100 ms) und `long_models` lange
/// Sprachmodelle mit p99 195 ms.
fn yaml(slots: usize, long_models: usize, preemptible: bool) -> String {
    let lanes = if preemptible {
        "  preemptible_lanes: 1\n"
    } else {
        ""
    };
    let mut yaml = format!(
        "version: 1
backend:
  type: triton
  grpc_endpoint: \"127.0.0.1:9201\"
  slots: {slots}
{lanes}models:
  camera:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 33, deadline_ms: 66, max_age_ms: 100 }}
    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 17000, p95_us: 18000, p99_us: 19000, samples: 110 }}
"
    );
    for i in 0..long_models {
        let preempt = if preemptible {
            "    preemptible: { residual_blocking_us: 20000, source: measured }\n"
        } else {
            ""
        };
        write!(
            yaml,
            "  llm{i}:
    class: best_effort
    queue: {{ policy: fifo, capacity: 2, overflow: backpressure_client }}
    contract: {{ deadline_ms: 2000, max_age_ms: 4000 }}
{preempt}    variants:
      - id: main
        backend_model: qwen
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 193000, p95_us: 195000, p99_us: 196000, samples: 110 }}
"
        )
        .unwrap();
    }
    yaml
}

/// Zwei lange Auftraege auf zwei Slots: die Kamera kann ihre 100 ms nicht
/// halten, und die Konfiguration sagt es — an beiden Verursachern.
#[test]
fn work_that_can_fill_every_slot_is_refused() {
    let findings = Config::from_yaml(&yaml(2, 2, false)).unwrap().diagnose();
    let blocking: Vec<_> = findings
        .iter()
        .filter(|f| f.to_string().contains("laenger als die engste Zusage"))
        .collect();
    assert_eq!(blocking.len(), 2, "{findings:?}");
    assert!(
        blocking.iter().any(|f| f.path == "models.llm0.contract"),
        "{findings:?}"
    );
    assert!(
        blocking.iter().any(|f| f.path == "models.llm1.contract"),
        "{findings:?}"
    );
}

/// Ein einzelner langer Auftrag neben zwei Slots laesst der Kamera noch einen.
/// Das ist die Lage der veroeffentlichten Demo, und sie bleibt zulaessig.
#[test]
fn one_long_job_beside_two_slots_stays_allowed() {
    let findings = Config::from_yaml(&yaml(2, 1, false)).unwrap().diagnose();
    assert!(findings.is_empty(), "{findings:?}");
}

/// Auf einem einzigen Slot genuegt ein langer Auftrag.
#[test]
fn one_long_job_on_one_slot_is_refused() {
    let findings = Config::from_yaml(&yaml(1, 1, false)).unwrap().diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.to_string().contains("laenger als die engste Zusage")),
        "{findings:?}"
    );
}

/// Wer seine Restblockierung gemessen hat, ist eingeplant (ADR-0035) — dann
/// ist dieselbe Last kein Befund mehr.
#[test]
fn declared_preemption_removes_the_finding() {
    let findings = Config::from_yaml(&yaml(2, 2, true)).unwrap().diagnose();
    assert!(
        !findings
            .iter()
            .any(|f| f.to_string().contains("laenger als die engste Zusage")),
        "{findings:?}"
    );
}

/// Die veroeffentlichte Demo bleibt befundfrei: ein Sprachbildmodell, zwei
/// Slots. Sonst waere die Regel eine Regression in dem, was wir zeigen.
#[test]
fn the_published_demo_stays_clean() {
    let findings = Config::from_yaml(DEMO).unwrap().diagnose();
    assert!(findings.is_empty(), "{findings:?}");
}
