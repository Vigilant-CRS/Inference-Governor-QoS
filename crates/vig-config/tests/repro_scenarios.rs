//! Die Szenariovorlagen des Reproduktionspakets (`tools/repro/scenarios/`).
//!
//! Sie sind das, was ein Aussenstehender als erstes kopiert. Bricht hier
//! etwas, bricht der einzige Weg, auf dem jemand ohne unsere Modelle unsere
//! Kernaussage nachpruefen kann.
//!
//! Geprueft wird ohne GPU und ohne Backend — alles, was sich am Schema und an
//! der Aufloesung entscheiden laesst.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use vig_config::Config;

const VIER_KAMERAS: &str = include_str!("../../../tools/repro/scenarios/b-vier-kameras.yaml");
const ZWEI_GROESSEN: &str = include_str!("../../../tools/repro/scenarios/c-zwei-groessen.yaml");

/// Setzt ein Profil ein, wie `vig calibrate` es nach einer geglueckten
/// Messreihe schreiben wuerde.
///
/// Die Quantile stehen ausgeschrieben da, statt aus `p50` gerechnet zu
/// werden. Zwei Gruende: der Workspace verbietet beilaeufige Arithmetik
/// (`clippy::arithmetic_side_effects`), und eine Rechnung waere hier
/// irrefuehrend — sie saehe nach einem Zusammenhang aus, den es zwischen
/// Median und p99 eines Modells nicht gibt.
fn mit_profil(yaml: &str, backend_model: &str, p50: u64, p95: u64, p99: u64) -> String {
    let anker = format!("        backend_model: {backend_model}\n");
    let ersatz = format!(
        "{anker}        profile: {{ p50_us: {p50}, p95_us: {p95}, p99_us: {p99}, samples: 200 }}\n"
    );
    let ersetzt = yaml.replace(&anker, &ersatz);
    assert_ne!(
        ersetzt, yaml,
        "Ankerzeile fuer {backend_model} nicht gefunden"
    );
    ersetzt
}

/// Der eigentliche Schutz dieses Pakets.
///
/// `vig calibrate` laesst das Profil einer Variante, deren Messreihe es
/// verworfen hat, **unveraendert stehen** und endet trotzdem mit Erfolg
/// (`apply()` ueberspringt sie). Stuenden in den Vorlagen Zahlen, bekaeme ein
/// Anwender im Fehlerfall eine Konfiguration, die gemessen aussieht und mit
/// den Laufzeiten einer fremden Maschine plant — ohne jedes Anzeichen.
///
/// Diese Zusicherung ist der Grund, warum die Vorlagen unvollstaendig sind.
/// Wer hier Zahlen eintraegt, macht das Paket unbrauchbar, ohne dass eine
/// andere Pruefung anschlaegt.
#[test]
fn die_vorlagen_tragen_keine_fremden_laufzeiten() {
    for (name, yaml) in [
        ("b-vier-kameras", VIER_KAMERAS),
        ("c-zwei-groessen", ZWEI_GROESSEN),
    ] {
        assert!(
            !yaml.contains("p50_us"),
            "{name}: die Vorlage traegt ein Laufzeitprofil. Profile gehoeren auf \
             die Maschine des Anwenders, nicht in die Vorlage (Spec 13.5)."
        );
    }
}

/// Lesbar ja, planbar nein — und zwar mit Absicht.
///
/// Ohne Profil kann der Scheduler nicht planen. Dass die Datei trotzdem
/// **geparst** werden kann, ist die Voraussetzung dafuer, dass
/// `vig calibrate` ueberhaupt gegen sie laufen kann: es arbeitet ueber
/// `profiling_targets()` und nicht ueber `resolve()`.
#[test]
fn die_vorlagen_lassen_sich_lesen_aber_nicht_aufloesen() {
    for (name, yaml) in [
        ("b-vier-kameras", VIER_KAMERAS),
        ("c-zwei-groessen", ZWEI_GROESSEN),
    ] {
        let config =
            Config::from_yaml(yaml).unwrap_or_else(|e| panic!("{name}: Vorlage nicht lesbar: {e}"));
        assert!(
            config.resolve().is_err(),
            "{name}: die Vorlage loest ohne Profile auf. Dann faellt eine \
             verworfene Messreihe nicht mehr auf."
        );
    }
}

/// Lastfall (c) steht und faellt damit, dass die beiden Groessen als
/// austauschbar **gelten** — sonst waehlt der Governor gar nicht, und die
/// Messung zeigt etwas anderes als ihren Titel.
///
/// Das war waehrend der Entwicklung schon einmal still abgeschaltet: eine
/// unvollstaendige Semantikangabe (fehlendes Boxlayout, fehlende
/// Klassenliste) genuegt, und `doctor` meldet nur eine Warnung unter vielen.
#[test]
fn zwei_groessen_sind_fachlich_austauschbar() {
    let yaml = mit_profil(ZWEI_GROESSEN, "rtdetr_r50", 29_600, 30_600, 31_600);
    let yaml = mit_profil(&yaml, "rtdetr_r18", 15_000, 16_000, 17_000);
    let config = Config::from_yaml(&yaml).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
    let resolved = config.resolve().unwrap();

    let detektor = resolved.model_index("detektor").unwrap();
    let contract = resolved.contracts.get(detektor.get()).unwrap();
    assert_eq!(
        contract.semantic_conflict(),
        None,
        "die beiden RT-DETR-Groessen gelten als fachlich verschieden"
    );
    assert!(
        contract.auto_variant_selection(),
        "die automatische Variantenwahl ist aus — dann misst Lastfall (c) \
         eine feste Variante und nicht die Wahl"
    );
}

/// Vier Kameras, ein Detektor, ein Slot — und nur drei davon zaehlen in die
/// Kapazitaetsrechnung.
///
/// `is_guarded()` ist `Protected | High`; der Uebersichtsstrom ist
/// `best_effort` und darf verhungern. Zaehlte er mit, waere die
/// Periodenwahl dieses Lastfalls falsch gerechnet.
#[test]
fn vier_kameras_teilen_sich_einen_slot() {
    let yaml = mit_profil(VIER_KAMERAS, "rtdetr_r18", 15_000, 16_000, 17_000);
    let config = Config::from_yaml(&yaml).unwrap();
    assert!(config.diagnose().is_empty(), "{:?}", config.diagnose());
    let resolved = config.resolve().unwrap();

    assert_eq!(resolved.model_names.len(), 4);
    assert_eq!(resolved.slots.regular_len(), 1);

    // Drei bewachte Stroeme zu je 66 ms mit p99 17.000 us und 110 % Marge:
    // 3 * 18.700 / 66.000 = 850 Promille. Der Hof-Strom (best_effort, 100 ms)
    // taucht darin nicht auf — mit ihm waeren es rund 1.037.
    let auslastung = resolved.protected_utilization_permille();
    assert!(
        (700..=950).contains(&auslastung),
        "geschuetzte Auslastung {auslastung} Promille liegt ausserhalb des \
         Bereichs, fuer den die Perioden gewaehlt sind"
    );
}
