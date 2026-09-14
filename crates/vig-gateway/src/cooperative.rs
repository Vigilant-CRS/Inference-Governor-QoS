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
//! funktioniert mit jedem Server, der Textgenerierung anbietet.
//!
//! Die Kosten der wiederholten Prefill-Berechnung traegt das Backend ueber
//! Prefix-Caching. Ob es das tut, entscheidet dieses Modul nicht — und bis
//! NV-16 nahm es stillschweigend an, dass es das tut. Der Kontext waechst mit
//! jedem Quantum; ohne wirksamen Cache waechst der Aufwand mit ihm, und ein
//! Auftrag aus n Quanten kostet quadratisch statt linear. Gemessen wird das
//! ueber `prefill_per_token` im Vertrag, und [`GenerativeJob::context_tokens`]
//! liefert die Groesse, gegen die es zaehlt. Steht der Wert auf null, heisst
//! das „gemessen wirkungslos oder nicht gemessen" — nicht mehr „kommt schon
//! hin".
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
    /// Geschaetzte Zahl der Token im urspruenglichen Prompt (NV-16).
    ///
    /// Zusammen mit [`Self::tokens`] der Kontext, den jede Fortsetzung erneut
    /// rechnen muss. Dieselbe grobe Schaetzung wie fuer die erzeugten Token —
    /// sie muss nur gut genug sein, um die Zuschneidung in die richtige
    /// Richtung zu bewegen.
    pub prompt_tokens: u32,
    /// Obergrenze der insgesamt erzeugten Token.
    pub max_total_tokens: u32,
    /// Die Samplingparameter, die der Client mitgegeben hat.
    ///
    /// Sie reisen unveraendert mit — bis auf `max_tokens`, das je Quantum
    /// gesetzt wird. Ein Client, der `temperature` oder `stop` vorgibt,
    /// bekaeme sonst eine Antwort aus einer anderen Konfiguration als der
    /// bestellten, ohne dass es jemand merkt.
    pub declared_sampling: Option<String>,
    /// Alle uebrigen Eingaben des urspruenglichen Requests.
    ///
    /// Bild, Maske, Region — bei einem VLM steht der eigentliche Inhalt genau
    /// hier. Sie beim Zuschneiden zu entfernen machte aus einer Bildfrage eine
    /// Textfrage.
    pub extra_inputs: Vec<(InferInputTensor, Vec<u8>)>,
    /// Wie viele Quanten dieser Auftrag bereits gebraucht hat.
    pub quanta: u32,
    /// Wie viele Token das zuletzt gebaute Quantum beim Backend bestellt hat.
    ///
    /// Die **harte** Schranke: `max_tokens` setzt das Backend selbst durch,
    /// in echten Token und nicht in geschaetzten. Mehr als diese Zahl kann ein
    /// Quantum nicht erzeugt haben, gleichgueltig wie der Tokenizer arbeitet
    /// (Review R06).
    ///
    /// `None` heisst: es wurde noch kein Quantum gebaut. Dann gibt es keine
    /// bestellte Obergrenze, und es bleibt bei der Schranke aus den Bytes.
    pub last_requested_tokens: Option<u32>,
    /// Was das zuletzt aufgenommene Quantum erzeugt hat, in Token (NV-16).
    ///
    /// Der Zuwachs, nicht der Stand. Die Buchhaltung braucht ihn, um
    /// Dekodierarbeit vom Re-Prefill zu trennen: `tokens` zaehlt kumulativ und
    /// wuerde bei jeder Fortsetzung erneut vollstaendig gebucht.
    pub last_quantum_tokens: u32,
}

