//! Ressourcendomaenen in der Konfiguration (NV-22, ADR-0037).
//!
//! Was hier geprueft wird, ist die Zusage „kein Doppelbesitz": jede GPU hat
//! genau einen Kapazitaetsbesitzer, jeder Endpunkt gehoert genau einer
//! Domaene, und eine Konfiguration ohne Domaenen loest sich auf wie vorher.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use vig_config::Config;
use vig_config::error::ConfigError;
use vig_core::ModelIdx;

const EXAMPLE: &str = include_str!("../../../examples/domains/vig.yaml");
const SINGLE: &str = include_str!("../../../examples/detector_plus_vlm/vig.yaml");

/// Ein Modell im YAML-Format, mit frei waehlbarem Zusatz (`domain:` usw.).
fn model(name: &str, class: &str, extra: &str, p99_us: u64) -> String {
    format!(
        r"
  {name}:
    class: {class}
{extra}
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 100, deadline_ms: 100, max_age_ms: 200 }}
    variants:
      - id: main
        backend_model: {name}_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: {p99_us}, p95_us: {p99_us}, p99_us: {p99_us}, samples: 1000 }}"
    )
}

/// Ein `backend`-Block mit einer Domaene `gpu1` und den gegebenen Modellen.
fn with_gpu1(domain: &str, models: &[String]) -> Config {
    let yaml = format!(
        r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:9001"
  slots: 1
  domains:
    gpu1:
{domain}
models:{}
"#,
        models.concat()
    );
    Config::from_yaml(&yaml).unwrap()
}

const GPU1: &str = r#"      gpu_index: 1
      grpc_endpoint: "127.0.0.1:9101"
      slots: 2"#;

fn paths(config: &Config) -> Vec<String> {
    config.diagnose().into_iter().map(|f| f.path).collect()
}

#[test]
fn the_shipped_example_resolves_into_two_schedulers() {
    let config = Config::from_yaml(EXAMPLE).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
    let resolved = config.resolve().unwrap();

    // Global: alle Modelle, in Namensreihenfolge.
    assert_eq!(resolved.model_names, ["depth", "detector", "vlm"]);
    let names: Vec<&str> = resolved.domains.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["default", "gpu1"]);

    let default = &resolved.domains[0];
    assert_eq!(default.gpu_index, 0);
    assert_eq!(default.models, [ModelIdx(1)]);
    assert_eq!(default.resolved.model_names, ["detector"]);
    assert_eq!(default.resolved.backend_endpoint, "127.0.0.1:8001");

    let gpu1 = &resolved.domains[1];
    assert_eq!(gpu1.gpu_index, 1);
    assert_eq!(gpu1.models, [ModelIdx(0), ModelIdx(2)]);
    assert_eq!(gpu1.resolved.model_names, ["depth", "vlm"]);
    assert_eq!(gpu1.resolved.backend_endpoint, "127.0.0.1:8101");
    assert_eq!(gpu1.local(ModelIdx(2)), Some(ModelIdx(1)));
    assert_eq!(
        gpu1.local(ModelIdx(1)),
        None,
        "der Detektor gehoert nicht hierher"
    );

    // Ohne eigenen Endpunkt laeuft ein Modell am Endpunkt seiner Domaene.
    assert_eq!(resolved.endpoint_of(ModelIdx(2)), "127.0.0.1:8101");
    assert_eq!(resolved.endpoint_of(ModelIdx(1)), "127.0.0.1:8001");
}

/// Ohne `domains:` aendert sich nichts — auch nicht beim Zurueckschreiben:
/// `vig calibrate` schreibt eine Konfiguration ohne Domaenen so, wie sie war.
#[test]
fn without_domains_nothing_changes() {
    let config = Config::from_yaml(SINGLE).unwrap();
    let resolved = config.resolve().unwrap();
    assert!(resolved.domains.is_empty());
    assert_eq!(resolved.slots.len(), 2);
    let yaml = config.to_yaml().unwrap();
    assert!(!yaml.contains("domain"), "{yaml}");
}

#[test]
fn a_domain_round_trips_through_yaml() {
    let config = Config::from_yaml(EXAMPLE).unwrap();
    let again = Config::from_yaml(&config.to_yaml().unwrap()).unwrap();
    let resolved = again.resolve().unwrap();
    assert_eq!(resolved.domains.len(), 2);
    assert_eq!(resolved.domains[1].resolved.slots.len(), 2);
}

