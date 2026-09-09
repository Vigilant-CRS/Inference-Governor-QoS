//! Profilmanifest v2 — woher ein Laufzeitprofil stammt und wofuer es gilt.
//!
//! ## Warum der alte Fingerabdruck nicht reicht
//!
//! Bis hierhin trug ein Profil einen einzigen Hash ueber Servername,
//! Serverversion, Modellname, Modellversionen, Plattform und die I/O-Signatur
//! (G-010). Der faengt den haeufigsten Bedienfehler — falsches Modell, falsche
//! Form — und genau deshalb ist er auch geblieben. Er faengt aber nicht:
//!
//! * **Ausgetauschte Gewichte unter gleicher Versionsnummer.** Form, Namen und
//!   Datentypen bleiben, die Laufzeit aendert sich. Das Profil sagt dann etwas
//!   ueber ein Modell aus, das nicht mehr laeuft.
//! * **Eine andere Runtime unter gleicher Servermetadatenlage.** Ein
//!   TensorRT-Plan, neu gebaut mit einer anderen TensorRT-Version, meldet
//!   dieselbe Plattform.
//! * **Eine andere Aufteilung derselben Karte.** Zwei Instanzen statt einer,
//!   MPS statt exklusiv, ein aktivierter Rate Limiter — die Zahlen gelten
//!   nicht mehr, und nichts an den Metadaten verraet es.
//! * **Ein anderes Geraet.** Der Server kennt seine GPU nicht; ueber das
//!   Protokoll ist sie nicht erreichbar.
//!
//! ## Die Regel dieses Moduls
//!
//! **Ein fehlendes Feld ist `unknown`, nie `verified`.** Ein Manifest, in dem
//! die Geraeteidentitaet fehlt, ist ein Manifest ohne Geraeteidentitaet — kein
//! bestandener Vergleich. Der Gesamtbefund ist deshalb hoechstens so gut wie
//! das schwaechste Feld, und ein einziger Widerspruch in einem
//! identitaetstragenden Feld macht das Profil ungueltig.
//!
//! ## Was hier bewusst *nicht* passiert
//!
//! Das Manifest **misst** nichts und **liest** nichts. Es ist reine Daten
//! plus Vergleich. Woher Geraetename, Treiberversion oder Artefakt-Digest
//! kommen, entscheidet der Aufrufer: `vig profile` traegt ein, was ihm der
//! Betreiber nennt und was es am Backend abzufragen gibt. Die automatische
//! Hardwareerfassung ist ein eigenes Paket (NV-04) und haengt von diesem hier
//! ab, nicht umgekehrt.

use serde::{Deserialize, Serialize};

/// Die Revision, die dieser Code schreibt.
///
/// Revision 1 ist das Legacy-Format: nur `fingerprint`, kein Manifest.
pub const MANIFEST_REVISION: u32 = 2;

/// Woher ein Profil stammt und wofuer es gilt.
///
/// Jedes Feld ist optional. Das ist kein Schlamperei-Zugestaendnis, sondern
/// die Bedingung dafuer, dass Fehlen von Widerspruch unterschieden werden
/// kann: `None` heisst "hier wurde nichts festgehalten", nicht "passt".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileManifest {
    /// Formatrevision des Manifests.
    #[serde(default = "default_revision")]
    pub revision: u32,
    /// Wann gemessen wurde, als RFC-3339-Zeitstempel.
    ///
    /// Reine Dokumentation; verglichen wird er nie. Ein aelteres Profil ist
    /// nicht automatisch ein falsches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
    /// Das vermessene Artefakt.
    #[serde(default)]
    pub artifact: ArtifactIdentity,
    /// Die ausfuehrende Softwareumgebung.
    #[serde(default)]
    pub runtime: RuntimeIdentity,
    /// Das rechnende Geraet.
    #[serde(default)]
    pub device: DeviceIdentity,
    /// Wie das Geraet waehrend der Messung aufgeteilt war.
    #[serde(default)]
    pub resources: ResourceLayout,
    /// Unter welchen Bedingungen gemessen wurde.
    #[serde(default)]
    pub measurement: MeasurementBounds,
    /// Wofuer die Zahlen beansprucht werden.
    #[serde(default)]
    pub validity: ValidityDomain,
}

const fn default_revision() -> u32 {
    MANIFEST_REVISION
}

