//! Das YAML-Schema und seine Uebersetzung in Kerntypen (Spec 6.2).

use crate::error::{ConfigError, Located};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use vig_core::Duration;
use vig_core::arrayvec::ArrayVec;
use vig_core::contract_ext::{
    ApprovedVariants, ContractExtension, DeliveryBoundary, DeliverySemantics, EvidenceLevel,
    MissBudget, ValidityEnvelope,
};
use vig_core::ids::{MAX_MODELS, MAX_VARIANTS, ModelIdx, VariantIdx};
use vig_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use vig_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use vig_core::queue::QueueConfig;
use vig_core::request::{Criticality, OverflowPolicy, QueuePolicy};
use vig_core::semantics::{
    CoordinateConvention, InputContract, LabelSet, OutputKind, OutputSemantics, Unit,
    VariantSemantics,
};
use vig_core::slots::SlotSet;

/// Die einzige unterstuetzte Schemaversion.
pub const SCHEMA_VERSION: u32 = 1;

/// Die vollstaendige Konfiguration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Schemaversion.
    pub version: u32,
    /// Das Ausfuehrungsbackend.
    pub backend: BackendConfig,
    /// Die logischen Modelle, nach Namen.
    ///
    /// `BTreeMap` und nicht `HashMap`: die Reihenfolge bestimmt die
    /// Modellindizes im Kern, und die muss zwischen zwei Starts derselben
    /// Datei identisch sein. Sonst waere ein aufgezeichneter Trace nicht mehr
    /// reproduzierbar (Spec 30.2).
    pub models: BTreeMap<String, ModelConfig>,
}

/// Das Ausfuehrungsbackend.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendConfig {
    /// Backendtyp; derzeit nur `triton`.
    #[serde(rename = "type")]
    pub kind: String,
    /// gRPC-Endpunkt des Backends.
    pub grpc_endpoint: String,
    /// Anzahl paralleler Ausfuehrungsslots (ADR-0004).
    ///
    /// Muss zur Instance-Group-Konfiguration des Backends passen. Zu hoch
    /// gesetzt entsteht hinter dem Governor eine unsichtbare Queue; zu niedrig
    /// bleibt Kapazitaet ungenutzt.
    pub slots: usize,
    /// Zusaetzliche Kredite je Slot (ADR-0002).
    #[serde(default = "default_pipelining")]
    pub pipelining_depth: usize,
    /// Modellpaare, die nicht gleichzeitig laufen duerfen (ADR-0006).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_corun: Vec<[String; 2]>,
    /// Pfad zum Modellrepository des Backends, sofern es sichtbar ist (NV-03).
    ///
    /// Nur dafuer da, den Artefakt-Digest eines Profils zu pruefen. Ueber das
    /// Inferenzprotokoll ist nicht erkennbar, ob jemand die Gewichtsdatei
    /// unter derselben Versionsnummer ausgetauscht hat; im Dateisystem schon.
    ///
    /// Ist der Pfad nicht gesetzt oder das Repository nicht erreichbar —
    /// entfernter Server, Container ohne gemeinsames Volume — bleibt der
    /// Digest `unknown`. Das ist kein Fehler, sondern eine Luecke, und
    /// `doctor` nennt sie beim Namen, statt sie als geprueft auszugeben.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_repository: Option<String>,
    /// Sicherheitsmarge auf die Laufzeitprognose, in Prozent (Spec 13.2).
    #[serde(default = "default_margin_percent")]
    pub safety_margin_percent: u32,
    /// Nach wie vielen Millisekunden ein Backendaufruf als haengend gilt.
    ///
    /// **Kein Deadline-Wert.** Verspaetung regeln Vertrag und Frische; dieser
    /// Wert erkennt ein Backend, das ueberhaupt nicht mehr antwortet. Er ist
    /// deshalb bewusst um Groessenordnungen groesser als jede Deadline.
    ///
    /// Beim Ablauf wird **nur der Client** freigegeben, nicht der Slot: die
    /// GPU rechnet moeglicherweise noch, und ein zurueckgegebener Kredit
    /// wuerde eine zweite Ausfuehrung auf dieselbe Recheneinheit legen. Der
    /// Slot bleibt in Quarantaene, bis das Backend tatsaechlich antwortet —
    /// und die Bereitschaftspruefung meldet das.
    #[serde(default = "default_inference_timeout_ms")]
    pub inference_timeout_ms: u64,
    /// Wie mit Aufrufen umgegangen wird, die der Governor nicht steuert.
    ///
    /// Siehe [`TrustMode`]. Voreinstellung ist `open` — das dokumentierte
    /// Verhalten aus Spec L-002, das einen Standardclient unveraendert
    /// weiterlaufen laesst. Fuer eine Installation, die nicht in einem
    /// abgeschlossenen Netz steht, ist `strict` die richtige Wahl.
    #[serde(default)]
    pub trust: TrustMode,
    /// Obergrenze fuer gleichzeitig gehaltene Requestnutzlast, in Mebibyte.
    ///
    /// Der Ereigniskanal begrenzt die **Anzahl** offener Requests, nicht ihren
    /// Speicher: 1.024 Requests zu je 64 MiB sind 64 GiB, vor Queues und
    /// Transportpuffern. Auf einem Edgegeraet mit 8 GB ist das kein
    /// theoretischer Fall.
    #[serde(default = "default_max_inflight_mib")]
    pub max_inflight_mib: u64,
    /// Transport- und Zugangssicherung (TLS, mTLS, Token).
    #[serde(default)]
    pub security: SecurityConfig,
}

/// Ein Tensor in der zugesagten Schnittstelle eines logischen Modells.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TensorSpec {
    /// Der Tensorname, wie ihn das Backend meldet.
    pub name: String,
    /// Der Datentyp, z. B. `FP32`, `INT64`, `BYTES`.
    pub datatype: String,
    /// Die Form. `-1` steht fuer eine dynamische Achse.
    pub dims: Vec<i64>,
}

/// Die zugesagte Schnittstelle eines logischen Modells.
///
/// Der Governor waehlt die Variante je Request und sagt es dem Client nicht.
/// Ob zwei Varianten austauschbar sind, laesst sich aus Metadaten nur zur
/// Haelfte beantworten: gleiche Namen, Typen und Formen sind **notwendig**,
/// aber nicht hinreichend. Zwei Detektoren koennen beide `boxes: FP32[-1,4]`
/// liefern und trotzdem verschiedene Koordinatensysteme meinen.
///
/// Diese Luecke kann keine Messung schliessen — nur eine Aussage. Wer sie hier
/// hinschreibt, sagt: *diese Signatur ist die Schnittstelle meines logischen
/// Modells, und jede Variante bedient sie mit derselben Bedeutung.* Der
/// Governor prueft dann mechanisch, dass keine Variante formal abweicht, und
/// verweigert den Start, wenn doch.
///
/// Ohne Angabe faellt er auf den Vergleich der Varianten untereinander zurueck
/// und schaltet die automatische Wahl bei Abweichung ab. Das ist die sichere,
/// aber schwaechere Antwort: sie erkennt Unterschiede, nicht Gleichheit.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IoSignature {
    /// Die Eingaben.
    pub inputs: Vec<TensorSpec>,
    /// Die Ausgaben.
    pub outputs: Vec<TensorSpec>,
}

impl IoSignature {
    /// Rendert die Signatur so, wie sie aus Backendmetadaten entsteht.
    ///
    /// Dieselbe Normalform auf beiden Seiten: sortiert, dynamische Achsen als
    /// `?`. Sonst verglichen zwei Darstellungen desselben Sachverhalts.
    #[must_use]
    pub fn normalised(&self) -> (Vec<String>, Vec<String>) {
        let render = |t: &TensorSpec| {
            let dims: Vec<String> = t
                .dims
                .iter()
                .map(|d| {
                    if *d < 0 {
                        "?".to_owned()
                    } else {
                        d.to_string()
                    }
                })
                .collect();
            format!("{}:{}[{}]", t.name, t.datatype, dims.join(","))
        };
        let mut inputs: Vec<String> = self.inputs.iter().map(render).collect();
        let mut outputs: Vec<String> = self.outputs.iter().map(render).collect();
        inputs.sort();
        outputs.sort();
        (inputs, outputs)
    }
}

