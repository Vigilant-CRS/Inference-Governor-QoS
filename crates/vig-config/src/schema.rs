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

/// Uebersetzt die gemessene Interferenztabelle in die Kernform (NV-11).
///
/// Ein unbekannter Modellname ist ein Befund und keine stille Auslassung: eine
/// Tabelle, die halb ankommt, plant fuer die Haelfte der Paare mit einer Null,
/// die niemand gemessen hat.
fn resolve_interference(
    pairs: &[InterferencePair],
    model_names: &[String],
    findings: &mut Vec<Located>,
) -> vig_core::interference::Interference {
    let mut table = vig_core::interference::Interference::new();
    for (i, pair) in pairs.iter().enumerate() {
        let path = format!("backend.interference[{i}]");
        let index = |name: &String| {
            model_names
                .iter()
                .position(|m| m == name)
                .and_then(|p| u16::try_from(p).ok())
                .map(vig_core::ModelIdx)
        };
        let (Some(victim), Some(co_tenant)) = (index(&pair.victim), index(&pair.co_tenant)) else {
            findings.push(
                ConfigError::UnknownModelReference {
                    name: format!("{} oder {}", pair.victim, pair.co_tenant),
                }
                .at(path),
            );
            continue;
        };
        if victim == co_tenant {
            findings.push(
                ConfigError::UnknownModelReference {
                    name: format!(
                        "{}: ein Modell stoert sich nicht selbst; das gehoert in den \
                         Belegungsgrad",
                        pair.victim
                    ),
                }
                .at(path),
            );
            continue;
        }
        let added = vig_core::Duration::from_micros(pair.added_us)
            .unwrap_or(vig_core::Duration::from_nanos_unbounded(0));
        // Die Ursache bleibt unbestimmt: `vig calibrate` misst die Wirkung,
        // nicht ihren Grund. Sie zu raten waere eine Aussage ueber die
        // Hardware, die aus dieser Messung nicht folgt.
        table.record_pair(
            victim,
            co_tenant,
            added,
            vig_core::interference::ConflictKind::Unspecified,
        );
    }
    table
}

/// Wie stark ein Modell ein anderes bremst, gemessen (NV-11, ADR-0026).
///
/// **Gerichtet**: `victim` leidet, `co_tenant` stoert. Die Umkehrung ist eine
/// eigene Zeile und kann sehr andere Zahlen tragen — ein 95-ms-VLM
/// verlaengert einen 5-ms-Detektor um ein Vielfaches seiner eigenen Laufzeit,
/// der Detektor das VLM kaum. Eine symmetrische Tabelle waere hier eine
/// Behauptung.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InterferencePair {
    /// Das Modell, dessen Laufzeit sich verlaengert.
    pub victim: String,
    /// Das Modell, das gleichzeitig laeuft.
    pub co_tenant: String,
    /// Wie viel Laufzeit dazukommt, in Mikrosekunden.
    ///
    /// Die absolute Zahl und kein Verhaeltnis: 20 % heissen bei 5 ms etwas
    /// anderes als bei 95 ms, und geplant wird mit Dauern.
    pub added_us: u64,
}

/// Ob und wie der Governor den GPU-Takt stellt (NV-13, ADR-0030).
///
/// ADR-0021 sagt: die Hardware wird gelesen, nie gestellt. ADR-0030 nennt die
/// Ausnahme und ihre Bedingungen — ausdruecklich einzuschalten, beobachtet
/// statt angenommen, mit einem Boden, unter den nicht gestellt wird.
///
/// Ohne diesen Block hat der Governor **keine** Stellbefugnis. Das ist der
/// Normalfall, und es bleibt der Normalfall.
///
/// Was hier steht, ist ein **Betriebspunkt**, keine Regelung: der Governor
/// fordert diesen Takt beim Start an und gibt ihn beim Beenden zurueck. Wann
/// eine Anhebung sich waehrend des Betriebs lohnt, ist eine Messfrage und
/// braucht eine Installation, auf der das Stellen ueberhaupt erlaubt ist
/// (ADR-0030, „die Struktur steht; die Politik nicht").
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActuationConfig {
    /// Die GPU, deren Takt gestellt wird.
    #[serde(default)]
    pub gpu_index: u32,
    /// Der Takt, der beim Start angefordert wird, in MHz.
    pub hold_mhz: u32,
    /// Der niedrigste Takt der Plattform, in MHz.
    pub platform_min_mhz: u32,
    /// Der hoechste Takt der Plattform, in MHz.
    pub platform_max_mhz: u32,
    /// Der niedrigste Takt, bei dem alle gegebenen Zusagen gelten, in MHz.
    ///
    /// Kommt aus den Profilmanifesten: wo ein Profil bei 1830 MHz gemessen
    /// wurde und ein Vertrag darauf beruht, ist 1830 der Boden. Unter ihn wird
    /// nicht gestellt, und ein **beobachteter** Takt darunter gilt nicht als
    /// Bestaetigung.
    pub promised_floor_mhz: u32,
    /// Wie lange nach einer Aenderung nicht wieder gestellt werden darf, in
    /// Millisekunden.
    #[serde(default)]
    pub dwell_ms: u64,
    /// Wie weit der beobachtete Takt abweichen darf, bis er als wirksam gilt.
    ///
    /// Fuer die Rundung der Karte auf ihre eigenen Taktstufen — nicht fuer
    /// den zugesagten Boden.
    #[serde(default = "default_tolerance_mhz")]
    pub tolerance_mhz: u32,
    /// Wie lange nach einer Anforderung auf die Bestaetigung gewartet wird.
    #[serde(default = "default_settle_ms")]
    pub settle_ms: u64,
}

const fn default_tolerance_mhz() -> u32 {
    50
}

const fn default_settle_ms() -> u64 {
    200
}

/// Welche Anwendungshinweise dieser Governor annimmt (NV-18, ADR-0029).
///
/// Ein Hinweis darf **verschaerfen, nie lockern** — das ist die Regel aus
/// ADR-0029, und sie steht im Kern. Diese Konfiguration sagt, wer ueberhaupt
/// gehoert wird und wie weit.
///
/// Ohne diesen Block nimmt der Governor keinen Hinweis an. Das ist Absicht:
/// eine Anwendung, die den Vertrag verschieben darf, ist eine
/// Betreiberentscheidung und keine Voreinstellung.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HintsConfig {
    /// Die Kennungen, die gehoert werden.
    ///
    /// Woher eine Kennung kommt und wie sie belegt wird — Token,
    /// mTLS-Zertifikat, Unix-Peer — entscheidet die Zugangsschicht. Hier
    /// steht nur, welche zaehlt.
    pub authority: u64,
    /// Ob lockernde Hinweise angenommen werden duerfen.
    ///
    /// Ein Aktionshorizont lockert: er sagt „so frisch brauche ich es gerade
    /// nicht". Das ist der einzige Hinweistyp, der eine Zusage schwaecher
    /// macht, und er braucht deshalb eine ausdrueckliche Freigabe.
    #[serde(default)]
    pub allow_loosening: bool,
    /// Die freigegebenen Betriebsmodus-Kennungen.
    ///
    /// Ein Modus ausserhalb dieser Liste wird abgelehnt, auch von einer
    /// berechtigten Stelle. Der Betreiber benennt die Modi, nicht die
    /// Anwendung.
    #[serde(default)]
    pub approved_modes: Vec<u32>,
    /// Das kuerzeste Hoechstalter, das ein Hinweis fordern darf, in
    /// Millisekunden.
    ///
    /// Ohne Untergrenze koennte eine Anwendung durch immer schaerfere
    /// Forderungen die gesamte Kapazitaet auf sich ziehen. Verschaerfen ist
    /// sicher fuer die **Zusage** und nicht fuer die **Nachbarn**.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_max_age_ms: Option<u64>,
    /// Die laengste Dauer, die ein Aktionshorizont beanspruchen darf, in
    /// Millisekunden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_action_horizon_ms: Option<u64>,
    /// Die laengste Geltungsdauer, die ein Hinweis beanspruchen darf, in
    /// Millisekunden (Security-Review N6). Ohne Angabe eine Minute.
    ///
    /// Ein Hinweis mit einer Frist von Jahren waere eine dauerhafte
    /// Vertragsaenderung durch die Anwendung. Ein laengerer wird verworfen,
    /// nicht gekuerzt: gekuerzt gaelte er anders, als die Anwendung glaubt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ttl_ms: Option<u64>,
}

/// Die Voreinstellung fuer `hints.max_ttl_ms`.
pub const DEFAULT_HINT_MAX_TTL_MS: u64 = 60_000;

