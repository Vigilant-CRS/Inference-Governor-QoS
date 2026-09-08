//! Kooperative Quanten fuer generative Modelle (WP26, ADR-0014).
//!
//! Ein generativer Auftrag wird nicht als ein Block ausgefuehrt, sondern als
//! Folge kuerzerer Auftraege. Zwischen zwei Quanten ist der Slot frei, und
//! geschuetzte Arbeit kommt vorbei. Die Blockadedauer sinkt damit von der
//! Dauer des gesamten Auftrags auf die eines Quantums — der Unterschied
//! zwischen „laeuft nie" und „laeuft langsamer".
//!
//! ## Der Zustand reist im Prompt
//!
//! Jedes Quantum bekommt den urspruenglichen Prompt plus das bisher Erzeugte.
//! Das braucht keinen Eingriff in die KV-Cache-Verwaltung des Backends und
//! funktioniert mit jedem Server, der Textgenerierung anbietet. Die Kosten der
//! wiederholten Prefill-Berechnung traegt das Backend ueber Prefix-Caching;
//! ohne dieses Caching ist das Verfahren nicht wirtschaftlich.
//!
//! ## Woran das Ende erkannt wird
//!
//! Das Triton-vLLM-Backend liefert in der einfachen Betriebsart nur den Text,
//! keinen Abbruchgrund. Das Ende wird deshalb daran erkannt, dass ein Quantum
//! **nichts Neues** erzeugt hat, oder dass die Gesamtobergrenze erreicht ist.
//! Das ist eine Heuristik, und sie wird hier als solche benannt: ein Modell,
//! das mitten in der Erzeugung ein leeres Quantum liefert, gilt als fertig.
//! Sobald das Backend einen Abbruchgrund ausgibt, gehoert er hierher.

use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{ModelInferRequest, ModelInferResponse};

/// Der Name des Texteingabetensors im Triton-vLLM-Backend.
pub const TEXT_INPUT: &str = "text_input";
/// Der Name des Parametertensors im Triton-vLLM-Backend.
pub const SAMPLING_PARAMETERS: &str = "sampling_parameters";
/// Der Name des Textausgabetensors im Triton-vLLM-Backend.
pub const TEXT_OUTPUT: &str = "text_output";

/// Der Fortschritt eines zerlegten Auftrags.
#[derive(Debug, Clone)]
pub struct GenerativeJob {
    /// Der urspruengliche Prompt des Clients.
    pub prompt: String,
    /// Das bisher Erzeugte.
    pub generated: String,
    /// Geschaetzte Zahl bereits erzeugter Token.
    pub tokens: u32,
    /// Obergrenze der insgesamt erzeugten Token.
    pub max_total_tokens: u32,
    /// Wie viele Quanten dieser Auftrag bereits gebraucht hat.
    pub quanta: u32,
}

impl GenerativeJob {
    /// Legt einen Auftrag aus dem urspruenglichen Request an.
    ///
    /// Gibt `None` zurueck, wenn der Request keinen Texteingang hat — dann ist
    /// er nicht zerlegbar, gleichgueltig was die Konfiguration sagt.
    #[must_use]
    pub fn from_request(request: &ModelInferRequest, max_total_tokens: u32) -> Option<Self> {
        let prompt = read_text_input(request)?;
        Some(Self {
            prompt,
            generated: String::new(),
            tokens: 0,
            max_total_tokens,
            quanta: 0,
        })
    }

    /// Wie viele Token dieser Auftrag noch erzeugen darf.
    #[must_use]
    pub fn remaining_tokens(&self) -> u32 {
        self.max_total_tokens.saturating_sub(self.tokens)
    }

