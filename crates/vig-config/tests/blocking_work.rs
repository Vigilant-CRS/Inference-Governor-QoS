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

/// Dieselbe Lage wie `yaml(1, 1, false)`, nur ist das Sprachmodell zerlegbar.
///
/// `min_tokens` bestimmt, wie lange das laengste Quantum dauert: bei 258
/// Token/s und 6277 us Sockel kostet ein 8-Token-Quantum rund 37 ms, ein
/// 32-Token-Quantum rund 130 ms. Die Zusage der Kamera betraegt 100 ms —
/// dazwischen liegt die Grenze, um die es geht.
fn decomposable_yaml(min_tokens: u32) -> String {
    let cooperative = format!(
        "    cooperative: {{ tokens_per_second: 258, min_tokens: {min_tokens}, \
         max_total_tokens: 48, base_cost_us: 6277, prefill_per_token_us: 1 }}\n"
    );
    yaml(1, 1, false).replace(
        "    variants:\n      - id: main\n        backend_model: qwen",
        &format!("{cooperative}    variants:\n      - id: main\n        backend_model: qwen"),
    )
}

/// Ein zerlegbarer Auftrag belegt den Slot nur fuer ein Quantum (ADR-0014).
///
/// Der Anlass ist ein Widerspruch im eigenen Werkzeug, gefunden am
/// 16.09.2026: der Befund riet zu `cooperative:`, aber die Regel fragte gar
/// nicht danach. Wer dem Rat folgte, bekam denselben Befund erneut — und der
/// Governor verweigerte den Start. Damit war die Zerlegung in genau dem Fall
/// gesperrt, fuer den sie gebaut wurde: auf **einer** Ausfuehrungseinheit.
#[test]
fn a_small_quantum_is_no_longer_a_blocker() {
    let findings = Config::from_yaml(&decomposable_yaml(8)).unwrap().diagnose();
    assert!(findings.is_empty(), "{findings:?}");
}

/// Zerlegen allein genuegt nicht — das Quantum muss auch hineinpassen.
///
/// Die Gegenprobe zur Lockerung: ein 32-Token-Quantum dauert rund 130 ms und
/// reisst die 100-ms-Zusage genauso wie der ungeteilte Auftrag. Der Befund
/// bleibt also, nennt aber einen **anderen** Ausweg: wer `cooperative:` schon
/// gesetzt hat, braucht nicht denselben Rat noch einmal, sondern ein
/// kleineres Quantum.
#[test]
fn a_quantum_that_does_not_fit_stays_a_blocker() {
    let findings = Config::from_yaml(&decomposable_yaml(32))
        .unwrap()
        .diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.to_string().contains("schon sein laengstes Quantum")),
        "{findings:?}"
    );
    assert!(
        !findings
            .iter()
            .any(|f| f.to_string().contains("ist `cooperative:` der wirksamste Ausweg")),
        "wer zerlegt, darf nicht zum Zerlegen geraten bekommen: {findings:?}"
    );
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

/// Eine Zusage ist ein Anspruch — und wird wie eine bewachte Klasse geschuetzt
/// (ADR-0047).
///
/// Gemessen am 16.09.2026: ein `normal`-Strom mit 100 ms Frist und einer
/// Zusage von 800 ‰ erfuellte sie neben einem 195-ms-Aufruf zu 370 ‰, mit
/// einem 70-ms-Aufruf zu 645 ‰ und ohne ihn zu 965 ‰. Die Unmachbarkeits-
/// Abweisungen fielen dabei von 355 auf null. Die Konfiguration konnte das
/// vorher nicht sagen, weil nur bewachte Klassen zaehlten.
#[test]
fn an_objective_is_protected_like_a_guarded_class() {
    let yaml = |with_objective: bool| {
        let objective = if with_objective {
            "      objective: { coverage_permille: 800, window_ms: 10000 }\n"
        } else {
            ""
        };
        format!(
            "version: 1
backend:
  type: triton
  grpc_endpoint: \"127.0.0.1:9201\"
  slots: 1
models:
  rear:
    class: normal
    queue: {{ policy: latest, capacity: 1 }}
    contract:
      period_ms: 50
      deadline_ms: 100
      max_age_ms: 100
{objective}    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 13000, p95_us: 14000, p99_us: 15000, samples: 110 }}
  llm:
    class: best_effort
    queue: {{ policy: fifo, capacity: 2, overflow: backpressure_client }}
    contract: {{ deadline_ms: 2000, max_age_ms: 4000 }}
    variants:
      - id: main
        backend_model: qwen
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 193000, p95_us: 195000, p99_us: 196000, samples: 110 }}
"
        )
    };

    // Mit Zusage: der lange Auftrag macht die 100-ms-Frist unhaltbar.
    let findings = Config::from_yaml(&yaml(true)).unwrap().diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.to_string().contains("laenger als die engste Zusage")),
        "eine Zusage muss geschuetzt werden: {findings:?}"
    );

    // Ohne Zusage hat niemand einen Anspruch, den der lange Auftrag brechen
    // koennte — dann ist dieselbe Datei zulaessig.
    let findings = Config::from_yaml(&yaml(false)).unwrap().diagnose();
    assert!(
        !findings
            .iter()
            .any(|f| f.to_string().contains("laenger als die engste Zusage")),
        "ohne Anspruch kein Befund: {findings:?}"
    );
}

/// Die veroeffentlichte Demo bleibt befundfrei: ein Sprachbildmodell, zwei
/// Slots. Sonst waere die Regel eine Regression in dem, was wir zeigen.
#[test]
fn the_published_demo_stays_clean() {
    let findings = Config::from_yaml(DEMO).unwrap().diagnose();
    assert!(findings.is_empty(), "{findings:?}");
}
