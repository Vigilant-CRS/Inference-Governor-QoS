//! `vig init` — eine Startkonfiguration aus einer laufenden Inferenzinstanz.
//!
//! Die Einstiegshuerde dieses Produkts ist nicht das Protokoll — das ist
//! kompatibel — sondern die erste `vig.yaml`. Wer den Governor ausprobieren
//! will, muss heute Modellnamen, Formen und Datentypen aus dem Backend
//! abschreiben, bevor er ueberhaupt zum eigentlichen Thema kommt.
//!
//! Das ist die Haelfte, die eine Maschine besser weiss als ein Mensch. Die
//! andere Haelfte weiss nur der Betreiber: wie oft eine Kamera liefert, wie
//! alt ein Ergebnis sein darf, welcher Strom wichtiger ist. Dieses Werkzeug
//! traegt das erste ein und **laesst das zweite ausdruecklich offen**.
//!
//! ## Warum die erzeugte Datei nicht laedt
//!
//! Die Platzhalter sind keine Zahlen, sondern Namen wie `TODO_PERIOD_MS`. Der
//! Parser bricht daran ab und nennt Zeile und Feld. Das ist Absicht: eine
//! Datei, die mit erfundenen Perioden anstandslos startet, verschiebt den
//! Fehler in den Betrieb — und erfundene Zahlen sehen in einer
//! Konfigurationsdatei genauso aus wie gemessene.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;
use vig_backend_triton::TritonClient;
use vig_protocol_oip::inference::RepositoryIndexRequest;

/// Ein am Backend gefundenes Modell mit dem, was die Maschine ueber es weiss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Discovered {
    /// Name im Backend.
    pub(crate) name: String,
    /// Plattform bzw. Backendtyp, soweit gemeldet (`onnxruntime_onnx`, ...).
    pub(crate) platform: String,
    /// Eingabetensoren: Name, Datentyp, Form.
    pub(crate) inputs: Vec<Tensor>,
    /// Ausgabetensoren.
    pub(crate) outputs: Vec<Tensor>,
}

/// Ein Tensor laut Modellmetadaten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tensor {
    pub(crate) name: String,
    pub(crate) datatype: String,
    pub(crate) shape: Vec<i64>,
}