impl HintsConfig {
    /// Uebersetzt die Konfiguration in die Kernform.
    fn resolve(&self) -> vig_core::hints::HintPolicy {
        // Die Modi als Maske: Kennungen ab 32 passen nicht hinein und werden
        // damit nicht freigegeben. Das ist die sichere Richtung — eine
        // Kennung, die niemand freigeben kann, wird abgelehnt.
        let mut approved_modes = 0_u32;
        for id in &self.approved_modes {
            if *id < 32 {
                approved_modes |= 1_u32 << *id;
            }
        }
        vig_core::hints::HintPolicy {
            authority: Some(vig_core::hints::Authority(self.authority)),
            allow_loosening: self.allow_loosening,
            approved_modes,
            min_max_age: self
                .min_max_age_ms
                .and_then(vig_core::Duration::from_millis),
            max_action_horizon: self
                .max_action_horizon_ms
                .and_then(vig_core::Duration::from_millis),
        }
    }
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
    /// Spuren fuer praemptierbare Hintergrundarbeit (ADR-0035).
    ///
    /// Eine Spur fuehrt nur Modelle mit `preemptible:` aus, und die belegen
    /// dafuer keinen der `slots`. Voreinstellung null: keine Spur, und keine
    /// Entscheidung aendert sich.
    #[serde(default, skip_serializing_if = "is_zero_lanes")]
    pub preemptible_lanes: usize,
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
    /// Die gemessene, gerichtete Interferenztabelle (NV-11, ADR-0026).
    ///
    /// Leer heisst: nicht gemessen. Dann bleibt der Slot-Belegungsgrad die
    /// Naeherung, die er laut ADR-0006 immer war. Gefuellt wird sie von
    /// `vig calibrate`, das beide Richtungen einzeln misst — bis hierher
    /// wurden diese Zahlen gemessen, berichtet und **weggeworfen**.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interference: Vec<InterferencePair>,
    /// Ob und wie der Governor den GPU-Takt stellt (NV-13, ADR-0030).
    ///
    /// Nicht gesetzt heisst: gar nicht — der Normalfall. ADR-0021 bleibt die
    /// Regel, ADR-0030 die benannte Ausnahme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actuation: Option<ActuationConfig>,
    /// Welche Anwendungshinweise angenommen werden (NV-18, ADR-0029).
    ///
    /// Nicht gesetzt heisst: keine. Vor dieser Zeile war der Hinweisregler
    /// gebaut, getestet und durch keine Konfiguration erreichbar — ein
    /// Betreiber konnte ihn nicht einschalten (Review R09).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hints: Option<HintsConfig>,
    /// Ob das Missbudget in die Kandidatenwahl eingeht (NV-24, ADR-0027).
    ///
    /// Voreinstellung **aus**. Eingeschaltet geht innerhalb einer
    /// Kritikalitaetsklasse ein Strom mit erschoepftem Missbudget vor einem
    /// mit Spielraum. Zwischen Klassen aendert sich nichts: der Vorrang von
    /// `protected` vor `best_effort` ist Betreiberpolicy und bleibt es.
    ///
    /// Vor dieser Zeile war der Regler gebaut, getestet und **nicht
    /// einschaltbar** — es gab keinen dokumentierten Konfigurationsschritt,
    /// der ihn erreicht haette (Review R09). „Voreinstellung aus" und „nicht
    /// erreichbar" sind verschiedene Aussagen.
    #[serde(default)]
    pub miss_aware_policy: bool,
    /// Ob der Look-ahead auch die Versorgung schuetzt (ADR-0041).
    ///
    /// Voreinstellung **aus**. Eingeschaltet haelt der Governor
    /// Hintergrundarbeit auch dann zurueck, wenn die naechste geschuetzte
    /// Ankunft ihre Deadline noch haelt, ihr Ergebnis aber erst nach dem
    /// Ablauf des vorigen kaeme — also genau dann, wenn der Verbraucher
    /// dazwischen eine Luecke haette (ADR-0005, NV-01).
    ///
    /// Der Preis ist Hintergrundfortschritt. Deshalb ist es eine Handlung des
    /// Betreibers und keine, die die Policy selbst trifft.
    #[serde(default)]
    pub protect_supply: bool,
    /// Ob die zustandsabhaengige Prognose entscheidet (NV-06, ADR-0023).
    ///
    /// Voreinstellung `shadow`: sie wird gefuettert und verglichen,
    /// entschieden wird mit `max(offline_p99, online_p95)`. `active` laesst
    /// eine belegte Zelle entscheiden, auch nach unten — und genau deshalb ist
    /// es eine Handlung des Betreibers, keine, die die Policy selbst trifft.
    ///
    /// Vor dieser Zeile gab es den scharfen Betrieb nur als Methode am Kern;
    /// kein Konfigurationsschritt erreichte ihn (Review R09).
    #[serde(default)]
    pub prediction: PredictionMode,
    /// Ob sich die Planung an der Karte kalibriert, auf der sie laeuft
    /// (ADR-0038).
    ///
    /// Nicht gesetzt heisst: wie bisher — die konfigurierte Marge ist der
    /// Boden, das Profil-p99 die Untergrenze. Gesetzt lernt der Governor je
    /// Karte einen Faktor zwischen Profil und gemessener Laufzeit, auch unter
    /// 100 %, und plant nie unter dem beobachteten Median. Wie
    /// `prediction: active` eine Handlung des Betreibers; beides zusammen
    /// wird abgelehnt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin_learning: Option<MarginLearningConfig>,
    /// Transport- und Zugangssicherung (TLS, mTLS, Token).
    #[serde(default)]
    pub security: SecurityConfig,
    /// Weitere Ressourcendomaenen, nach Namen (NV-22, ADR-0037).
    ///
    /// Eine Domaene ist eine GPU mit genau einem Kapazitaetsbesitzer. Dieser
    /// Block selbst ist die Domaene `default` auf GPU 0 und nimmt jedes Modell
    /// ohne `domain:`. Leer heisst: eine GPU, ein Scheduler, wie immer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub domains: BTreeMap<String, DomainConfig>,
}

/// Der Name der Domaene, die der `backend`-Block selbst beschreibt.
pub const DEFAULT_DOMAIN: &str = "default";

/// Die GPU der Domaene `default`.
///
/// Dieselbe, die die Hardwarebeobachtung vor NV-22 immer gelesen hat.
pub const DEFAULT_GPU_INDEX: u32 = 0;

/// Eine weitere Ressourcendomaene (NV-22, ADR-0037).
///
/// Nur was die **Kapazitaet** einer GPU beschreibt. Zugang, Vertrauen,
/// Hinweise, Timeouts und das Nutzlastbudget gelten fuer den ganzen Governor
/// und stehen im `backend`-Block.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainConfig {
    /// Die GPU dieser Domaene, wie `nvidia-smi` sie zaehlt.
    ///
    /// Zwei Domaenen auf derselben GPU werden abgelehnt: zwei Server auf einer
    /// GPU sind keine zwei Recheneinheiten (ADR-0004).
    pub gpu_index: u32,
    /// Der gRPC-Endpunkt des Backends auf dieser GPU.
    pub grpc_endpoint: String,
    /// Ausfuehrungsslots dieser GPU (ADR-0004).
    pub slots: usize,
    /// Zusaetzliche Kredite je Slot (ADR-0002).
    #[serde(default = "default_pipelining")]
    pub pipelining_depth: usize,
    /// Modellpaare dieser Domaene, die nicht gleichzeitig laufen duerfen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_corun: Vec<[String; 2]>,
    /// Spuren fuer praemptierbare Arbeit auf dieser GPU (ADR-0035).
    #[serde(default, skip_serializing_if = "is_zero_lanes")]
    pub preemptible_lanes: usize,
    /// Die gemessene Interferenztabelle dieser GPU (NV-11).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interference: Vec<InterferencePair>,
}

/// Wie die zustandsabhaengige Prognose wirkt (NV-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionMode {
    /// Mitschreiben und vergleichen, nicht entscheiden.
    #[default]
    Shadow,
    /// Eine belegte Zelle entscheidet.
    Active,
}

impl PredictionMode {
    /// Die Kernform.
    #[must_use]
    pub const fn to_core(self) -> vig_core::predictor::Mode {
        match self {
            Self::Shadow => vig_core::predictor::Mode::Shadow,
            Self::Active => vig_core::predictor::Mode::Active,
        }
    }
}

/// Die Kalibrierung an der Karte (ADR-0038).
///
/// Jedes Feld hat eine Voreinstellung; `margin_learning: {}` schaltet sie mit
/// diesen ein.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MarginLearningConfig {
    /// Der harte Boden des gelernten Faktors, in Prozent des Profils
    /// (10 bis 100, Voreinstellung 50).
    ///
    /// Unter 100 plant die Karte schneller, als ihr Profil sagt — weil sie es
    /// gemessen hat. Unter dem beobachteten Median plant sie ohnehin nie.
    #[serde(default = "default_learning_min_factor")]
    pub min_factor_percent: u32,
    /// Die Obergrenze des gelernten Faktors, in Prozent des Profils
    /// (100 bis 10000, Voreinstellung 1000).
    ///
    /// Hoeher als eine konfigurierte Marge sein darf: ein Profil von
    /// schnellerer Hardware soll auch auf einem langsamen Geraet konvergieren.
    #[serde(default = "default_learning_max_factor")]
    pub max_factor_percent: u32,
    /// Ausfuehrungen je Modell, bevor der Faktor mutiger werden darf
    /// (Voreinstellung 48). Vorsichtiger wird er sofort.
    #[serde(default = "default_learning_observations")]
    pub min_observations: u32,
}

const fn default_learning_min_factor() -> u32 {
    vig_core::learning::MarginLearning::DEFAULT_MIN_FACTOR_PERCENT
}

