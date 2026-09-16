//! `contract.objective` in der Konfiguration (ADR-0047).
//!
//! Eine Zusage hat zwei Haelften: wie viel im Fenster ankommt, und wie lange
//! nie nichts ankommt. Der Fall, um den es geht, ist der des Betreibers:
//! „Front 98 %, Heck 20 % — aber mindestens einmal je Sekunde eines."

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use vig_config::Config;
use vig_core::Duration;
use vig_core::objective::Objective;

/// Zwei Kameras; die zweite bekommt ihre Vertragszeilen frei gewaehlt.
fn yaml_with(second_contract: &str) -> String {
    format!(
        "version: 1
backend:
  type: triton
  grpc_endpoint: \"127.0.0.1:9201\"
  slots: 2
models:
  front:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract:
      period_ms: 33
      deadline_ms: 66
      max_age_ms: 100
    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 17000, p95_us: 18000, p99_us: 19000, samples: 110 }}
  rear:
    class: normal
    queue: {{ policy: latest, capacity: 1 }}
    contract:
{second_contract}    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 17000, p95_us: 18000, p99_us: 19000, samples: 110 }}
"
    )
}

/// Zwei Kameras, die zweite mit frei waehlbarem Ziel.
fn yaml(second_objective: &str) -> String {
    format!(
        "version: 1
backend:
  type: triton
  grpc_endpoint: \"127.0.0.1:9201\"
  slots: 2
models:
  front:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract:
      period_ms: 33
      deadline_ms: 66
      max_age_ms: 100
      objective: {{ coverage_permille: 980, window_ms: 10000 }}
    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 17000, p95_us: 18000, p99_us: 19000, samples: 110 }}
  rear:
    class: normal
    queue: {{ policy: latest, capacity: 1 }}
    contract:
      period_ms: 33
      deadline_ms: 66
      max_age_ms: 100
{second_objective}    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 17000, p95_us: 18000, p99_us: 19000, samples: 110 }}
"
    )
}

const BOTH_HALVES: &str =
    "      objective: { coverage_permille: 200, window_ms: 10000, max_gap_ms: 1000 }\n";

fn ms(value: u64) -> Duration {
    Duration::from_millis(value).unwrap()
}

/// Der Fall des Betreibers, vollstaendig: 98 % fuer die eine Kamera, 20 %
/// plus „einmal je Sekunde" fuer die andere.
#[test]
fn both_halves_reach_the_contract() {
    let config = Config::from_yaml(&yaml(BOTH_HALVES)).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
    let resolved = config.resolve().unwrap();

    let front = resolved.model_index("front").unwrap();
    assert_eq!(
        resolved.contracts.get(front.get()).unwrap().objective,
        Some(Objective {
            coverage_permille: 980,
            window: ms(10_000),
            max_gap: None,
        })
    );

    let rear = resolved.model_index("rear").unwrap();
    assert_eq!(
        resolved.contracts.get(rear.get()).unwrap().objective,
        Some(Objective {
            coverage_permille: 200,
            window: ms(10_000),
            max_gap: Some(ms(1_000)),
        })
    );
}

/// Ohne die Zeile plant der Governor wie bisher.
#[test]
fn without_an_objective_nothing_is_set() {
    let resolved = Config::from_yaml(&yaml("")).unwrap().resolve().unwrap();
    let rear = resolved.model_index("rear").unwrap();
    assert!(
        resolved
            .contracts
            .get(rear.get())
            .unwrap()
            .objective
            .is_none()
    );
}

/// Ein Ziel, das keine seiner beiden Haelften nennt, ist keine Zusage.
#[test]
fn an_empty_objective_is_refused() {
    let findings = Config::from_yaml(&yaml("      objective: { window_ms: 10000 }\n"))
        .unwrap()
        .diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.to_string().contains("mindestens coverage_permille")),
        "{findings:?}"
    );
}

/// Der Anteil zaehlt Zyklen — ohne Takt gibt es keine.
#[test]
fn a_share_without_a_period_is_refused() {
    // Die zweite Kamera hat einen Anteil, aber keinen Takt.
    let without_period = yaml_with(
        "      deadline_ms: 66
      max_age_ms: 100
      objective: { coverage_permille: 500, window_ms: 10000 }
",
    );
    let findings = Config::from_yaml(&without_period).unwrap().diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.to_string().contains("braucht period_ms")),
        "{findings:?}"
    );

    // Gegenprobe, sonst beweist der Test nichts: mit Takt darf genau dieser
    // Befund nicht erscheinen.
    let with_period = yaml_with(
        "      period_ms: 33
      deadline_ms: 66
      max_age_ms: 100
      objective: { coverage_permille: 500, window_ms: 10000 }
",
    );
    let findings = Config::from_yaml(&with_period).unwrap().diagnose();
    assert!(
        !findings
            .iter()
            .any(|f| f.to_string().contains("braucht period_ms")),
        "{findings:?}"
    );
}

