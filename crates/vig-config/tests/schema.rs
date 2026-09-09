//! Konfigurationsvalidierung (Spec 7.1 L-020, Spec 23).

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use vig_config::Config;

const EXAMPLE: &str = include_str!("../../../examples/detector_plus_vlm/vig.yaml");

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
            .any(|f| f.to_string().contains("vig profile")),
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

/// G-010: der Fingerabdruck muss aus der Datei bis in `Resolved` durchkommen —
/// sonst kann `serve` beim Start nichts vergleichen.
#[test]
fn the_profile_fingerprint_survives_resolution() {
    let yaml = r#"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:8001
  slots: 1
  pipelining_depth: 0
  safety_margin_percent: 110
models:
  detector:
    class: protected
    queue: { policy: latest, capacity: 1 }
    contract: { period_ms: 33, deadline_ms: 33, max_age_ms: 66 }
    variants:
      - id: main
        backend_model: rfdetr
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 1000, p95_us: 1200, p99_us: 1400, samples: 120, fingerprint: "abc123" }
  other:
    class: high
    queue: { policy: latest, capacity: 1 }
    contract: { period_ms: 50, deadline_ms: 50, max_age_ms: 100 }
    variants:
      - id: main
        backend_model: pose_main
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 500, p95_us: 600, p99_us: 700, samples: 120 }
"#;
    let resolved = Config::from_yaml(yaml).unwrap().resolve().unwrap();
    let detector = resolved.model_index("detector").unwrap();
    let other = resolved.model_index("other").unwrap();

    assert_eq!(
        resolved.profile_fingerprints[detector.get()][0].as_deref(),
        Some("abc123")
    );
    // Ohne Fingerabdruck bleibt es `None`. Das wird spaeter als "nicht
    // pruefbar" gemeldet und nicht als Abweichung — ein Profil aus der Zeit
    // vor G-010 darf nicht stillschweigend die Marge erhoehen (ADR-0016).
    assert_eq!(resolved.profile_fingerprints[other.get()][0], None);
}

/// WP12: Nebenlastprofile muessen bis in das Stufenprofil des Kerns kommen.
///
/// `VariantProfile::from_levels` existierte seit ADR-0004 und war unbenutzt,
/// weil die Konfiguration nur Alleinbetrieb ausdruecken konnte. Dieser Test
/// haelt fest, dass der Weg jetzt offen ist.
#[test]
fn profiles_under_load_reach_the_planner() {
    let yaml = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:8001
  slots: 2
  pipelining_depth: 0
  safety_margin_percent: 100
models:
  detector:
    class: protected
    queue: { policy: latest, capacity: 1 }
    contract: { period_ms: 100, deadline_ms: 100, max_age_ms: 200 }
    variants:
      - id: main
        backend_model: rfdetr
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 10000, p95_us: 11000, p99_us: 12000, samples: 120 }
        under_load:
          - { p50_us: 20000, p95_us: 21000, p99_us: 22000, samples: 120 }
";
    let resolved = Config::from_yaml(yaml).unwrap().resolve().unwrap();
    let detector = resolved.model_index("detector").unwrap();
    let contract = resolved.contracts.get(detector.get()).unwrap();
    let profile = &contract.variants.get(0).unwrap().profile;

    // Belegungsgrad 0 ist der Alleinbetrieb, 1 die gemessene Nebenlast.
    let solo = profile.at_occupancy(0).unwrap();
    let busy = profile.at_occupancy(1).unwrap();
    assert_eq!(solo.p99.as_nanos(), 12_000_000);
    assert_eq!(
        busy.p99.as_nanos(),
        22_000_000,
        "unter Nebenlast muss das gemessene Profil gelten, nicht das Alleinprofil"
    );
}