    /// Baut den Request fuer das naechste Quantum.
    ///
    /// Der Texteingang wird auf Prompt plus bisher Erzeugtes gesetzt, die
    /// Tokenzahl auf die Quantengroesse begrenzt.
    #[must_use]
    pub fn build_quantum(
        &self,
        template: &ModelInferRequest,
        quantum_tokens: u32,
    ) -> ModelInferRequest {
        let mut request = template.clone();
        let tokens = quantum_tokens.min(self.remaining_tokens()).max(1);
        let continuation = format!("{}{}", self.prompt, self.generated);

        request.inputs = vec![
            text_tensor(TEXT_INPUT, &continuation),
            text_tensor(
                SAMPLING_PARAMETERS,
                &format!("{{\"max_tokens\": {tokens}, \"temperature\": 0.0}}"),
            ),
        ];
        request.raw_input_contents = vec![
            length_prefixed(&continuation),
            length_prefixed(&format!(
                "{{\"max_tokens\": {tokens}, \"temperature\": 0.0}}"
            )),
        ];
        request
    }

    /// Nimmt das Ergebnis eines Quantums auf.
    ///
    /// Gibt zurueck, ob der Auftrag damit abgeschlossen ist.
    pub fn absorb(&mut self, response: &ModelInferResponse) -> bool {
        self.quanta = self.quanta.saturating_add(1);
        let Some(text) = read_text_output(response) else {
            // Ohne verwertbare Ausgabe ist nichts fortzusetzen.
            return true;
        };

        // Das Backend liefert Prompt plus Fortsetzung oder nur die
        // Fortsetzung; beides wird unterstuetzt, indem der bekannte Anfang
        // abgeschnitten wird.
        let known = format!("{}{}", self.prompt, self.generated);
        let delta = text.strip_prefix(&known).unwrap_or(&text);

        if delta.is_empty() {
            return true;
        }
        self.generated.push_str(delta);
        // Grobe Schaetzung: rund vier Zeichen je Token. Sie muss nur gut genug
        // sein, um die Gesamtobergrenze einzuhalten.
        self.tokens = self
            .tokens
            .saturating_add(u32::try_from(delta.len().div_ceil(4)).unwrap_or(u32::MAX));
        self.tokens >= self.max_total_tokens
    }

    /// Baut die Antwort an den Client aus dem gesammelten Text.
    #[must_use]
    pub fn build_response(&self, template: &ModelInferResponse) -> ModelInferResponse {
        let mut response = template.clone();
        response.raw_output_contents = vec![length_prefixed(&self.generated)];
        response
    }
}

/// Ein BYTES-Tensor mit einem einzelnen String.
fn text_tensor(name: &str, _value: &str) -> InferInputTensor {
    InferInputTensor {
        name: name.to_owned(),
        datatype: "BYTES".to_owned(),
        shape: vec![1],
        parameters: std::collections::HashMap::new(),
        contents: None,
    }
}

/// Ein String im laengenpraefigierten BYTES-Format des Protokolls.
fn length_prefixed(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len().saturating_add(4));
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

/// Liest einen laengenpraefigierten String.
fn read_length_prefixed(bytes: &[u8]) -> Option<String> {
    let (header, rest) = bytes.split_at_checked(4)?;
    let length = u32::from_le_bytes([
        *header.first()?,
        *header.get(1)?,
        *header.get(2)?,
        *header.get(3)?,
    ]);
    let end = usize::try_from(length).ok()?.min(rest.len());
    String::from_utf8(rest.get(..end)?.to_vec()).ok()
}

/// Liest den Texteingang eines Requests.
#[must_use]
pub fn read_text_input(request: &ModelInferRequest) -> Option<String> {
    let index = request.inputs.iter().position(|i| i.name == TEXT_INPUT)?;
    let raw = request.raw_input_contents.get(index)?;
    read_length_prefixed(raw)
}