impl GenerativeJob {
    /// Legt einen Auftrag aus dem urspruenglichen Request an.
    ///
    /// Gibt `None` zurueck, wenn der Request keinen Texteingang hat — dann ist
    /// er nicht zerlegbar, gleichgueltig was die Konfiguration sagt. Ebenso,
    /// wenn seine Samplingparameter kein JSON-Objekt sind: ein Quantum
    /// daraus zu bauen hiesse, sie zu reparieren oder zu verwerfen, und
    /// beides waere eine Entscheidung ueber das Ergebnis, die dem Client
    /// gehoert. Ungeteilt entscheidet das Backend, was es damit tut.
    #[must_use]
    pub fn from_request(request: &ModelInferRequest, max_total_tokens: u32) -> Option<Self> {
        let prompt = read_text_input(request)?;
        let declared_sampling =
            read_sampling_parameters(request).filter(|text| !text.trim().is_empty());
        if declared_sampling
            .as_deref()
            .is_some_and(|text| !is_json_object(text))
        {
            return None;
        }
        let prompt_tokens = u32::try_from(prompt.len().div_ceil(4)).unwrap_or(u32::MAX);
        // Die Obergrenze des Clients gilt, wenn er eine nennt. Die
        // Konfiguration begrenzt, was der Betreiber zulaesst — sie darf
        // nicht anheben, was der Aufrufer bestellt hat. Wer 4 Token
        // anfordert und 32 bekommt, zahlt fuer Arbeit, die er nicht wollte,
        // und bekommt eine Antwort, die er nicht erwartet.
        let declared = read_max_tokens(request);
        let effective = declared.map_or(max_total_tokens, |d| d.min(max_total_tokens));
        Some(Self {
            prompt,
            generated: String::new(),
            tokens: 0,
            prompt_tokens,
            max_total_tokens: effective,
            declared_sampling,
            extra_inputs: extra_inputs(request),
            quanta: 0,
            last_requested_tokens: None,
            last_quantum_tokens: 0,
        })
    }

    /// Der Kontext, den die naechste Fortsetzung neu rechnen muss (NV-16).
    ///
    /// Prompt plus alles bisher Erzeugte. Er waechst mit jedem Quantum, und
    /// genau deshalb kostet ein spaetes Quantum mehr als ein frueheres.
    ///
    /// Die Summe der je Quantum aufgerundeten Schaetzungen, nicht die
    /// Schaetzung ueber die Gesamtlaenge: bei n Quanten liegt sie bis zu n-1
    /// Token zu **hoch**. Das ist die sichere Richtung — ein ueberschaetzter
    /// Kontext ergibt ein kleineres Quantum, und ein kleineres Quantum
    /// gefaehrdet keine geschuetzte Ankunft. Wer eine Zahl braucht, die gegen
    /// einen echten Tokenizer standhaelt, braucht einen echten Tokenizer;
    /// diese hier soll die Zuschneidung in die richtige Richtung bewegen.
    #[must_use]
    pub const fn context_tokens(&self) -> u32 {
        self.prompt_tokens.saturating_add(self.tokens)
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
        &mut self,
        template: &ModelInferRequest,
        quantum_tokens: u32,
    ) -> ModelInferRequest {
        let mut request = template.clone();
        let tokens = quantum_tokens.min(self.remaining_tokens()).max(1);
        // Gemerkt, weil es die einzige **harte** Obergrenze ist, die dieses
        // Modul hat: das Backend setzt `max_tokens` in echten Token durch.
        self.last_requested_tokens = Some(tokens);
        let continuation = format!("{}{}", self.prompt, self.generated);
        let sampling = self.sampling_for(tokens);

        // Text und Samplingparameter werden ersetzt, **alles andere bleibt**.
        // Bei einem VLM steht der eigentliche Inhalt in den uebrigen Eingaben.
        let mut inputs = vec![text_tensor(TEXT_INPUT, &continuation)];
        let mut raw = vec![length_prefixed(&continuation)];
        inputs.push(text_tensor(SAMPLING_PARAMETERS, &sampling));
        raw.push(length_prefixed(&sampling));
        for (tensor, bytes) in &self.extra_inputs {
            inputs.push(tensor.clone());
            raw.push(bytes.clone());
        }

        request.inputs = inputs;
        request.raw_input_contents = raw;
        request
    }