const fn default_learning_max_factor() -> u32 {
    vig_core::learning::MarginLearning::DEFAULT_MAX_FACTOR_PERCENT
}

const fn default_learning_observations() -> u32 {
    vig_core::learning::MarginLearning::DEFAULT_OBSERVATIONS
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
#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// Datei mit erlaubten Bearer-Token, eines je Zeile: `token` oder
    /// `name:token`, mindestens 16 Zeichen.
    ///
    /// Die pragmatische Variante fuer Umgebungen ohne Zertifikatsverwaltung.
    /// Leerzeilen und `#`-Kommentare werden ignoriert. Ist die Datei gesetzt,
    /// wird **jede gRPC-Anfrage** ohne gueltiges Token abgelehnt — auch die an
    /// unkonfigurierte Modelle und `ServerLive`/`ServerReady`. Der Metrikport
    /// (`/metrics`, `/healthz`, `/readyz`) prueft keine Token; er gehoert auf
    /// Loopback. Anwendungshinweise (NV-18) geben nur **benannte** Token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
    /// Datei mit Administrationstoken, im selben Format (Security-Review H3).
    ///
    /// Endpunkte, die den Zustand des Backends aendern — Modelle laden und
    /// entladen, Tracing, Loglevel, CUDA-Shared-Memory, „alle Regionen
    /// abmelden" —, sind **ohne** diese Datei gesperrt, auch im offenen
    /// Modus. Ein Administrationstoken gilt auch fuer gewoehnliche Anfragen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_token_file: Option<PathBuf>,
    /// Praefix, den jeder ueber den Governor registrierte
    /// Shared-Memory-Schluessel tragen muss (Security-Review H2).
    ///
    /// Das Backend sieht das `/dev/shm` des Hosts. Ohne Praefix liesse sich
    /// ueber den Governor das Segment jedes anderen Prozesses registrieren.
    #[serde(default = "default_shm_key_prefix")]
    pub shm_key_prefix: String,
    /// Wie viele Regionen hoechstens gleichzeitig registriert sind
    /// (Security-Review N7).
    #[serde(default = "default_max_shm_regions")]
    pub max_shm_regions: usize,
    /// Grenzen des gRPC-Transports (Security-Review M1).
    #[serde(default)]
    pub transport: TransportLimits,
}

impl Default for SecurityConfig {
    /// Von Hand und nicht abgeleitet: ein abgeleitetes `Default` gaebe einen
    /// leeren Schluesselpraefix — also gar keinen — und null Regionen.
    fn default() -> Self {
        Self {
            tls_cert: None,
            tls_key: None,
            client_ca: None,
            token_file: None,
            admin_token_file: None,
            shm_key_prefix: default_shm_key_prefix(),
            max_shm_regions: default_max_shm_regions(),
            transport: TransportLimits::default(),
        }
    }
}

fn default_shm_key_prefix() -> String {
    "/vig_".to_owned()
}

const fn default_max_shm_regions() -> usize {
    256
}

/// Die Grenzen des gRPC-Transports (Security-Review M1).
///
/// Ohne sie haelt ein Client beliebig viele Streams je Verbindung offen, jeden
/// mit bis zu 64 MiB, oder leere Verbindungen ohne Frist — und erschoepft
/// Speicher und Dateideskriptoren, bevor ein einziges Token geprueft ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct TransportLimits {
    /// Gleichzeitige HTTP/2-Streams je Verbindung.
    pub max_concurrent_streams: u32,
    /// Gleichzeitig bearbeitete Anfragen je Verbindung.
    pub concurrency_per_connection: usize,
    /// Hoechstdauer einer Anfrage in Millisekunden.
    ///
    /// Grosszuegig, weil ein zerlegter generativer Auftrag viele Quanten
    /// dauern kann; die Frist begrenzt haengende Clients, nicht Inferenzen.
    pub request_timeout_ms: u64,
    /// Abstand der HTTP/2- und TCP-Keepalives in Millisekunden.
    pub keepalive_interval_ms: u64,
    /// Wie lange auf eine Keepalive-Antwort gewartet wird, in Millisekunden.
    pub keepalive_timeout_ms: u64,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_concurrent_streams: 256,
            concurrency_per_connection: 64,
            request_timeout_ms: 600_000,
            keepalive_interval_ms: 30_000,
            keepalive_timeout_ms: 10_000,
        }
    }
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
            ("admin_token_file", self.admin_token_file.as_ref()),
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
        self.validate_limits(findings);
    }

    /// Prueft Schluesselpraefix, Regionsgrenze und Transportgrenzen
    /// (Security-Review H2, M1, N7).
    fn validate_limits(&self, findings: &mut Vec<Located>) {
        let prefix = self.shm_key_prefix.as_str();
        let rest = prefix.strip_prefix('/').unwrap_or("");
        if rest.is_empty() || rest.contains('/') || rest.contains('\0') {
            findings.push(
                ConfigError::OutOfRange {
                    expected: "ein POSIX-Schluesselpraefix wie \"/vig_\": beginnt mit '/', \
                               danach mindestens ein Zeichen und kein weiteres '/' — \
                               ein leerer Praefix erlaubte jeden Schluessel",
                }
                .at("backend.security.shm_key_prefix"),
            );
        }
        if self.max_shm_regions == 0 {
            findings.push(
                ConfigError::OutOfRange {
                    expected: "mindestens eine Region",
                }
                .at("backend.security.max_shm_regions"),
            );
        }
        let t = &self.transport;
        for (label, value) in [
            (
                "max_concurrent_streams",
                u64::from(t.max_concurrent_streams),
            ),
            (
                "concurrency_per_connection",
                u64::try_from(t.concurrency_per_connection).unwrap_or(u64::MAX),
            ),
            ("request_timeout_ms", t.request_timeout_ms),
            ("keepalive_interval_ms", t.keepalive_interval_ms),
            ("keepalive_timeout_ms", t.keepalive_timeout_ms),
        ] {
            if value == 0 {
                findings.push(
                    ConfigError::OutOfRange {
                        expected: "groesser als null; null haette keine Grenze, sondern \
                                   einen Endpunkt, der nichts annimmt",
                    }
                    .at(format!("backend.security.transport.{label}")),
                );
            }
        }
    }
}

impl BackendConfig {
    /// Prueft die Betriebsgrenzen und gibt das Inferenztimeout zurueck.
    fn limits(&self, findings: &mut Vec<Located>) -> Option<Duration> {
        self.security.validate(findings);
        if let Some(ttl) = self.hints.as_ref().and_then(|h| h.max_ttl_ms)
            && (ttl == 0 || vig_core::Duration::from_millis(ttl).is_none())
        {
            findings.push(
                ConfigError::OutOfRange {
                    expected: "hints.max_ttl_ms zwischen 1 und 3.600.000 (eine Stunde, \
                               die laengste Vertragsdauer)",
                }
                .at("backend.hints.max_ttl_ms"),
            );
        }
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
    /// Die Arbeit dieses Modells wird von geschuetzter Arbeit unterbrochen
    /// (ADR-0035).
    ///
    /// Eine Eigenschaft des **Backends**, nicht des Modells: ein Prozess
    /// niedriger Prioritaet unter einem Praemptionsmechanismus wie XSched.
    /// Der Governor ruft keine Praemption auf; er plant mit ihrer gemessenen
    /// Restblockierung. Braucht `backend.preemptible_lanes` und einen eigenen
    /// `backend_endpoint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preemptible: Option<PreemptibleConfig>,
    /// Die Ressourcendomaene, auf deren GPU dieses Modell laeuft (NV-22).
    ///
    /// Ohne Angabe die Domaene `default`, also der `backend`-Block. Die
    /// Zuordnung ist fest: ein Modell wechselt zur Laufzeit nicht die GPU
    /// (ADR-0037).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

impl ModelConfig {
    /// Die Domaene dieses Modells; `None` fuer `default`.
    fn domain_name(&self) -> Option<&str> {
        self.domain.as_deref().filter(|d| *d != DEFAULT_DOMAIN)
    }
}

/// Die Angaben eines praemptierbaren Modells (ADR-0035).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreemptibleConfig {
    /// Die Restblockierung in Mikrosekunden: um so viel verspaetet laufende
    /// Arbeit dieses Modells einen geschuetzten Auftrag hoechstens (p99).
    ///
    /// Gemessen von `vig calibrate` als p99 der geschuetzten Laufzeit mit
    /// laufender Hintergrundarbeit minus p99 allein.
    pub residual_blocking_us: u64,
    /// `measured` (von `vig calibrate`) oder `declared` (von Hand).
    ///
    /// Voreinstellung `declared`: eine Zahl, deren Herkunft niemand
    /// angegeben hat, gilt nicht als gemessen. `vig doctor` warnt dann.
    #[serde(default = "default_preemption_source")]
    pub source: String,
}

fn default_preemption_source() -> String {
    "declared".to_owned()
}

/// Die aufgeloeste Praemptionsangabe eines Modells (ADR-0035).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPreemptible {
    /// Die Restblockierung, die geschuetzte Arbeit waehrenddessen traegt.
    pub residual_blocking: Duration,
    /// Ob sie gemessen oder nur angegeben ist.
    pub measured: bool,
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
    /// Mindestlaufzeit fuer nachrangige Arbeit (ADR-0046).
    ///
    /// „Mindestens `budget_ms` Ausfuehrungszeit je `window_ms`": solange das
    /// Budget im gleitenden Fenster nicht aufgebraucht ist, steht wartende
    /// Arbeit dieses Modells ueber `normal` und unter `high`. Nur fuer
    /// `normal` und `best_effort`; bezahlt wird es von den nachrangigen
    /// Klassen, nie von bewachter Arbeit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_runtime: Option<MinRuntimeConfig>,
    /// Die Zusage dieses Stroms (ADR-0047).
    ///
    /// „98 % der Zyklen frisch, und nie laenger als eine Sekunde nichts."
    /// Ordnet **innerhalb** der Klasse um, nie darueber. Ohne diese Zeile
    /// plant der Governor wie bisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<ObjectiveConfig>,
}

