//! Was kann dieses Backend? (Plug-and-Play)
//!
//! Vigilant spricht das Open Inference Protocol, und das ist ein offener
//! Standard: Triton spricht es, ebenso OpenVINO Model Server, MLServer,
//! TorchServe und KServe. Bisher hat der Governor trotzdem stillschweigend
//! Triton angenommen — insbesondere, dass System Shared Memory verfuegbar ist.
//!
//! Das muss man nicht annehmen. Der Standard sieht vor, dass ein Server seine
//! Erweiterungen in den Servermetadaten nennt; Triton meldet dort unter
//! anderem `system_shared_memory`, `sequence` und `binary_tensor_data`. Wer
//! diese Liste liest, statt zu raten, kann dem Betreiber sagen, was auf
//! *seinem* Server funktioniert und was ihn stattdessen erwartet.
//!
//! Der Unterschied ist nicht akademisch: ohne Shared Memory kostet ein
//! 6,2-MB-Bild auf dem Kopierpfad +11,7 ms statt +160 us — Faktor 73
//! (`docs/benchmark/data-plane.md`). Ein Betreiber, der das erst im Betrieb
//! merkt, hat die falsche Hardware gekauft.

use vig_protocol_oip::inference::ServerMetadataResponse;

/// Eine Erweiterung, auf die Vigilant sich stuetzen kann.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extension {
    /// Referenzuebergabe ueber System Shared Memory (Spec 14, ADR-0003).
    SystemSharedMemory,
    /// Referenzuebergabe ueber CUDA Shared Memory.
    CudaSharedMemory,
    /// Tensoren als Rohbytes statt als typisierte Felder.
    BinaryTensorData,
    /// Sequence Batching — Voraussetzung fuer `stateful` (WP24).
    Sequence,
    /// Modellverzeichnis abfragbar; erlaubt spaeter automatische Entdeckung.
    ModelRepository,
}

impl Extension {
    /// Der Name, unter dem der Standard die Erweiterung fuehrt.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SystemSharedMemory => "system_shared_memory",
            Self::CudaSharedMemory => "cuda_shared_memory",
            Self::BinaryTensorData => "binary_tensor_data",
            Self::Sequence => "sequence",
            Self::ModelRepository => "model_repository",
        }
    }
}

/// Die Erweiterungen, auf die Vigilant sich stuetzt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Alles, was der Server meldet — wortwoertlich.
    pub advertised: Vec<String>,
}

impl Capabilities {
    /// Liest die Faehigkeiten aus den Servermetadaten.
    ///
    /// Meldet ein Server gar keine Erweiterungen, gilt alles als nicht
    /// vorhanden. Das ist die sichere Richtung: Vigilant nimmt dann den
    /// Kopierpfad und funktioniert, statt eine Erweiterung zu benutzen, die
    /// es nicht gibt.
    #[must_use]
    pub fn from_metadata(metadata: &ServerMetadataResponse) -> Self {
        Self {
            advertised: metadata.extensions.clone(),
        }
    }

    /// Meldet der Server diese Erweiterung?
    ///
    /// Triton haengt an manche Namen eine Klammer mit Unterfunktionen, etwa
    /// `model_repository(unload_dependents)`. Der Praefix zaehlt.
    #[must_use]
    pub fn has(&self, extension: Extension) -> bool {
        let name = extension.name();
        self.advertised
            .iter()
            .any(|e| e == name || e.starts_with(&format!("{name}(")))
    }

    /// Kann der Datenpfad Referenzen durchreichen?
    #[must_use]
    pub fn can_pass_references(&self) -> bool {
        self.has(Extension::SystemSharedMemory) || self.has(Extension::CudaSharedMemory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(extensions: &[&str]) -> ServerMetadataResponse {
        ServerMetadataResponse {
            name: "server".to_owned(),
            version: "1".to_owned(),
            extensions: extensions.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn triton_advertises_what_vig_needs() {
        let caps = Capabilities::from_metadata(&meta(&[
            "classification",
            "sequence",
            "model_repository",
            "model_repository(unload_dependents)",
            "system_shared_memory",
            "cuda_shared_memory",
            "binary_tensor_data",
        ]));
        assert!(caps.has(Extension::SystemSharedMemory));
        assert!(caps.has(Extension::Sequence));
        assert!(caps.can_pass_references());
    }

    /// Ein reiner CPU-Server ohne Shared Memory muss erkannt werden, nicht
    /// angenommen.
    #[test]
    fn a_server_without_shared_memory_is_recognised() {
        let caps = Capabilities::from_metadata(&meta(&["binary_tensor_data"]));
        assert!(!caps.has(Extension::SystemSharedMemory));
        assert!(!caps.can_pass_references());
        assert!(caps.has(Extension::BinaryTensorData));
    }

    /// Meldet ein Server nichts, wird nichts angenommen.
    #[test]
    fn silence_means_no_extensions() {
        let caps = Capabilities::from_metadata(&meta(&[]));
        assert_eq!(caps, Capabilities::default());
        assert!(!caps.can_pass_references());
    }

    /// Triton haengt an manche Erweiterungen eine Klammer mit Unterfunktionen.
    #[test]
    fn parenthesised_variants_still_count() {
        let caps = Capabilities::from_metadata(&meta(&["model_repository(unload_dependents)"]));
        assert!(caps.has(Extension::ModelRepository));
    }
}