/// Transport- und Zugangssicherung des Inferenzendpunkts.
///
/// Ohne diesen Block laeuft der Endpunkt unverschluesselt und ohne
/// Identitaetspruefung — das ist der richtige Standard fuer den Fall, fuer den
/// der Governor gebaut ist (ein Geraet, ein Betreiber, Loopback), und der
/// falsche fuer alles andere. Deshalb bindet `vig serve` per Voreinstellung
/// auf Loopback: wer den Endpunkt oeffnet, muss beides ausdruecklich tun.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// Serverzertifikat im PEM-Format.
    ///
    /// Zusammen mit [`Self::key`] schaltet es TLS ein. Eines ohne das andere
    /// ist ein Konfigurationsfehler und kein halber Schutz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_cert: Option<PathBuf>,
    /// Privater Schluessel zum Serverzertifikat, PEM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_key: Option<PathBuf>,
    /// Zertifizierungsstelle, gegen die Clientzertifikate geprueft werden.
    ///
    /// Gesetzt heisst **mTLS**: ohne gueltiges Clientzertifikat kommt keine
    /// Verbindung zustande. Das ist die belastbare Variante — ein Token reist
    /// in jeder Anfrage mit und kann kopiert werden, ein privater Schluessel
    /// nicht.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ca: Option<PathBuf>,
    /// Datei mit erlaubten Bearer-Token, eines je Zeile.
    ///
    /// Die pragmatische Variante fuer Umgebungen ohne Zertifikatsverwaltung.
    /// Leerzeilen und `#`-Kommentare werden ignoriert. Ist die Datei gesetzt,
    /// wird **jede** Anfrage ohne gueltiges Token abgelehnt — auch die an
    /// unkonfigurierte Modelle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
}

/// Wie weit der Governor Aufrufern vertraut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrustMode {
    /// Alle Aufrufer sind vertrauenswuerdig (Spec L-002).
    ///
    /// Unkonfigurierte Modelle werden unveraendert durchgereicht, und ein
    /// Client darf seine Wichtigkeitsklasse selbst angeben. Richtig fuer den
    /// Fall, fuer den der Governor gebaut ist: ein Geraet, ein Betreiber,
    /// ein abgeschlossenes Netz.
    #[default]
    Open,
    /// Nur konfigurierte Arbeit, und die Klasse bestimmt die Konfiguration.
    ///
    /// Ein unkonfiguriertes Modell wird abgelehnt, statt am Governor vorbei
    /// ausgefuehrt zu werden — sonst genuegt der physische Modellname, um die
    /// gesamte Steuerung zu umgehen. Eine Clientangabe darf die Klasse nur
    /// **senken**, nie anheben: sonst setzt sich jeder Aufrufer selbst auf
    /// `protected`, und die Prioritaeten sind eine Empfehlung.
    Strict,
}

const fn default_inference_timeout_ms() -> u64 {
    30_000
}

const fn default_max_inflight_mib() -> u64 {
    512
}

impl SecurityConfig {
    /// Wahr, wenn TLS eingeschaltet ist.
    #[must_use]
    pub const fn tls_enabled(&self) -> bool {
        self.tls_cert.is_some() && self.tls_key.is_some()
    }

    /// Prueft die Sicherheitskonfiguration auf Widersprueche.
    ///
    /// Ein halb konfiguriertes TLS ist gefaehrlicher als gar keins: es sieht
    /// nach Schutz aus und ist keiner. Deshalb wird es abgelehnt statt
    /// ignoriert.
    fn validate(&self, findings: &mut Vec<Located>) {
        match (&self.tls_cert, &self.tls_key) {
            (Some(_), None) => findings.push(
                ConfigError::Missing {
                    what: "tls_key gehoert zu tls_cert",
                }
                .at("backend.security.tls_key"),
            ),
            (None, Some(_)) => findings.push(
                ConfigError::Missing {
                    what: "tls_cert gehoert zu tls_key",
                }
                .at("backend.security.tls_cert"),
            ),
            _ => {}
        }
        if self.client_ca.is_some() && !self.tls_enabled() {
            findings.push(
                ConfigError::Missing {
                    what: "client_ca braucht TLS; Clientzertifikate ohne TLS-Verbindung gibt es nicht",
                }
                .at("backend.security.client_ca"),
            );
        }
        for (label, path) in [
            ("tls_cert", self.tls_cert.as_ref()),
            ("tls_key", self.tls_key.as_ref()),
            ("client_ca", self.client_ca.as_ref()),
            ("token_file", self.token_file.as_ref()),
        ] {
            if let Some(p) = path
                && !p.exists()
            {
                findings.push(
                    ConfigError::Missing {
                        what: "Datei nicht gefunden",
                    }
                    .at(format!("backend.security.{label}")),
                );
            }
        }
    }
}

impl BackendConfig {
    /// Prueft die Betriebsgrenzen und gibt das Inferenztimeout zurueck.
    fn limits(&self, findings: &mut Vec<Located>) -> Option<Duration> {
        self.security.validate(findings);
        match duration_ms(self.inference_timeout_ms, "inference_timeout_ms") {
            Ok(d) => Some(d),
            Err(e) => {
                findings.push(e.at("backend.inference_timeout_ms"));
                None
            }
        }
    }
}

const fn default_pipelining() -> usize {
    1
}

const fn default_margin_percent() -> u32 {
    110
}

/// Ein logisches Modell.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// Wichtigkeitsklasse: `protected`, `high`, `normal`, `best_effort`.
    pub class: String,
    /// Queue-Verhalten.
    pub queue: QueueConfigYaml,
    /// Zeitvertrag.
    pub contract: ContractConfig,
    /// Physische Varianten, absteigend nach Qualitaet.
    pub variants: Vec<VariantConfig>,
    /// Die zugesagte Schnittstelle dieses logischen Modells.
    ///
    /// Ohne Angabe vergleicht der Governor die Varianten nur untereinander und
    /// schaltet die automatische Wahl bei Abweichung ab. Mit Angabe prueft er
    /// jede Variante gegen die Zusage und verweigert den Start, wenn eine sie
    /// nicht erfuellt. Siehe [`IoSignature`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_signature: Option<IoSignature>,
    /// Wahr fuer sequenzbasierte Modelle (Spec 12.5).
    #[serde(default)]
    pub stateful: bool,
    /// Zerlegbarkeit in kooperative Quanten (ADR-0014).
    ///
    /// Nur fuer Modelle, deren Arbeit fachlich zerlegbar ist — also
    /// generative. Ein Detektor gehoert nicht dazu; sein Vorwaertslauf ist
    /// unteilbar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooperative: Option<CooperativeConfig>,
    /// Wahr, wenn das Backendmodell nur ueber den Stream-Endpunkt antwortet.
    ///
    /// Generative Backends — vLLM, TensorRT-LLM — sind in Triton grundsaetzlich
    /// decoupled. Das ist eine Eigenschaft des **Modells**, nicht der
    /// Zerlegung: ein decoupled Modell braucht den Stream-Aufruf auch dann,
    /// wenn es gar nicht in Quanten zerlegt wird. Beides zu vermengen macht
    /// jeden Vergleich zwischen "mit" und "ohne Zerlegung" wertlos.
    #[serde(default)]
    pub decoupled: bool,
    /// Abweichender Backend-Endpunkt fuer dieses Modell.
    ///
    /// Reale Anlagen trennen Vision- und Sprachmodelle auf verschiedene
    /// Server: die Backends brauchen unterschiedliche Bibliotheksstaende und
    /// lassen sich nicht in einem Prozess betreiben. Das aendert nichts an der
    /// Kapazitaetsrechnung — die Slots modellieren die **GPU**, nicht den
    /// Prozess, und zwei Server auf einer GPU teilen sich weiterhin eine
    /// Ausfuehrungseinheit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_endpoint: Option<String>,
}