/// Ohne Nebenlastmessungen bleibt es beim Alleinprofil — das bisherige
/// Verhalten darf sich nicht stillschweigend aendern.
#[test]
fn without_load_profiles_the_solo_profile_still_applies() {
    let yaml = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:8001
  slots: 2
  pipelining_depth: 0
  safety_margin_percent: 100
models:
  detector:
    class: protected
    queue: { policy: latest, capacity: 1 }
    contract: { period_ms: 100, deadline_ms: 100, max_age_ms: 200 }
    variants:
      - id: main
        backend_model: rfdetr
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 10000, p95_us: 11000, p99_us: 12000, samples: 120 }
";
    let resolved = Config::from_yaml(yaml).unwrap().resolve().unwrap();
    let detector = resolved.model_index("detector").unwrap();
    let contract = resolved.contracts.get(detector.get()).unwrap();
    let profile = &contract.variants.get(0).unwrap().profile;
    assert_eq!(profile.at_occupancy(0).unwrap().p99.as_nanos(), 12_000_000);
}

// ---------------------------------------------------------------------------
// Codereview vom 07.09.2026: eine angenommene Konfiguration muss haltbar sein
// ---------------------------------------------------------------------------

fn minimal(extra_contract: &str, extra_model: &str) -> String {
    format!(
        r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:8001
  slots: 1
  pipelining_depth: 0
models:
  detector:
    class: protected
    queue: {{ policy: fifo, capacity: 64 }}
    contract: {{ deadline_ms: 100{extra_contract} }}
{extra_model}    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 1000 }}
"
    )
}

/// Ein nicht darstellbares `max_age_ms` wird abgelehnt, nicht abgeschaltet.
///
/// Frueher verschwand der Fehler in einem `.ok()`, und `max_age` wurde `None`
/// — also „dieser Strom altert nie". Genau die Regel, die das Produkt
/// ausmacht, war damit durch einen Tippfehler abschaltbar, ohne eine Meldung.
#[test]
fn an_out_of_range_max_age_is_rejected_instead_of_silently_disabled() {
    let yaml = minimal(", max_age_ms: 18446744073709551615", "");
    let err = Config::from_yaml(&yaml).unwrap().resolve().unwrap_err();
    assert!(
        format!("{err:?}").contains("max_age_ms"),
        "der Befund nennt das Feld: {err:?}"
    );
}

/// Dasselbe fuer `period_ms`.
#[test]
fn an_out_of_range_period_is_rejected_instead_of_silently_disabled() {
    let yaml = minimal(", period_ms: 18446744073709551615", "");
    assert!(Config::from_yaml(&yaml).unwrap().resolve().is_err());
}

/// Ein leeres kooperatives Tokenintervall verhindert den Start.
///
/// `min_tokens > max_total_tokens` erreichte frueher erst im Dispatch ein
/// `u32::clamp` und panickte dort — unter `panic = "abort"` das Ende des
/// Prozesses. Eine Konfiguration, die den Governor abschiessen kann, darf
/// nicht angenommen werden.
#[test]
fn an_empty_cooperative_token_range_prevents_startup() {
    let yaml = minimal(
        "",
        "    cooperative: { tokens_per_second: 1000, min_tokens: 8, max_total_tokens: 1, base_cost_us: 1000 }\n",
    );
    assert!(Config::from_yaml(&yaml).unwrap().resolve().is_err());

    // Gegenprobe: dasselbe Modell mit sinnvollem Intervall laeuft an.
    let ok = minimal(
        "",
        "    cooperative: { tokens_per_second: 1000, min_tokens: 1, max_total_tokens: 8, base_cost_us: 1000 }\n",
    );
    assert!(Config::from_yaml(&ok).unwrap().resolve().is_ok());
}

// ---------------------------------------------------------------------------
// Zugesagte Schnittstelle je logischem Modell
// ---------------------------------------------------------------------------