/// Liest die Textausgabe einer Antwort.
#[must_use]
pub fn read_text_output(response: &ModelInferResponse) -> Option<String> {
    let index = response
        .outputs
        .iter()
        .position(|o| o.name == TEXT_OUTPUT)
        .unwrap_or(0);
    let raw = response.raw_output_contents.get(index)?;
    read_length_prefixed(raw)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use vig_protocol_oip::inference::model_infer_response::InferOutputTensor;

    fn request_with(prompt: &str) -> ModelInferRequest {
        ModelInferRequest {
            model_name: "vlm".to_owned(),
            model_version: String::new(),
            id: "1".to_owned(),
            parameters: std::collections::HashMap::new(),
            inputs: vec![text_tensor(TEXT_INPUT, prompt)],
            outputs: Vec::new(),
            raw_input_contents: vec![length_prefixed(prompt)],
        }
    }

    fn response_with(text: &str) -> ModelInferResponse {
        ModelInferResponse {
            model_name: "vlm".to_owned(),
            model_version: String::new(),
            id: "1".to_owned(),
            parameters: std::collections::HashMap::new(),
            outputs: vec![InferOutputTensor {
                name: TEXT_OUTPUT.to_owned(),
                datatype: "BYTES".to_owned(),
                shape: vec![1],
                parameters: std::collections::HashMap::new(),
                contents: None,
            }],
            raw_output_contents: vec![length_prefixed(text)],
        }
    }

    #[test]
    fn a_job_without_text_input_is_not_splittable() {
        let mut plain = request_with("hallo");
        plain.inputs.clear();
        plain.raw_input_contents.clear();
        assert!(GenerativeJob::from_request(&plain, 64).is_none());
    }

    /// Der Zustand reist im Prompt: jedes Quantum sieht Prompt plus bisher
    /// Erzeugtes.
    #[test]
    fn each_quantum_carries_the_accumulated_text() {
        let template = request_with("Beschreibe die Szene:");
        let mut job = GenerativeJob::from_request(&template, 64).unwrap();

        let first = job.build_quantum(&template, 8);
        assert_eq!(read_text_input(&first).unwrap(), "Beschreibe die Szene:");

        let done = job.absorb(&response_with("Beschreibe die Szene: Ein Roboter"));
        assert!(!done, "der Auftrag ist noch nicht fertig");
        assert_eq!(job.generated, " Ein Roboter");

        let second = job.build_quantum(&template, 8);
        assert_eq!(
            read_text_input(&second).unwrap(),
            "Beschreibe die Szene: Ein Roboter"
        );
    }

    #[test]
    fn a_backend_that_returns_only_the_continuation_also_works() {
        let template = request_with("Prompt");
        let mut job = GenerativeJob::from_request(&template, 64).unwrap();
        job.absorb(&response_with(" und weiter"));
        assert_eq!(job.generated, " und weiter");
    }

    /// Ein leeres Quantum bedeutet: das Modell ist fertig.
    #[test]
    fn an_empty_quantum_ends_the_job() {
        let template = request_with("Prompt");
        let mut job = GenerativeJob::from_request(&template, 64).unwrap();
        assert!(
            job.absorb(&response_with("Prompt")),
            "kein Zuwachs, also fertig"
        );
    }

    /// Spec 8.3: keine unbeschraenkte Arbeit aus fremd kontrollierter Eingabe.
    #[test]
    fn the_total_token_budget_is_enforced() {
        let template = request_with("P");
        let mut job = GenerativeJob::from_request(&template, 8).unwrap();
        let done = job.absorb(&response_with(&format!("P{}", "x".repeat(64))));
        assert!(done, "die Gesamtobergrenze beendet den Auftrag");
        assert!(job.tokens >= 8);
        assert_eq!(job.remaining_tokens(), 0);
    }

    #[test]
    fn the_quantum_never_exceeds_what_remains() {
        let template = request_with("P");
        let mut job = GenerativeJob::from_request(&template, 10).unwrap();
        job.absorb(&response_with(&format!("P{}", "x".repeat(32))));
        let request = job.build_quantum(&template, 100);
        // Nach 32 Zeichen sind rund 8 Token verbraucht, es bleiben 2 von 10.
        // Auch bei grosszuegig angefragtem Quantum wird nur das Restbudget
        // angefordert.
        assert_eq!(job.remaining_tokens(), 2);
        let params = String::from_utf8(
            request
                .raw_input_contents
                .get(1)
                .cloned()
                .unwrap_or_default(),
        )
        .unwrap_or_default();
        assert!(params.contains("\"max_tokens\": 2"), "{params}");
        assert!(!params.contains("100"), "{params}");
    }
}