/// Die Identitaet des Modellartefakts selbst.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactIdentity {
    /// Digest ueber die Artefaktdateien, als `sha256:<hex>`.
    ///
    /// Dies ist das Feld, das ausgetauschte Gewichte unter gleicher
    /// Versionsnummer faengt — und der Grund, warum es dieses Modul gibt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Woraus der Digest gebildet wurde, etwa `onnx`, `plan` oder `directory`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Groesse der einbezogenen Dateien in Bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Die vom Backend gemeldeten Modellversionen, unveraendert.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<String>,
}

/// Die Identitaet der ausfuehrenden Software.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    /// Servername, etwa `triton`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Serverversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    /// Die Plattform des Modells, etwa `tensorrt_plan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Version der ausfuehrenden Bibliothek, etwa `TensorRT 10.3.0`.
    ///
    /// Ueber das Inferenzprotokoll nicht erreichbar; kommt vom Betreiber.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_version: Option<String>,
}

/// Die Identitaet des Geraets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceIdentity {
    /// Geraetename, wie ihn der Treiber meldet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Compute Capability, etwa `8.6`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_capability: Option<String>,
    /// Geraetespeicher in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u64>,
    /// Treiberversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
}

/// Wie das Geraet waehrend der Messung aufgeteilt war.
///
/// Zwei Instanzen desselben Modells auf derselben Karte ergeben andere
/// Laufzeiten als eine. Ohne dieses Feld sieht ein Profil aus wie das andere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLayout {
    /// Aufteilungsart: `exclusive`, `mps`, `mig:<profil>`, `timeslice`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
    /// Anzahl der Modellinstanzen auf dem Geraet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instances: Option<u32>,
    /// Zustand des Rate Limiters, etwa `off` oder `resources`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limiter: Option<String>,
}

/// Unter welchen Bedingungen die Zahlen entstanden sind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementBounds {
    /// Herkunft der Eingabedaten, etwa `zeros` oder ein Datensatzname.
    ///
    /// Ein mit Nullen gemessenes Profil ist fuer datenabhaengige Modelle —
    /// generative vor allem — kein Profil des Produktivbetriebs. Das
    /// hinzuschreiben ist billiger als es spaeter zu erraten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset: Option<String>,
    /// Digest des Datensatzes, wenn es einen gibt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_digest: Option<String>,
    /// Batchgroesse waehrend der Messung.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<u32>,
    /// Wie viele Anfragen gleichzeitig unterwegs waren.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
    /// Aufwaermlaeufe vor der Messung.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup: Option<u32>,
    /// Wie viele **unabhaengige** Laeufe zusammengefasst wurden.
    ///
    /// Zweihundert Messwerte aus einem Prozessstart sind nicht dasselbe wie
    /// zweihundert aus fuenf. Ein einzelner Lauf kann eine Taktstufe, einen
    /// Cachezustand oder einen Nachbarprozess konserviert haben.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub independent_runs: Option<u32>,
}

/// Wofuer die gemessenen Zahlen beansprucht werden.
///
/// Die Gueltigkeitsdomaene ist eine **Zusage des Messenden**, keine
/// Ableitung aus den Messwerten. Sie zu ueberschreiten macht das Profil nicht
/// falsch, aber unbelegt — und der Unterschied gehoert in die Planung.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidityDomain {
    /// Bis zu wie vielen gleichzeitig aktiven Modellen die Zahlen gelten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_models: Option<u32>,
    /// Bis zu welcher serialisierten Auslastung in Prozent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_occupancy_pct: Option<u32>,
    /// Bis zu welcher Eingabegroesse in KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_kib: Option<u64>,
}

/// Ein Betriebspunkt, gegen den eine Gueltigkeitsdomaene geprueft wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperatingPoint {
    /// Wie viele Modelle gerade aktiv sind.
    pub concurrent_models: u32,
    /// Die serialisierte Auslastung in Prozent.
    pub occupancy_pct: u32,
    /// Die Eingabegroesse in KiB.
    pub input_kib: u64,
}