    /// Die Samplingparameter dieses Quantums.
    ///
    /// Die Vorgabe des Clients bleibt erhalten; nur `max_tokens` wird auf die
    /// Quantengroesse gesetzt. Ohne Vorgabe entsteht ein minimales Objekt —
    /// und ausdruecklich kein erfundenes `temperature`, denn das waere eine
    /// Entscheidung ueber das Ergebnis, die dem Client gehoert.
    fn sampling_for(&self, tokens: u32) -> String {
        // Strukturiert, nicht textuell. Die Textsuche davor machte aus
        // `{"temperature":0.7,"max_tokens":64}` ein
        // `{"max_tokens": 8, "temperature":0.7,}` — ungueltiges JSON, sobald
        // `max_tokens` nicht vorne stand (Review R05). Ueber den Parser
        // bleibt jeder Wert erhalten; es aendern sich nur Leerraum und die
        // Reihenfolge der Schluessel, und beides hat in einem JSON-Objekt
        // keine Bedeutung. `from_request` laesst nur Objekte herein.
        let declared = self
            .declared_sampling
            .as_deref()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
        let Some(serde_json::Value::Object(mut fields)) = declared else {
            return format!("{{\"max_tokens\": {tokens}}}");
        };
        fields.insert("max_tokens".to_owned(), serde_json::Value::from(tokens));
        serde_json::Value::Object(fields).to_string()
    }