/// Die Zusage in der Konfiguration (ADR-0047).
///
/// Zwei Haelften, beide einzeln optional, mindestens eine muss dastehen: der
/// **Anteil** sagt, wie viel im Fenster ankommt, die **Luecke**, wie lange nie
/// nichts ankommt. Ein Anteil allein sagt nichts ueber die Verteilung — 20 %
/// koennten acht Sekunden Stille und dann zwei Sekunden Vollgas sein.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveConfig {
    /// Anteil frischer Zyklen im Fenster, in Promille. Ohne Angabe: keine
    /// Anteilszusage, dann zaehlt allein die Luecke.
    #[serde(default)]
    pub coverage_permille: u16,
    /// Die Laenge des gleitenden Fensters, hoechstens 60000.
    pub window_ms: u64,
    /// Die laengste erlaubte Zeit ohne Ergebnis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gap_ms: Option<u64>,
}

/// Das Mindestlaufzeitbudget in der Konfiguration (ADR-0046).
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MinRuntimeConfig {
    /// So viel Ausfuehrungszeit steht dem Modell je Fenster mindestens zu.
    pub budget_ms: u64,
    /// Die Laenge des gleitenden Fensters, hoechstens 60000.
    pub window_ms: u64,
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
    /// Wie weit die Aufnahme eines Frames hoechstens neben ihrem Raster
    /// liegt, in Millisekunden.
    ///
    /// An einem bewachten Modell haelt der Look-ahead eine ueberfaellige
    /// Ankunft bis zur doppelten Huelle offen, statt den Frame aufzugeben
    /// (ADR-0036). Gemessen wird der Jitter nicht.
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
#[derive(Debug, Clone)]
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
    /// Ob das Missbudget in die Kandidatenwahl eingeht (NV-24).
    pub miss_aware_policy: bool,
    /// Ob der Look-ahead auch die Versorgung schuetzt (ADR-0041).
    pub protect_supply: bool,
    /// Ob die zustandsabhaengige Prognose entscheidet (NV-06).
    pub prediction: vig_core::predictor::Mode,
    /// Die Kalibrierung an der Karte, falls eingeschaltet (ADR-0038).
    pub margin_learning: Option<vig_core::learning::MarginLearning>,
    /// Die Hinweispolicy des Betreibers (NV-18).
    pub hint_policy: vig_core::hints::HintPolicy,
    /// Die laengste zulaessige Geltungsdauer eines Hinweises; `None` ohne
    /// Hinweisblock (Security-Review N6).
    pub hint_max_ttl: Option<vig_core::Duration>,
    /// Die Aktuationskonfiguration, falls eine gesetzt ist (NV-13).
    pub actuation: Option<ActuationConfig>,
    /// Die gemessene Interferenztabelle (NV-11).
    pub interference: vig_core::interference::Interference,
    /// Die zugesagte Schnittstelle je Modell, in Indexreihenfolge.
    ///
    /// `None`, wo keine hinterlegt ist.
    pub io_signatures: Vec<Option<IoSignature>>,
    /// Je Modell die Praemptionsangabe, in Indexreihenfolge (ADR-0035).
    ///
    /// `None` fuer jedes Modell, dessen Arbeit nicht unterbrochen wird.
    pub preemptible: Vec<Option<ResolvedPreemptible>>,
    /// Die Ressourcendomaenen (NV-22, ADR-0037); leer ohne `backend.domains`.
    ///
    /// **Mit Domaenen beschreiben `slots` und `interference` oben keine
    /// Kapazitaet mehr**, nur noch die unbelegte Grundform des
    /// `backend`-Blocks: jede Domaene hat ihre eigene Slotmenge und ihre
    /// eigene Tabelle, in ihrem eigenen Modellindex. Wer ueber Kapazitaet
    /// urteilt — der Actor, `vig doctor` —, fragt hier.
    pub domains: Vec<ResolvedDomain>,
}

/// Eine aufgeloeste Ressourcendomaene (NV-22, ADR-0037).
#[derive(Debug, Clone)]
pub struct ResolvedDomain {
    /// Der Name; `default` fuer den `backend`-Block.
    pub name: String,
    /// Die GPU dieser Domaene.
    pub gpu_index: u32,
    /// Die globalen Modellindizes ihrer Modelle, in lokaler Reihenfolge.
    ///
    /// `models[i]` ist das Modell, das in `resolved` den Index `i` traegt.
    pub models: Vec<ModelIdx>,
    /// Die Konfiguration, die ihr Scheduler sieht: nur ihre Modelle, ihre
    /// Slots, ihre Endpunkte.
    pub resolved: std::sync::Arc<Resolved>,
}

