//! Das ausgelieferte Beispiel schaltet die Zerlegung ein (ADR-0014).
//!
//! Der Anlass ist ein Versaeumnis: die Zerlegung war seit dem 01.09.2026
//! gebaut, gemessen und dokumentiert — und in **keiner** Datei unter
//! `examples/` eingeschaltet. Wer den Befund der Blockierpruefung las, fand
//! den empfohlenen Ausweg nirgends vorgemacht. Dieser Test haelt fest, dass
//! es mindestens ein Beispiel gibt, das ihn zeigt, und dass seine Zahlen
//! gemessen sind.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use vig_config::Config;
use vig_core::Duration;

const EXAMPLE: &str = include_str!("../../../examples/cooperative_llm/vig.yaml");

/// Die Datei ist gueltig und wirft keinen Befund.
///
/// Sie ist eine Vorlage, keine Messkonfiguration: anders als
/// `gate_m3/vig.yaml` oder `krakow-sat-slots1.yaml`, die bewusst Ueberlast
/// beschreiben, soll ein Betreiber diese hier uebernehmen koennen.
#[test]
fn the_example_is_free_of_findings() {
    let config = Config::from_yaml(EXAMPLE).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
}

/// Das Sprachmodell traegt die Zerlegung, und sie kommt im Vertrag an.
#[test]
fn the_language_model_is_decomposable() {
    let resolved = Config::from_yaml(EXAMPLE).unwrap().resolve().unwrap();
    let assistant = resolved.model_index("assistant").unwrap();
    let Some(cooperative) = resolved.contracts.get(assistant.get()).unwrap().cooperative else {
        panic!("das Sprachmodell des Beispiels traegt kein `cooperative:`");
    };
    // Gemessen am 16.09.2026, zwei unabhaengige Verfahren (Messreihe
    // 8/16/32/48 Token: 6100 us / 256 Tok/s; `vig calibrate`: 6277 us /
    // 258 Tok/s). Festgehalten wird der kalibrierte Wert.
    assert_eq!(cooperative.tokens_per_second, 258);
    assert_eq!(cooperative.base_cost, Duration::from_micros(6_277).unwrap());
    // Prefix-Caching wirkt: der gewachsene Kontext kostet fast nichts. Das ist
    // die Bedingung, unter der sich Zerlegung lohnt (ADR-0031).
    assert!(cooperative.prefill_per_token < Duration::from_micros(10).unwrap());
}

/// Auf **einem** Slot waere dieselbe Last ohne Zerlegung nicht zulaessig.
///
/// Das Beispiel selbst faehrt zwei Slots, weil eine ausgelieferte Vorlage
/// ihre eigene Zusage halten soll: mit einem Slot reisst die Kamera ihre
/// 100 ms (gemessen 213 bis 236 ms, auch bei nur zehn Anfragen in 30 s).
/// Die Zerlegung ist hier also kein Zulassungsersatz, sondern gemessener
/// Nutzen — 608 Abweisungen gegen null.
///
/// Dass sie auf einer Ausfuehrungseinheit den Unterschied zwischen
/// „laeuft gar nicht" und „laeuft" macht, prueft dieser Test an derselben
/// Datei mit einem Slot. Faellt der Befund hier weg, zeigt das Beispiel den
/// Fall nicht mehr, fuer den ADR-0014 geschrieben wurde.
#[test]
fn on_one_slot_the_same_load_needs_the_decomposition() {
    let one_slot = EXAMPLE.replace("  slots: 2", "  slots: 1");
    assert!(one_slot.contains("slots: 1"), "Slotzahl nicht ersetzt");

    // Mit Zerlegung: zulaessig, weil ein Quantum in die Zusage passt.
    let findings = Config::from_yaml(&one_slot).unwrap().diagnose();
    assert!(findings.is_empty(), "mit Zerlegung: {findings:?}");

    // Ohne sie: der ungeteilte Aufruf rechnet laenger als die Zusage.
    let bare = strip_cooperative(&one_slot);
    let findings = Config::from_yaml(&bare).unwrap().diagnose();
    assert!(
        findings
            .iter()
            .any(|f| f.path == "models.assistant.contract"),
        "ohne Zerlegung muesste die Blockierpruefung greifen: {findings:?}"
    );
}

/// Entfernt den `cooperative:`-Block und seine Felder aus dem YAML.
fn strip_cooperative(yaml: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("cooperative:") {
            inside = true;
            continue;
        }
        if inside {
            // Die Felder des Blocks sind tiefer eingerueckt als er selbst.
            let indent = line.len().saturating_sub(trimmed.len());
            if !trimmed.is_empty() && indent <= 4 {
                inside = false;
            } else {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
