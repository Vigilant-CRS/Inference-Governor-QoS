//! Ein Request mit nullgefuellten Eingaben passend zu den Modellmetadaten.
//!
//! Die Funktion lag urspruenglich im Profiler. Sie steht hier, weil inzwischen
//! drei Werkzeuge sie brauchen — Profiler, Kalibrator und Portabilitaetstest —
//! und weil sie nichts Triton-Eigenes enthaelt: sie folgt den Metadaten, die
//! jeder OIP-Server liefert.

use crate::error::BackendError;
use std::collections::HashMap;
use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{ModelInferRequest, ModelMetadataResponse};

/// Baut einen Request mit nullgefuellten Eingaben passend zu den Metadaten.
///
/// Nullen und keine Zufallsdaten: die Laufzeit eines Inferenzkernels haengt bei
/// den hier betrachteten Modellen nicht vom Inhalt ab, und reproduzierbare
/// Eingaben machen zwei Profilierungslaeufe vergleichbar.
///
/// # Errors
///
/// [`BackendError::Malformed`], wenn eine Eingabe eine dynamische Dimension
/// jenseits der Batchachse hat oder ihr Datentyp unbekannt ist.
pub fn zero_request(
    model: &str,
    metadata: &ModelMetadataResponse,
) -> Result<ModelInferRequest, BackendError> {
    let mut inputs = Vec::new();
    let mut contents = Vec::new();

    for input in &metadata.inputs {
        let mut shape = Vec::with_capacity(input.shape.len());
        for (position, dimension) in input.shape.iter().enumerate() {
            match *dimension {
                // Dynamische Dimensionen: die fuehrende gilt als Batch und
                // wird auf 1 gesetzt. Jede weitere waere geraten, und ein
                // geratenes Profil ist schlechter als keines.
                -1 if position == 0 => shape.push(1),
                -1 => {
                    return Err(BackendError::Malformed {
                        detail: format!(
                            "{model}: Eingabe {:?} hat die dynamische Dimension {position}; \
                             sie muss im Modellrepository festgelegt werden, sonst ist die \
                             gemessene Laufzeit nicht reproduzierbar",
                            input.name
                        ),
                    });
                }
                value => shape.push(value),
            }
        }

        let elements: i64 = shape.iter().copied().product();
        let width = element_size(&input.datatype).ok_or_else(|| BackendError::Malformed {
            detail: format!("{model}: unbekannter Datentyp {:?}", input.datatype),
        })?;
        let bytes = usize::try_from(elements)
            .ok()
            .and_then(|e| e.checked_mul(width))
            .ok_or_else(|| BackendError::Malformed {
                detail: format!("{model}: Eingabe {:?} ist zu gross", input.name),
            })?;

        inputs.push(InferInputTensor {
            name: input.name.clone(),
            datatype: input.datatype.clone(),
            shape,
            parameters: HashMap::new(),
            contents: None,
        });
        contents.push(vec![0_u8; bytes]);
    }

    Ok(ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: "vig-profile".to_owned(),
        parameters: HashMap::new(),
        inputs,
        outputs: Vec::new(),
        raw_input_contents: contents,
    })
}

/// Die Groesse eines Elements des angegebenen OIP-Datentyps in Bytes.
const fn element_size(datatype: &str) -> Option<usize> {
    Some(match datatype.as_bytes() {
        b"BOOL" | b"INT8" | b"UINT8" => 1,
        b"INT16" | b"UINT16" | b"FP16" | b"BF16" => 2,
        b"INT32" | b"UINT32" | b"FP32" => 4,
        b"INT64" | b"UINT64" | b"FP64" => 8,
        // BYTES ist laengenpraefigiert und ohne Modellwissen nicht
        // konstruierbar; das muss der Nutzer erfahren, statt eine falsche
        // Groesse zu bekommen.
        _ => return None,
    })
}

/// Baut einen Textrequest fuer ein generatives Modell.
///
/// Dieselbe Form, die die Zerlegung spaeter verwendet: ein `text_input` und
/// die Sampling-Parameter mit `max_tokens`. Waere die Messform eine andere als
/// die Betriebsform, maesse die Kalibrierung etwas, das im Betrieb nie
/// vorkommt.
#[must_use]
pub fn text_request(model: &str, prompt: &str, max_tokens: u32) -> ModelInferRequest {
    let sampling = format!("{{\"max_tokens\": {max_tokens}, \"temperature\": 0.0}}");
    ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: "vig-calibrate".to_owned(),
        parameters: HashMap::new(),
        inputs: vec![
            text_tensor("text_input"),
            text_tensor("sampling_parameters"),
        ],
        outputs: Vec::new(),
        raw_input_contents: vec![length_prefixed(prompt), length_prefixed(&sampling)],
    }
}

/// Ein BYTES-Tensor mit einem einzelnen String.
fn text_tensor(name: &str) -> InferInputTensor {
    InferInputTensor {
        name: name.to_owned(),
        datatype: "BYTES".to_owned(),
        shape: vec![1],
        parameters: HashMap::new(),
        contents: None,
    }
}

/// Kodiert einen String so, wie OIP Rohdaten fuer `BYTES` erwartet.
fn length_prefixed(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len().saturating_add(4));
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}