/// Eine hinterlegte `io_signature` wird gelesen und normalisiert.
///
/// Der Governor wählt die Variante je Request und sagt es dem Client nicht.
/// Gleiche Namen, Typen und Formen sind für Austauschbarkeit notwendig, aber
/// nicht hinreichend: zwei Detektoren können beide `boxes: FP32[-1,4]` liefern
/// und verschiedene Koordinatensysteme meinen. Diese Lücke schließt keine
/// Messung, nur eine Aussage des Betreibers — und die steht hier.
#[test]
fn a_declared_io_signature_is_normalised_for_comparison() {
    let yaml = r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:8001
  slots: 1
  pipelining_depth: 0
models:
  detector:
    class: protected
    queue: { policy: latest, capacity: 1 }
    contract: { deadline_ms: 33 }
    io_signature:
      inputs:
        - { name: images, datatype: FP32, dims: [1, 3, 512, 512] }
      outputs:
        - { name: scores, datatype: FP32, dims: [-1] }
        - { name: boxes,  datatype: FP32, dims: [-1, 4] }
    variants:
      - id: main
        backend_model: rfdetr
        quality: { value: 1.0, source: measured }
        profile: { p50_us: 15000, p95_us: 17000, p99_us: 17500, samples: 120 }
";
    let resolved = Config::from_yaml(yaml).unwrap().resolve().unwrap();
    let signature = resolved.io_signatures[0].as_ref().unwrap();
    let (inputs, outputs) = signature.normalised();

    assert_eq!(inputs, vec!["images:FP32[1,3,512,512]"]);
    // Sortiert und mit `?` für dynamische Achsen — dieselbe Normalform, die
    // aus den Backendmetadaten entsteht. Zwei Darstellungen desselben
    // Sachverhalts zu vergleichen wäre der Fehler.
    assert_eq!(outputs, vec!["boxes:FP32[?,4]", "scores:FP32[?]"]);
}

/// Ohne Angabe bleibt sie leer — und der schwächere Vergleich der Varianten
/// untereinander greift.
#[test]
fn a_model_without_a_declared_signature_has_none() {
    let resolved = Config::from_yaml(EXAMPLE).unwrap().resolve().unwrap();
    assert!(resolved.io_signatures.iter().all(Option::is_none));
}

/// Ein halb konfiguriertes TLS sieht nach Schutz aus und ist keiner.
#[test]
fn half_configured_tls_is_rejected() {
    let with = |security: &str| {
        format!(
            r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:8001
  slots: 1
  pipelining_depth: 0
  security:
{security}
models:
  detector:
    class: protected
    queue: {{ policy: fifo, capacity: 4 }}
    contract: {{ deadline_ms: 33 }}
    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 15000, p95_us: 17000, p99_us: 17500, samples: 120 }}
"
        )
    };

    // Zertifikat ohne Schlüssel.
    assert!(
        Config::from_yaml(&with("    tls_cert: /nonexistent/cert.pem"))
            .unwrap()
            .resolve()
            .is_err()
    );
    // Clientzertifikate ohne TLS gibt es nicht.
    assert!(
        Config::from_yaml(&with("    client_ca: /nonexistent/ca.pem"))
            .unwrap()
            .resolve()
            .is_err()
    );
}

// ---------------------------------------------------------------------------
// NV-03 — Profilmanifest v2
// ---------------------------------------------------------------------------

/// Ein Profil aus der Zeit vor NV-03: nur Quantile, kein Fingerabdruck, kein
/// Manifest. Es muss unveraendert nutzbar bleiben.
const LEGACY_V0: &str = include_str!("golden/profile-legacy-v0.yaml");
/// Ein Profil aus der G-010-Zeit: mit Fingerabdruck, ohne Manifest.
const LEGACY_V1: &str = include_str!("golden/profile-legacy-v1.yaml");
/// Ein Profil mit vollstaendigem Manifest.
const MANIFEST_V2: &str = include_str!("golden/profile-manifest-v2.yaml");

#[test]
fn a_profile_from_before_fingerprints_still_resolves() {
    let resolved = Config::from_yaml(LEGACY_V0).unwrap().resolve().unwrap();
    let detector = resolved.model_index("detector").unwrap();
    assert_eq!(resolved.profile_fingerprints[detector.get()][0], None);
    let manifest = resolved.profile_manifests[detector.get()][0]
        .as_ref()
        .unwrap();
    assert!(
        manifest.is_legacy(),
        "ein Profil ohne Manifestblock ist ein Legacy-Profil"
    );
    assert!(manifest.is_empty());
}

#[test]
fn a_profile_with_only_a_fingerprint_is_still_legacy() {
    let resolved = Config::from_yaml(LEGACY_V1).unwrap().resolve().unwrap();
    let detector = resolved.model_index("detector").unwrap();
    assert_eq!(
        resolved.profile_fingerprints[detector.get()][0].as_deref(),
        Some("00112233445566aa")
    );
    let manifest = resolved.profile_manifests[detector.get()][0]
        .as_ref()
        .unwrap();
    assert!(
        manifest.is_legacy(),
        "ein Hash ist keine Herkunftsangabe; er laesst sich nicht zurueckrechnen"
    );
}

