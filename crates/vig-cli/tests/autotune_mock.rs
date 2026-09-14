//! `vig autotune` gegen ein Mock-Backend, ohne GPU.
//!
//! Die Ehrlichkeitsregeln einzeln pruefen die Unittests in `autotune.rs`.
//! Hier laeuft das **gebaute Binary** durch: echter Prozess, echtes gRPC,
//! echte Dateien auf der Platte. Geprueft wird die eine Eigenschaft, an der
//! alles haengt — dass dieser Befehl unter keinen Umstaenden eine
//! Qualifikation *erteilt* und jeden Schritt, der nicht durchlief, mit Grund
//! ausweist.
//!
//! Das Mock-Backend ist dasselbe wie in den Gateway-Tests. Es zweimal zu
//! schreiben hiesse, zwei Backends gegeneinander driften zu lassen.

// Ein Test darf am Index scheitern: `report["steps"]` ist hier die Zusicherung,
// dass das Feld existiert. In der Bibliothek waere das ein Absturz, im Test ist
// es die Aussage.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing
)]

#[path = "../../vig-gateway/tests/mock_backend.rs"]
mod mock_backend;

use std::time::Duration;

/// Eine Konfiguration mit ausgefuellten Vertraegen — `autotune` misst, es
/// erfindet keine.
fn config_for(endpoint: &str) -> String {
    format!(
        "version: 1
backend:
  type: triton
  grpc_endpoint: \"{endpoint}\"
  slots: 1
models:
  detector:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 33, deadline_ms: 33, max_age_ms: 66 }}
    variants:
      - id: main
        backend_model: detector
        quality: {{ value: 1.0, source: measured }}
"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn autotune_against_a_mock_backend_never_issues_a_release() {
    let backend = std::sync::Arc::new(mock_backend::MockBackend::new(Duration::from_millis(2)));
    let address = mock_backend::start(backend).await;
    let endpoint = address.to_string();

    let dir = std::env::temp_dir().join(format!("vig-autotune-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("vig.yaml");
    std::fs::write(&config, config_for(&endpoint)).unwrap();
    let out = dir.join("qualification");

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_vig"))
        .arg("autotune")
        .args(["--endpoint", &endpoint])
        .arg("-c")
        .arg(&config)
        .arg("-o")
        .arg(&out)
        .args(["--quick", "--samples", "12"])
        // `vig-fit` gehoert zum Messkasten und liegt beim Test daneben im
        // Zielverzeichnis. Hier soll es *nicht* laufen: der Test prueft den
        // Ablauf, nicht einen zweiminuetigen Lastlauf. Der Befund „Werkzeug
        // fehlt" ist zugleich der Fall, den ein Anwender ohne vig-bench hat.
        .env("VIG_FIT_BIN", dir.join("kein-vig-fit"))
        .output()
        .expect("das gebaute vig-Binary muss startbar sein");

    let report_json = out.join("qualification.json");
    let report_md = out.join("qualification.md");
    assert!(
        report_json.is_file() && report_md.is_file(),
        "beide Fassungen des Berichts muessen auf der Platte liegen, auch wenn der Lauf \
         unterwegs abbricht; stdout war:\n{}\n{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );

    let text = std::fs::read_to_string(&report_json).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();

    // Der Kern: erteilt wird nie etwas.
    let release = report["release"].as_str().unwrap();
    assert!(
        release == "refused" || release == "not issued",
        "autotune darf keine Qualifikation erteilen, sagte aber {release:?}"
    );

    // Jeder Schritt, der nicht sauber durchlief, traegt einen Grund.
    let steps = report["steps"].as_array().unwrap();
    assert!(
        !steps.is_empty(),
        "mindestens ein Schritt muss gelaufen sein"
    );
    for step in steps {
        let outcome = step["outcome"].as_str().unwrap();
        if outcome != "done" {
            assert!(
                step["reason"].as_str().is_some_and(|r| !r.is_empty()),
                "Schritt {step:?} ist nicht durchgelaufen und nennt keinen Grund"
            );
        }
    }

    // Das fehlende `vig-fit` ist ein offener Punkt, kein stiller Erfolg.
    if let Some(fit) = steps.iter().find(|s| s["step"] == "fit") {
        assert_eq!(fit["outcome"], "skipped");
        assert!(fit["reason"].as_str().unwrap().contains("vig-fit"));
    }

    // Der Bericht sagt in jedem Fall, was er nicht sagt.
    let markdown = std::fs::read_to_string(&report_md).unwrap();
    assert!(markdown.contains("What this report does not say"));
    assert!(markdown.contains("It is not a release."));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn open_contracts_stop_the_run_instead_of_being_invented() {
    let backend = std::sync::Arc::new(mock_backend::MockBackend::new(Duration::from_millis(1)));
    let address = mock_backend::start(backend).await;
    let endpoint = address.to_string();

    let dir = std::env::temp_dir().join(format!("vig-autotune-todo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("vig.yaml");
    // Eine Vorlage, wie `vig init` sie hinterlaesst: die Hardware steht drin,
    // der Vertrag nicht.
    std::fs::write(
        &config,
        config_for(&endpoint).replace("period_ms: 33", "period_ms: TODO_PERIOD_MS"),
    )
    .unwrap();
    let out = dir.join("qualification");

    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_vig"))
        .arg("autotune")
        .args(["--endpoint", &endpoint])
        .arg("-c")
        .arg(&config)
        .arg("-o")
        .arg(&out)
        .args(["--quick", "--only", "discover"])
        .output()
        .expect("das gebaute vig-Binary muss startbar sein");

    let text = std::fs::read_to_string(out.join("qualification.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    let steps = report["steps"].as_array().unwrap();
    let discover = steps.first().expect("der erste Schritt muss dastehen");
    assert_eq!(discover["outcome"], "failed");
    let reason = discover["reason"].as_str().unwrap();
    assert!(
        reason.contains("TODO_PERIOD_MS"),
        "der Bericht muss den offenen Vertrag benennen: {reason}"
    );
    assert!(
        reason.contains("no measurement can find it out"),
        "und sagen, warum ihn kein Messwerkzeug ausfuellen kann: {reason}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
