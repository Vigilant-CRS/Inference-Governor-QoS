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

use vig_backend_triton::BackendError;
use vig_protocol_oip::bytes::{decode_single_bytes_element, encode_bytes_element};
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
    ///
    /// Dasselbe gilt fuer einen Text- oder Parametertensor, der nicht aus
    /// genau einem sauber gerahmten String besteht: unveraendert
    /// weitergereicht meldet Triton den kaputten Tensor selbst. Ein
    /// unlesbarer Parametertensor ist dabei **nicht** dasselbe wie keiner —
    /// sonst ersetzte das erste Quantum ihn stillschweigend.
    #[must_use]
    pub fn from_request(request: &ModelInferRequest, max_total_tokens: u32) -> Option<Self> {
        let prompt = read_text_input(request)?;
        if request.inputs.iter().any(|i| i.name == SAMPLING_PARAMETERS)
            && read_sampling_parameters(request).is_none()
        {
            return None;
        }
        let declared_sampling =
            read_sampling_parameters(request).filter(|text| !text.trim().is_empty());
        if declared_sampling
            .as_deref()
            .is_some_and(|text| !is_splittable_sampling(text))
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
    ///
    /// `None`, wenn kein Tokenbudget mehr besteht oder Prompt plus Erzeugtes
    /// nicht mehr in einen `BYTES`-Rahmen passt (ab 4 GiB). Einen gekuerzten
    /// Kontext zu schicken hiesse, das Modell anders weiterschreiben zu lassen.
    #[must_use]
    pub fn build_quantum(
        &mut self,
        template: &ModelInferRequest,
        quantum_tokens: u32,
    ) -> Option<ModelInferRequest> {
        let mut request = template.clone();
        let tokens = quantum_tokens.min(self.remaining_tokens());
        if tokens == 0 {
            return None;
        }
        let continuation = format!("{}{}", self.prompt, self.generated);
        let sampling = self.sampling_for(tokens);

        // Text und Samplingparameter werden ersetzt, **alles andere bleibt**.
        // Bei einem VLM steht der eigentliche Inhalt in den uebrigen Eingaben.
        let text = template
            .inputs
            .iter()
            .find(|input| input.name == TEXT_INPUT)?;
        let mut inputs = vec![text.clone()];
        let mut raw = vec![length_prefixed(&continuation)?];
        inputs.push(
            template
                .inputs
                .iter()
                .find(|input| input.name == SAMPLING_PARAMETERS)
                .cloned()
                .unwrap_or_else(|| text_tensor(SAMPLING_PARAMETERS, &sampling)),
        );
        raw.push(length_prefixed(&sampling)?);
        for (tensor, bytes) in &self.extra_inputs {
            inputs.push(tensor.clone());
            raw.push(bytes.clone());
        }

        // Gemerkt, weil es die einzige **harte** Obergrenze ist, die dieses
        // Modul hat: das Backend setzt `max_tokens` in echten Token durch.
        self.last_requested_tokens = Some(tokens);
        request.inputs = inputs;
        request.raw_input_contents = raw;
        Some(request)
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
    ///
    /// # Errors
    ///
    /// [`BackendError::Malformed`], wenn die Antwort keine eindeutige
    /// Textausgabe mit genau einem sauber gerahmten UTF-8-String traegt. Das
    /// ist kein Ende der Erzeugung, sondern eine kaputte Antwort: sie als
    /// „nichts Neues" zu lesen gaebe dem Client das bisher Erzeugte als
    /// vollstaendig zurueck, und eine abgeschnittene Laenge haette ihm sogar
    /// einen Teil davon als Ergebnis untergeschoben.
    pub fn absorb(&mut self, response: &ModelInferResponse) -> Result<bool, BackendError> {
        self.quanta = self.quanta.saturating_add(1);
        self.last_quantum_tokens = 0;
        let Some(raw) = text_output_raw(response) else {
            return Err(BackendError::Malformed {
                detail: format!(
                    "`{TEXT_OUTPUT}` fehlt oder ist kein einzelner BYTES-Tensor mit Rohdaten"
                ),
            });
        };
        let Some(text) = read_length_prefixed(raw) else {
            return Err(BackendError::Malformed {
                detail: format!(
                    "`{TEXT_OUTPUT}` ist nicht genau ein gerahmter UTF-8-String \
                     ({} Rohbytes)",
                    raw.len()
                ),
            });
        };

        // Das Backend liefert Prompt plus Fortsetzung oder nur die
        // Fortsetzung; beides wird unterstuetzt, indem der bekannte Anfang
        // abgeschnitten wird.
        let known = format!("{}{}", self.prompt, self.generated);
        let delta = text.strip_prefix(&known).unwrap_or(&text);

        if delta.is_empty() {
            return Ok(true);
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
        Ok(self.tokens >= self.max_total_tokens)
    }

    /// Baut die Antwort an den Client aus dem gesammelten Text.
    ///
    /// # Errors
    ///
    /// [`BackendError::Malformed`], wenn der Text nicht in einen `BYTES`-Rahmen
    /// passt oder die Antwort keinen eindeutig zugeordneten Texttensor traegt.
    pub fn build_response(
        &self,
        template: &ModelInferResponse,
    ) -> Result<ModelInferResponse, BackendError> {
        let raw = length_prefixed(&self.generated).ok_or_else(|| BackendError::Malformed {
            detail: format!(
                "der gesammelte Text ({} Bytes) passt nicht in einen BYTES-Rahmen",
                self.generated.len()
            ),
        })?;
        let mut response = template.clone();
        let index = text_output_index(&response).ok_or_else(|| BackendError::Malformed {
            detail: format!("die Antwort enthaelt keinen `{TEXT_OUTPUT}`-Tensor"),
        })?;
        if response.raw_output_contents.len() != response.outputs.len() {
            return Err(BackendError::Malformed {
                detail: "Ausgabetensoren und Rohdaten sind nicht eindeutig zugeordnet".to_owned(),
            });
        }
        let target =
            response
                .raw_output_contents
                .get_mut(index)
                .ok_or_else(|| BackendError::Malformed {
                    detail: format!("Rohdaten fuer `{TEXT_OUTPUT}` fehlen"),
                })?;
        *target = raw;
        Ok(response)
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

/// Schreibt einen laengenpraefigierten String, wie das vLLM-Backend ihn
/// erwartet: vier Bytes Laenge, little endian, dann die Nutzbytes.
///
/// Eine duenne Huelle um [`vig_protocol_oip::bytes::encode_bytes_element`]:
/// das Format hat dort seinen einzigen Besitzer, nachdem vier getrennte
/// Kopien auseinandergelaufen waren. `None`, wenn der String nicht in einen
/// Rahmen passt (ab 4 GiB).
#[must_use]
pub fn write_length_prefixed(value: &str) -> Option<Vec<u8>> {
    length_prefixed(value)
}

fn length_prefixed(value: &str) -> Option<Vec<u8>> {
    encode_bytes_element(value.as_bytes())
}

/// Liest einen laengenpraefigierten String.
///
/// Nur genau ein Element mit exakt passender Laenge. Die fruehere Fassung
/// kuerzte eine zu grosse Laengenangabe auf die vorhandenen Bytes und
/// verwarf, was hinter einer zu kleinen stand — ein Batch mit Form `[2]`
/// verlor so still sein zweites Element. `None` heisst fuer einen Request:
/// nicht zerlegen, unveraendert weiterreichen. Fuer eine Antwort macht
/// [`GenerativeJob::absorb`] daraus einen Fehler.
fn read_length_prefixed(bytes: &[u8]) -> Option<String> {
    let element = decode_single_bytes_element(bytes)?;
    String::from_utf8(element.to_vec()).ok()
}

/// Liest den Texteingang eines Requests.
#[must_use]
pub fn read_text_input(request: &ModelInferRequest) -> Option<String> {
    read_length_prefixed(single_input_raw(request, TEXT_INPUT)?)
}

/// Liest die Samplingparameter eines Requests, falls vorhanden.
#[must_use]
pub fn read_sampling_parameters(request: &ModelInferRequest) -> Option<String> {
    read_length_prefixed(single_input_raw(request, SAMPLING_PARAMETERS)?)
}

/// Nur einen eindeutig benannten BYTES-Tensor mit einem Element umschreiben.
/// Andernfalls wuerde der Quantumbau Form, Typ oder doppelte Eingaben reparieren.
fn single_input_raw<'a>(request: &'a ModelInferRequest, name: &str) -> Option<&'a [u8]> {
    let mut matches = request
        .inputs
        .iter()
        .enumerate()
        .filter(|(_, input)| input.name == name);
    let (index, input) = matches.next()?;
    if matches.next().is_some()
        || input.datatype != "BYTES"
        || !input.shape.iter().all(|dimension| *dimension == 1)
        || input.contents.is_some()
    {
        return None;
    }
    request.raw_input_contents.get(index).map(Vec::as_slice)
}

/// Liest die vom Client angeforderte Tokenobergrenze.
///
/// Nur das Feld `max_tokens` auf oberster Ebene zaehlt. Die Textsuche davor
/// fand auch ein `max_tokens` in einem verschachtelten Objekt oder in einem
/// String (Review R05). `None` bedeutet fehlend oder unlesbar;
/// [`GenerativeJob::from_request`] unterscheidet beides vor dem Zuschneiden.
#[must_use]
pub fn read_max_tokens(request: &ModelInferRequest) -> Option<u32> {
    let sampling = read_sampling_parameters(request)?;
    let value: serde_json::Value = serde_json::from_str(&sampling).ok()?;
    let tokens = value.as_object()?.get("max_tokens")?.as_u64()?;
    Some(u32::try_from(tokens).unwrap_or(u32::MAX))
}

/// Ein JSON-Objekt mit fehlender oder positiver ganzzahliger Tokenobergrenze.
/// Eine ungueltige Vorgabe unveraendert dem Backend ueberlassen, statt sie
/// beim Zuschneiden still durch eine gueltige zu ersetzen.
fn is_splittable_sampling(sampling: &str) -> bool {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(sampling) else {
        return false;
    };
    fields
        .get("max_tokens")
        .is_none_or(|value| value.as_u64().is_some_and(|tokens| tokens > 0))
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
///
/// `None` ohne lesbaren einzelnen Texttensor. [`GenerativeJob::absorb`]
/// meldet diesen Fall als fehlerhafte Backendantwort.
#[must_use]
pub fn read_text_output(response: &ModelInferResponse) -> Option<String> {
    read_length_prefixed(text_output_raw(response)?)
}

/// Die Rohdaten der Textausgabe, falls die Antwort welche traegt.
fn text_output_raw(response: &ModelInferResponse) -> Option<&[u8]> {
    let index = text_output_index(response)?;
    response.raw_output_contents.get(index).map(Vec::as_slice)
}

/// Eine Textausgabe darf nicht mit einem beliebigen ersten Tensor verwechselt werden.
fn text_output_index(response: &ModelInferResponse) -> Option<usize> {
    let mut matches = response
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, output)| output.name == TEXT_OUTPUT);
    let (index, output) = matches.next()?;
    if matches.next().is_some()
        || output.datatype != "BYTES"
        || !output.shape.iter().all(|dimension| *dimension == 1)
        || output.contents.is_some()
    {
        return None;
    }
    Some(index)
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
            raw_input_contents: vec![length_prefixed(prompt).unwrap()],
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
            raw_output_contents: vec![length_prefixed(text).unwrap()],
        }
    }

    /// Drei Rahmen, die keiner sind: Laengenangabe zu gross, zu klein, und
    /// zwei Elemente in einem Tensor.
    fn malformed_frames(text: &str) -> [Vec<u8>; 3] {
        let length = u32::try_from(text.len()).unwrap();
        let frame = |declared: u32| {
            let mut out = declared.to_le_bytes().to_vec();
            out.extend_from_slice(text.as_bytes());
            out
        };
        let mut two = length_prefixed(text).unwrap();
        two.extend(length_prefixed(text).unwrap());
        [
            frame(length.checked_add(8).unwrap()),
            frame(length.checked_sub(2).unwrap()),
            two,
        ]
    }

    /// Ein kaputt gerahmter Texteingang wird nicht zerlegt (Review C3).
    ///
    /// Vorher kuerzte der Leser eine zu grosse Laengenangabe auf die
    /// vorhandenen Bytes, verwarf, was hinter einer zu kleinen stand, und
    /// las von zwei Elementen nur das erste — der Auftrag lief dann mit einem
    /// anderen Prompt als dem gesendeten, und `text_tensor` schrieb die Form
    /// auf `[1]` um. Jetzt bleibt der Request, wie er ist, und Triton meldet
    /// den kaputten Tensor.
    #[test]
    fn a_malformed_text_input_is_passed_through_unsplit() {
        for raw in malformed_frames("Beschreibe: ") {
            let mut request = request_with("Beschreibe: ");
            request.raw_input_contents = vec![raw.clone()];
            assert_eq!(read_text_input(&request), None, "{raw:?}");
            assert!(
                GenerativeJob::from_request(&request, 64).is_none(),
                "{raw:?}"
            );
        }
    }

    /// Ein unlesbarer Parametertensor ist nicht dasselbe wie keiner.
    ///
    /// Sonst ersetzte das erste Quantum ihn durch ein erfundenes
    /// `{"max_tokens": n}`, und die Vorgabe des Clients waere still weg.
    #[test]
    fn malformed_sampling_parameters_are_passed_through_unsplit() {
        for raw in malformed_frames(r#"{"max_tokens": 4, "temperature": 0.7}"#) {
            let mut request = with_sampling("{}");
            *request.raw_input_contents.get_mut(1).unwrap() = raw.clone();
            assert!(
                GenerativeJob::from_request(&request, 64).is_none(),
                "{raw:?}"
            );
        }
    }

    /// Eine kaputt gerahmte Textausgabe ist ein Fehler, kein Ende (Review C3).
    ///
    /// Als „nichts Neues" gelesen, bekaeme der Client das bisher Erzeugte als
    /// vollstaendige Antwort — und eine abgeschnittene Laenge haette ihm
    /// einen Teil der Ausgabe als Ergebnis untergeschoben.
    #[test]
    fn a_malformed_text_output_is_an_error() {
        for raw in malformed_frames("Prompt und weiter") {
            let template = request_with("Prompt");
            let mut job = GenerativeJob::from_request(&template, 64).unwrap();
            let mut response = response_with("");
            response.raw_output_contents = vec![raw.clone()];
            assert_eq!(read_text_output(&response), None, "{raw:?}");
            assert!(
                matches!(job.absorb(&response), Err(BackendError::Malformed { .. })),
                "{raw:?}"
            );
            assert_eq!(job.generated, "", "nichts davon wird uebernommen");
        }

        // Fehlende Rohdaten sind kein gueltiges leeres Quantum.
        let mut job = GenerativeJob::from_request(&request_with("Prompt"), 64).unwrap();
        let mut empty = response_with("");
        empty.raw_output_contents.clear();
        assert!(job.absorb(&empty).is_err());
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
            .push(length_prefixed("{\"max_tokens\": 4, \"temperature\": 0.7}").unwrap());

        // Die Konfiguration erlaubt 64 — die Bestellung des Clients gilt.
        let mut job = GenerativeJob::from_request(&request, 64).unwrap();
        assert_eq!(job.max_total_tokens, 4);

        let quantum = job.build_quantum(&request, 32).unwrap();
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
        request
            .raw_input_contents
            .push(length_prefixed(sampling).unwrap());
        request
    }

    #[test]
    fn invalid_token_limits_are_not_rewritten_into_valid_work() {
        for limit in ["0", "-1", "1.5", "\"4\"", "null", "true"] {
            let request = with_sampling(&format!("{{\"max_tokens\":{limit}}}"));
            assert!(
                GenerativeJob::from_request(&request, 64).is_none(),
                "die ungueltige Vorgabe {limit} darf nicht durch 1 oder 64 ersetzt werden"
            );
        }
    }

    #[test]
    fn an_exhausted_job_cannot_order_another_token() {
        let template = request_with("Prompt:");
        let mut job = GenerativeJob::from_request(&template, 8).unwrap();
        job.tokens = 8;
        assert!(job.build_quantum(&template, 8).is_none());
    }

    #[test]
    fn incompatible_text_metadata_is_not_silently_repaired() {
        let mut batched = request_with("Prompt:");
        batched.inputs.first_mut().unwrap().shape = vec![2];
        let mut numeric = request_with("Prompt:");
        numeric.inputs.first_mut().unwrap().datatype = "FP32".into();
        let mut duplicate = request_with("Prompt:");
        duplicate
            .inputs
            .push(duplicate.inputs.first().unwrap().clone());
        duplicate
            .raw_input_contents
            .push(length_prefixed("other prompt").unwrap());
        for request in [batched, numeric, duplicate] {
            assert!(GenerativeJob::from_request(&request, 64).is_none());
        }
    }

    #[test]
    fn a_quantum_preserves_valid_single_element_tensor_shapes() {
        let mut template = with_sampling("{\"max_tokens\":16}");
        for input in &mut template.inputs {
            input.shape = vec![1, 1];
        }
        let mut job = GenerativeJob::from_request(&template, 64).unwrap();
        let quantum = job.build_quantum(&template, 8).unwrap();
        assert_eq!(quantum.inputs, template.inputs);
        assert_eq!(read_max_tokens(&quantum), Some(8));
    }

    #[test]
    fn a_non_text_output_is_not_interpreted_as_generated_text() {
        let mut response = response_with("metadata");
        response.outputs.first_mut().unwrap().name = "status".into();
        let mut job = GenerativeJob::from_request(&request_with("Prompt:"), 64).unwrap();
        assert!(job.absorb(&response).is_err());
        assert!(job.generated.is_empty());
    }

    #[test]
    fn collected_text_keeps_output_metadata_and_payloads_aligned() {
        let mut response = response_with("last quantum");
        response.outputs.insert(
            0,
            InferOutputTensor {
                name: "score".into(),
                datatype: "FP32".into(),
                shape: vec![1],
                ..Default::default()
            },
        );
        let score = 0.5_f32.to_le_bytes().to_vec();
        response.raw_output_contents.insert(0, score.clone());
        let mut job = GenerativeJob::from_request(&request_with("Prompt:"), 64).unwrap();
        job.generated = "whole answer".into();
        let collected = job.build_response(&response).unwrap();
        assert_eq!(collected.outputs, response.outputs);
        assert_eq!(collected.raw_output_contents.len(), collected.outputs.len());
        assert_eq!(collected.raw_output_contents.first(), Some(&score));
        assert_eq!(
            read_text_output(&collected).as_deref(),
            Some("whole answer")
        );
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
            let quantum = job.build_quantum(&request, 8).unwrap();
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
        let quantum = job.build_quantum(&request, 8).unwrap();

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

        let first = job.build_quantum(&template, 8).unwrap();
        assert_eq!(read_text_input(&first).unwrap(), "Beschreibe die Szene:");

        let done = job
            .absorb(&response_with("Beschreibe die Szene: Ein Roboter"))
            .unwrap();
        assert!(!done, "der Auftrag ist noch nicht fertig");
        assert_eq!(job.generated, " Ein Roboter");

        let second = job.build_quantum(&template, 8).unwrap();
        assert_eq!(
            read_text_input(&second).unwrap(),
            "Beschreibe die Szene: Ein Roboter"
        );
    }

    #[test]
    fn a_backend_that_returns_only_the_continuation_also_works() {
        let template = request_with("Prompt");
        let mut job = GenerativeJob::from_request(&template, 64).unwrap();
        job.absorb(&response_with(" und weiter")).unwrap();
        assert_eq!(job.generated, " und weiter");
    }

    /// Ein leeres Quantum bedeutet: das Modell ist fertig.
    #[test]
    fn an_empty_quantum_ends_the_job() {
        let template = request_with("Prompt");
        let mut job = GenerativeJob::from_request(&template, 64).unwrap();
        assert!(
            job.absorb(&response_with("Prompt")).unwrap(),
            "kein Zuwachs, also fertig"
        );
    }

    /// Spec 8.3: keine unbeschraenkte Arbeit aus fremd kontrollierter Eingabe.
    #[test]
    fn the_total_token_budget_is_enforced() {
        let template = request_with("P");
        let mut job = GenerativeJob::from_request(&template, 8).unwrap();
        let done = job
            .absorb(&response_with(&format!("P{}", "x".repeat(64))))
            .unwrap();
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
        let _ = job.build_quantum(&template, 8).unwrap();
        job.absorb(&response_with(&format!("P{}", "x".repeat(32))))
            .unwrap();
        let request = job.build_quantum(&template, 100).unwrap();
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
        let done = job.absorb(&response_with("1234")).unwrap();
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
        let _ = job.build_quantum(&template, 8).unwrap();
        job.absorb(&response_with(&"x".repeat(40))).unwrap();
        assert_eq!(
            job.tokens, 8,
            "40 Bytes, aber nur 8 Token bestellt — mehr kann nicht entstanden sein"
        );
    }
}
