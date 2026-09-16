//! `contract.min_runtime` in der Konfiguration (ADR-0046).

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use vig_config::Config;
use vig_core::Duration;
use vig_core::runtime_budget::RuntimeBudget;

const DEMO: &str = include_str!("../../../examples/demo/krakow-cams4.yaml");

/// Ein Detektor und ein Sprachmodell; `vlm_class` und `min_runtime` frei.
fn yaml(slots: usize, vlm_class: &str, min_runtime: &str) -> String {
    format!(
        "version: 1
backend:
  type: triton
  grpc_endpoint: \"127.0.0.1:9201\"
  slots: {slots}
models:
  detector:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 33, deadline_ms: 66, max_age_ms: 100 }}
    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 19000, p95_us: 20000, p99_us: 22000, samples: 200 }}
  vlm:
    class: {vlm_class}
    queue: {{ policy: fifo, capacity: 2, overflow: backpressure_client }}
    contract:
      deadline_ms: 5000
      max_age_ms: 8000
{min_runtime}    variants:
      - id: main
        backend_model: smolvlm
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 121000, p95_us: 156000, p99_us: 170000, samples: 100 }}
"
    )
}

const BUDGET: &str = "      min_runtime: { budget_ms: 300, window_ms: 1000 }\n";

#[test]
fn a_budget_reaches_the_contract() {
    let config = Config::from_yaml(&yaml(2, "best_effort", BUDGET)).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
    let resolved = config.resolve().unwrap();
    let vlm = resolved.model_index("vlm").unwrap();
    assert_eq!(
        resolved.contracts.get(vlm.get()).unwrap().min_runtime,
        Some(RuntimeBudget {
            budget: Duration::from_millis(300).unwrap(),
            window: Duration::from_millis(1_000).unwrap(),
        })
    );
    // 300 ‰ eines Slots, ueber zwei Slots 150 ‰.
    assert_eq!(resolved.runtime_budget_permille(), 150);
}

#[test]
fn without_a_budget_nothing_is_set() {
    let resolved = Config::from_yaml(&yaml(2, "best_effort", ""))
        .unwrap()
        .resolve()
        .unwrap();
    assert!(resolved.contracts.iter().all(|c| c.min_runtime.is_none()));
    assert_eq!(resolved.runtime_budget_permille(), 0);
}

#[test]
fn a_budget_on_a_guarded_class_is_refused() {
    for class in ["high", "protected"] {
        let findings = Config::from_yaml(&yaml(2, class, BUDGET))
            .unwrap()
            .diagnose();
        assert!(
            findings
                .iter()
                .any(|f| f.to_string().contains("min_runtime nur fuer normal")),
            "{class}: {findings:?}"
        );
    }
    assert!(
        Config::from_yaml(&yaml(2, "normal", BUDGET))
            .unwrap()
            .diagnose()
            .is_empty()
    );
}

#[test]
fn a_zero_window_is_refused() {
    let zero = "      min_runtime: { budget_ms: 300, window_ms: 0 }\n";
    let findings = Config::from_yaml(&yaml(2, "best_effort", zero))
        .unwrap()
        .diagnose();
    assert!(!findings.is_empty());
}

#[test]
fn a_budget_beyond_the_slots_is_refused_with_its_location() {
    let two_seconds = "      min_runtime: { budget_ms: 1500, window_ms: 1000 }\n";
    let findings = Config::from_yaml(&yaml(1, "best_effort", two_seconds))
        .unwrap()
        .diagnose();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].path, "models.vlm.contract.min_runtime");
    assert!(
        Config::from_yaml(&yaml(2, "best_effort", two_seconds))
            .unwrap()
            .diagnose()
            .is_empty()
    );
}

#[test]
fn an_unknown_field_in_the_budget_is_refused() {
    let typo = "      min_runtime: { budget_ms: 300, windows_ms: 1000 }\n";
    assert!(Config::from_yaml(&yaml(2, "best_effort", typo)).is_err());
}

/// Die Demo-Konfiguration traegt das Budget, mit dem sie aufgenommen wurde:
/// 300 ms je Sekunde fuer das Sprachmodell, und sonst keines
/// (docs/benchmark/demo-2026-09-15.md, Abschnitt "With a runtime budget").
#[test]
fn the_budget_in_the_demo_resolves() {
    let budgeted = "    contract: { deadline_ms: 5000, max_age_ms: 8000, \
                     min_runtime: { budget_ms: 300, window_ms: 1000 } }\n";
    assert!(DEMO.contains(budgeted), "Budgetzeile fehlt in der Demo");
    let config = Config::from_yaml(DEMO).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
    let resolved = config.resolve().unwrap();
    let vlm = resolved.model_index("vlm").unwrap();
    assert!(
        resolved
            .contracts
            .get(vlm.get())
            .unwrap()
            .min_runtime
            .is_some(),
        "das Sprachmodell der Demo hat kein Budget"
    );
    // Nur das Sprachmodell: die Kameras bezahlen es, sie bekommen keines.
    let with_budget = resolved
        .contracts
        .iter()
        .filter(|c| c.min_runtime.is_some())
        .count();
    assert_eq!(with_budget, 1, "nur das Sprachmodell traegt ein Budget");
}