impl ResolvedDomain {
    /// Der lokale Index eines globalen Modells, falls es zu dieser Domaene
    /// gehoert.
    #[must_use]
    pub fn local(&self, model: ModelIdx) -> Option<ModelIdx> {
        self.models
            .iter()
            .position(|m| *m == model)
            .and_then(|i| u16::try_from(i).ok())
            .map(ModelIdx)
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde verlangt fuer skip_serializing_if eine Funktion fn(&T) -> bool"
)]
fn is_zero_lanes(lanes: &usize) -> bool {
    *lanes == 0
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
    /// Der Anteil der Slots, den die Mindestlaufzeitbudgets belegen, in
    /// Promille (ADR-0046).
    ///
    /// `Summe(budget_i / window_i)` ueber alle Modelle mit Budget, geteilt
    /// durch die regulaeren Slots — in derselben Einheit wie
    /// [`Self::protected_utilization_permille`]. Beides zusammen ueber 1000
    /// heisst: die Budgets koennen nur aus Zeit bedient werden, die der
    /// bewachten Arbeit gehoert, und das tun sie nicht.
    #[must_use]
    pub fn runtime_budget_permille(&self) -> u64 {
        let total = self
            .contracts
            .iter()
            .filter_map(|c| c.min_runtime)
            .map(|b| b.slot_share_permille())
            .fold(0_u64, u64::saturating_add);
        let slots = u64::try_from(self.slots.regular_len()).unwrap_or(1).max(1);
        total.checked_div(slots).unwrap_or(0)
    }

    /// Der Anteil der Slots, den die Zusagen nachrangiger Stroeme
    /// beanspruchen, in Promille (ADR-0047).
    ///
    /// `Summe(z_i * C_i / T_i)` ueber alle **nicht bewachten** Modelle mit
    /// Zusage, geteilt durch die regulaeren Slots — dieselbe Einheit wie
    /// [`Self::protected_utilization_permille`], damit sich beides addieren
    /// laesst.
    ///
    /// Zwei Entscheidungen stecken darin:
    ///
    /// * **Bewachte Stroeme zaehlen nicht doppelt.** Ein `protected`-Strom mit
    ///   Zusage steckt ueber seine Klasse schon vollstaendig in der
    ///   geschuetzten Auslastung; seine Zusage ordnet nur innerhalb der Klasse
    ///   um und fordert keine zusaetzliche Kapazitaet. Beides zu addieren
    ///   hiesse, die sorgfaeltigste Konfiguration abzulehnen.
    /// * **Es zaehlt die strengere Haelfte.** `z_i` ist
    ///   [`Objective::effective_permille`]: eine Luecke von einer Sekunde
    ///   verlangt bei 33 ms Takt mindestens 33 ‰, auch wenn der Anteil
    ///   darunter steht. Ohne Takt traegt die Luecke die ganze Rechnung —
    ///   `C_i / max_gap_i`.
    #[must_use]
    pub fn objective_utilization_permille(&self) -> u64 {
        self.objective_utilization(false)
    }

    /// Additional objective demand not already covered by the same model's
    /// minimum runtime budget. Work can satisfy both promises at once;
    /// reservations of different models must never offset each other.
    #[must_use]
    pub fn additional_objective_utilization_permille(&self) -> u64 {
        self.objective_utilization(true)
    }

    fn objective_utilization(&self, subtract_own_budget: bool) -> u64 {
        let mut total = 0_u64;
        for contract in self.contracts.iter() {
            if contract.criticality.is_guarded() {
                continue;
            }
            let (Some(objective), Some(best)) = (contract.objective, contract.variants.get(0))
            else {
                continue;
            };
            let Ok(runtime) = best.profile.conservative_at(0, self.margin) else {
                continue;
            };
            // Ohne Takt gibt es keine Zyklen; dann ist die Luecke der Takt.
            let Some(period) = contract.period.or(objective.max_gap) else {
                continue;
            };
            let share = runtime
                .as_nanos()
                .saturating_mul(u64::from(objective.effective_permille(period)))
                .checked_div(period.as_nanos().max(1))
                .unwrap_or(0);
            let reserved = if subtract_own_budget {
                contract
                    .min_runtime
                    .map_or(0, |budget| budget.slot_share_permille())
            } else {
                0
            };
            total = total.saturating_add(share.saturating_sub(reserved));
        }
        let slots = u64::try_from(self.slots.regular_len()).unwrap_or(1).max(1);
        total.checked_div(slots).unwrap_or(0)
    }

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
        // Ueber die regulaeren Slots: eine Spur rechnet keine geschuetzte
        // Arbeit (ADR-0035).
        let slots = u64::try_from(self.slots.regular_len()).unwrap_or(1).max(1);
        total.checked_div(slots).unwrap_or(0)
    }

    /// Schaltet die automatische Variantenwahl eines Modells ab — in der
    /// Gesamtsicht und in der Domaene, deren Scheduler es plant (NV-22).
    ///
    /// Nur in der Gesamtsicht abgeschaltet, liefe der Scheduler der Domaene
    /// weiter mit der alten Zusage.
    pub fn pin_best_variant(&mut self, model: ModelIdx) {
        if let Some(contract) = self.contracts.get_mut(model.get()) {
            contract.variants_interchangeable = false;
        }
        for domain in &mut self.domains {
            if let Some(local) = domain.local(model) {
                let resolved = std::sync::Arc::make_mut(&mut domain.resolved);
                if let Some(contract) = resolved.contracts.get_mut(local.get()) {
                    contract.variants_interchangeable = false;
                }
            }
        }
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

/// Ob lange nachrangige Arbeit die Zusage bewachter Modelle unmoeglich macht
/// (ADR-0035).
///
/// Ein Aufruf, der laeuft, laeuft zu Ende: ohne `preemptible:` gibt es keine
/// Praemption. Belegen so viele nicht bewachte Modelle alle regulaeren Slots,
/// deren konservative Laufzeit ueber der engsten Zusage eines bewachten
/// Modells liegt, dann ist diese Zusage arithmetisch nicht haltbar — und das
/// soll die Konfiguration sagen, nicht erst die Messung.
///
/// Gemessen am 16.09.2026: zwei Sprachmodelle mit 195 ms p99 auf zwei Slots
/// rissen eine geschuetzte Kamera mit 100 ms Hoechstalter auf eine Luecke von
/// 245 ms; ohne die beiden blieb dieselbe Kamera bei 86 ms.
fn check_blocking_work(
    model_names: &[String],
    contracts: &ArrayVec<ModelContract, MAX_MODELS>,
    preemptible: &[Option<ResolvedPreemptible>],
    slots: &SlotSet,
    margin: SafetyMargin,
    findings: &mut Vec<Located>,
) {
    let regular = slots.regular_len();
    if regular == 0 {
        return;
    }
    // Die engste Zusage, die gehalten werden muss. Das Hoechstalter ist die
    // fachliche Grenze; fehlt es, zaehlt die Frist. Eine vereinbarte Luecke
    // (ADR-0047) ist ebenfalls eine Zusage und kann strenger sein.
    //
    // Nicht nur bewachte Klassen: ein Strom mit `objective` hat einen
    // ausgesprochenen Anspruch, und lange nicht praemptierbare Arbeit macht
    // ihn genauso unhaltbar. Gemessen am 16.09.2026: ein `normal`-Strom mit
    // 100 ms Frist erfuellte seine Zusage neben einem 195-ms-Aufruf zu 370 ‰,
    // ohne ihn zu 965 ‰ — bei 87 statt 73 Prozent Auslastung. Es fehlte nicht
    // die Kapazitaet, es fehlte der freie Slot.
    let promise_of = |contract: &ModelContract| -> Option<Duration> {
        if !contract.criticality.is_guarded() && contract.objective.is_none() {
            return None;
        }
        let limit = contract.max_age.unwrap_or(contract.deadline);
        Some(match contract.objective.and_then(|o| o.max_gap) {
            Some(gap) if gap < limit => gap,
            _ => limit,
        })
    };
    let Some(tightest) = contracts.iter().filter_map(promise_of).min() else {
        return;
    };

    // Bewusst **nicht** eingerechnet: wie viele Slots die bewachte Arbeit im
    // Mittel belegt. Der Gedanke lag nahe — ein dauerhaft belegter Slot plus
    // ein langer Auftrag sperren alles aus —, aber die eigene Messung
    // widerlegt ihn: am 16.09.2026 war genau diese Lage (ein Blocker, zwei
    // Slots, eine Kamera mit 60 % Dauerlast) einwandfrei, 1 ‰ unabgedeckt bei
    // 39 ms laengster Luecke. Eine Schwelle, die das ablehnt, waere strenger
    // als die Wirklichkeit und wuerde elf ausgelieferte Beispiele
    // zurueckweisen, von denen die meisten tragen.

    let mut blockers = Vec::new();
    for (i, (name, contract)) in model_names.iter().zip(contracts.iter()).enumerate() {
        if contract.criticality.is_guarded() {
            continue;
        }
        // Wer seine Restblockierung erklaert hat, ist eingeplant (ADR-0035).
        if preemptible.get(i).copied().flatten().is_some() {
            continue;
        }
        let Some(best) = contract.variants.get(0) else {
            continue;
        };
        let Ok(runtime) = best.profile.conservative_at(0, margin) else {
            continue;
        };
        if blocking_time(contract, runtime) > tightest {
            blockers.push((name, contract.cooperative.is_some()));
        }
    }

    // Erst wenn sie jeden regulaeren Slot besetzen koennen, ist die Zusage
    // verloren. Ein einzelner langer Auftrag neben zwei Slots laesst dem
    // bewachten Modell noch einen.
    //
    // Gezaehlt werden **gleichzeitige Auftraege, nicht Modelle**:
    // `SlotSet::ready_slot` kennt keine Grenze „ein Modell, ein Slot" — es
    // nimmt den ersten Slot, der das Modell zulaesst und noch Kredit hat. Ein
    // Modell mit `capacity: 2` belegt damit zwei Slots und zaehlte trotzdem
    // als *ein* Blocker. Gefunden in der erneuten Pruefung vom 16.09.2026.
    if blockers.len() >= regular {
        for (name, decomposable) in blockers {
            findings.push(
                ConfigError::Inconsistent {
                    what: blocking_advice(decomposable),
                }
                .at(format!("models.{name}.contract")),
            );
        }
    }
}

/// Wie lange dieses Modell einen Slot **am Stueck** belegt.
///
/// Fuer einen unteilbaren Auftrag ist das seine konservative Laufzeit. Ein
/// Modell mit `cooperative:` (ADR-0014) belegt den Slot dagegen nur fuer ein
/// Quantum — zwischen zweien ist er frei, und genau darum geht es.
///
/// Geprueft wird die Mindestquantengroesse bei gewachsenem Ausgabetext.
/// Der Clientprompt ist hier unbekannt; seine Kosten prueft der Scheduler
/// pro Request. Diese Rechnung ist kein Nachweis einer maximalen Blockade.
/// Ein Offlineprofil fuer einen kurzen Prompt begrenzt die Kosten eines
/// spaeteren Re-Prefills nicht und darf sie deshalb nicht heruntersetzen.
///
/// Ohne diese Unterscheidung war `cooperative:` in genau dem Fall gesperrt,
/// fuer den es gebaut wurde — auf **einer** Ausfuehrungseinheit. Der Befund
/// riet zur Zerlegung; wer ihr folgte, bekam denselben Befund erneut und
/// konnte den Governor nicht starten. Gefunden am 16.09.2026 beim Versuch,
/// `examples/cooperative_llm/vig.yaml` zu messen.
fn blocking_time(contract: &ModelContract, runtime: Duration) -> Duration {
    match contract.cooperative {
        Some(cooperative) => {
            cooperative.cost_of_with_context(cooperative.min_tokens, cooperative.max_total_tokens)
        }
        None => runtime,
    }
}

/// Der Befundtext, je nachdem ob das Modell zerlegbar ist.
///
/// Wer `cooperative:` gesetzt hat, hat den Rat bereits befolgt. Ihm denselben
/// Rat ein zweites Mal zu geben waere die schlechteste aller Antworten: er
/// sucht den Fehler dann dort, wo keiner ist. Was fehlt, ist ein kleineres
/// Quantum — und das sagt der zweite Text.
const fn blocking_advice(decomposable: bool) -> &'static str {
    if decomposable {
        "dieses Modell ist zerlegbar, aber schon sein laengstes Quantum rechnet laenger als die \
         engste Zusage eines bewachten Modells — und es gibt genug solche Modelle, um jeden \
         regulaeren Slot zu belegen. Zerlegen allein genuegt hier nicht. Abhilfe: kleineres \
         `min_tokens`, kleineres `max_total_tokens` (das spaeteste Quantum traegt den laengsten \
         Kontext), ein Backend mit wirksamem Prefix-Cache, oder `preemptible:` mit gemessener \
         Restblockierung (ADR-0035)"
    } else {
        "dieses Modell rechnet laenger als die engste Zusage eines bewachten \
         Modells, und es gibt genug solche Modelle, um jeden regulaeren Slot \
         zu belegen. Ohne Praemption laeuft ein begonnener Aufruf zu Ende, \
         also ist die Zusage nicht haltbar. Bei einem generativen Modell ist \
         `cooperative:` der wirksamste Ausweg — der Auftrag laeuft dann in \
         Quanten, und zwischen zweien ist der Slot frei (ADR-0014). Sonst: \
         `preemptible:` mit gemessener Restblockierung (ADR-0035), mehr \
         Slots, oder eine kuerzere Variante"
    }
}