/// Die Angaben eines zerlegbaren Modells.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CooperativeConfig {
    /// Gemessene Erzeugungsrate in Token je Sekunde.
    pub tokens_per_second: u32,
    /// Kleinste sinnvolle Quantengroesse.
    #[serde(default = "default_min_tokens")]
    pub min_tokens: u32,
    /// Obergrenze der insgesamt erzeugten Token je Auftrag.
    pub max_total_tokens: u32,
    /// Gemessene feste Kosten je Quantum in Mikrosekunden.
    ///
    /// Round-Trip, Backend-Scheduling und erneute Prefill-Berechnung. Ohne
    /// Vorgabe, weil ein geratener Wert hier besonders teuer ist: ist der
    /// Sockel so gross wie der Slack bis zur naechsten geschuetzten Ankunft,
    /// passt kein Quantum, egal wie klein. Auf der Messmaschine sind es
    /// 18.000 µs (siehe `docs/benchmark/wp26.md`).
    pub base_cost_us: u64,
    /// Gemessener Aufwand je Token bereits vorhandenen Kontexts, in
    /// Mikrosekunden (NV-16).
    ///
    /// Faehrt der Zustand im Prompt mit, rechnet **jede** Fortsetzung den
    /// gesamten bisherigen Kontext neu — und der waechst mit jedem erzeugten
    /// Token. Ohne diesen Wert plant der Governor jedes Quantum gleich teuer;
    /// die spaeten ziehen dann ueber ihre Luecke hinaus.
    ///
    /// Fehlt er, verhaelt sich die Zuschneidung wie vor NV-16: alte
    /// Konfigurationen bleiben unveraendert gueltig. Null ist auch der
    /// richtige Wert fuer ein Backend mit wirksamem Prefix-Cache — nur sollte
    /// das gemessen und nicht angenommen sein.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub prefill_per_token_us: u64,
    /// Hoechster zulaessiger Aufschlag der Zerlegung, in Promille (NV-16).
    ///
    /// Nicht gesetzt heisst: keine Grenze, es wird zerlegt wie bisher. Ist ein
    /// Wert gesetzt und der vorausgerechnete Aufschlag ueberschreitet ihn,
    /// laeuft der Auftrag ungeteilt. `vig doctor` nennt den Aufschlag in jedem
    /// Fall, auch ohne Grenze.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_overhead_permille: Option<u32>,
}

impl CooperativeConfig {
    /// Uebersetzt die Konfiguration in die Kernform.
    ///
    /// # Errors
    ///
    /// Wenn `base_cost_us` nicht als Dauer darstellbar ist.
    fn resolve(self) -> Result<vig_core::model::Cooperative, ConfigError> {
        Ok(vig_core::model::Cooperative {
            tokens_per_second: self.tokens_per_second,
            min_tokens: self.min_tokens,
            max_total_tokens: self.max_total_tokens,
            base_cost: duration_us(self.base_cost_us, "base_cost_us")?,
            prefill_per_token: duration_us(self.prefill_per_token_us, "prefill_per_token_us")?,
            max_overhead_permille: self.max_overhead_permille,
        })
    }
}

const fn default_min_tokens() -> u32 {
    8
}

/// Das Queue-Verhalten eines Modells.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueueConfigYaml {
    /// `latest`, `latest_per_key`, `fifo` oder `never_drop`.
    pub policy: String,
    /// Kapazitaet. Pflichtangabe — ein Default waere still gefaehrlich
    /// (Spec L-003).
    pub capacity: usize,
    /// `reject_new`, `reject_oldest_non_protected` oder `backpressure_client`.
    #[serde(default = "default_overflow")]
    pub overflow: String,
}

fn default_overflow() -> String {
    "reject_new".to_owned()
}

/// Der Zeitvertrag eines Modells.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContractConfig {
    /// Erwartete Periode; ohne sie gibt es keinen Look-ahead (Spec 10.8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_ms: Option<u64>,
    /// Relative Deadline ab Generation Time. Pflichtangabe.
    pub deadline_ms: u64,
    /// Fachliches Hoechstalter.
    ///
    /// Ohne diesen Wert kann Vigilant keine Arbeit als wertlos erkennen und
    /// verliert sein staerkstes Werkzeug (ADR-0010).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_ms: Option<u64>,
    /// Niedrigste akzeptable Variantenqualitaet, 0.0 bis 1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_quality: Option<f64>,
    /// Mindestverweildauer vor einer Variantenaufwertung (Spec 12.4).
    #[serde(default = "default_dwell_ms")]
    pub variant_dwell_ms: u64,
    /// Der versionierte Vertragszusatz (NV-02).
    ///
    /// Optional und additiv. Fehlt er, gilt der Vertrag wie vor NV-02 — alte
    /// Konfigurationsdateien bleiben unveraendert nutzbar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<ContractExtensionConfig>,
}

/// Der Vertragszusatz in der Konfiguration (NV-02).
///
/// Die Felder beschreiben, was der **Verbraucher** braucht — nicht, was
/// gemessen wurde. Anforderungen kommen vom Betreiber, Messwerte vom
/// Profiler; die eine Groesse aus der anderen abzuleiten waere der Weg zu
/// einem Vertrag, den man immer einhaelt, weil man ihn passend gemacht hat.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContractExtensionConfig {
    /// Formatversion des Zusatzes. Eine unbekannte Version wird abgelehnt.
    #[serde(default = "default_extension_version")]
    pub version: u32,
    /// Der Abtasttakt des Verbrauchers in Millisekunden.
    ///
    /// **Nicht** die Ankunftsperiode der Requests: der Vertragstakt kommt aus
    /// dem Vertrag. Wuerde er aus den angenommenen Requests abgeleitet,
    /// koennte man ihn durch Ablehnen aller Requests einhalten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumer_period_ms: Option<u64>,
    /// Versatz des ersten Abtastzeitpunkts in Millisekunden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_ms: Option<u64>,
    /// Zulaessige Schwankung eines Abtastzeitpunkts in Millisekunden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_jitter_ms: Option<u64>,
    /// Bis wohin die Zusage reicht: `governor` oder `consumer`.
    #[serde(default = "default_delivery_boundary")]
    pub delivery_boundary: String,
    /// Was der Verbraucher aus einer Lieferung macht:
    /// `latest_state`, `every_event` oder `stateful_sequence`.
    #[serde(default = "default_delivery_semantics")]
    pub delivery_semantics: String,
    /// Ob jeder Zyklus einen neuen Messwert verlangt.
    #[serde(default)]
    pub require_new_sample_each_cycle: bool,
    /// Ueber wie viele Zyklen beobachtet wird.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_window: Option<u32>,
    /// Die Weakly-hard-Bedingung.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miss_budget: Option<MissBudgetConfig>,
    /// Mindestfortschritt fuer Hintergrundlast, in Prozent der Zyklen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_background_progress_pct: Option<u32>,
    /// Die Kurznamen der freigegebenen Varianten.
    ///
    /// Leer heisst „alle freigegeben". Eine Liste, die keine existierende
    /// Variante trifft, wird abgelehnt — sonst waere ein Tippfehler eine
    /// stille Vollsperrung.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_variants: Vec<String>,
    /// Bis zu welcher Eingabegroesse in KiB der Vertrag beansprucht wird.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_kib: Option<u64>,
    /// Bis zu welcher serialisierten Auslastung in Prozent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_occupancy_pct: Option<u32>,
    /// Welche Nachweisstufe verlangt wird: `observed`, `qualified_slo`
    /// oder `proven`.
    ///
    /// Getrennt vom beobachteten SLO und niemals aus ihm abgeleitet.
    #[serde(default = "default_evidence")]
    pub evidence_required: String,
    /// Die Revision des Vertrags, vom Betreiber vergeben.
    #[serde(default = "default_contract_version")]
    pub contract_version: u32,
}

/// Die Weakly-hard-Bedingung in der Konfiguration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MissBudgetConfig {
    /// M — hoechstens so viele Misses je Fenster.
    pub max_misses: u32,
    /// K — die Fenstergroesse in Verbraucherzyklen.
    pub window_cycles: u32,
    /// L — hoechstens so viele Misses hintereinander.
    ///
    /// „Hoechstens zwei in hundert und nie zwei hintereinander" ist
    /// `max_misses: 2, window_cycles: 100, max_consecutive: 1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_consecutive: Option<u32>,
}

const fn default_extension_version() -> u32 {
    vig_core::contract_ext::CONTRACT_EXTENSION_VERSION
}

fn default_delivery_boundary() -> String {
    "governor".to_owned()
}

fn default_delivery_semantics() -> String {
    "latest_state".to_owned()
}

fn default_evidence() -> String {
    "observed".to_owned()
}

const fn default_contract_version() -> u32 {
    1
}

const fn default_dwell_ms() -> u64 {
    100
}