#[test]
fn a_manifest_reaches_the_resolved_configuration() {
    let resolved = Config::from_yaml(MANIFEST_V2).unwrap().resolve().unwrap();
    let detector = resolved.model_index("detector").unwrap();
    let manifest = resolved.profile_manifests[detector.get()][0]
        .as_ref()
        .unwrap();
    assert!(!manifest.is_legacy());
    assert_eq!(
        manifest.artifact.digest.as_deref(),
        Some("sha256:8f1c0e4b2a7d6f3e5c9b0a1d2e3f405162738495a6b7c8d9e0f1a2b3c4d5e6f70")
    );
    assert_eq!(manifest.device.name.as_deref(), Some("NVIDIA RTX A2000"));
    assert_eq!(manifest.resources.instances, Some(1));
    assert_eq!(manifest.measurement.independent_runs, Some(3));
    assert_eq!(manifest.validity.max_occupancy_pct, Some(92));
    assert_eq!(resolved.model_repository.as_deref(), Some("/models"));
}

#[test]
fn a_manifest_survives_the_full_configuration_roundtrip() {
    // Nicht nur der Manifestblock fuer sich, sondern eingebettet in eine
    // vollstaendige Konfiguration: `vig calibrate` schreibt genau so.
    let before = Config::from_yaml(MANIFEST_V2).unwrap();
    let text = serde_norway::to_string(&before).unwrap();
    let after = Config::from_yaml(&text).unwrap();
    let a = before.models["detector"].variants[0]
        .profile
        .as_ref()
        .unwrap()
        .manifest
        .as_ref()
        .unwrap();
    let b = after.models["detector"].variants[0]
        .profile
        .as_ref()
        .unwrap()
        .manifest
        .as_ref()
        .unwrap();
    assert_eq!(a, b);
}

#[test]
fn an_unknown_manifest_key_is_rejected() {
    // Ein Tippfehler in einem Herkunftsfeld darf nicht als `unknown`
    // durchgehen — sonst waere das Feld still leer, und still leer ist genau
    // das, was NV-03 verhindern soll.
    let broken = MANIFEST_V2.replace("compute_capability:", "compute_capabilty:");
    assert!(Config::from_yaml(&broken).is_err());
}

// ---------------------------------------------------------------------------
// NV-02 — Vertragszusatz
// ---------------------------------------------------------------------------

/// Ein Vertrag mit vollstaendigem Zusatz.
const EXTENSION_V1: &str = include_str!("golden/contract-extension-v1.yaml");

#[test]
fn a_contract_without_an_extension_still_resolves() {
    // Die Abnahme, die alles andere traegt: alte Dateien bleiben nutzbar.
    let resolved = Config::from_yaml(EXAMPLE).unwrap().resolve().unwrap();
    for contract in resolved.contracts.iter() {
        assert!(contract.extension.is_none());
    }
}

#[test]
fn a_full_extension_reaches_the_contract() {
    let resolved = Config::from_yaml(EXTENSION_V1).unwrap().resolve().unwrap();
    let detector = resolved.model_index("detector").unwrap();
    let ext = resolved
        .contracts
        .get(detector.get())
        .unwrap()
        .extension
        .unwrap();

    assert_eq!(
        ext.version,
        vig_core::contract_ext::CONTRACT_EXTENSION_VERSION
    );
    assert_eq!(ext.consumer_period.unwrap().as_millis(), 33);
    assert_eq!(
        ext.delivery_boundary,
        vig_core::contract_ext::DeliveryBoundary::Consumer
    );
    assert_eq!(
        ext.evidence_required,
        vig_core::contract_ext::EvidenceLevel::QualifiedSlo
    );
    assert_eq!(ext.contract_version, 4);
    let budget = ext.miss_budget.unwrap();
    assert_eq!(budget.max_misses, 2);
    assert_eq!(budget.window_cycles, 100);
    assert_eq!(budget.max_consecutive, Some(1));
    assert_eq!(ext.validity_envelope.max_occupancy_pct, Some(92));
    assert_eq!(ext.minimum_background_progress_pct, Some(5));
}

