//! Das YAML-Schema und seine Uebersetzung in Kerntypen (Spec 6.2).

use crate::error::{ConfigError, Located};
use onetimer_core::Duration;
use onetimer_core::arrayvec::ArrayVec;
use onetimer_core::ids::{MAX_MODELS, MAX_VARIANTS, ModelIdx};
use onetimer_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use onetimer_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use onetimer_core::queue::QueueConfig;
use onetimer_core::request::{Criticality, OverflowPolicy, QueuePolicy};
use onetimer_core::slots::SlotSet;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Die einzige unterstuetzte Schemaversion.
pub const SCHEMA_VERSION: u32 = 1;

/// Die vollstaendige Konfiguration.
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(default)]
    pub no_corun: Vec<[String; 2]>,
    /// Sicherheitsmarge auf die Laufzeitprognose, in Prozent (Spec 13.2).
    #[serde(default = "default_margin_percent")]
    pub safety_margin_percent: u32,
}

const fn default_pipelining() -> usize {
    1
}

const fn default_margin_percent() -> u32 {
    110
}

/// Ein logisches Modell.
#[derive(Debug, Clone, Deserialize)]
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
    /// Wahr fuer sequenzbasierte Modelle (Spec 12.5).
    #[serde(default)]
    pub stateful: bool,
    /// Zerlegbarkeit in kooperative Quanten (ADR-0014).
    ///
    /// Nur fuer Modelle, deren Arbeit fachlich zerlegbar ist — also
    /// generative. Ein Detektor gehoert nicht dazu; sein Vorwaertslauf ist
    /// unteilbar.
    #[serde(default)]
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
    #[serde(default)]
    pub backend_endpoint: Option<String>,
}

/// Die Angaben eines zerlegbaren Modells.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CooperativeConfig {
    /// Gemessene Erzeugungsrate in Token je Sekunde.
    pub tokens_per_second: u32,
    /// Kleinste sinnvolle Quantengroesse.
    #[serde(default = "default_min_tokens")]
    pub min_tokens: u32,
    /// Obergrenze der insgesamt erzeugten Token je Auftrag.
    pub max_total_tokens: u32,
}

const fn default_min_tokens() -> u32 {
    8
}

/// Das Queue-Verhalten eines Modells.
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractConfig {
    /// Erwartete Periode; ohne sie gibt es keinen Look-ahead (Spec 10.8).
    #[serde(default)]
    pub period_ms: Option<u64>,
    /// Relative Deadline ab Generation Time. Pflichtangabe.
    pub deadline_ms: u64,
    /// Fachliches Hoechstalter.
    ///
    /// Ohne diesen Wert kann OneTimer keine Arbeit als wertlos erkennen und
    /// verliert sein staerkstes Werkzeug (ADR-0010).
    #[serde(default)]
    pub max_age_ms: Option<u64>,
    /// Niedrigste akzeptable Variantenqualitaet, 0.0 bis 1.0.
    #[serde(default)]
    pub min_quality: Option<f64>,
    /// Mindestverweildauer vor einer Variantenaufwertung (Spec 12.4).
    #[serde(default = "default_dwell_ms")]
    pub variant_dwell_ms: u64,
}

const fn default_dwell_ms() -> u64 {
    100
}

/// Eine physische Variante.
#[derive(Debug, Clone, Deserialize)]
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
    /// Im Regelfall von `onetimer profile` erzeugt. Fehlt es, kann der
    /// Scheduler nicht planen — dann wird der Start verweigert, statt mit
    /// geratenen Laufzeiten zu arbeiten.
    #[serde(default)]
    pub profile: Option<ProfileConfig>,
}

/// Der Qualitaetswert einer Variante samt Herkunft.
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(default)]
    pub measured_on: Option<String>,
}

fn default_quality_source() -> String {
    "unknown".to_owned()
}

/// Ein Laufzeitprofil je Belegungsgrad.
#[derive(Debug, Clone, Deserialize)]
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
    /// Der Backend-Endpunkt je Modell, in Indexreihenfolge.
    pub model_endpoints: Vec<String>,
    /// Ob das Backendmodell nur ueber den Stream-Endpunkt antwortet.
    pub model_decoupled: Vec<bool>,
    /// Die Sicherheitsmarge.
    pub margin: SafetyMargin,
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

impl Config {
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
    /// `onetimer profile` braucht genau das und **nicht mehr**: es soll
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
    /// Der **erste** gefundene Fehler samt Fundstelle. `onetimer doctor` ruft
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
    /// Das ist der Modus fuer `onetimer doctor` (Spec 23): wer eine
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
        if self.backend.kind != "triton" {
            findings.push(
                ConfigError::UnknownValue {
                    found: self.backend.kind.clone(),
                    allowed: "triton",
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
        let mut contracts = ArrayVec::new();
        let mut backend_models = Vec::new();

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
                }
                None => return None,
            }
        }

        Self::apply_corun_rules(&self.backend.no_corun, &model_names, &mut slots, findings);

        Some(Resolved {
            backend_endpoint: self.backend.grpc_endpoint.clone(),
            slots,
            contracts,
            model_names,
            backend_models,
            model_endpoints,
            model_decoupled,
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
                        what: "kein Laufzeitprofil; `onetimer profile` ausfuehren oder \
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
            let _ = variants.push(Variant {
                quality: QualityValue {
                    value: quality,
                    source,
                },
                profile: VariantProfile::solo(profile),
            });
            physical.push(v.backend_model.clone());
        }
        Some((variants, physical))
    }
}

impl ModelConfig {
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
        let period = self
            .contract
            .period_ms
            .and_then(|p| duration_ms(p, "period_ms").ok());
        let max_age = self
            .contract
            .max_age_ms
            .and_then(|a| duration_ms(a, "max_age_ms").ok());
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

        let contract = ModelContract {
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
            cooperative: self.cooperative.map(|c| onetimer_core::model::Cooperative {
                tokens_per_second: c.tokens_per_second,
                min_tokens: c.min_tokens,
                max_total_tokens: c.max_total_tokens,
            }),
        };

        if let Err(e) = contract.validate() {
            findings.push(ConfigError::Contract(e).at(path.to_owned()));
        }
        Some((contract, physical))
    }
}