/// Eine physische Variante.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VariantConfig {
    /// Kurzname der Variante.
    pub id: String,
    /// Der Modellname im Backend.
    pub backend_model: String,
    /// Relative Qualitaet samt Herkunft (ADR-0007).
    pub quality: QualityConfig,
    /// Laufzeitprofil.
    ///
    /// Im Regelfall von `vig profile` erzeugt. Fehlt es, kann der
    /// Scheduler nicht planen — dann wird der Start verweigert, statt mit
    /// geratenen Laufzeiten zu arbeiten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileConfig>,
    /// Laufzeitprofile unter Nebenlast, aufsteigend nach Belegungsgrad.
    ///
    /// Index 0 beschreibt „ein weiterer Slot ist belegt", Index 1 „zwei
    /// weitere" und so fort; [`Self::profile`] bleibt der Alleinbetrieb.
    ///
    /// Von `vig calibrate` erzeugt (WP12). Fehlt die Liste, plant der
    /// Governor unter Nebenlast mit dem Alleinprofil — sichtbar optimistisch,
    /// weshalb der Online Estimator es als Erstes korrigiert (ADR-0006).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub under_load: Vec<ProfileConfig>,
    /// Was diese Variante fachlich liefert und verlangt (NV-10).
    ///
    /// Ohne diesen Block entscheidet allein die I/O-Signatur ueber
    /// Austauschbarkeit — wie vor NV-10. Beschreibt **eine** Variante eines
    /// Modells ihre Bedeutung, muessen es alle tun: eine halb beschriebene
    /// Variantenreihe ist gefaehrlicher als eine gar nicht beschriebene, weil
    /// sie nach Sorgfalt aussieht.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantics: Option<SemanticsConfig>,
    /// Zeit fuer Vor- und Nachverarbeitung dieser Variante, in Mikrosekunden.
    ///
    /// Resize, Normierung, Boxdekodierung — Arbeit, die **ausserhalb** des
    /// Backends entsteht und deshalb aus jedem Backendprofil herausfaellt.
    /// Zwei Varianten mit verschiedener Eingabeaufloesung unterscheiden sich
    /// hier oft mehr als in der Inferenz selbst.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub preprocess_us: u64,
}

// Serde verlangt eine Referenz in `skip_serializing_if`; darauf hat der
// Aufrufer keinen Einfluss.
#[allow(clippy::trivially_copy_pass_by_ref, reason = "Serde-Signatur")]
const fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// Die fachliche Beschreibung einer Variante (NV-10).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticsConfig {
    /// Was die Variante an ihrer Eingabe verlangt.
    #[serde(default)]
    pub input: InputContractConfig,
    /// Was sie ausgibt, in Ausgabereihenfolge.
    pub outputs: Vec<OutputSemanticsConfig>,
}

/// Der Eingabevertrag einer Variante.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputContractConfig {
    /// Tensorlayout, etwa `nchw`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layout: String,
    /// Farbraum, etwa `rgb` oder `bgr`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color_space: String,
    /// Normierung, etwa `imagenet`, `zero_one` oder `none`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub normalization: String,
    /// Erwartete Breite in Pixeln.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub width: u32,
    /// Erwartete Hoehe in Pixeln.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub height: u32,
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "Serde-Signatur")]
const fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// Die Bedeutung einer Ausgabe.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSemanticsConfig {
    /// Die fachliche Art: `detections`, `keypoints`, `depth`,
    /// `classification`, `segmentation`, `text` oder `opaque`.
    pub kind: String,
    /// Die Labels, **in ihrer Reihenfolge**.
    ///
    /// Die Reihenfolge ist die Bedeutung: dieselben Klassen anders sortiert
    /// ergeben dieselben Zahlen mit anderem Inhalt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Das Bezugssystem: `normalized`, `input_pixels`, `source_pixels`
    /// oder `meters`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub coordinates: String,
    /// Die Einheit: `none`, `probability`, `logits`, `meters`,
    /// `millimeters` oder `inverse_depth`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unit: String,
    /// Das Layout, etwa `xyxy` oder `cxcywh`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layout: String,
}

/// Der Qualitaetswert einer Variante samt Herkunft.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QualityConfig {
    /// Relativer Wert zwischen 0.0 und 1.0.
    pub value: f64,
    /// `measured`, `user_declared` oder `unknown`.
    ///
    /// Ohne Angabe gilt `unknown`, und das deaktiviert die automatische
    /// Variantenwahl fuer dieses Modell (ADR-0007). Der vorsichtige Default
    /// ist Absicht: wer nichts sagt, bekommt keine automatische Degradation
    /// auf Basis einer geratenen Zahl.
    #[serde(default = "default_quality_source")]
    pub source: String,
    /// Datensatz, auf dem gemessen wurde.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_on: Option<String>,
}

fn default_quality_source() -> String {
    "unknown".to_owned()
}

/// Baut aus Allein- und Nebenlastprofilen ein Stufenprofil (ADR-0004, WP12).
///
/// Ohne Nebenlastmessungen bleibt es beim Alleinprofil. Das ist der ehrlichere
/// Ausgangspunkt als eine geratene Verlangsamung: es ist erkennbar optimistisch
/// und wird vom Online Estimator zuerst korrigiert.
fn build_variant_profile(
    solo: RuntimeProfile,
    under_load: &[ProfileConfig],
) -> Result<VariantProfile, vig_core::profile::ProfileError> {
    if under_load.is_empty() {
        return Ok(VariantProfile::solo(solo));
    }
    let mut levels = vig_core::arrayvec::ArrayVec::new();
    let _ = levels.push(solo);
    for p in under_load {
        let level = RuntimeProfile::new(
            Duration::from_nanos_unbounded(p.p50_us.saturating_mul(1_000)),
            Duration::from_nanos_unbounded(p.p95_us.saturating_mul(1_000)),
            Duration::from_nanos_unbounded(p.p99_us.saturating_mul(1_000)),
            p.samples,
        )?;
        if levels.push(level).is_err() {
            // Mehr Stufen als Slots ist keine feinere Messung, sondern ein
            // Konfigurationsfehler.
            break;
        }
    }
    VariantProfile::from_levels(levels)
}

/// Uebersetzt die fachliche Beschreibung einer Variante (NV-10).
///
/// # Errors
///
/// Wenn eine Art, ein Bezugssystem oder eine Einheit unbekannt ist. Ein
/// Tippfehler darf nicht als „nicht angegeben" durchgehen: nicht angegeben
/// schaltet die automatische Variantenwahl ab, ein Tippfehler wuerde sie
/// stillschweigend auf eine falsche Bedeutung stellen.
fn build_semantics(config: Option<&SemanticsConfig>) -> Result<VariantSemantics, ConfigError> {
    let Some(config) = config else {
        return Ok(VariantSemantics::default());
    };
    if config.outputs.is_empty() {
        return Err(ConfigError::Missing {
            what: "mindestens eine Ausgabe, sonst ist der Block leer und irrefuehrend",
        });
    }
    if config.outputs.len() > vig_core::semantics::MAX_OUTPUTS {
        return Err(ConfigError::OutOfRange {
            expected: "hoechstens acht beschriebene Ausgaben",
        });
    }

    let mut outputs = ArrayVec::new();
    for output in &config.outputs {
        let kind = match output.kind.as_str() {
            "detections" => OutputKind::Detections,
            "keypoints" => OutputKind::Keypoints,
            "depth" => OutputKind::Depth,
            "classification" => OutputKind::Classification,
            "segmentation" => OutputKind::Segmentation,
            "text" => OutputKind::Text,
            "opaque" => OutputKind::Opaque,
            other => {
                return Err(ConfigError::UnknownValue {
                    found: other.to_owned(),
                    allowed: "detections, keypoints, depth, classification, \
                              segmentation, text, opaque",
                });
            }
        };
        let coordinates = match output.coordinates.as_str() {
            "" => CoordinateConvention::Unspecified,
            "normalized" => CoordinateConvention::Normalized,
            "input_pixels" => CoordinateConvention::InputPixels,
            "source_pixels" => CoordinateConvention::SourcePixels,
            "meters" => CoordinateConvention::Meters,
            other => {
                return Err(ConfigError::UnknownValue {
                    found: other.to_owned(),
                    allowed: "normalized, input_pixels, source_pixels, meters",
                });
            }
        };
        let unit = match output.unit.as_str() {
            "" => Unit::Unspecified,
            "none" => Unit::None,
            "probability" => Unit::Probability,
            "logits" => Unit::Logits,
            "meters" => Unit::Meters,
            "millimeters" => Unit::Millimeters,
            "inverse_depth" => Unit::InverseDepth,
            other => {
                return Err(ConfigError::UnknownValue {
                    found: other.to_owned(),
                    allowed: "none, probability, logits, meters, millimeters, inverse_depth",
                });
            }
        };
        let semantics = OutputSemantics {
            kind,
            labels: LabelSet::of(output.labels.iter().map(String::as_str)),
            coordinates,
            unit,
            layout: vig_core::semantics::tag(&output.layout),
        };
        if outputs.push(semantics).is_err() {
            return Err(ConfigError::OutOfRange {
                expected: "hoechstens acht beschriebene Ausgaben",
            });
        }
    }

    Ok(VariantSemantics {
        input: InputContract {
            layout: vig_core::semantics::tag(&config.input.layout),
            color_space: vig_core::semantics::tag(&config.input.color_space),
            normalization: vig_core::semantics::tag(&config.input.normalization),
            width: config.input.width,
            height: config.input.height,
        },
        outputs,
    })
}