/// Haeufiger als jeden Zyklus kann kein Ergebnis entstehen.
#[test]
fn a_gap_below_the_period_is_refused() {
    let findings = Config::from_yaml(&yaml(
        "      objective: { coverage_permille: 200, window_ms: 10000, max_gap_ms: 20 }\n",
    ))
    .unwrap()
    .diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.to_string().contains("unter period_ms")),
        "{findings:?}"
    );
}

/// Ein Fenster ueber einer Minute traegt die Aussage nicht mehr.
#[test]
fn a_window_beyond_a_minute_is_refused() {
    let findings = Config::from_yaml(&yaml(
        "      objective: { coverage_permille: 200, window_ms: 61000 }\n",
    ))
    .unwrap()
    .diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.to_string().contains("window_ms hoechstens")),
        "{findings:?}"
    );
}

/// Wer sich vertippt, soll es merken — nicht still ohne Zusage laufen.
#[test]
fn an_unknown_field_in_the_objective_is_refused() {
    assert!(
        Config::from_yaml(&yaml(
            "      objective: { coverage_promille: 200, window_ms: 10000 }\n"
        ))
        .is_err()
    );
}

/// Was eine Zusage an Kapazitaet beansprucht (ADR-0047, Zulassung).
///
/// Ein nachrangiger Strom mit Zusage belegt Slots und muss in die Rechnung;
/// ein bewachter mit Zusage steckt ueber seine Klasse schon drin und darf
/// nicht doppelt zaehlen.
#[test]
fn an_objective_on_lower_work_claims_capacity() {
    // `rear` ist `normal`, 20 % bei 33 ms Takt und 18 ms konservativer
    // Laufzeit: rund 0,2 * 18/33 = 109 ‰ auf zwei Slots also rund 54 ‰.
    let resolved = Config::from_yaml(&yaml(
        "      objective: { coverage_permille: 200, window_ms: 10000 }\n",
    ))
    .unwrap()
    .resolve()
    .unwrap();
    let claimed = resolved.objective_utilization_permille();
    assert!(
        (40..=70).contains(&claimed),
        "beansprucht {claimed} ‰, erwartet rund 54 ‰"
    );
}

/// Der geschuetzte Strom traegt in dieser Vorlage selbst eine Zusage von
/// 980 ‰ — die darf die Rechnung nicht erhoehen, sonst waere jede sorgfaeltig
/// beschriebene Konfiguration unzulaessig.
#[test]
fn an_objective_on_guarded_work_is_not_counted_twice() {
    let resolved = Config::from_yaml(&yaml("")).unwrap().resolve().unwrap();
    assert_eq!(
        resolved.objective_utilization_permille(),
        0,
        "die geschuetzte Kamera traegt eine Zusage und ist schon eingerechnet"
    );
    // Und die geschuetzte Auslastung selbst ist davon unberuehrt.
    assert!(resolved.protected_utilization_permille() > 0);
}

/// Ohne Takt traegt die Luecke die Rechnung: „einmal je Sekunde" bei 18 ms
/// Laufzeit sind rund 18 ‰, auf zwei Slots also rund 9 ‰.
#[test]
fn without_a_period_the_gap_carries_the_claim() {
    let resolved = Config::from_yaml(&yaml_with(
        "      deadline_ms: 2000
      max_age_ms: 4000
      objective: { window_ms: 10000, max_gap_ms: 1000 }
",
    ))
    .unwrap()
    .resolve()
    .unwrap();
    let claimed = resolved.objective_utilization_permille();
    assert!(
        (5..=15).contains(&claimed),
        "beansprucht {claimed} ‰, erwartet rund 9 ‰"
    );
}

/// Die strengere Haelfte bestimmt, was wirklich versprochen ist: „einmal je
/// Sekunde" sind bei 33 ms Takt 33 ‰, auch wenn der Anteil darunter steht.
#[test]
fn the_stricter_half_decides() {
    let lax = Objective {
        coverage_permille: 10,
        window: ms(10_000),
        max_gap: Some(ms(1_000)),
    };
    assert_eq!(lax.effective_permille(ms(33)), 33);

    let strict = Objective {
        coverage_permille: 980,
        window: ms(10_000),
        max_gap: Some(ms(1_000)),
    };
    assert_eq!(strict.effective_permille(ms(33)), 980);
}
