//! Der Umgebungsfingerabdruck eines Backendmodells (G-010, L-014).
//!
//! Ein Laufzeitprofil gilt nur unter den Bedingungen, unter denen es gemessen
//! wurde. Wird das Modell ausgetauscht oder der Server aktualisiert, sind die
//! Quantile Vergangenheit — die Spezifikation verlangt in G-010 ausdruecklich,
//! dass ein solches Profil nicht stillschweigend als exakt gueltig gilt.
//!
//! ## Was von aussen sichtbar ist
//!
//! OneTimer sitzt vor dem Server und nicht in ihm. Beobachtbar ist nur, was
//! das Backend ueber sich selbst meldet: Servername und -version, der
//! Modellname, die Modellversionen, die Plattform und die Ein-/Ausgabesignatur.
//!
//! Diese Auswahl faengt die haeufigen Faelle: Serverwechsel, Backendwechsel
//! (ONNX nach TensorRT), geaenderte Aufloesung, neue Modellversion. Sie faengt
//! **nicht** den Fall, dass jemand die Gewichtsdatei unter derselben
//! Versionsnummer austauscht, ohne die Signatur zu aendern — das kann von
//! aussen niemand sehen. Dagegen hilft nur der Online Estimator, der die
//! tatsaechlichen Laufzeiten misst und das Profil binnen Sekunden korrigiert.
//!
//! Der Fingerabdruck ist also eine billige erste Verteidigung, keine Garantie.
//! Er wird auch so behandelt (ADR-0016).

use onetimer_protocol_oip::inference::{ModelMetadataResponse, ServerMetadataResponse};

/// FNV-1a, 64 Bit.
///
/// Von Hand implementiert und nicht `DefaultHasher`: dessen Ergebnis darf sich
/// laut eigener Dokumentation zwischen Rust-Versionen aendern. Ein
/// Fingerabdruck, der nach einem Toolchain-Update anders lautet, wuerde bei
/// jedem Upgrade falschen Alarm ausloesen und damit genau die Warnung
/// entwerten, um die es hier geht.
struct Fnv1a(u64);

impl Fnv1a {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
        // Feldtrenner, damit ("ab","c") und ("a","bc") verschieden hashen.
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }

    const fn finish(&self) -> u64 {
        self.0
    }
}

/// Der Fingerabdruck aus Server- und Modellmetadaten.
///
/// Stabil ueber Prozessstarts und Rust-Versionen; unabhaengig von der
/// Reihenfolge, in der der Server Ein- und Ausgaben meldet, ist er
/// **nicht** — das ist gewollt, eine geaenderte Signaturreihenfolge ist eine
/// Aenderung am Modell.
#[must_use]
pub fn fingerprint(server: &ServerMetadataResponse, model: &ModelMetadataResponse) -> String {
    let mut hasher = Fnv1a::new();
    hasher.write(server.name.as_bytes());
    hasher.write(server.version.as_bytes());
    hasher.write(model.name.as_bytes());
    for version in &model.versions {
        hasher.write(version.as_bytes());
    }
    hasher.write(model.platform.as_bytes());
    for tensor in &model.inputs {
        hasher.write(tensor.name.as_bytes());
        hasher.write(tensor.datatype.as_bytes());
        for dim in &tensor.shape {
            hasher.write(&dim.to_le_bytes());
        }
    }
    for tensor in &model.outputs {
        hasher.write(tensor.name.as_bytes());
        hasher.write(tensor.datatype.as_bytes());
        for dim in &tensor.shape {
            hasher.write(&dim.to_le_bytes());
        }
    }
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use onetimer_protocol_oip::inference::model_metadata_response::TensorMetadata;

    fn server(version: &str) -> ServerMetadataResponse {
        ServerMetadataResponse {
            name: "triton".to_owned(),
            version: version.to_owned(),
            extensions: Vec::new(),
        }
    }

    fn model(platform: &str, shape: Vec<i64>) -> ModelMetadataResponse {
        ModelMetadataResponse {
            name: "rfdetr".to_owned(),
            versions: vec!["1".to_owned()],
            platform: platform.to_owned(),
            inputs: vec![TensorMetadata {
                name: "input".to_owned(),
                datatype: "FP32".to_owned(),
                shape,
            }],
            outputs: Vec::new(),
        }
    }

    #[test]
    fn the_same_environment_yields_the_same_fingerprint() {
        let a = fingerprint(
            &server("2.70.0"),
            &model("onnxruntime_onnx", vec![1, 3, 512, 512]),
        );
        let b = fingerprint(
            &server("2.70.0"),
            &model("onnxruntime_onnx", vec![1, 3, 512, 512]),
        );
        assert_eq!(a, b);
    }

    /// Ein Backendwechsel aendert die Laufzeiten drastisch und muss auffallen.
    #[test]
    fn a_different_platform_yields_a_different_fingerprint() {
        let onnx = fingerprint(
            &server("2.70.0"),
            &model("onnxruntime_onnx", vec![1, 3, 512, 512]),
        );
        let trt = fingerprint(
            &server("2.70.0"),
            &model("tensorrt_plan", vec![1, 3, 512, 512]),
        );
        assert_ne!(onnx, trt);
    }

    /// Eine andere Aufloesung ist ein anderes Modell.
    #[test]
    fn a_different_shape_yields_a_different_fingerprint() {
        let small = fingerprint(
            &server("2.70.0"),
            &model("onnxruntime_onnx", vec![1, 3, 512, 512]),
        );
        let large = fingerprint(
            &server("2.70.0"),
            &model("onnxruntime_onnx", vec![1, 3, 800, 800]),
        );
        assert_ne!(small, large);
    }

    #[test]
    fn a_server_upgrade_yields_a_different_fingerprint() {
        let old = fingerprint(
            &server("2.70.0"),
            &model("onnxruntime_onnx", vec![1, 3, 512, 512]),
        );
        let new = fingerprint(
            &server("2.71.0"),
            &model("onnxruntime_onnx", vec![1, 3, 512, 512]),
        );
        assert_ne!(old, new);
    }

    /// Der Feldtrenner verhindert, dass verschobene Grenzen gleich hashen.
    #[test]
    fn field_boundaries_matter() {
        let mut a = Fnv1a::new();
        a.write(b"ab");
        a.write(b"c");
        let mut b = Fnv1a::new();
        b.write(b"a");
        b.write(b"bc");
        assert_ne!(a.finish(), b.finish());
    }
}