    /// Nimmt das Ergebnis eines Quantums auf.
    ///
    /// Gibt zurueck, ob der Auftrag damit abgeschlossen ist.
    pub fn absorb(&mut self, response: &ModelInferResponse) -> bool {
        self.quanta = self.quanta.saturating_add(1);
        self.last_quantum_tokens = 0;
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
        // Verbraucht wird das Kleinere aus zwei **Obergrenzen** (Review R06):
        //
        // * was beim Backend bestellt war — `max_tokens` setzt es in echten
        //   Token durch, und mehr kann nicht entstanden sein;
        // * wie viele Token in diesen Bytes ueberhaupt Platz haben — jedes
        //   Token belegt mindestens ein Byte, also hoechstens `len` Stueck.
        //
        // Das Minimum zweier Obergrenzen ist wieder eine Obergrenze, und damit
        // ist `max_total_tokens` eine Zusage statt einer Schaetzung. Die alte
        // Rechnung `Bytes / 4` war **keine** obere Schranke: vier
        // Ein-Byte-Token — vier Ziffern etwa — zaehlten als eines, und drei
        // weitere wurden freigegeben.
        //
        // Fuer gewoehnlichen Text ist die bestellte Zahl die kleinere und
        // damit massgeblich; die Bytegrenze greift nur, wo das Backend
        // frueher aufgehoert hat, als es durfte.
        let by_bytes = u32::try_from(delta.len()).unwrap_or(u32::MAX);
        let by_order = self.last_requested_tokens.unwrap_or(u32::MAX);
        let consumed = by_bytes.min(by_order);
        self.last_quantum_tokens = consumed;
        self.tokens = self.tokens.saturating_add(consumed);
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
/// Schreibt einen laengenpraefigierten String, wie das vLLM-Backend ihn
/// erwartet: vier Bytes Laenge, little endian, dann die Nutzbytes.
///
/// Oeffentlich, weil dasselbe Format im Baum bereits mehrfach getrennt
/// geschrieben wird (`vig-backend-triton/src/request.rs`,
/// `vig-bench/src/pilot.rs`, `vig-bench/src/bin/wp26.rs`) — und die Kopien
/// laufen schon auseinander: Zwei saettigen bei `u32::MAX`, zwei fallen auf
/// `0` zurueck. Ein Drahtformat, das viermal beschrieben wird, hat keinen
/// Besitzer. Der Lasttreiber nimmt deshalb diese Fassung, statt eine fuenfte
/// anzulegen; die drei uebrigen zusammenzufuehren ist eine eigene Aufgabe.
#[must_use]
pub fn write_length_prefixed(value: &str) -> Vec<u8> {
    length_prefixed(value)
}

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

/// Liest die Samplingparameter eines Requests, falls vorhanden.
#[must_use]
pub fn read_sampling_parameters(request: &ModelInferRequest) -> Option<String> {
    let index = request
        .inputs
        .iter()
        .position(|i| i.name == SAMPLING_PARAMETERS)?;
    let raw = request.raw_input_contents.get(index)?;
    read_length_prefixed(raw)
}

/// Liest die vom Client angeforderte Tokenobergrenze.
///
/// Nur das Feld `max_tokens` auf oberster Ebene zaehlt. Die Textsuche davor
/// fand auch ein `max_tokens` in einem verschachtelten Objekt oder in einem
/// String (Review R05). Keine ganze Zahl, kein Objekt oder kein JSON heisst:
/// keine Vorgabe des Clients, und es gilt die Obergrenze der Konfiguration.
#[must_use]
pub fn read_max_tokens(request: &ModelInferRequest) -> Option<u32> {
    let sampling = read_sampling_parameters(request)?;
    let value: serde_json::Value = serde_json::from_str(&sampling).ok()?;
    let tokens = value.as_object()?.get("max_tokens")?.as_u64()?;
    Some(u32::try_from(tokens).unwrap_or(u32::MAX))
}

/// Ob die Samplingparameter ein JSON-Objekt sind.
fn is_json_object(sampling: &str) -> bool {
    matches!(
        serde_json::from_str::<serde_json::Value>(sampling),
        Ok(serde_json::Value::Object(_))
    )
}

/// Alle Eingaben ausser Text und Samplingparametern, mit ihren Rohdaten.
#[must_use]
fn extra_inputs(request: &ModelInferRequest) -> Vec<(InferInputTensor, Vec<u8>)> {
    request
        .inputs
        .iter()
        .enumerate()
        .filter(|(_, i)| i.name != TEXT_INPUT && i.name != SAMPLING_PARAMETERS)
        .map(|(index, i)| {
            let bytes = request
                .raw_input_contents
                .get(index)
                .cloned()
                .unwrap_or_default();
            (i.clone(), bytes)
        })
        .collect()
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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

    /// Die Zerlegung darf nicht mehr Tokens bestellen als der Client.
    ///
    /// Der Client verlangte hoechstens vier; das erste Quantum verlangte 32.
    /// Er zahlt dann fuer Arbeit, die er nicht wollte, und bekommt eine
    /// Antwort, die er nicht erwartet — bei einem generativen Modell ist
    /// beides teuer.
    #[test]
    fn a_quantum_never_asks_for_more_tokens_than_the_client_did() {
        let mut request = request_with("Beschreibe: ");
        request.inputs.push(text_tensor(SAMPLING_PARAMETERS, ""));
        request
            .raw_input_contents
            .push(length_prefixed("{\"max_tokens\": 4, \"temperature\": 0.7}"));

        // Die Konfiguration erlaubt 64 — die Bestellung des Clients gilt.
        let mut job = GenerativeJob::from_request(&request, 64).unwrap();
        assert_eq!(job.max_total_tokens, 4);

        let quantum = job.build_quantum(&request, 32);
        assert_eq!(read_max_tokens(&quantum), Some(4));
        assert_eq!(
            sampling_of(&quantum).get("temperature"),
            Some(&serde_json::json!(0.7)),
            "und seine uebrigen Vorgaben reisen unveraendert mit"
        );
    }

    fn with_sampling(sampling: &str) -> ModelInferRequest {
        let mut request = request_with("Beschreibe: ");
        request.inputs.push(text_tensor(SAMPLING_PARAMETERS, ""));
        request.raw_input_contents.push(length_prefixed(sampling));
        request
    }

    fn sampling_of(request: &ModelInferRequest) -> serde_json::Map<String, serde_json::Value> {
        let text = read_sampling_parameters(request).unwrap();
        match serde_json::from_str(&text) {
            Ok(serde_json::Value::Object(fields)) => fields,
            other => panic!("kein JSON-Objekt: {text} ({other:?})"),
        }
    }

    /// Die Samplingparameter bleiben gueltiges JSON, gleich wo `max_tokens`
    /// steht (Review R05).
    ///
    /// Stand es nicht vorne, entfernte die Textsuche mit dem letzten Feld auch
    /// die schliessende Klammer, und die Kommabereinigung sah das
    /// hinterbliebene Komma nicht: `{"max_tokens": 8, "temperature":0.7,}`.
    /// Ein verschachteltes `max_tokens` oder eines in einem String ist nicht
    /// die Vorgabe des Clients und bleibt, wie es war.
    #[test]
    fn the_sampling_stays_valid_json_whatever_the_field_order() {
        let cases = [
            r#"{"temperature":0.7,"max_tokens":64}"#,
            r#"{"max_tokens":64,"temperature":0.7}"#,
            r#"{ "top_p": 0.9, "max_tokens": 64 , "seed": 7 }"#,
            r#"{"extra":{"max_tokens":999},"max_tokens":64,"n":1}"#,
            r#"{"stop":["\"max_tokens\": 3", "}"],"max_tokens":64}"#,
            r#"{"temperature":0.2}"#,
            "{}",
        ];
        for declared in cases {
            let request = with_sampling(declared);
            let mut job = GenerativeJob::from_request(&request, 64)
                .unwrap_or_else(|| panic!("zerlegbar: {declared}"));
            let quantum = job.build_quantum(&request, 8);
            let mut sent = sampling_of(&quantum);
            assert_eq!(
                sent.remove("max_tokens"),
                Some(serde_json::json!(8)),
                "{declared}"
            );
            let mut original: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(declared).unwrap();
            original.remove("max_tokens");
            assert_eq!(
                sent, original,
                "alle uebrigen Felder unveraendert: {declared}"
            );
        }
    }

    /// Nur das oberste `max_tokens` ist die Vorgabe des Clients.
    #[test]
    fn only_the_top_level_max_tokens_is_the_clients_limit() {
        let read = |sampling: &str| read_max_tokens(&with_sampling(sampling));
        assert_eq!(read(r#"{"temperature":0.7,"max_tokens":12}"#), Some(12));
        assert_eq!(
            read(r#"{"extra":{"max_tokens":999},"max_tokens":12}"#),
            Some(12)
        );
        assert_eq!(read(r#"{"extra":{"max_tokens":999}}"#), None);
        assert_eq!(read(r#"{"stop":"\"max_tokens\": 3"}"#), None);
        assert_eq!(
            read(r#"{"max_tokens":"12"}"#),
            None,
            "ein String ist keine Zahl"
        );
        assert_eq!(read("kein json"), None);
    }

    /// Samplingparameter, die kein JSON-Objekt sind, werden nicht repariert.
    ///
    /// Ein Quantum daraus zu bauen hiesse, sie zu korrigieren oder
    /// wegzuwerfen. Der Auftrag laeuft ungeteilt; was das Backend mit
    /// kaputten Parametern tut, entscheidet das Backend.
    #[test]
    fn sampling_that_is_not_a_json_object_is_not_split() {
        for broken in [r#"{"max_tokens": 4,"#, "[1, 2]", "\"text\"", "max_tokens=4"] {
            assert!(
                GenerativeJob::from_request(&with_sampling(broken), 64).is_none(),
                "{broken}"
            );
        }
        let empty = GenerativeJob::from_request(&with_sampling("  "), 64).unwrap();
        assert!(
            empty.declared_sampling.is_none(),
            "leer heisst: keine Vorgabe"
        );
    }

    /// Zusaetzliche Eingaben ueberleben die Zerlegung.
    ///
    /// Bei einem VLM steht der eigentliche Inhalt genau dort. Sie zu entfernen
    /// machte aus einer Bildfrage eine Textfrage — und die Antwort saehe
    /// plausibel aus.
    #[test]
    fn a_quantum_keeps_the_other_inputs() {
        let mut request = request_with("Was ist auf dem Bild?");
        request.inputs.push(InferInputTensor {
            name: "image".to_owned(),
            datatype: "FP32".to_owned(),
            shape: vec![1, 3, 224, 224],
            parameters: std::collections::HashMap::new(),
            contents: None,
        });
        request.raw_input_contents.push(vec![7_u8; 32]);

        let mut job = GenerativeJob::from_request(&request, 16).unwrap();
        let quantum = job.build_quantum(&request, 8);

        let image = quantum
            .inputs
            .iter()
            .position(|i| i.name == "image")
            .expect("das Bild ist noch da");
        assert_eq!(quantum.raw_input_contents.get(image), Some(&vec![7_u8; 32]));
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
    /// Der Kontext waechst mit jedem Quantum — Prompt plus Erzeugtes.
    ///
    /// Die Groesse, an der die Zuschneidung des naechsten Quantums haengt
    /// (NV-16). Der Prompt wird grob geschaetzt: rund vier Zeichen je Token.
    /// Genauer muss es nicht sein — die Schaetzung muss die Zuschneidung nur
    /// in die richtige Richtung bewegen, und ein Tokenizer je Backend waere
    /// ein Preis, den diese Genauigkeit nicht wert ist.
    #[test]
    fn the_context_grows_with_every_quantum() {
        let mut job = GenerativeJob::from_request(&request_with("Beschreibe: "), 64).unwrap();
        // 12 Zeichen, rund 3 Token.
        assert_eq!(job.prompt_tokens, 3);
        assert_eq!(
            job.context_tokens(),
            3,
            "vor dem ersten Quantum nur der Prompt"
        );

        job.tokens = 8;
        assert_eq!(job.context_tokens(), 11);
        job.tokens = 24;
        assert_eq!(job.context_tokens(), 27);
    }

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
        // Wie im Betrieb: erst ein Quantum bestellen, dann die Antwort
        // aufnehmen. Die Bestellung ist die harte Obergrenze — das Backend
        // setzt `max_tokens` in echten Token durch, und mehr kann nicht
        // entstanden sein. Ohne sie waeren 32 Zeichen bis zu 32 Token, und
        // das Budget waere aufgebraucht (Review R06).
        let _ = job.build_quantum(&template, 8);
        job.absorb(&response_with(&format!("P{}", "x".repeat(32))));
        let request = job.build_quantum(&template, 100);
        // Acht bestellte Token sind verbraucht, es bleiben 2 von 10. Auch bei
        // grosszuegig angefragtem Quantum wird nur das Restbudget angefordert.
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

    /// Die Tokenobergrenze haelt auch gegen Ein-Byte-Token (Review R06).
    ///
    /// `Bytes / 4` ist eine Schaetzung und **keine** obere Schranke: vier
    /// Ein-Byte-Token — vier Ziffern etwa — zaehlten als eines, und drei
    /// weitere wurden freigegeben. Aus fremd kontrollierter Eingabe entstand
    /// so mehr Arbeit, als der Betreiber zugelassen hatte (Spec 8.3).
    ///
    /// Gezaehlt wird jetzt das Kleinere aus zwei Obergrenzen: was beim
    /// Backend bestellt war und wie viele Token in diesen Bytes ueberhaupt
    /// Platz haben. Das Minimum zweier Obergrenzen ist wieder eine.
    #[test]
    fn the_token_budget_holds_against_single_byte_tokens() {
        let mut job = GenerativeJob::from_request(&request_with("Prompt:"), 4).unwrap();
        // Vier tatsaechliche Token in vier Bytes.
        let done = job.absorb(&response_with("1234"));
        assert!(done, "vier Token verbraucht, gezaehlt {} von 4", job.tokens);
        assert_eq!(job.remaining_tokens(), 0);
    }

    /// Die bestellte Zahl ist die schaerfere Schranke, wo sie greift.
    ///
    /// Die Gegenprobe zum Test darueber: haette nur die Bytegrenze gegolten,
    /// waere gewoehnlicher Text viermal zu schnell aufgebraucht. Das Backend
    /// setzt `max_tokens` in echten Token durch, und diese Zahl ist dann die
    /// kleinere.
    #[test]
    fn the_ordered_amount_binds_where_it_is_smaller() {
        let template = request_with("Prompt:");
        let mut job = GenerativeJob::from_request(&template, 64).unwrap();
        let _ = job.build_quantum(&template, 8);
        job.absorb(&response_with(&"x".repeat(40)));
        assert_eq!(
            job.tokens, 8,
            "40 Bytes, aber nur 8 Token bestellt — mehr kann nicht entstanden sein"
        );
    }
}