/// `domain: gpu1` ohne angelegte Domaene liefe sonst still auf GPU 0.
#[test]
fn a_model_naming_an_unknown_domain_is_refused() {
    let single = format!(
        "version: 1\nbackend:\n  type: triton\n  grpc_endpoint: \"127.0.0.1:9001\"\n  slots: 1\nmodels:{}",
        model("vlm", "best_effort", "    domain: gpu1", 90_000)
    );
    let findings = Config::from_yaml(&single).unwrap().diagnose();
    assert!(
        findings.iter().any(|f| f.path == "models.vlm.domain"
            && matches!(&f.error, ConfigError::UnknownDomain { name } if name == "gpu1")),
        "{findings:?}"
    );

    let typo = with_gpu1(
        GPU1,
        &[
            model("detector", "protected", "", 10_000),
            model("vlm", "best_effort", "    domain: gpu2", 90_000),
        ],
    );
    assert!(paths(&typo).contains(&"models.vlm.domain".to_owned()));
}

#[test]
fn a_domain_without_models_is_refused() {
    let config = with_gpu1(GPU1, &[model("detector", "protected", "", 10_000)]);
    assert!(paths(&config).contains(&"backend.domains.gpu1".to_owned()));
}

/// Zwei Server auf einer GPU sind keine zwei Recheneinheiten (ADR-0004).
#[test]
fn two_domains_on_one_gpu_are_refused() {
    let gpu0 = r#"      gpu_index: 0
      grpc_endpoint: "127.0.0.1:9101"
      slots: 1"#;
    let config = with_gpu1(
        gpu0,
        &[
            model("detector", "protected", "", 10_000),
            model("vlm", "best_effort", "    domain: gpu1", 90_000),
        ],
    );
    assert!(paths(&config).contains(&"backend.domains.gpu1.gpu_index".to_owned()));

    // Steht kein Modell im backend-Block, gibt es die Domaene `default` nicht,
    // und GPU 0 ist frei.
    let only_named = with_gpu1(
        gpu0,
        &[model("vlm", "best_effort", "    domain: gpu1", 90_000)],
    );
    assert!(
        only_named.diagnose().is_empty(),
        "{:?}",
        only_named.diagnose()
    );
    let resolved = only_named.resolve().unwrap();
    let names: Vec<&str> = resolved.domains.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["gpu1"]);
}

/// Zwei Scheduler, die Arbeit an denselben Server geben, sehen einander nicht.
#[test]
fn an_endpoint_belongs_to_one_domain() {
    let config = with_gpu1(
        GPU1,
        &[
            model("detector", "protected", "", 10_000),
            model(
                "vlm",
                "best_effort",
                "    domain: gpu1\n    backend_endpoint: \"127.0.0.1:9001\"",
                90_000,
            ),
        ],
    );
    assert!(paths(&config).contains(&"models.vlm.backend_endpoint".to_owned()));
}

/// Co-Run-Verbote und Interferenz gelten innerhalb einer GPU — und der
/// Befund sagt das, statt ein „unbekanntes Modell" zu melden.
#[test]
fn pairs_across_domains_are_refused_with_the_right_reason() {
    let yaml = format!(
        r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:9001"
  slots: 1
  no_corun:
    - [detector, vlm]
  domains:
    gpu1:
{GPU1}
models:{}{}
"#,
        model("detector", "protected", "", 10_000),
        model("vlm", "best_effort", "    domain: gpu1", 90_000),
    );
    let findings = Config::from_yaml(&yaml).unwrap().diagnose();
    let at_rule: Vec<_> = findings
        .iter()
        .filter(|f| f.path == "backend.no_corun[0]")
        .collect();
    assert_eq!(at_rule.len(), 1, "{findings:?}");
    assert!(matches!(at_rule[0].error, ConfigError::Inconsistent { .. }));
}

/// Ein Befund aus der Kapazitaet einer Domaene steht an der Domaene, nicht
/// am backend-Block.
#[test]
fn a_finding_inside_a_domain_names_the_domain() {
    let no_slots = r#"      gpu_index: 1
      grpc_endpoint: "127.0.0.1:9101"
      slots: 0"#;
    let config = with_gpu1(
        no_slots,
        &[
            model("detector", "protected", "", 10_000),
            model("vlm", "best_effort", "    domain: gpu1", 90_000),
        ],
    );
    let found = paths(&config);
    assert!(
        found.contains(&"backend.domains.gpu1.slots".to_owned()),
        "{found:?}"
    );
    assert!(!found.contains(&"backend.slots".to_owned()), "{found:?}");

    let preemptible_without_lane = with_gpu1(
        GPU1,
        &[
            model("detector", "protected", "", 10_000),
            model(
                "vlm",
                "best_effort",
                "    domain: gpu1\n    backend_endpoint: \"127.0.0.1:9102\"\n    preemptible: { residual_blocking_us: 14000, source: measured }",
                90_000,
            ),
        ],
    );
    assert!(
        paths(&preemptible_without_lane)
            .contains(&"backend.domains.gpu1.preemptible_lanes".to_owned())
    );
}