/// Ob die Slots jedes Mindestlaufzeitbudget in einem Fenster rechnen koennen
/// (ADR-0046).
///
/// Dieselbe Pruefung macht der Scheduler beim Bau; hier steht sie, damit der
/// Befund eine Fundstelle hat. Mit Domaenen prueft die Aufloesung jeder
/// Domaene gegen deren Slots.
fn check_runtime_budgets(
    model_names: &[String],
    contracts: &ArrayVec<ModelContract, MAX_MODELS>,
    slots: &SlotSet,
    findings: &mut Vec<Located>,
) {
    let regular = slots.regular_len();
    for (name, contract) in model_names.iter().zip(contracts.iter()) {
        if let Some(budget) = contract.min_runtime
            && !budget.fits_slots(regular)
        {
            findings.push(
                ConfigError::Contract(
                    vig_core::runtime_budget::RuntimeBudgetError::BeyondSlots { slots: regular }
                        .into(),
                )
                .at(format!("models.{name}.contract.min_runtime")),
            );
        }
    }
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

        // ADR-0038: die Kalibrierung an der Karte. Ohne Block aendert sich
        // nichts; mit ihm werden Bereich und Widersprueche geprueft.
        let margin_learning = match self.backend.margin_learning {
            None => None,
            Some(learning) => {
                let range = vig_core::learning::MarginLearning::new(
                    learning.min_factor_percent,
                    learning.max_factor_percent,
                    learning.min_observations,
                );
                if range.is_none() {
                    findings.push(
                        ConfigError::OutOfRange {
                            expected: "min_factor_percent 10 bis 100, max_factor_percent \
                                       100 bis 10000, min_observations 1 bis 100000",
                        }
                        .at("backend.margin_learning"),
                    );
                }
                if learning.max_factor_percent < self.backend.safety_margin_percent {
                    findings.push(
                        ConfigError::Inconsistent {
                            what: "max_factor_percent liegt unter safety_margin_percent; \
                                   der Faktor begaenne ueber seiner eigenen Obergrenze",
                        }
                        .at("backend.margin_learning.max_factor_percent"),
                    );
                }
                if self.backend.prediction == PredictionMode::Active {
                    findings.push(
                        ConfigError::Inconsistent {
                            what: "margin_learning und prediction: active lernen beide den \
                                   Abstand zwischen Profil und Karte; zwei Regler auf \
                                   demselben Plan jagen einander (ADR-0038)",
                        }
                        .at("backend.margin_learning"),
                    );
                }
                range
            }
        };

        let mut slots =
            match SlotSet::homogeneous(self.backend.slots, self.backend.pipelining_depth) {
                Ok(s) => s,
                Err(e) => {
                    findings.push(ConfigError::Slots(e).at("backend.slots"));
                    return None;
                }
            };

        let model_names: Vec<String> = self.models.keys().cloned().collect();
        // Ohne eigenen Endpunkt laeuft ein Modell am Endpunkt seiner Domaene
        // (NV-22) — und ohne Domaene am `backend`-Block, wie bisher.
        let model_endpoints: Vec<String> = self
            .models
            .values()
            .map(|m| {
                m.backend_endpoint.clone().unwrap_or_else(|| {
                    m.domain_name()
                        .and_then(|d| self.backend.domains.get(d))
                        .map_or_else(
                            || self.backend.grpc_endpoint.clone(),
                            |d| d.grpc_endpoint.clone(),
                        )
                })
            })
            .collect();
        self.check_domain_references(findings);
        let multi = !self.backend.domains.is_empty();
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

        // Mit Domaenen gehoeren Co-Run-Verbote, Spuren und Interferenz zur
        // Kapazitaet einer GPU und werden dort geprueft (ADR-0037); hier, in
        // der Gesamtsicht, gaebe es fuer sie keinen gemeinsamen Modellindex.
        let mut preemptible = Vec::new();
        let mut interference = vig_core::interference::Interference::new();
        if !multi {
            Self::apply_corun_rules(&self.backend.no_corun, &model_names, &mut slots, findings);
            preemptible = self.resolve_preemption(
                &model_names,
                &model_endpoints,
                &contracts,
                &mut slots,
                findings,
            );
            interference = resolve_interference(&self.backend.interference, &model_names, findings);
            check_runtime_budgets(&model_names, &contracts, &slots, findings);
            check_blocking_work(
                &model_names,
                &contracts,
                &preemptible,
                &slots,
                margin,
                findings,
            );
        }

        // Die Grenzen vor den Domaenen: deren Aufloesung meldet dieselben
        // Befunde noch einmal und soll sie als bekannt vorfinden.
        let limits = self.backend.limits(findings);
        let domains = if multi {
            self.resolve_domains(&model_endpoints, findings)
        } else {
            Vec::new()
        };
        if multi {
            preemptible = vec![None; model_names.len()];
            for domain in &domains {
                for (local, global) in domain.models.iter().enumerate() {
                    if let (Some(entry), Some(target)) = (
                        domain.resolved.preemptible.get(local),
                        preemptible.get_mut(global.get()),
                    ) {
                        *target = *entry;
                    }
                }
            }
        }
        let inference_timeout = limits?;

        Some(Resolved {
            inference_timeout,
            trust: self.backend.trust,
            max_inflight_bytes: self.backend.max_inflight_mib.saturating_mul(1024 * 1024),
            security: self.backend.security.clone(),
            miss_aware_policy: self.backend.miss_aware_policy,
            protect_supply: self.backend.protect_supply,
            prediction: self.backend.prediction.to_core(),
            margin_learning,
            actuation: self.backend.actuation.clone(),
            interference,
            domains,
            hint_policy: self
                .backend
                .hints
                .as_ref()
                .map_or_else(vig_core::hints::HintPolicy::closed, HintsConfig::resolve),
            hint_max_ttl: self
                .backend
                .hints
                .as_ref()
                .map(|h| h.max_ttl_ms.unwrap_or(DEFAULT_HINT_MAX_TTL_MS))
                .and_then(vig_core::Duration::from_millis),
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
            preemptible,
            margin,
        })
    }

    /// Prueft die Praemptionsangaben und legt die Spuren an (ADR-0035).
    ///
    /// Jede Pruefung steht fuer eine Konfiguration, die sich widerspricht:
    /// geschuetzte Arbeit, die unterbrochen werden soll; eine Restblockierung
    /// von null; ein praemptierbares Modell im selben Prozess wie die
    /// geschuetzten, obwohl die Prioritaet je Prozess gilt; ein `no_corun`
    /// genau zwischen den beiden, deren Nebeneinander die Praemption erlaubt;
    /// Modelle ohne Spur, oder eine Spur ohne Modell.
    fn resolve_preemption(
        &self,
        model_names: &[String],
        model_endpoints: &[String],
        contracts: &ArrayVec<ModelContract, MAX_MODELS>,
        slots: &mut SlotSet,
        findings: &mut Vec<Located>,
    ) -> Vec<Option<ResolvedPreemptible>> {
        let guarded = |i: usize| contracts.get(i).is_some_and(|c| c.criticality.is_guarded());
        let mut out: Vec<Option<ResolvedPreemptible>> = Vec::with_capacity(model_names.len());
        let mut mask = vig_core::slots::ModelMask::NONE;

        for (i, (name, model)) in self.models.iter().enumerate() {
            let Some(config) = model.preemptible.as_ref() else {
                out.push(None);
                continue;
            };
            let path = format!("models.{name}.preemptible");
            let measured = match config.source.as_str() {
                "measured" => true,
                "declared" => false,
                other => {
                    findings.push(
                        ConfigError::UnknownValue {
                            found: other.to_owned(),
                            allowed: "measured, declared",
                        }
                        .at(format!("{path}.source")),
                    );
                    false
                }
            };
            if config.residual_blocking_us == 0 {
                findings.push(
                    ConfigError::OutOfRange {
                        expected: "residual_blocking_us groesser als null — eine Praemption \
                                   ohne Restblockierung gibt es nicht; gemessen wird sie mit \
                                   vig calibrate",
                    }
                    .at(format!("{path}.residual_blocking_us")),
                );
            }
            if guarded(i) {
                findings.push(
                    ConfigError::Inconsistent {
                        what: "praemptierbar kann nur Arbeit der Klassen normal oder \
                               best_effort sein: geschuetzte Arbeit wird nicht unterbrochen, \
                               sie unterbricht",
                    }
                    .at(format!("models.{name}.class")),
                );
            }
            let endpoint = model_endpoints.get(i);
            let shares_process =
                (0..contracts.len()).any(|j| guarded(j) && model_endpoints.get(j) == endpoint);
            if shares_process {
                findings.push(
                    ConfigError::Inconsistent {
                        what: "ein praemptierbares Modell braucht einen eigenen Backendprozess \
                               (backend_endpoint): die Prioritaet, mit der das Backend \
                               unterbricht, gilt je Prozess",
                    }
                    .at(format!("models.{name}.backend_endpoint")),
                );
            }
            if let Ok(raw) = u16::try_from(i) {
                mask = mask.with(ModelIdx(raw));
            }
            out.push(Some(ResolvedPreemptible {
                residual_blocking: Duration::from_nanos_unbounded(
                    config.residual_blocking_us.saturating_mul(1_000),
                ),
                measured,
            }));
        }

        self.refuse_corun_with_preemptible(model_names, contracts, &out, findings);
        self.add_preemptible_lanes(mask, slots, findings);
        out
    }

    /// `no_corun` zwischen einem praemptierbaren und einem geschuetzten
    /// Modell widerspricht sich: die Praemption ist genau das erlaubte
    /// Nebeneinander (ADR-0035).
    fn refuse_corun_with_preemptible(
        &self,
        model_names: &[String],
        contracts: &ArrayVec<ModelContract, MAX_MODELS>,
        preemptible: &[Option<ResolvedPreemptible>],
        findings: &mut Vec<Located>,
    ) {
        let guarded = |i: usize| contracts.get(i).is_some_and(|c| c.criticality.is_guarded());
        let is_preemptible = |i: usize| preemptible.get(i).is_some_and(Option::is_some);
        let index = |n: &String| model_names.iter().position(|m| m == n);
        for (k, [a, b]) in self.backend.no_corun.iter().enumerate() {
            if let (Some(a), Some(b)) = (index(a), index(b))
                && ((is_preemptible(a) && guarded(b)) || (is_preemptible(b) && guarded(a)))
            {
                findings.push(
                    ConfigError::Inconsistent {
                        what: "no_corun zwischen einem praemptierbaren und einem geschuetzten \
                               Modell widerspricht sich: die Praemption ist genau das erlaubte \
                               Nebeneinander",
                    }
                    .at(format!("backend.no_corun[{k}]")),
                );
            }
        }
    }

    /// Legt die Spuren an — oder meldet, warum es keine geben kann
    /// (ADR-0035): praemptierbare Modelle ohne Spur liefen wieder als
    /// unteilbarer Block, eine Spur ohne Modell bliebe leer.
    fn add_preemptible_lanes(
        &self,
        mask: vig_core::slots::ModelMask,
        slots: &mut SlotSet,
        findings: &mut Vec<Located>,
    ) {
        let lanes = self.backend.preemptible_lanes;
        if mask.is_empty() {
            if lanes > 0 {
                findings.push(
                    ConfigError::Inconsistent {
                        what: "backend.preemptible_lanes ohne ein einziges Modell mit \
                               preemptible: ist eine Spur, auf der nie etwas laeuft",
                    }
                    .at("backend.preemptible_lanes"),
                );
            }
        } else if lanes == 0 {
            findings.push(
                ConfigError::Missing {
                    what: "praemptierbare Modelle brauchen backend.preemptible_lanes >= 1; \
                           sonst liefen sie als unteilbarer Block auf einem geschuetzten Slot",
                }
                .at("backend.preemptible_lanes"),
            );
        } else if let Err(e) = slots.add_preemptible_lanes(lanes, mask) {
            findings.push(ConfigError::Slots(e).at("backend.preemptible_lanes"));
        }
    }
}