impl ValidityDomain {
    /// Ob die Domaene diesen Betriebspunkt abdeckt.
    ///
    /// Eine nicht angegebene Grenze deckt nichts ab, sondern sagt nichts:
    /// Der Rueckgabewert ist dann `None`. Nur wo eine Grenze steht, gibt es
    /// eine belegte Ja/Nein-Antwort.
    #[must_use]
    pub fn covers(&self, point: &OperatingPoint) -> Option<bool> {
        let mut stated = false;
        let mut inside = true;
        if let Some(limit) = self.max_concurrent_models {
            stated = true;
            inside = inside && point.concurrent_models <= limit;
        }
        if let Some(limit) = self.max_occupancy_pct {
            stated = true;
            inside = inside && point.occupancy_pct <= limit;
        }
        if let Some(limit) = self.max_input_kib {
            stated = true;
            inside = inside && point.input_kib <= limit;
        }
        stated.then_some(inside)
    }
}

/// Der Befund eines einzelnen Feldvergleichs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldVerdict {
    /// Beide Seiten nennen denselben Wert.
    Match,
    /// Beide Seiten nennen einen Wert, und die Werte widersprechen sich.
    Divergent {
        /// Was im Profil steht.
        declared: String,
        /// Was gerade beobachtet wird.
        actual: String,
    },
    /// Mindestens eine Seite sagt nichts.
    Unknown {
        /// Wer schweigt.
        missing: MissingSide,
    },
}

/// Welche Seite eines Vergleichs geschwiegen hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingSide {
    /// Das Profil enthaelt das Feld nicht.
    Declared,
    /// Die Beobachtung liefert das Feld nicht.
    Observed,
    /// Beide nicht.
    Both,
}

fn compare_field(declared: Option<&str>, observed: Option<&str>) -> FieldVerdict {
    match (declared, observed) {
        (Some(a), Some(b)) if a == b => FieldVerdict::Match,
        (Some(a), Some(b)) => FieldVerdict::Divergent {
            declared: a.to_owned(),
            actual: b.to_owned(),
        },
        (None, Some(_)) => FieldVerdict::Unknown {
            missing: MissingSide::Declared,
        },
        (Some(_), None) => FieldVerdict::Unknown {
            missing: MissingSide::Observed,
        },
        (None, None) => FieldVerdict::Unknown {
            missing: MissingSide::Both,
        },
    }
}

/// Ein Feldvergleich mit Namen und Gewicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldComparison {
    /// Der Pfad des Feldes, etwa `artifact.digest`.
    pub field: &'static str,
    /// Ob ein Widerspruch hier das Profil ungueltig macht.
    pub identity_bearing: bool,
    /// Der Befund.
    pub verdict: FieldVerdict,
}

/// Der Gesamtbefund eines Manifestvergleichs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ManifestVerdict {
    /// Ein identitaetstragendes Feld widerspricht sich. Das Profil gilt nicht.
    Invalid,
    /// Kein Widerspruch, aber Luecken. Das Profil ist nicht belegt.
    Unverified,
    /// Alle identitaetstragenden Felder stimmen ueberein und sind belegt.
    Verified,
}

/// Das Ergebnis eines vollstaendigen Manifestvergleichs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestComparison {
    /// Jeder verglichene Feldbefund, in stabiler Reihenfolge.
    pub fields: Vec<FieldComparison>,
}

impl ManifestComparison {
    /// Vergleicht ein hinterlegtes Manifest mit einer Beobachtung.
    ///
    /// Identitaetstragend sind Artefakt, Runtime, Geraet und Aufteilung —
    /// bewusst nicht die Messbedingungen und nicht die Gueltigkeitsdomaene.
    /// Eine mit anderer Batchgroesse gemessene Zahl ist keine Aussage ueber
    /// ein anderes Modell, sondern eine ueber einen anderen Betriebspunkt;
    /// die gehoert in die Planung, nicht in die Identitaet.
    #[must_use]
    pub fn new(declared: &ProfileManifest, observed: &ProfileManifest) -> Self {
        let mut fields = Vec::new();
        let mut push =
            |field: &'static str, identity_bearing: bool, a: Option<&str>, b: Option<&str>| {
                fields.push(FieldComparison {
                    field,
                    identity_bearing,
                    verdict: compare_field(a, b),
                });
            };