/// Eine Spur gehoert zur GPU, auf der das praemptierbare Modell laeuft.
#[test]
fn a_lane_lives_in_the_domain_of_its_model() {
    let lane = r#"      gpu_index: 1
      grpc_endpoint: "127.0.0.1:9101"
      slots: 1
      preemptible_lanes: 1"#;
    let config = with_gpu1(
        lane,
        &[
            model("detector", "protected", "", 10_000),
            model("depth", "high", "    domain: gpu1", 10_000),
            model(
                "vlm",
                "best_effort",
                "    domain: gpu1\n    backend_endpoint: \"127.0.0.1:9102\"\n    preemptible: { residual_blocking_us: 14000, source: measured }",
                90_000,
            ),
        ],
    );
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
    let resolved = config.resolve().unwrap();
    let gpu1 = &resolved.domains[1];
    assert_eq!(gpu1.resolved.slots.len(), 2);
    assert_eq!(gpu1.resolved.slots.regular_len(), 1);
    assert_eq!(
        resolved.domains[0].resolved.slots.len(),
        1,
        "GPU 0 hat keine Spur"
    );
    let vlm = resolved.model_index("vlm").unwrap();
    assert!(resolved.preemptible[vlm.get()].is_some());
}

#[test]
fn the_name_default_is_the_backend_block() {
    let explicit = format!(
        "version: 1\nbackend:\n  type: triton\n  grpc_endpoint: \"127.0.0.1:9001\"\n  slots: 1\nmodels:{}",
        model("detector", "protected", "    domain: default", 10_000)
    );
    let config = Config::from_yaml(&explicit).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());

    let reserved = format!(
        r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:9001"
  slots: 1
  domains:
    default:
{GPU1}
    "GPU 2":
      gpu_index: 2
      grpc_endpoint: "127.0.0.1:9201"
      slots: 1
models:{}
"#,
        model("detector", "protected", "", 10_000)
    );
    let found = paths(&Config::from_yaml(&reserved).unwrap());
    assert!(
        found.contains(&"backend.domains.default".to_owned()),
        "{found:?}"
    );
    assert!(
        found.contains(&"backend.domains.GPU 2".to_owned()),
        "{found:?}"
    );
}

/// Die Aufloesung jeder Domaene prueft ihre Modelle noch einmal; der Befund
/// steht trotzdem nur einmal da.
#[test]
fn findings_are_not_reported_twice() {
    let broken =
        model("vlm", "best_effort", "    domain: gpu1", 90_000).replace("value: 1.0", "value: 1.5");
    let config = with_gpu1(GPU1, &[model("detector", "protected", "", 10_000), broken]);
    let path = "models.vlm.variants[0].quality.value";
    assert_eq!(
        paths(&config).iter().filter(|p| *p == path).count(),
        1,
        "{:?}",
        paths(&config)
    );
}

/// Was `vig serve` beim Start an einem Vertrag aendert, muss auch der
/// Scheduler der Domaene sehen.
#[test]
fn pinning_a_variant_reaches_the_domain() {
    let mut resolved = Config::from_yaml(EXAMPLE).unwrap().resolve().unwrap();
    let vlm = resolved.model_index("vlm").unwrap();
    resolved.pin_best_variant(vlm);
    let interchangeable =
        |contracts: &vig_core::arrayvec::ArrayVec<
            vig_core::ModelContract,
            { vig_core::ids::MAX_MODELS },
        >,
         i: usize| { contracts.get(i).unwrap().variants_interchangeable };
    assert!(!interchangeable(&resolved.contracts, vlm.get()));
    let gpu1 = &resolved.domains[1];
    let local = gpu1.local(vlm).unwrap();
    assert!(!interchangeable(&gpu1.resolved.contracts, local.get()));
    assert!(
        interchangeable(&resolved.domains[0].resolved.contracts, 0),
        "die andere Domaene bleibt, wie sie war"
    );
}