/// Die Profil-Fingerabdruecke eines Modells, in Variantenreihenfolge (G-010).
///
/// `None` heisst "nicht hinterlegt" und nicht "passt nicht" — der Unterschied
/// entscheidet spaeter darueber, ob die Marge angehoben wird (ADR-0016).
fn fingerprints_of(model: &ModelConfig) -> Vec<Option<String>> {
    model
        .variants
        .iter()
        .map(|v| v.profile.as_ref().and_then(|p| p.fingerprint.clone()))
        .collect()
}

/// Die Profilmanifeste eines Modells, in Variantenreihenfolge (NV-03).
///
/// Ein Profil ohne Manifestblock liefert hier ein Legacy-Manifest, kein
/// `None` — der Unterschied zwischen "kein Profil" und "Profil ohne
/// Herkunftsangabe" soll nicht verschwinden. Wo gar kein Profil hinterlegt
/// ist, steht `None`.
fn manifests_of(model: &ModelConfig) -> Vec<Option<crate::manifest::ProfileManifest>> {
    model
        .variants
        .iter()
        .map(|v| {
            v.profile.as_ref().map(|p| {
                p.manifest
                    .clone()
                    .unwrap_or_else(crate::manifest::ProfileManifest::legacy)
            })
        })
        .collect()
}

/// Ein Laufzeitprofil je Belegungsgrad.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    /// Median in Mikrosekunden.
    pub p50_us: u64,
    /// 95-%-Quantil in Mikrosekunden.
    pub p95_us: u64,
    /// 99-%-Quantil in Mikrosekunden.
    pub p99_us: u64,
    /// Anzahl der Messungen.
    pub samples: u32,
    /// Der Fingerabdruck der Umgebung, unter der gemessen wurde (G-010).
    ///
    /// Von `vig profile` eingetragen. Weicht er beim Start von dem ab,
    /// was das Backend meldet, gilt das Profil als nicht verifiziert und wird
    /// vorsichtiger geplant (ADR-0016). Fehlt er, kann nichts verglichen
    /// werden — `doctor` sagt das dann auch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Woher dieses Profil stammt und wofuer es gilt (NV-03).
    ///
    /// Der Fingerabdruck oben sagt, *ob sich die Metadatenlage geaendert hat*.
    /// Das Manifest sagt, *was* gemessen wurde, *worauf* und *unter welchen
    /// Bedingungen* — und faengt damit den Fall, den ein Hash ueber Metadaten
    /// nicht fangen kann: dieselbe Signatur, andere Gewichte.
    ///
    /// Fehlt es, ist das Profil ein Legacy-Profil. Es bleibt nutzbar, gilt
    /// aber als unbelegt, nicht als geprueft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<crate::manifest::ProfileManifest>,
}

/// Die aufgeloeste, gepruefte Konfiguration.
#[derive(Debug)]
pub struct Resolved {
    /// gRPC-Endpunkt des Backends.
    pub backend_endpoint: String,
    /// Die Slot-Menge samt Co-Run-Verboten.
    pub slots: SlotSet,
    /// Die Modellvertraege, in Indexreihenfolge.
    pub contracts: ArrayVec<ModelContract, MAX_MODELS>,
    /// Logische Modellnamen, in Indexreihenfolge.
    pub model_names: Vec<String>,
    /// Backend-Modellnamen je `[Modell][Variante]`.
    pub backend_models: Vec<Vec<String>>,
    /// Der beim Profilieren aufgezeichnete Fingerabdruck je `[Modell][Variante]`.
    ///
    /// `None`, wenn das Profil vor Einfuehrung von G-010 entstanden ist oder
    /// von Hand geschrieben wurde.
    pub profile_fingerprints: Vec<Vec<Option<String>>>,
    /// Das Profilmanifest je `[Modell][Variante]` (NV-03).
    ///
    /// `None`, wo kein Profil hinterlegt ist. Ein Profil ohne Manifestblock
    /// erscheint als Legacy-Manifest (Revision 1), in dem jedes Feld
    /// `unknown` ist.
    pub profile_manifests: Vec<Vec<Option<crate::manifest::ProfileManifest>>>,
    /// Der Backend-Endpunkt je Modell, in Indexreihenfolge.
    pub model_endpoints: Vec<String>,
    /// Pfad zum Modellrepository, sofern konfiguriert (NV-03).
    pub model_repository: Option<String>,
    /// Ob das Backendmodell nur ueber den Stream-Endpunkt antwortet.
    pub model_decoupled: Vec<bool>,
    /// Die Sicherheitsmarge.
    pub margin: SafetyMargin,
    /// Ab wann ein Backendaufruf als haengend gilt.
    pub inference_timeout: Duration,
    /// Wie weit Aufrufern vertraut wird.
    pub trust: TrustMode,
    /// Obergrenze fuer gleichzeitig gehaltene Requestnutzlast, in Bytes.
    pub max_inflight_bytes: u64,
    /// Transport- und Zugangssicherung.
    pub security: SecurityConfig,
    /// Die zugesagte Schnittstelle je Modell, in Indexreihenfolge.
    ///
    /// `None`, wo keine hinterlegt ist.
    pub io_signatures: Vec<Option<IoSignature>>,
}

impl Resolved {
    /// Den Index eines logischen Modells nachschlagen.
    #[must_use]
    pub fn model_index(&self, name: &str) -> Option<ModelIdx> {
        self.model_names
            .iter()
            .position(|n| n == name)
            .and_then(|i| u16::try_from(i).ok())
            .map(ModelIdx)
    }

    /// Die geschuetzte serialisierte Auslastung in Promille (Spec 10.9).
    ///
    /// `U = Summe(C_i / T_i)` ueber alle geschuetzten Modelle, geteilt durch die
    /// Slotzahl. `C_i` ist die **konservative** Planungslaufzeit, also
    /// `p99 * Sicherheitsmarge` — genau der Wert, mit dem der Scheduler
    /// tatsaechlich plant.
    ///
    /// Keine vollstaendige Schedulability-Garantie: bei realer Nebenlaeufigkeit
    /// und nicht-praeemptiven Abschnitten waere das falsch. Aber ein Wert ueber
    /// 1000 bedeutet, dass die geschuetzten Vertraege allein die Kapazitaet
    /// uebersteigen — dann bleibt fuer Best-Effort-Arbeit strukturell nichts
    /// uebrig, und der Look-ahead wird jeden Start verhindern.
    ///
    /// Diese Rechnung gehoert hierher und nicht nur ins CLI: wer eine
    /// Konfiguration programmatisch aufloest, braucht dieselbe Warnung.
    #[must_use]
    pub fn protected_utilization_permille(&self) -> u64 {
        let mut total = 0_u64;
        for contract in self.contracts.iter() {
            if !contract.criticality.is_guarded() {
                continue;
            }
            let (Some(period), Some(best)) = (contract.period, contract.variants.get(0)) else {
                continue;
            };
            let Ok(runtime) = best.profile.conservative_at(0, self.margin) else {
                continue;
            };
            let share = runtime
                .as_nanos()
                .saturating_mul(1_000)
                .checked_div(period.as_nanos().max(1))
                .unwrap_or(0);
            total = total.saturating_add(share);
        }
        let slots = u64::try_from(self.slots.len()).unwrap_or(1).max(1);
        total.checked_div(slots).unwrap_or(0)
    }