        push(
            "artifact.digest",
            true,
            declared.artifact.digest.as_deref(),
            observed.artifact.digest.as_deref(),
        );
        let declared_versions = join_versions(&declared.artifact.versions);
        let observed_versions = join_versions(&observed.artifact.versions);
        push(
            "artifact.versions",
            true,
            declared_versions.as_deref(),
            observed_versions.as_deref(),
        );
        push(
            "runtime.server",
            true,
            declared.runtime.server.as_deref(),
            observed.runtime.server.as_deref(),
        );
        push(
            "runtime.server_version",
            true,
            declared.runtime.server_version.as_deref(),
            observed.runtime.server_version.as_deref(),
        );
        push(
            "runtime.platform",
            true,
            declared.runtime.platform.as_deref(),
            observed.runtime.platform.as_deref(),
        );
        push(
            "runtime.library_version",
            true,
            declared.runtime.library_version.as_deref(),
            observed.runtime.library_version.as_deref(),
        );
        push(
            "device.name",
            true,
            declared.device.name.as_deref(),
            observed.device.name.as_deref(),
        );
        push(
            "device.compute_capability",
            true,
            declared.device.compute_capability.as_deref(),
            observed.device.compute_capability.as_deref(),
        );
        push(
            "device.driver",
            true,
            declared.device.driver.as_deref(),
            observed.device.driver.as_deref(),
        );
        push(
            "resources.partition",
            true,
            declared.resources.partition.as_deref(),
            observed.resources.partition.as_deref(),
        );
        let declared_instances = declared.resources.instances.map(|n| n.to_string());
        let observed_instances = observed.resources.instances.map(|n| n.to_string());
        push(
            "resources.instances",
            true,
            declared_instances.as_deref(),
            observed_instances.as_deref(),
        );
        push(
            "resources.rate_limiter",
            true,
            declared.resources.rate_limiter.as_deref(),
            observed.resources.rate_limiter.as_deref(),
        );
        push(
            "measurement.dataset",
            false,
            declared.measurement.dataset.as_deref(),
            observed.measurement.dataset.as_deref(),
        );

        Self { fields }
    }

    /// Der Gesamtbefund: so gut wie das schwaechste identitaetstragende Feld.
    #[must_use]
    pub fn verdict(&self) -> ManifestVerdict {
        let mut worst = ManifestVerdict::Verified;
        for field in &self.fields {
            if !field.identity_bearing {
                continue;
            }
            let this = match field.verdict {
                FieldVerdict::Match => ManifestVerdict::Verified,
                FieldVerdict::Divergent { .. } => ManifestVerdict::Invalid,
                FieldVerdict::Unknown { .. } => ManifestVerdict::Unverified,
            };
            worst = worst.min(this);
        }
        worst
    }

    /// Die widersprechenden Felder.
    pub fn divergences(&self) -> impl Iterator<Item = &FieldComparison> {
        self.fields
            .iter()
            .filter(|f| matches!(f.verdict, FieldVerdict::Divergent { .. }))
    }

    /// Die Felder, zu denen nichts feststellbar war.
    pub fn unknowns(&self) -> impl Iterator<Item = &FieldComparison> {
        self.fields
            .iter()
            .filter(|f| matches!(f.verdict, FieldVerdict::Unknown { .. }))
    }
}

fn join_versions(versions: &[String]) -> Option<String> {
    if versions.is_empty() {
        return None;
    }
    Some(versions.join(","))
}

impl ProfileManifest {
    /// Ob dieses Manifest ueberhaupt etwas aussagt.
    ///
    /// Ein Manifest, in dem nur die Revision steht, ist ein leeres Manifest;
    /// es zu schreiben waere irrefuehrend.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recorded_at.is_none()
            && self.artifact == ArtifactIdentity::default()
            && self.runtime == RuntimeIdentity::default()
            && self.device == DeviceIdentity::default()
            && self.resources == ResourceLayout::default()
            && self.measurement == MeasurementBounds::default()
            && self.validity == ValidityDomain::default()
    }

    /// Ein Legacy-Profil als Manifest lesen.
    ///
    /// Ein Profil aus der Zeit vor diesem Modul kennt nur einen Hash. Daraus
    /// laesst sich kein einziges Identitaetsfeld rekonstruieren — der Hash ist
    /// nicht umkehrbar. Das Ergebnis ist deshalb ein Manifest der Revision 1,
    /// in dem alles `unknown` ist. Genau so soll es sich auch verhalten:
    /// Ein Legacy-Profil ist nicht verifiziert, es ist unbelegt.
    #[must_use]
    pub fn legacy() -> Self {
        Self {
            revision: 1,
            ..Self::default()
        }
    }

    /// Ob dieses Manifest aus der Zeit vor Revision 2 stammt.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        self.revision < MANIFEST_REVISION
    }
}