/// Fuehrt `vig init` aus.
///
/// # Errors
///
/// Wenn das Backend nicht erreichbar ist oder die Zieldatei nicht geschrieben
/// werden kann.
pub(crate) async fn run(
    endpoint: &str,
    out: Option<&Path>,
    force: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if let Some(path) = out
        && path.exists()
        && !force
    {
        eprintln!(
            "FEHLER {} existiert schon. Eine bestehende Konfiguration wird nicht \
             ueberschrieben — sie enthaelt Entscheidungen und Kommentare, die \
             dieses Werkzeug nicht kennt. Mit --force ausdruecklich ersetzen.",
            path.display()
        );
        return Ok(ExitCode::FAILURE);
    }

    let client = TritonClient::new(endpoint);
    let health = client.health().await?;
    if !health.ready {
        eprintln!(
            "FEHLER das Backend unter {endpoint} ist nicht bereit. \
             `vig init` liest die geladenen Modelle; ohne sie gaebe es nur \
             eine leere Vorlage."
        );
        return Ok(ExitCode::FAILURE);
    }

    let discovered = Box::pin(discover(&client)).await?;
    if discovered.is_empty() {
        eprintln!(
            "FEHLER {endpoint} meldet kein geladenes Modell. Erst das Backend \
             mit den Modellen starten, die der Governor spaeter steuern soll."
        );
        return Ok(ExitCode::FAILURE);
    }

    let yaml = render(endpoint, &discovered);
    match out {
        Some(path) => {
            std::fs::write(path, &yaml)?;
            println!(
                "{} geschrieben: {} Modelle",
                path.display(),
                discovered.len()
            );
            println!(
                "Als Naechstes:\n  \
                 1. Die TODO-Felder fuellen — Periode, Frist, Hoechstalter, Klasse.\n  \
                 2. `vig profile -c {}` laufen lassen und die Profile einsetzen.\n  \
                 3. `vig doctor -c {}` bis es gruen ist.",
                path.display(),
                path.display()
            );
        }
        None => print!("{yaml}"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Fragt das Backend nach seinen Modellen und deren Metadaten.
async fn discover(client: &TritonClient) -> Result<Vec<Discovered>, Box<dyn std::error::Error>> {
    let index = client
        .raw()
        .await?
        .repository_index(RepositoryIndexRequest {
            repository_name: String::new(),
            ready: true,
        })
        .await?
        .into_inner();

    // Mehrere Versionen desselben Modells sind fuer eine Startkonfiguration
    // dieselbe Zeile; die Version gehoert in den Vertrag, nicht in den Namen.
    let mut names: Vec<String> = index.models.into_iter().map(|m| m.name).collect();
    names.sort();
    names.dedup();

    let mut found = Vec::new();
    for name in names {
        let metadata = match client.model_metadata(&name).await {
            Ok(metadata) => metadata,
            Err(error) => {
                // Ein Modell, dessen Metadaten fehlen, ist kein Grund
                // aufzuhoeren — es ist eine Zeile weniger in der Vorlage.
                eprintln!("  {name}: Metadaten nicht abrufbar ({error}); uebersprungen");
                continue;
            }
        };
        let tensors =
            |list: Vec<vig_protocol_oip::inference::model_metadata_response::TensorMetadata>| {
                list.into_iter()
                    .map(|t| Tensor {
                        name: t.name,
                        datatype: t.datatype,
                        shape: t.shape,
                    })
                    .collect()
            };
        found.push(Discovered {
            name: metadata.name,
            platform: metadata.platform,
            inputs: tensors(metadata.inputs),
            outputs: tensors(metadata.outputs),
        });
    }
    Ok(found)
}

/// Baut den Text der Startkonfiguration.
///
/// Reine Funktion ueber dem, was das Backend gemeldet hat — damit das Format
/// ohne Netzwerk pruefbar ist.
pub(crate) fn render(endpoint: &str, models: &[Discovered]) -> String {
    let mut out = String::new();
    out.push_str(&header(endpoint, models.len()));
    let _ = write!(
        out,
        "version: 1\n\nbackend:\n  type: triton\n  grpc_endpoint: \"{endpoint}\"\n"
    );
    out.push_str(SLOTS_BLOCK);
    out.push_str("\nmodels:\n");

    for (position, model) in models.iter().enumerate() {
        out.push_str(&model_block(model, position == 0));
    }
    out.push_str(FOOTER);
    out
}

fn header(endpoint: &str, count: usize) -> String {
    format!(
        "# Startkonfiguration, erzeugt von `vig init` aus {endpoint}.\n\
         #\n\
         # Gefunden: {count} Modell(e). Namen, Formen und Datentypen stammen aus\n\
         # dem Backend. Alles mit TODO weiss nur der Betreiber; **die Datei laedt\n\
         # absichtlich nicht**, solange dort ein Platzhalter steht. Eine Vorlage\n\
         # mit erfundenen Perioden waere die teurere Variante desselben Fehlers:\n\
         # sie startet, und die falsche Zahl faellt erst im Betrieb auf.\n\
         #\n\
         # Reihenfolge: TODOs fuellen, dann `vig profile`, dann `vig doctor`.\n\n"
    )
}

/// Der Backendblock unterhalb des Endpunkts.
const SLOTS_BLOCK: &str = concat!(
    "  # Wie viele Auftraege das Backend wirklich gleichzeitig rechnet — nicht\n",
    "  # wie viele es annimmt. Eine Instanz je Modell heisst 1. Zu hoch gesetzt\n",
    "  # plant der Governor mit einer Parallelitaet, die es nicht gibt; zu\n",
    "  # niedrig verschenkt er Ueberlappung, die das Backend koennte.\n",
    "  slots: 1\n",
    "  # Ein Auftrag darf unterwegs sein, bevor die Antwort auf den vorigen da\n",
    "  # ist. Ohne das laeuft die GPU zwischen zwei Auftraegen leer.\n",
    "  pipelining_depth: 1\n",
    "  # Aufschlag auf das gemessene Profil. 110 heisst: plane mit 10 Prozent\n",
    "  # mehr als gemessen.\n",
    "  safety_margin_percent: 110\n",
);

fn model_block(model: &Discovered, first: bool) -> String {
    let mut block = String::new();
    let key = sanitise(&model.name);
    block.push('\n');
    let _ = write!(block, "  # Backendmodell `{}`", model.name);
    if !model.platform.is_empty() {
        let _ = write!(block, " ({})", model.platform);
    }
    block.push('\n');
    for input in &model.inputs {
        let _ = writeln!(
            block,
            "  #   Eingang  {:<16} {:<8} {:?}",
            input.name, input.datatype, input.shape
        );
    }
    for output in &model.outputs {
        let _ = writeln!(
            block,
            "  #   Ausgang  {:<16} {:<8} {:?}",
            output.name, output.datatype, output.shape
        );
    }
    if first {
        block.push_str(CLASS_HELP);
    }
    let _ = writeln!(block, "  {key}:");
    block.push_str("    class: TODO_CLASS          # protected | high | best_effort\n");
    block.push_str("    queue: { policy: latest, capacity: 1 }\n");
    block.push_str(CONTRACT_HELP);
    block.push_str(
        "    contract: { period_ms: TODO_PERIOD_MS, deadline_ms: TODO_DEADLINE_MS, max_age_ms: TODO_MAX_AGE_MS }\n",
    );
    block.push_str("    variants:\n");
    let _ = write!(
        block,
        "      - id: main\n        backend_model: {}\n",
        model.name
    );
    block.push_str("        quality: { value: 1.00, source: user_declared }\n");
    block.push_str(PROFILE_HELP);
    block.push_str(
        "        profile: { p50_us: TODO_MEASURE, p95_us: TODO_MEASURE, p99_us: TODO_MEASURE, samples: 0 }\n",
    );
    block
}

/// Erklaert die Wichtigkeitsklassen — einmal, beim ersten Modell.
const CLASS_HELP: &str = concat!(
    "  #\n",
    "  # class: protected   diese Zusage wird gehalten, notfalls auf Kosten der\n",
    "  #                    anderen. Hoechstens so viel davon, wie die Karte traegt.\n",
    "  #        high        wichtig, aber nachrangig gegenueber protected.\n",
    "  #        best_effort laeuft, wenn Platz ist. Ohne Zusage.\n",
);

/// Erklaert die drei Zeiten, die nur der Betreiber kennt.
const CONTRACT_HELP: &str = concat!(
    "    # period_ms   Abstand zwischen zwei Aufnahmen dieser Quelle. Zu gross\n",
    "    #             angegeben plant der Governor zu wenig Kapazitaet ein.\n",
    "    # deadline_ms Bis wann ein Ergebnis fertig sein muss, ab Aufnahme\n",
    "    #             gerechnet. Zu gross heisst: es wird nie etwas verworfen,\n",
    "    #             auch wenn es niemandem mehr nuetzt.\n",
    "    # max_age_ms  Ab wann ein fertiges Ergebnis wertlos ist. Das ist die\n",
    "    #             Zahl, mit der der Governor ueberhaupt erst verwerfen darf;\n",
    "    #             ohne sie faellt sein staerkstes Werkzeug aus (ADR-0010).\n",
);

/// Erklaert, warum hier kein Wert steht.
const PROFILE_HELP: &str = concat!(
    "        # Kein geratenes Profil: `vig profile -c <diese Datei>` misst es am\n",
    "        # Backend und gibt einen einfuegefertigen Block aus. Ein geschaetztes\n",
    "        # Profil ergibt geschaetzte Entscheidungen.\n",
);

const FOOTER: &str = "\n\
    # Was hier absichtlich fehlt:\n\
    #\n\
    # * **Interferenz** (`backend.interference`, `no_corun`): welches Modellpaar\n\
    #   sich gegenseitig bremst, misst `vig calibrate`. Ohne Eintrag rechnet der\n\
    #   Governor ohne Aufschlag — das ist „unbekannt\", nicht „nachweislich null\".\n\
    # * **Sicherheit** (`backend.security`): ohne Block lauscht `vig serve` nur\n\
    #   auf Loopback und verweigert jede andere Adresse ohne Identitaetspruefung.\n\
    # * **Varianten**: mehrere Modellstaende derselben Aufgabe. Erst sinnvoll,\n\
    #   wenn ihre Qualitaet belegt ist — gleiche Tensorform heisst nicht gleiche\n\
    #   Bedeutung (ADR-0025).\n";

/// Macht aus einem Backendnamen einen brauchbaren logischen Namen.
///
/// Der logische Name ist der, den der Client spaeter anfragt. Punkte und
/// Doppelpunkte wuerden das YAML unnoetig verschachteln.
fn sanitise(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_owned();
    if trimmed.is_empty() {
        "model".to_owned()
    } else {
        trimmed
    }
}

/// Zaehlt, welche Platzhalter noch offen sind — fuer `doctor` und Tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn open_placeholders(yaml: &str) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for marker in [
        "TODO_CLASS",
        "TODO_PERIOD_MS",
        "TODO_DEADLINE_MS",
        "TODO_MAX_AGE_MS",
        "TODO_MEASURE",
    ] {
        let count = yaml.matches(marker).count();
        if count > 0 {
            counts.insert(marker, count);
        }
    }
    counts
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{Discovered, Tensor, open_placeholders, render, sanitise};

    fn model(name: &str) -> Discovered {
        Discovered {
            name: name.to_owned(),
            platform: "onnxruntime_onnx".to_owned(),
            inputs: vec![Tensor {
                name: "images".to_owned(),
                datatype: "FP32".to_owned(),
                shape: vec![1, 3, 512, 512],
            }],
            outputs: vec![Tensor {
                name: "dets".to_owned(),
                datatype: "FP32".to_owned(),
                shape: vec![1, 300, 4],
            }],
        }
    }

    /// Was die Maschine weiss, steht drin: Modellname, Form, Datentyp.
    #[test]
    fn the_template_carries_what_the_backend_reported() {
        let yaml = render("127.0.0.1:8001", &[model("rfdetr")]);
        assert!(yaml.contains("grpc_endpoint: \"127.0.0.1:8001\""));
        assert!(yaml.contains("backend_model: rfdetr"));
        assert!(yaml.contains("onnxruntime_onnx"));
        assert!(yaml.contains("[1, 3, 512, 512]"), "Form fehlt: {yaml}");
        assert!(yaml.contains("FP32"));
    }

    /// Und was nur der Betreiber weiss, bleibt als Platzhalter stehen —
    /// nicht als erfundene Zahl.
    #[test]
    fn everything_only_the_operator_knows_stays_a_placeholder() {
        let yaml = render("127.0.0.1:8001", &[model("rfdetr")]);
        let open = open_placeholders(&yaml);
        assert_eq!(open.get("TODO_PERIOD_MS"), Some(&1));
        assert_eq!(open.get("TODO_DEADLINE_MS"), Some(&1));
        assert_eq!(open.get("TODO_MAX_AGE_MS"), Some(&1));
        assert_eq!(open.get("TODO_CLASS"), Some(&1));
        assert_eq!(open.get("TODO_MEASURE"), Some(&3), "p50/p95/p99");
    }

    /// Die Vorlage laedt absichtlich nicht, solange ein Platzhalter steht.
    #[test]
    fn the_template_refuses_to_load_while_a_placeholder_is_open() {
        let yaml = render("127.0.0.1:8001", &[model("rfdetr")]);
        assert!(
            vig_config::Config::from_yaml(&yaml).is_err(),
            "eine Vorlage mit Platzhaltern darf nicht stillschweigend laden"
        );
    }

    /// Gefuellt ist sie eine gueltige Konfiguration — und `diagnose` findet
    /// nichts mehr zu beanstanden.
    #[test]
    fn filled_in_it_is_a_valid_configuration() {
        let yaml = render("127.0.0.1:8001", &[model("rfdetr")])
            .replace("TODO_CLASS", "protected")
            .replace("TODO_PERIOD_MS", "33")
            .replace("TODO_DEADLINE_MS", "33")
            .replace("TODO_MAX_AGE_MS", "66")
            .replace(
                "p50_us: TODO_MEASURE, p95_us: TODO_MEASURE, p99_us: TODO_MEASURE, samples: 0",
                "p50_us: 15639, p95_us: 18074, p99_us: 19596, samples: 200",
            );
        let config = vig_config::Config::from_yaml(&yaml).expect("gefuellt gueltig");
        assert!(
            config.diagnose().is_empty(),
            "Befunde: {:?}",
            config.diagnose()
        );
        config.resolve().expect("aufloesbar");
    }

    /// Mehrere Modelle ergeben mehrere Bloecke, jeder mit eigenem Namen.
    #[test]
    fn every_model_gets_its_own_block() {
        let yaml = render("h:1", &[model("rfdetr"), model("pose_main")]);
        assert!(yaml.contains("  rfdetr:\n"));
        assert!(yaml.contains("  pose_main:\n"));
        assert_eq!(open_placeholders(&yaml).get("TODO_PERIOD_MS"), Some(&2));
    }

    /// Namen, die YAML nicht mag, werden entschaerft statt abgelehnt.
    #[test]
    fn awkward_names_become_usable_keys() {
        assert_eq!(sanitise("model.v2"), "model_v2");
        assert_eq!(sanitise("a:b"), "a_b");
        assert_eq!(sanitise("___"), "model");
    }
}
