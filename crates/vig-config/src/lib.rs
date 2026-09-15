//! Konfigurationsschema, Parser und Validator (Spec 6.2, 7.1 L-020, 23).
//!
//! Zwei Zusagen bestimmen dieses Crate:
//!
//! * **Spec L-020 — Fail-safe Configuration.** Eine ungueltige Konfiguration
//!   darf den Prozess nicht stillschweigend mit riskanten Defaultwerten starten
//!   lassen. Es gibt hier deshalb keine Reparatur, nur Ablehnung mit einer
//!   Meldung, die sagt, was zu tun ist.
//! * **Spec 6.1 — niedrige Integrationshuerde.** Was der Nutzer nicht angibt,
//!   soll einen erklaerbaren Default haben. Defaults, die *riskant* waeren,
//!   gibt es nicht: Queue-Kapazitaeten, Deadlines und Qualitaetswerte muessen
//!   deklariert werden.
//!
//! Die Trennung zwischen beidem verlaeuft entlang der Frage, ob ein falscher
//! Wert **still** falsch waere. Eine fehlende Queue-Kapazitaet ist stumm
//! gefaehrlich (unbeschraenktes Puffern), ein fehlendes `pipelining_depth`
//! nicht.

pub mod error;
pub mod manifest;
pub mod schema;
pub mod window;

pub use error::{ConfigError, Located};
pub use manifest::{
    ArtifactIdentity, DeviceIdentity, FieldVerdict, MANIFEST_REVISION, ManifestComparison,
    ManifestVerdict, MeasurementBounds, OperatingPoint, ProfileManifest, ResourceLayout,
    RuntimeIdentity, ValidityDomain,
};
pub use schema::{
    BackendConfig, Config, ContractConfig, CooperativeConfig, ModelConfig, QualityConfig,
    QueueConfigYaml, VariantConfig,
};