    /// Der Backend-Endpunkt eines Modells.
    #[must_use]
    pub fn endpoint_of(&self, model: ModelIdx) -> &str {
        self.model_endpoints
            .get(model.get())
            .map_or(self.backend_endpoint.as_str(), String::as_str)
    }

    /// Ob ein Modell nur ueber den Stream-Endpunkt antwortet.
    #[must_use]
    pub fn is_decoupled(&self, model: ModelIdx) -> bool {
        self.model_decoupled
            .get(model.get())
            .copied()
            .unwrap_or(false)
    }

    /// Alle verwendeten Backend-Endpunkte, ohne Doppelungen.
    #[must_use]
    pub fn endpoints(&self) -> Vec<String> {
        let mut all = vec![self.backend_endpoint.clone()];
        for endpoint in &self.model_endpoints {
            if !all.contains(endpoint) {
                all.push(endpoint.clone());
            }
        }
        all
    }

    /// Den Backend-Modellnamen einer Variante nachschlagen.
    #[must_use]
    pub fn backend_model(&self, model: ModelIdx, variant: usize) -> Option<&str> {
        self.backend_models
            .get(model.get())?
            .get(variant)
            .map(String::as_str)
    }
}

fn parse_class(value: &str) -> Result<Criticality, ConfigError> {
    match value {
        "protected" => Ok(Criticality::Protected),
        "high" => Ok(Criticality::High),
        "normal" => Ok(Criticality::Normal),
        "best_effort" => Ok(Criticality::BestEffort),
        other => Err(ConfigError::UnknownValue {
            found: other.to_owned(),
            allowed: "protected, high, normal, best_effort",
        }),
    }
}

fn parse_policy(value: &str) -> Result<QueuePolicy, ConfigError> {
    match value {
        "latest" => Ok(QueuePolicy::Latest),
        "latest_per_key" => Ok(QueuePolicy::LatestPerKey),
        "fifo" => Ok(QueuePolicy::Fifo),
        "never_drop" => Ok(QueuePolicy::NeverDrop),
        other => Err(ConfigError::UnknownValue {
            found: other.to_owned(),
            allowed: "latest, latest_per_key, fifo, never_drop",
        }),
    }
}

fn parse_overflow(value: &str) -> Result<OverflowPolicy, ConfigError> {
    match value {
        "reject_new" => Ok(OverflowPolicy::RejectNew),
        "reject_oldest_non_protected" => Ok(OverflowPolicy::RejectOldestNonProtected),
        "backpressure_client" => Ok(OverflowPolicy::BackpressureClient),
        other => Err(ConfigError::UnknownValue {
            found: other.to_owned(),
            allowed: "reject_new, reject_oldest_non_protected, backpressure_client",
        }),
    }
}

fn parse_quality_source(value: &str) -> Result<QualitySource, ConfigError> {
    match value {
        "measured" => Ok(QualitySource::Measured),
        "user_declared" => Ok(QualitySource::UserDeclared),
        "unknown" => Ok(QualitySource::Unknown),
        other => Err(ConfigError::UnknownValue {
            found: other.to_owned(),
            allowed: "measured, user_declared, unknown",
        }),
    }
}

/// Wandelt einen Qualitaetswert aus `[0.0, 1.0]` in Tausendstel.
///
/// Der Umweg ueber Tausendstel ist Absicht: der Kern vergleicht Qualitaeten und
/// sortiert nach ihnen, und beides soll nicht von Gleitkommarundung abhaengen.
fn quality_from_f64(value: f64) -> Result<Quality, ConfigError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(ConfigError::OutOfRange {
            expected: "quality.value zwischen 0.0 und 1.0",
        });
    }
    // Nach der Bereichspruefung liegt das Ergebnis sicher in [0, 1000].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let milli = (value * 1_000.0).round() as u16;
    Quality::from_milli(milli).ok_or(ConfigError::OutOfRange {
        expected: "quality.value zwischen 0.0 und 1.0",
    })
}

fn duration_ms(value: u64, what: &'static str) -> Result<Duration, ConfigError> {
    Duration::from_millis(value).ok_or(ConfigError::OutOfRange { expected: what })
}

fn duration_us(value: u64, what: &'static str) -> Result<Duration, ConfigError> {
    Duration::from_micros(value).ok_or(ConfigError::OutOfRange { expected: what })
}

impl Config {
    /// Serialisiert die Konfiguration zurueck nach YAML.
    ///
    /// Kommentare gehen dabei verloren — YAML wird ueber `serde` gelesen und
    /// geschrieben, nicht als Text bearbeitet. Deshalb schreibt `calibrate` in
    /// eine **neue** Datei und ueberschreibt nie die Vorlage: eine von Hand
    /// gepflegte Konfiguration enthaelt Begruendungen, und die sind mehr wert
    /// als die Bequemlichkeit einer Ersetzung an Ort und Stelle.
    ///
    /// # Errors
    ///
    /// Wenn die Struktur nicht als YAML darstellbar ist.
    pub fn to_yaml(&self) -> Result<String, Located> {
        serde_norway::to_string(self).map_err(|e| {
            ConfigError::Syntax {
                message: e.to_string(),
            }
            .at("<ausgabe>")
        })
    }

    /// Liest eine Konfiguration aus YAML.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Syntax`], wenn das YAML nicht lesbar ist. Unbekannte
    /// Felder sind ein Fehler, kein Hinweis: ein vertippter Schluessel wuerde
    /// sonst stillschweigend ignoriert, und der Nutzer glaubte, er haette
    /// etwas konfiguriert (Spec L-020).
    pub fn from_yaml(text: &str) -> Result<Self, Located> {
        serde_norway::from_str(text).map_err(|e| {
            ConfigError::Syntax {
                message: e.to_string(),
            }
            .at("<datei>")
        })
    }

    /// Die Backend-Adresse und die physischen Modellnamen, ohne
    /// vollstaendige Aufloesung.
    ///
    /// `vig profile` braucht genau das und **nicht mehr**: es soll
    /// Laufzeitprofile erst erzeugen. Wuerde es die volle Aufloesung
    /// verlangen, muesste die Konfiguration bereits Profile enthalten — der
    /// Nutzer haette also von Hand hinschreiben muessen, was das Werkzeug
    /// gerade messen soll. Der Workflow aus Spec 6.3 (doctor, profile, serve)
    /// waere damit unbenutzbar.
    #[must_use]
    pub fn profiling_targets(&self) -> (String, Vec<(String, Vec<String>)>) {
        let models = self
            .models
            .iter()
            .map(|(name, model)| {
                let variants = model
                    .variants
                    .iter()
                    .map(|v| v.backend_model.clone())
                    .collect();
                (name.clone(), variants)
            })
            .collect();
        (self.backend.grpc_endpoint.clone(), models)
    }

    /// Prueft die Konfiguration und uebersetzt sie in Kerntypen.
    ///
    /// # Errors
    ///
    /// Der **erste** gefundene Fehler samt Fundstelle. `vig doctor` ruft
    /// stattdessen [`Config::diagnose`] auf und bekommt alle Befunde.
    pub fn resolve(&self) -> Result<Resolved, Located> {
        let mut findings = Vec::new();
        let resolved = self.resolve_collecting(&mut findings);
        match findings.into_iter().next() {
            Some(first) => Err(first),
            None => resolved.ok_or_else(|| ConfigError::NoModels.at("models")),
        }
    }

    /// Sammelt **alle** Befunde, ohne beim ersten abzubrechen.
    ///
    /// Das ist der Modus fuer `vig doctor` (Spec 23): wer eine
    /// Konfiguration repariert, will alle Probleme auf einmal sehen und nicht
    /// nach jedem Lauf ein neues entdecken.
    #[must_use]
    pub fn diagnose(&self) -> Vec<Located> {
        let mut findings = Vec::new();
        let _ = self.resolve_collecting(&mut findings);
        findings
    }

    /// Traegt die Co-Run-Verbote in die Slot-Menge ein (ADR-0006).
    fn apply_corun_rules(
        rules: &[[String; 2]],
        model_names: &[String],
        slots: &mut SlotSet,
        findings: &mut Vec<Located>,
    ) {
        for (i, pair) in rules.iter().enumerate() {
            let path = format!("backend.no_corun[{i}]");
            let mut indices = Vec::new();
            for name in pair {
                match model_names.iter().position(|n| n == name) {
                    Some(idx) => indices.push(idx),
                    None => findings.push(
                        ConfigError::UnknownModelReference { name: name.clone() }.at(path.clone()),
                    ),
                }
            }
            if let [a, b] = indices.as_slice()
                && let (Ok(a), Ok(b)) = (u16::try_from(*a), u16::try_from(*b))
            {
                slots.forbid_corun(ModelIdx(a), ModelIdx(b));
            }
        }
    }