/// Das Manifest als einrueckungsfertiger YAML-Block.
///
/// `vig profile` und `vig calibrate` schreiben die Konfigurationsdatei des
/// Nutzers nicht um — sie enthaelt Kommentare, und ein YAML-Serialisierer
/// verliert sie. Stattdessen geben sie einen Block aus, der sich einfuegen
/// laesst. Damit dieser Block garantiert wieder einlesbar ist, wird er
/// serialisiert und nicht von Hand zusammengesetzt.
///
/// # Errors
///
/// Wenn das Manifest nicht serialisierbar ist. Bei den hier verwendeten
/// Typen kann das nicht vorkommen; die Signatur sagt es trotzdem, statt zu
/// panicken.
pub fn to_yaml_block(manifest: &ProfileManifest, indent: usize) -> Result<String, String> {
    let text = serde_norway::to_string(manifest).map_err(|e| e.to_string())?;
    let pad = " ".repeat(indent);
    let mut out = String::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        out.push_str(&pad);
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn full() -> ProfileManifest {
        ProfileManifest {
            revision: MANIFEST_REVISION,
            recorded_at: Some("2026-09-09T21:00:00Z".to_owned()),
            artifact: ArtifactIdentity {
                digest: Some("sha256:aaaa".to_owned()),
                source: Some("directory".to_owned()),
                bytes: Some(123),
                versions: vec!["1".to_owned()],
            },
            runtime: RuntimeIdentity {
                server: Some("triton".to_owned()),
                server_version: Some("2.52.0".to_owned()),
                platform: Some("tensorrt_plan".to_owned()),
                library_version: Some("TensorRT 10.3.0".to_owned()),
            },
            device: DeviceIdentity {
                name: Some("NVIDIA GeForce RTX 3070".to_owned()),
                compute_capability: Some("8.6".to_owned()),
                memory_mib: Some(8192),
                driver: Some("560.35.03".to_owned()),
            },
            resources: ResourceLayout {
                partition: Some("exclusive".to_owned()),
                instances: Some(1),
                rate_limiter: Some("off".to_owned()),
            },
            measurement: MeasurementBounds {
                dataset: Some("zeros".to_owned()),
                dataset_digest: None,
                batch_size: Some(1),
                concurrency: Some(1),
                warmup: Some(20),
                independent_runs: Some(1),
            },
            validity: ValidityDomain {
                max_concurrent_models: Some(4),
                max_occupancy_pct: Some(92),
                max_input_kib: Some(3072),
            },
        }
    }

    #[test]
    fn a_manifest_survives_a_roundtrip() {
        let before = full();
        let text = to_yaml_block(&before, 0).unwrap();
        let after: ProfileManifest = serde_norway::from_str(&text).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn the_yaml_block_is_indentable() {
        let text = to_yaml_block(&full(), 8).unwrap();
        for line in text.lines() {
            assert!(line.starts_with("        "), "nicht eingerueckt: {line:?}");
        }
    }

    #[test]
    fn an_identical_manifest_verifies() {
        let comparison = ManifestComparison::new(&full(), &full());
        assert_eq!(comparison.verdict(), ManifestVerdict::Verified);
        assert_eq!(comparison.divergences().count(), 0);
        assert_eq!(comparison.unknowns().count(), 0);
    }

    #[test]
    fn the_same_tensor_shape_with_another_engine_hash_invalidates() {
        // Der Fall, den der alte Metadaten-Fingerabdruck nicht fangen kann:
        // Signatur, Version und Plattform unveraendert, andere Gewichte.
        let declared = full();
        let mut observed = full();
        observed.artifact.digest = Some("sha256:bbbb".to_owned());
        let comparison = ManifestComparison::new(&declared, &observed);
        assert_eq!(comparison.verdict(), ManifestVerdict::Invalid);
        let names: Vec<_> = comparison.divergences().map(|f| f.field).collect();
        assert_eq!(names, vec!["artifact.digest"]);
    }

    #[test]
    fn another_runtime_invalidates() {
        let mut observed = full();
        observed.runtime.library_version = Some("TensorRT 10.7.0".to_owned());
        let comparison = ManifestComparison::new(&full(), &observed);
        assert_eq!(comparison.verdict(), ManifestVerdict::Invalid);
    }

    #[test]
    fn another_resource_layout_invalidates() {
        let mut observed = full();
        observed.resources.instances = Some(2);
        let comparison = ManifestComparison::new(&full(), &observed);
        assert_eq!(comparison.verdict(), ManifestVerdict::Invalid);
        let names: Vec<_> = comparison.divergences().map(|f| f.field).collect();
        assert_eq!(names, vec!["resources.instances"]);
    }

    #[test]
    fn another_device_invalidates() {
        let mut observed = full();
        observed.device.name = Some("NVIDIA Jetson Orin".to_owned());
        assert_eq!(
            ManifestComparison::new(&full(), &observed).verdict(),
            ManifestVerdict::Invalid
        );
    }

    #[test]
    fn a_missing_field_is_unknown_and_never_verified() {
        let mut declared = full();
        declared.device.driver = None;
        let comparison = ManifestComparison::new(&declared, &full());
        assert_eq!(comparison.verdict(), ManifestVerdict::Unverified);
        let missing: Vec<_> = comparison.unknowns().map(|f| f.field).collect();
        assert_eq!(missing, vec!["device.driver"]);
    }

    #[test]
    fn silence_on_both_sides_is_not_agreement() {
        let mut declared = full();
        let mut observed = full();
        declared.artifact.digest = None;
        observed.artifact.digest = None;
        let comparison = ManifestComparison::new(&declared, &observed);
        assert_eq!(
            comparison.verdict(),
            ManifestVerdict::Unverified,
            "zwei Mal nichts ergibt keine Uebereinstimmung"
        );
    }

    #[test]
    fn a_divergence_outweighs_a_gap() {
        let mut declared = full();
        declared.device.driver = None;
        let mut observed = full();
        observed.artifact.digest = Some("sha256:bbbb".to_owned());
        assert_eq!(
            ManifestComparison::new(&declared, &observed).verdict(),
            ManifestVerdict::Invalid
        );
    }

    #[test]
    fn measurement_conditions_do_not_bear_identity() {
        let mut observed = full();
        observed.measurement.dataset = Some("coco-val".to_owned());
        observed.measurement.batch_size = Some(8);
        observed.validity.max_occupancy_pct = Some(50);
        let comparison = ManifestComparison::new(&full(), &observed);
        assert_eq!(
            comparison.verdict(),
            ManifestVerdict::Verified,
            "ein anderer Betriebspunkt ist kein anderes Modell"
        );
        assert_eq!(
            comparison.divergences().count(),
            1,
            "gemeldet wird er trotzdem"
        );
    }

    #[test]
    fn a_legacy_profile_is_unverified_not_verified() {
        let legacy = ProfileManifest::legacy();
        assert!(legacy.is_legacy());
        assert!(legacy.is_empty());
        assert_eq!(
            ManifestComparison::new(&legacy, &full()).verdict(),
            ManifestVerdict::Unverified
        );
    }

    #[test]
    fn a_manifest_without_a_revision_defaults_to_the_current_one() {
        let text = "artifact:\n  digest: sha256:aaaa\n";
        let parsed: ProfileManifest = serde_norway::from_str(text).unwrap();
        assert_eq!(parsed.revision, MANIFEST_REVISION);
        assert_eq!(parsed.artifact.digest.as_deref(), Some("sha256:aaaa"));
    }

    #[test]
    fn an_unstated_validity_domain_says_nothing() {
        let point = OperatingPoint {
            concurrent_models: 4,
            occupancy_pct: 92,
            input_kib: 3072,
        };
        assert_eq!(ValidityDomain::default().covers(&point), None);
        assert_eq!(full().validity.covers(&point), Some(true));
    }

    #[test]
    fn exceeding_a_stated_limit_leaves_the_domain() {
        let inside = OperatingPoint {
            concurrent_models: 4,
            occupancy_pct: 92,
            input_kib: 3072,
        };
        let outside = OperatingPoint {
            occupancy_pct: 93,
            ..inside
        };
        assert_eq!(full().validity.covers(&outside), Some(false));
    }

    #[test]
    fn a_partially_stated_domain_answers_only_for_what_it_states() {
        let domain = ValidityDomain {
            max_occupancy_pct: Some(80),
            ..ValidityDomain::default()
        };
        let point = OperatingPoint {
            concurrent_models: 99,
            occupancy_pct: 70,
            input_kib: u64::MAX,
        };
        assert_eq!(domain.covers(&point), Some(true));
    }
}
