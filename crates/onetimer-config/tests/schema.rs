//! Konfigurationsvalidierung (Spec 7.1 L-020, Spec 23).

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use onetimer_config::Config;

const EXAMPLE: &str = include_str!("../../../examples/detector_plus_vlm/onetimer.yaml");

#[test]
fn the_shipped_example_resolves() {
    let config = Config::from_yaml(EXAMPLE).unwrap();
    let findings = config.diagnose();
    assert!(
        findings.is_empty(),
        "Beispielkonfiguration hat Befunde: {findings:?}"
    );

    let resolved = config.resolve().unwrap();
    assert_eq!(resolved.model_names, vec!["detector", "vlm"]);
    assert_eq!(resolved.contracts.len(), 2);
    assert_eq!(resolved.slots.len(), 1);
    assert_eq!(resolved.backend_endpoint, "127.0.0.1:8001");

    let detector = resolved.model_index("detector").unwrap();
    assert_eq!(resolved.backend_model(detector, 0), Some("detector_large"));
    assert_eq!(resolved.backend_model(detector, 1), Some("detector_small"));
}

/// Spec 6.1: `no_corun` verweist auf logische Modellnamen; ein Tippfehler darf
/// nicht dazu fuehren, dass das Verbot einfach wirkungslos bleibt.
#[test]
fn a_corun_rule_pointing_at_nothing_is_reported() {
    let text = EXAMPLE.replace("- [detector, vlm]", "- [detektor, vlm]");
    let findings = Config::from_yaml(&text).unwrap().diagnose();
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0].to_string().contains("detektor"),
        "{}",
        findings[0]
    );
}

/// Ein vertippter Schluessel wuerde sonst ignoriert, und der Nutzer glaubte,
/// er haette etwas konfiguriert.
#[test]
fn an_unknown_key_is_rejected() {
    let text = EXAMPLE.replace("max_age_ms: 66", "max_age: 66");
    let err = Config::from_yaml(&text).unwrap_err();
    assert!(err.to_string().contains("max_age"), "{err}");
}

#[test]
fn a_missing_runtime_profile_stops_the_start() {
    let text = EXAMPLE.replace(
        "        profile: { p50_us: 10000, p95_us: 13000, p99_us: 16000, samples: 2000 }\n",
        "",
    );
    let findings = Config::from_yaml(&text).unwrap().diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.to_string().contains("onetimer profile")),
        "der Befund muss sagen, was zu tun ist: {findings:?}"
    );
}

/// Spec G-011: ein stateful Modell darf keine supersedierende Policy haben.
#[test]
fn a_stateful_model_with_latest_is_rejected() {
    let text = EXAMPLE.replace(
        "  detector:\n    class: protected",
        "  detector:\n    stateful: true\n    class: protected",
    );
    let findings = Config::from_yaml(&text).unwrap().diagnose();
    assert!(
        findings.iter().any(|f| f.to_string().contains("stateful")),
        "{findings:?}"
    );
}

/// ADR-0007: ohne Qualitaetsherkunft keine automatische Variantenwahl.
#[test]
fn unknown_quality_provenance_disables_auto_selection() {
    let text = EXAMPLE.replace("source: measured", "source: unknown");
    let config = Config::from_yaml(&text).unwrap();
    assert!(
        config.diagnose().is_empty(),
        "unknown ist zulaessig, nicht fehlerhaft"
    );
    let resolved = config.resolve().unwrap();
    assert!(!resolved.contracts.get(0).unwrap().auto_variant_selection());
}

#[test]
fn diagnose_reports_every_problem_at_once() {
    let text = EXAMPLE
        .replace("version: 1", "version: 7")
        .replace("class: protected", "class: kritisch")
        .replace("policy: latest ", "policy: newest ");
    let findings = Config::from_yaml(&text).unwrap().diagnose();
    let joined = findings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        findings.len() >= 3,
        "doctor soll alle Befunde auf einmal zeigen, waren {}: {joined}",
        findings.len()
    );
    assert!(joined.contains("version"), "{joined}");
    assert!(joined.contains("kritisch"), "{joined}");
    assert!(joined.contains("newest"), "{joined}");
}

#[test]
fn out_of_range_values_are_rejected_not_clamped() {
    let text = EXAMPLE.replace("value: 0.93", "value: 1.7");
    let findings = Config::from_yaml(&text).unwrap().diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.to_string().contains("quality.value")),
        "{findings:?}"
    );
}