    // Diese Funktion ist lang, weil sie **eine** Sache tut: aus einer
    // Konfigurationsdatei einen geprueften Laufzeitzustand machen, und dabei
    // jeden Befund an seiner Fundstelle melden. Sie aufzuteilen hiesse, den
    // `findings`-Puffer durch drei Ebenen zu reichen — die Fehlermeldung wuerde
    // dadurch nicht besser, nur die Zeilenzahl kleiner.
    #[expect(clippy::too_many_lines, reason = "eine zusammenhaengende Aufloesung")]
    fn resolve_collecting(&self, findings: &mut Vec<Located>) -> Option<Resolved> {
        if self.version != SCHEMA_VERSION {
            findings.push(
                ConfigError::UnsupportedVersion {
                    found: self.version,
                    supported: SCHEMA_VERSION,
                }
                .at("version"),
            );
        }
        // Vigilant spricht das Open Inference Protocol, und das ist ein
        // offener Standard. `triton` bleibt als Name erhalten, weil er in
        // bestehenden Konfigurationen steht; `oip` ist der ehrlichere.
        //
        // Unbekannte Werte bleiben ein Fehler: ein vertippter Backendname
        // wuerde sonst stillschweigend angenommen (Spec L-020). Welche
        // Erweiterungen ein Server tatsaechlich beherrscht, entscheidet
        // ohnehin nicht dieser Name, sondern die Abfrage beim Start.
        if !matches!(self.backend.kind.as_str(), "triton" | "oip" | "kserve") {
            findings.push(
                ConfigError::UnknownValue {
                    found: self.backend.kind.clone(),
                    allowed: "triton, oip, kserve",
                }
                .at("backend.type"),
            );
        }
        if self.models.is_empty() {
            findings.push(ConfigError::NoModels.at("models"));
            return None;
        }
        if self.models.len() > MAX_MODELS {
            findings.push(
                ConfigError::TooManyModels {
                    found: self.models.len(),
                    maximum: MAX_MODELS,
                }
                .at("models"),
            );
            return None;
        }

        let margin = SafetyMargin::from_percent(self.backend.safety_margin_percent)
            .unwrap_or(SafetyMargin::DEFAULT);
        if SafetyMargin::from_percent(self.backend.safety_margin_percent).is_none() {
            findings.push(
                ConfigError::OutOfRange {
                    expected: "safety_margin_percent zwischen 100 und 300",
                }
                .at("backend.safety_margin_percent"),
            );
        }

        let mut slots =
            match SlotSet::homogeneous(self.backend.slots, self.backend.pipelining_depth) {
                Ok(s) => s,
                Err(e) => {
                    findings.push(ConfigError::Slots(e).at("backend.slots"));
                    return None;
                }
            };

        let model_names: Vec<String> = self.models.keys().cloned().collect();
        let model_endpoints: Vec<String> = self
            .models
            .values()
            .map(|m| {
                m.backend_endpoint
                    .clone()
                    .unwrap_or_else(|| self.backend.grpc_endpoint.clone())
            })
            .collect();
        let model_decoupled: Vec<bool> = self.models.values().map(|m| m.decoupled).collect();
        let io_signatures: Vec<Option<IoSignature>> = self
            .models
            .values()
            .map(|m| m.io_signature.clone())
            .collect();
        let mut contracts = ArrayVec::new();
        let mut backend_models = Vec::new();
        let mut profile_fingerprints: Vec<Vec<Option<String>>> = Vec::new();
        let mut profile_manifests: Vec<Vec<Option<crate::manifest::ProfileManifest>>> = Vec::new();

        for (name, model) in &self.models {
            let path = format!("models.{name}");
            match model.to_contract(&path, findings) {
                Some((contract, physical)) => {
                    if contracts.push(contract).is_err() {
                        findings.push(
                            ConfigError::TooManyModels {
                                found: self.models.len(),
                                maximum: MAX_MODELS,
                            }
                            .at("models"),
                        );
                        return None;
                    }
                    backend_models.push(physical);
                    profile_fingerprints.push(fingerprints_of(model));
                    profile_manifests.push(manifests_of(model));
                }
                None => return None,
            }
        }

        Self::apply_corun_rules(&self.backend.no_corun, &model_names, &mut slots, findings);

        let inference_timeout = self.backend.limits(findings)?;

        Some(Resolved {
            inference_timeout,
            trust: self.backend.trust,
            max_inflight_bytes: self.backend.max_inflight_mib.saturating_mul(1024 * 1024),
            security: self.backend.security.clone(),
            backend_endpoint: self.backend.grpc_endpoint.clone(),
            slots,
            contracts,
            model_names,
            backend_models,
            profile_fingerprints,
            profile_manifests,
            model_repository: self.backend.model_repository.clone(),
            model_endpoints,
            model_decoupled,
            io_signatures,
            margin,
        })
    }
}

impl ModelConfig {
    /// Loest die Variantenliste auf.
    ///
    /// Getrennt von [`ModelConfig::to_contract`], weil beides unterschiedliche
    /// Fehlerklassen hat: dort Vertragswerte, hier Qualitaet und Profile.
    fn resolve_variants(
        &self,
        path: &str,
        findings: &mut Vec<Located>,
    ) -> Option<(ArrayVec<Variant, MAX_VARIANTS>, Vec<String>)> {
        if self.variants.is_empty() {
            findings.push(
                ConfigError::Missing {
                    what: "mindestens eine Variante mit backend_model ist noetig",
                }
                .at(format!("{path}.variants")),
            );
            return None;
        }
        if self.variants.len() > MAX_VARIANTS {
            findings.push(
                ConfigError::OutOfRange {
                    expected: "hoechstens 8 Varianten je Modell",
                }
                .at(format!("{path}.variants")),
            );
            return None;
        }

        let mut variants = ArrayVec::new();
        let mut physical = Vec::new();
        for (i, v) in self.variants.iter().enumerate() {
            let sub = format!("{path}.variants[{i}]");
            let quality = match quality_from_f64(v.quality.value) {
                Ok(q) => q,
                Err(e) => {
                    findings.push(e.at(format!("{sub}.quality.value")));
                    Quality::FULL
                }
            };
            let source = match parse_quality_source(&v.quality.source) {
                Ok(s) => s,
                Err(e) => {
                    findings.push(e.at(format!("{sub}.quality.source")));
                    QualitySource::Unknown
                }
            };
            let Some(p) = v.profile.as_ref() else {
                findings.push(
                    ConfigError::Missing {
                        what: "kein Laufzeitprofil; `vig profile` ausfuehren oder \
                               profile: {p50_us, p95_us, p99_us, samples} angeben",
                    }
                    .at(format!("{sub}.profile")),
                );
                return None;
            };
            let profile = match RuntimeProfile::new(
                Duration::from_nanos_unbounded(p.p50_us.saturating_mul(1_000)),
                Duration::from_nanos_unbounded(p.p95_us.saturating_mul(1_000)),
                Duration::from_nanos_unbounded(p.p99_us.saturating_mul(1_000)),
                p.samples,
            ) {
                Ok(rp) => rp,
                Err(e) => {
                    findings.push(
                        ConfigError::OutOfRange {
                            expected: "p50 <= p95 <= p99 und mindestens 100 Messungen",
                        }
                        .at(format!("{sub}.profile ({e})")),
                    );
                    return None;
                }
            };
            let variant_profile = match build_variant_profile(profile, &v.under_load) {
                Ok(vp) => vp,
                Err(e) => {
                    findings.push(
                        ConfigError::OutOfRange {
                            expected: "gueltige Profile je Belegungsgrad, aufsteigend",
                        }
                        .at(format!("{sub}.under_load ({e})")),
                    );
                    return None;
                }
            };
            let semantics = match build_semantics(v.semantics.as_ref()) {
                Ok(sem) => sem,
                Err(e) => {
                    findings.push(e.at(format!("{sub}.semantics")));
                    return None;
                }
            };
            let _ = variants.push(Variant {
                quality: QualityValue {
                    value: quality,
                    source,
                },
                profile: variant_profile,
                semantics,
                preprocess: Duration::from_nanos_unbounded(v.preprocess_us.saturating_mul(1_000)),
            });
            physical.push(v.backend_model.clone());
        }
        Some((variants, physical))
    }
}