#[test]
fn approved_variants_are_resolved_by_name() {
    let resolved = Config::from_yaml(EXTENSION_V1).unwrap().resolve().unwrap();
    let detector = resolved.model_index("detector").unwrap();
    let contract = resolved.contracts.get(detector.get()).unwrap();
    assert!(contract.variant_approved(vig_core::ids::VariantIdx(0)));
    assert!(
        !contract.variant_approved(vig_core::ids::VariantIdx(1)),
        "small ist nicht freigegeben"
    );
    assert!(
        contract.meets_min_quality(vig_core::ids::VariantIdx(1)),
        "qualitativ waere sie zulaessig — das ist der Punkt"
    );
    assert!(!contract.variant_usable(vig_core::ids::VariantIdx(1)));
}

#[test]
fn an_unknown_variant_name_in_the_approval_list_is_rejected() {
    // Ein Tippfehler darf keine stille Vollsperrung sein.
    let text = EXTENSION_V1.replace("approved_variants: [large]", "approved_variants: [larg]");
    let finding = Config::from_yaml(&text).unwrap().resolve().unwrap_err();
    assert!(
        format!("{finding:?}").contains("approved_variants"),
        "{finding:?}"
    );
}

#[test]
fn an_unknown_extension_version_is_rejected() {
    let text = EXTENSION_V1.replace("        version: 1\n", "        version: 7\n");
    assert!(Config::from_yaml(&text).unwrap().resolve().is_err());
}

#[test]
fn an_unknown_delivery_semantics_is_rejected() {
    let text = EXTENSION_V1.replace(
        "delivery_semantics: latest_state",
        "delivery_semantics: newest",
    );
    assert!(Config::from_yaml(&text).unwrap().resolve().is_err());
}

#[test]
fn an_unknown_evidence_level_is_rejected() {
    let text = EXTENSION_V1.replace(
        "evidence_required: qualified_slo",
        "evidence_required: certain",
    );
    assert!(Config::from_yaml(&text).unwrap().resolve().is_err());
}

#[test]
fn an_invalid_miss_budget_is_rejected() {
    // M >= K erlaubt jeden Zyklus als Miss.
    let text = EXTENSION_V1.replace("max_misses: 2", "max_misses: 100");
    assert!(Config::from_yaml(&text).unwrap().resolve().is_err());
    // L > M beschreibt eine Regel, die nie greift.
    let text = EXTENSION_V1.replace("max_consecutive: 1", "max_consecutive: 3");
    assert!(Config::from_yaml(&text).unwrap().resolve().is_err());
    // K = 0 zaehlt nichts.
    let text = EXTENSION_V1.replace("window_cycles: 100", "window_cycles: 0");
    assert!(Config::from_yaml(&text).unwrap().resolve().is_err());
}

#[test]
fn a_miss_budget_without_a_consumer_period_is_rejected() {
    let text = EXTENSION_V1.replace("        consumer_period_ms: 33\n", "");
    assert!(
        Config::from_yaml(&text).unwrap().resolve().is_err(),
        "ohne Verbrauchertakt gibt es keinen Zyklus, ueber den gezaehlt wird"
    );
}

#[test]
fn an_unknown_extension_key_is_rejected() {
    // Ein Tippfehler in einem Vertragsfeld darf nicht still verschwinden:
    // er koennte genau die Einschraenkung sein, auf die sich jemand verlaesst.
    let text = EXTENSION_V1.replace("        phase_ms: 0\n", "        phase_millis: 0\n");
    assert!(Config::from_yaml(&text).is_err());
}

#[test]
fn an_extension_survives_a_configuration_roundtrip() {
    let before = Config::from_yaml(EXTENSION_V1).unwrap();
    let text = serde_norway::to_string(&before).unwrap();
    let after = Config::from_yaml(&text).unwrap();
    let a = before.models["detector"]
        .contract
        .extension
        .as_ref()
        .unwrap();
    let b = after.models["detector"]
        .contract
        .extension
        .as_ref()
        .unwrap();
    assert_eq!(a.miss_budget.as_ref().unwrap().max_consecutive, Some(1));
    assert_eq!(a.approved_variants, b.approved_variants);
    assert_eq!(a.evidence_required, b.evidence_required);
    assert_eq!(a.contract_version, b.contract_version);
}