/// Die Schluessel des `backend`-Blocks, die in einer Domaene eigene Werte
/// haben (ADR-0037). Ein Befund dazu gehoert an die Domaene, nicht an den
/// `backend`-Block.
const DOMAIN_KEYS: [&str; 6] = [
    "grpc_endpoint",
    "slots",
    "pipelining_depth",
    "no_corun",
    "preemptible_lanes",
    "interference",
];

/// Verlegt einen Befund aus der Aufloesung einer Domaene an ihre Fundstelle.
fn domain_path(path: &str, domain: Option<&str>) -> String {
    let (Some(domain), Some(rest)) = (domain, path.strip_prefix("backend.")) else {
        return path.to_owned();
    };
    let key = rest.split(['.', '[']).next().unwrap_or_default();
    if DOMAIN_KEYS.contains(&key) {
        format!("backend.domains.{domain}.{rest}")
    } else {
        path.to_owned()
    }
}

/// Ob ein Domaenenname taugt: er steht als Label in jeder Kennzahl.
fn valid_domain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl Config {
    /// Nennt jedes Modell eine Domaene, die es gibt? (NV-22)
    ///
    /// Auch ohne `backend.domains`: ein `domain: gpu1`, das niemand anlegt,
    /// liefe sonst still auf GPU 0 — genau die Verwechslung, die das Feld
    /// verhindern soll.
    fn check_domain_references(&self, findings: &mut Vec<Located>) {
        for (name, model) in &self.models {
            if let Some(domain) = model.domain_name()
                && !self.backend.domains.contains_key(domain)
            {
                findings.push(
                    ConfigError::UnknownDomain {
                        name: domain.to_owned(),
                    }
                    .at(format!("models.{name}.domain")),
                );
            }
        }
    }

    /// Loest jede Domaene fuer sich auf (ADR-0037).
    ///
    /// Jede Domaene wird als eigene Konfiguration aufgeloest — nur ihre
    /// Modelle, ihre Kapazitaet —, mit genau den Pruefungen, die ein Governor
    /// fuer eine GPU immer macht. Befunde, die schon die Gesamtsicht gemeldet
    /// hat, erscheinen nicht zweimal.
    fn resolve_domains(
        &self,
        model_endpoints: &[String],
        findings: &mut Vec<Located>,
    ) -> Vec<ResolvedDomain> {
        let covered = self.check_domains(model_endpoints, findings);
        let mut names: Vec<Option<&str>> = vec![None];
        names.extend(self.backend.domains.keys().map(|k| Some(k.as_str())));

        let mut out = Vec::new();
        for name in names {
            let members: Vec<ModelIdx> = self
                .models
                .values()
                .enumerate()
                .filter(|(_, m)| m.domain_name() == name)
                .filter_map(|(i, _)| u16::try_from(i).ok().map(ModelIdx))
                .collect();
            // Ohne Modell keine Domaene: fuer `default` ist das erlaubt (alle
            // Modelle stehen in benannten Domaenen), fuer eine benannte ein
            // Befund aus `check_domains`.
            if members.is_empty() {
                continue;
            }
            let mut local = Vec::new();
            let resolved = self.domain_config(name).resolve_collecting(&mut local);
            for finding in local {
                let finding = Located {
                    path: domain_path(&finding.path, name),
                    error: finding.error,
                };
                if !covered.contains(&finding.path) && !findings.contains(&finding) {
                    findings.push(finding);
                }
            }
            let gpu_index = name
                .and_then(|n| self.backend.domains.get(n))
                .map_or(DEFAULT_GPU_INDEX, |d| d.gpu_index);
            if let Some(resolved) = resolved {
                out.push(ResolvedDomain {
                    name: name.unwrap_or(DEFAULT_DOMAIN).to_owned(),
                    gpu_index,
                    models: members,
                    resolved: std::sync::Arc::new(resolved),
                });
            }
        }
        out
    }

    /// Die Konfiguration, die der Scheduler einer Domaene sieht.
    fn domain_config(&self, name: Option<&str>) -> Self {
        let mut backend = self.backend.clone();
        backend.domains = BTreeMap::new();
        if let Some(domain) = name.and_then(|n| self.backend.domains.get(n)) {
            backend.grpc_endpoint.clone_from(&domain.grpc_endpoint);
            backend.slots = domain.slots;
            backend.pipelining_depth = domain.pipelining_depth;
            backend.no_corun.clone_from(&domain.no_corun);
            backend.preemptible_lanes = domain.preemptible_lanes;
            backend.interference.clone_from(&domain.interference);
        }
        let models = self
            .models
            .iter()
            .filter(|(_, m)| m.domain_name() == name)
            .map(|(k, m)| {
                let mut m = m.clone();
                m.domain = None;
                (k.clone(), m)
            })
            .collect();
        Self {
            version: self.version,
            backend,
            models,
        }
    }

    /// Kein Doppelbesitz (ADR-0037).
    ///
    /// Gibt die Fundstellen zurueck, an denen ein Paar ueber Domaenengrenzen
    /// gemeldet wurde: die Aufloesung der Domaene faende dort ein
    /// „unbekanntes Modell", und das waere die falsche Erklaerung.
    fn check_domains(
        &self,
        model_endpoints: &[String],
        findings: &mut Vec<Located>,
    ) -> Vec<String> {
        self.check_domain_entries(findings);
        self.check_endpoint_owners(model_endpoints, findings);

        // Paare gelten innerhalb einer GPU.
        let mut covered = Vec::new();
        let base_corun = self
            .backend
            .no_corun
            .iter()
            .enumerate()
            .map(|(i, [a, b])| (format!("backend.no_corun[{i}]"), [a.as_str(), b.as_str()]));
        let base_interference = self.backend.interference.iter().enumerate().map(|(i, p)| {
            (
                format!("backend.interference[{i}]"),
                [p.victim.as_str(), p.co_tenant.as_str()],
            )
        });
        self.refuse_cross_domain_pairs(
            base_corun.chain(base_interference),
            None,
            findings,
            &mut covered,
        );
        for (name, domain) in &self.backend.domains {
            let corun = domain.no_corun.iter().enumerate().map(|(i, [a, b])| {
                (
                    format!("backend.domains.{name}.no_corun[{i}]"),
                    [a.as_str(), b.as_str()],
                )
            });
            let interference = domain.interference.iter().enumerate().map(|(i, p)| {
                (
                    format!("backend.domains.{name}.interference[{i}]"),
                    [p.victim.as_str(), p.co_tenant.as_str()],
                )
            });
            self.refuse_cross_domain_pairs(
                corun.chain(interference),
                Some(name),
                findings,
                &mut covered,
            );
        }
        covered
    }

    /// Name, Endpunkt, Belegung und GPU jeder benannten Domaene.
    fn check_domain_entries(&self, findings: &mut Vec<Located>) {
        let populated =
            |domain: Option<&str>| self.models.values().any(|m| m.domain_name() == domain);

        let mut gpus: Vec<u32> = Vec::new();
        if populated(None) {
            gpus.push(DEFAULT_GPU_INDEX);
        }
        for (name, domain) in &self.backend.domains {
            let path = format!("backend.domains.{name}");
            if name == DEFAULT_DOMAIN {
                findings.push(
                    ConfigError::Inconsistent {
                        what: "der Name default steht fuer den backend-Block selbst",
                    }
                    .at(path.clone()),
                );
            } else if !valid_domain_name(name) {
                findings.push(
                    ConfigError::OutOfRange {
                        expected: "Domaenennamen aus Kleinbuchstaben, Ziffern, _ und -, \
                                   hoechstens 32 Zeichen; der Name steht als Label in \
                                   jeder Kennzahl",
                    }
                    .at(path.clone()),
                );
            }
            if domain.grpc_endpoint.trim().is_empty() {
                findings.push(
                    ConfigError::Missing {
                        what: "eine Domaene braucht den Endpunkt ihres Backends",
                    }
                    .at(format!("{path}.grpc_endpoint")),
                );
            }
            if !populated(Some(name)) {
                findings.push(
                    ConfigError::Inconsistent {
                        what: "eine Domaene ohne Modell ist ein Kapazitaetsbesitzer ohne \
                               Arbeit; models.<name>.domain nennt sie",
                    }
                    .at(path.clone()),
                );
                continue;
            }
            if gpus.contains(&domain.gpu_index) {
                findings.push(
                    ConfigError::Inconsistent {
                        what: "zwei Domaenen auf derselben GPU: eine GPU hat genau einen \
                               Kapazitaetsbesitzer, und zwei Server auf einer GPU sind \
                               keine zwei Recheneinheiten (ADR-0004)",
                    }
                    .at(format!("{path}.gpu_index")),
                );
            } else {
                gpus.push(domain.gpu_index);
            }
        }
    }

    /// Ein Endpunkt gehoert genau einer Domaene.
    ///
    /// Sonst gaeben zwei Scheduler Arbeit an dieselbe Recheneinheit, und
    /// keiner saehe die des anderen.
    fn check_endpoint_owners(&self, model_endpoints: &[String], findings: &mut Vec<Located>) {
        let mut owners: Vec<(&str, Option<&str>)> = Vec::new();
        for ((name, model), endpoint) in self.models.iter().zip(model_endpoints) {
            let domain = model.domain_name();
            match owners.iter().find(|(e, _)| *e == endpoint.as_str()) {
                Some((_, owner)) if *owner != domain => findings.push(
                    ConfigError::Inconsistent {
                        what: "ein Endpunkt gehoert genau einer Domaene: zwei Scheduler, \
                               die Arbeit an denselben Server geben, sehen einander nicht",
                    }
                    .at(format!("models.{name}.backend_endpoint")),
                ),
                Some(_) => {}
                None => owners.push((endpoint.as_str(), domain)),
            }
        }
    }

    /// Meldet Paare, deren Modelle in verschiedenen Domaenen stehen.
    ///
    /// Ein Name, den es gar nicht gibt, bleibt der Aufloesung der Domaene
    /// ueberlassen: dort ist „nicht konfiguriert" die richtige Erklaerung.
    fn refuse_cross_domain_pairs<'a>(
        &self,
        pairs: impl Iterator<Item = (String, [&'a str; 2])>,
        domain: Option<&str>,
        findings: &mut Vec<Located>,
        covered: &mut Vec<String>,
    ) {
        for (path, pair) in pairs {
            let domains: Vec<Option<Option<&str>>> = pair
                .iter()
                .map(|m| self.models.get(*m).map(ModelConfig::domain_name))
                .collect();
            let known = domains.iter().all(Option::is_some);
            if known && domains.iter().any(|d| *d != Some(domain)) {
                findings.push(
                    ConfigError::Inconsistent {
                        what: "no_corun und Interferenz gelten innerhalb einer Domaene: \
                               Modelle auf verschiedenen GPUs laufen ohnehin \
                               gleichzeitig, und eine Stoerung ueber Geraetegrenzen hat \
                               niemand gemessen",
                    }
                    .at(path.clone()),
                );
                covered.push(path);
            }
        }
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

    /// Das Mindestlaufzeitbudget aus `contract.min_runtime` (ADR-0046).
    ///
    /// Null, zu lang und die falsche Klasse prueft der Vertrag; die Slotzahl
    /// kennt erst die Aufloesung.
    fn build_min_runtime(
        &self,
        path: &str,
        findings: &mut Vec<Located>,
    ) -> Option<vig_core::runtime_budget::RuntimeBudget> {
        let raw = self.contract.min_runtime?;
        match (
            duration_ms(raw.budget_ms, "min_runtime.budget_ms"),
            duration_ms(raw.window_ms, "min_runtime.window_ms"),
        ) {
            (Ok(budget), Ok(window)) => {
                Some(vig_core::runtime_budget::RuntimeBudget { budget, window })
            }
            (Err(e), _) | (_, Err(e)) => {
                findings.push(e.at(format!("{path}.contract.min_runtime")));
                None
            }
        }
    }

    /// Die Zeiten des Vertrags aus ihren Millisekundenangaben.
    ///
    /// Sie gehoeren zusammen, und sie teilen eine Regel: „nicht angegeben" und
    /// „angegeben und unzulaessig" duerfen nicht dasselbe Ergebnis haben. Ein
    /// still verworfenes `max_age_ms` hiesse, dieser Strom altert nie und
    /// veraltete Frames werden nie verworfen — genau die Regel, die das
    /// Produkt ausmacht, waere durch einen Tippfehler abgeschaltet.
    fn build_times(
        &self,
        path: &str,
        findings: &mut Vec<Located>,
    ) -> (Duration, Option<Duration>, Option<Duration>) {
        let mut fail = |e: ConfigError, sub: &str| {
            findings.push(e.at(format!("{path}.{sub}")));
        };
        let deadline = match duration_ms(self.contract.deadline_ms, "deadline_ms") {
            Ok(d) => d,
            Err(e) => {
                fail(e, "contract.deadline_ms");
                Duration::from_nanos_unbounded(1)
            }
        };
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
        (deadline, period, max_age)
    }

    /// Die Zusage aus der Konfiguration (ADR-0047).
    ///
    /// Wie das Budget: hier entstehen nur die Zeiten, geprueft wird gegen die
    /// Periode im Vertrag — dort steht sie.
    fn build_objective(
        &self,
        path: &str,
        findings: &mut Vec<Located>,
    ) -> Option<vig_core::objective::Objective> {
        let raw = self.contract.objective?;
        let window = match duration_ms(raw.window_ms, "objective.window_ms") {
            Ok(window) => window,
            Err(e) => {
                findings.push(e.at(format!("{path}.contract.objective")));
                return None;
            }
        };
        let max_gap = match raw.max_gap_ms {
            None => None,
            Some(gap) => match duration_ms(gap, "objective.max_gap_ms") {
                Ok(gap) => Some(gap),
                Err(e) => {
                    findings.push(e.at(format!("{path}.contract.objective")));
                    return None;
                }
            },
        };
        Some(vig_core::objective::Objective {
            coverage_permille: raw.coverage_permille,
            window,
            max_gap,
        })
    }

    fn to_contract(
        &self,
        path: &str,
        findings: &mut Vec<Located>,
    ) -> Option<(ModelContract, Vec<String>)> {
        // Vor der Closure: beide leihen `findings` aus, und zwei Ausleihen
        // zugleich gibt es nicht.
        let (deadline, period, max_age) = self.build_times(path, findings);

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

        let min_runtime = self.build_min_runtime(path, findings);
        let objective = self.build_objective(path, findings);

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
            min_runtime,
            objective,
        };

        if let Err(e) = contract.validate() {
            findings.push(ConfigError::Contract(e).at(path.to_owned()));
        }
        Some((contract, physical))
    }
}