impl ModelConfig {
    /// Uebersetzt den Vertragszusatz, falls einer da ist (NV-02).
    ///
    /// `Err(())` heisst: der Zusatz ist unbrauchbar und die Befunde stehen
    /// bereits in `findings`. Ein halb verstandener Vertrag wird nicht
    /// benutzt — er koennte genau die Einschraenkung enthalten, auf die sich
    /// der Betreiber verlaesst.
    #[allow(clippy::too_many_lines, reason = "eine flache Feldabbildung")]
    fn build_extension(
        &self,
        path: &str,
        findings: &mut Vec<Located>,
    ) -> Result<Option<ContractExtension>, ()> {
        let Some(raw) = self.contract.extension.as_ref() else {
            return Ok(None);
        };
        let mut failed = false;
        let mut fail = |e: ConfigError, sub: &str, failed: &mut bool| {
            findings.push(e.at(format!("{path}.contract.extension.{sub}")));
            *failed = true;
        };

        let delivery_boundary = match raw.delivery_boundary.as_str() {
            "governor" => DeliveryBoundary::Governor,
            "consumer" => DeliveryBoundary::Consumer,
            other => {
                fail(
                    ConfigError::UnknownValue {
                        found: other.to_owned(),
                        allowed: "governor, consumer",
                    },
                    "delivery_boundary",
                    &mut failed,
                );
                DeliveryBoundary::Governor
            }
        };
        let delivery_semantics = match raw.delivery_semantics.as_str() {
            "latest_state" => DeliverySemantics::LatestState,
            "every_event" => DeliverySemantics::EveryEvent,
            "stateful_sequence" => DeliverySemantics::StatefulSequence,
            other => {
                fail(
                    ConfigError::UnknownValue {
                        found: other.to_owned(),
                        allowed: "latest_state, every_event, stateful_sequence",
                    },
                    "delivery_semantics",
                    &mut failed,
                );
                DeliverySemantics::LatestState
            }
        };
        let evidence_required = match raw.evidence_required.as_str() {
            "observed" => EvidenceLevel::Observed,
            "qualified_slo" => EvidenceLevel::QualifiedSlo,
            "proven" => EvidenceLevel::Proven,
            other => {
                fail(
                    ConfigError::UnknownValue {
                        found: other.to_owned(),
                        allowed: "observed, qualified_slo, proven",
                    },
                    "evidence_required",
                    &mut failed,
                );
                EvidenceLevel::Observed
            }
        };

        // Freigegebene Varianten ueber ihre Kurznamen. Ein Name, den es nicht
        // gibt, ist ein Tippfehler mit Folgen — er wuerde eine Variante still
        // sperren statt eine andere freizugeben.
        let approved_variants = if raw.approved_variants.is_empty() {
            ApprovedVariants::all()
        } else {
            let mut indices = Vec::new();
            for name in &raw.approved_variants {
                match self.variants.iter().position(|v| v.id == *name) {
                    Some(index) => match u16::try_from(index) {
                        Ok(idx) => indices.push(VariantIdx(idx)),
                        Err(_) => fail(
                            ConfigError::OutOfRange {
                                expected: "ein Variantenindex im gueltigen Bereich",
                            },
                            "approved_variants",
                            &mut failed,
                        ),
                    },
                    None => fail(
                        ConfigError::UnknownValue {
                            found: name.clone(),
                            allowed: "ein in diesem Modell definierter Variantenname",
                        },
                        "approved_variants",
                        &mut failed,
                    ),
                }
            }
            ApprovedVariants::from_indices(&indices)
        };

        let extension = ContractExtension {
            version: raw.version,
            consumer_period: raw.consumer_period_ms.and_then(Duration::from_millis),
            phase: raw.phase_ms.and_then(Duration::from_millis),
            release_jitter_envelope: raw.release_jitter_ms.and_then(Duration::from_millis),
            delivery_boundary,
            delivery_semantics,
            require_new_sample_each_cycle: raw.require_new_sample_each_cycle,
            observation_window: raw.observation_window,
            miss_budget: raw.miss_budget.as_ref().map(|b| MissBudget {
                max_misses: b.max_misses,
                window_cycles: b.window_cycles,
                max_consecutive: b.max_consecutive,
            }),
            minimum_background_progress_pct: raw.minimum_background_progress_pct,
            approved_variants,
            validity_envelope: ValidityEnvelope {
                max_input_kib: raw.max_input_kib,
                max_occupancy_pct: raw.max_occupancy_pct,
            },
            evidence_required,
            contract_version: raw.contract_version,
        };

        if let Err(e) = extension.validate(self.variants.len()) {
            findings.push(ConfigError::Contract(e.into()).at(format!("{path}.contract.extension")));
            failed = true;
        }

        if failed { Err(()) } else { Ok(Some(extension)) }
    }

    fn to_contract(
        &self,
        path: &str,
        findings: &mut Vec<Located>,
    ) -> Option<(ModelContract, Vec<String>)> {
        let mut fail = |e: ConfigError, sub: &str| {
            findings.push(e.at(format!("{path}.{sub}")));
        };

        let criticality = match parse_class(&self.class) {
            Ok(c) => c,
            Err(e) => {
                fail(e, "class");
                Criticality::Normal
            }
        };
        let policy = match parse_policy(&self.queue.policy) {
            Ok(p) => p,
            Err(e) => {
                fail(e, "queue.policy");
                QueuePolicy::Fifo
            }
        };
        let overflow = match parse_overflow(&self.queue.overflow) {
            Ok(o) => o,
            Err(e) => {
                fail(e, "queue.overflow");
                OverflowPolicy::RejectNew
            }
        };

        let deadline = match duration_ms(self.contract.deadline_ms, "deadline_ms") {
            Ok(d) => d,
            Err(e) => {
                fail(e, "contract.deadline_ms");
                Duration::from_nanos_unbounded(1)
            }
        };
        // Beide Werte sind optional — aber „nicht angegeben" und „angegeben
        // und unzulaessig" duerfen nicht dasselbe Ergebnis haben. Ein still
        // verworfenes `max_age_ms` heisst: dieser Strom altert nie, veraltete
        // Frames werden nie verworfen. Genau die Regel, die das Produkt
        // ausmacht, waere dann durch einen Tippfehler abgeschaltet.
        let period = match self.contract.period_ms.map(|p| duration_ms(p, "period_ms")) {
            Some(Ok(d)) => Some(d),
            Some(Err(e)) => {
                fail(e, "contract.period_ms");
                None
            }
            None => None,
        };
        let max_age = match self
            .contract
            .max_age_ms
            .map(|a| duration_ms(a, "max_age_ms"))
        {
            Some(Ok(d)) => Some(d),
            Some(Err(e)) => {
                fail(e, "contract.max_age_ms");
                None
            }
            None => None,
        };
        let cooperative = match self.cooperative.map(CooperativeConfig::resolve) {
            Some(Ok(c)) => Some(c),
            Some(Err(e)) => {
                fail(e, "cooperative.base_cost_us");
                None
            }
            None => None,
        };
        let variant_dwell = duration_ms(self.contract.variant_dwell_ms, "variant_dwell_ms")
            .unwrap_or(Duration::ZERO);
        let min_quality = match self.contract.min_quality {
            Some(v) => match quality_from_f64(v) {
                Ok(q) => Some(q),
                Err(e) => {
                    fail(e, "contract.min_quality");
                    None
                }
            },
            None => None,
        };

        let (variants, physical) = self.resolve_variants(path, findings)?;

        let Ok(extension) = self.build_extension(path, findings) else {
            return None;
        };

        let contract = ModelContract {
            // Bis das Backend etwas anderes sagt.
            variants_interchangeable: true,
            criticality,
            queue: QueueConfig {
                policy,
                capacity: self.queue.capacity,
                overflow,
            },
            period,
            deadline,
            max_age,
            stateful: self.stateful,
            min_quality,
            variant_dwell,
            variants,
            cooperative,
            extension,
        };

        if let Err(e) = contract.validate() {
            findings.push(ConfigError::Contract(e).at(path.to_owned()));
        }
        Some((contract, physical))
    }
}
