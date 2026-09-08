//! Abgleich hinterlegter Profile mit der laufenden Umgebung (G-010).
//!
//! Ein Laufzeitprofil ist eine Messung unter Bedingungen. Aendern sich die
//! Bedingungen, ist die Messung nicht falsch, sondern unzustaendig — und die
//! Spezifikation verlangt, dass sie dann nicht stillschweigend weiterbenutzt
//! wird.
//!
//! Beide Werkzeuge brauchen denselben Abgleich: `doctor` meldet ihn, `serve`
//! zieht Konsequenzen daraus. Deshalb steht er hier und nicht zweimal.

use std::collections::HashMap;
use vig_backend_triton::TritonClient;
use vig_config::schema::Resolved;
use vig_core::ModelIdx;
use vig_protocol_oip::inference::{ServerMetadataRequest, ServerMetadataResponse};

/// Was der Abgleich einer Variante ergeben hat.
pub(crate) enum Trust {
    /// Der Fingerabdruck stimmt mit der laufenden Umgebung ueberein.
    Verified,
    /// Es ist keiner hinterlegt — vor G-010 erzeugt oder von Hand geschrieben.
    Missing,
    /// Der Fingerabdruck weicht ab. Das Profil gilt nicht mehr.
    Mismatch {
        /// Was in der Konfiguration steht.
        declared: String,
        /// Was das Backend jetzt meldet.
        actual: String,
    },
    /// Die Metadaten waren nicht abrufbar; es konnte nichts verglichen werden.
    Unavailable(String),
}

/// Das Ergebnis fuer eine Variante.
pub(crate) struct Checked {
    /// Index des logischen Modells.
    pub model: ModelIdx,
    /// Logischer Name, fuer die Ausgabe.
    pub logical: String,
    /// Backend-Modellname, fuer die Ausgabe.
    pub physical: String,
    /// Der Befund.
    pub trust: Trust,
}

impl Checked {
    /// Ist das Profil dieser Variante nachweislich ungueltig?
    pub(crate) const fn is_mismatch(&self) -> bool {
        matches!(self.trust, Trust::Mismatch { .. })
    }
}

/// Prueft alle Varianten gegen ihr Backend.
pub(crate) async fn check(resolved: &Resolved) -> Vec<Checked> {
    let mut server_meta: HashMap<String, ServerMetadataResponse> = HashMap::new();
    let mut results = Vec::new();

    for (i, names) in resolved.backend_models.iter().enumerate() {
        let Ok(index) = u16::try_from(i) else {
            continue;
        };
        let model = ModelIdx(index);
        let logical = resolved
            .model_names
            .get(i)
            .map_or_else(|| "?".to_owned(), Clone::clone);
        let endpoint = resolved.endpoint_of(model).to_owned();
        let client = TritonClient::new(&endpoint);

        // Die Servermetadaten aendern sich waehrend eines Laufs nicht; einmal
        // je Endpunkt genuegt.
        if !server_meta.contains_key(&endpoint) {
            let fetched = match client.raw().await {
                Ok(mut raw) => raw
                    .server_metadata(ServerMetadataRequest {})
                    .await
                    .map(tonic::Response::into_inner)
                    .ok(),
                Err(_) => None,
            };
            if let Some(meta) = fetched {
                server_meta.insert(endpoint.clone(), meta);
            }
        }

        for (j, physical) in names.iter().enumerate() {
            let declared = resolved
                .profile_fingerprints
                .get(i)
                .and_then(|v| v.get(j))
                .and_then(Option::as_ref);

            let trust = match (server_meta.get(&endpoint), declared) {
                (_, None) => Trust::Missing,
                (None, Some(_)) => {
                    Trust::Unavailable(format!("{endpoint}: Servermetadaten nicht abrufbar"))
                }
                (Some(server), Some(declared)) => match client.model_metadata(physical).await {
                    Ok(meta) => {
                        let actual = vig_backend_triton::fingerprint(server, &meta);
                        if actual == *declared {
                            Trust::Verified
                        } else {
                            Trust::Mismatch {
                                declared: declared.clone(),
                                actual,
                            }
                        }
                    }
                    Err(e) => Trust::Unavailable(format!("{physical}: {e}")),
                },
            };

            results.push(Checked {
                model,
                logical: logical.clone(),
                physical: (*physical).clone(),
                trust,
            });
        }
    }
    results
}

/// Die I/O-Signatur einer Variante, so wie das Backend sie meldet.
///
/// Nur Namen, Datentypen und Formen — nicht die Bedeutung. Zwei Varianten mit
/// gleicher Signatur koennen fachlich trotzdem Verschiedenes ausgeben; gleiche
/// Signatur ist die **notwendige**, nicht die hinreichende Bedingung. Was hier
/// erkannt wird, ist der Fall, in dem ein Variantenwechsel den Client garantiert
/// bricht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Signature {
    /// Eingaben, normalisiert und sortiert.
    pub inputs: Vec<String>,
    /// Ausgaben, normalisiert und sortiert.
    pub outputs: Vec<String>,
}

impl Signature {
    fn of(meta: &vig_protocol_oip::inference::ModelMetadataResponse) -> Self {
        // `-1` in einer Form heisst „dynamisch" und ist kein Unterschied.
        let render = |name: &str, datatype: &str, shape: &[i64]| {
            let dims: Vec<String> = shape
                .iter()
                .map(|d| {
                    if *d < 0 {
                        "?".to_owned()
                    } else {
                        d.to_string()
                    }
                })
                .collect();
            format!("{name}:{datatype}[{}]", dims.join(","))
        };
        let mut inputs: Vec<String> = meta
            .inputs
            .iter()
            .map(|i| render(&i.name, &i.datatype, &i.shape))
            .collect();
        let mut outputs: Vec<String> = meta
            .outputs
            .iter()
            .map(|o| render(&o.name, &o.datatype, &o.shape))
            .collect();
        // Die Reihenfolge im Metadatensatz ist nicht zugesichert.
        inputs.sort();
        outputs.sort();
        Self { inputs, outputs }
    }

    /// Eine lesbare Fassung fuer die Meldung.
    pub(crate) fn describe(&self) -> String {
        format!(
            "in({}) out({})",
            self.inputs.join(" "),
            self.outputs.join(" ")
        )
    }
}

/// Ein Modell, dessen Varianten nicht dieselbe I/O-Signatur haben.
pub(crate) struct SignatureConflict {
    /// Index des logischen Modells.
    pub model: ModelIdx,
    /// Logischer Name.
    pub logical: String,
    /// Die erste Variante mit ihrer Signatur.
    pub reference: (String, Signature),
    /// Die abweichende Variante mit ihrer Signatur.
    pub divergent: (String, Signature),
}

/// Prueft, ob die Varianten eines Modells austauschbar sind.
///
/// Der Governor waehlt die Variante **je Request** und sagt es dem Client
/// nicht. Diese Freiheit setzt voraus, dass alle Varianten dieselbe Schnittstelle
/// bedienen — sonst bekommt ein Client nach einem Wechsel einen Backendfehler
/// oder, schlimmer, einen Tensor mit anderer Bedeutung bei gleicher Form.
///
/// Gemeldet wird nur, was nachweislich unterschiedlich ist. Ein nicht
/// abrufbares Metadatum erzeugt keinen Konflikt: unbekannt ist nicht
/// dasselbe wie ungleich.
pub(crate) async fn signature_conflicts(resolved: &Resolved) -> Vec<SignatureConflict> {
    let mut conflicts = Vec::new();

    for (i, names) in resolved.backend_models.iter().enumerate() {
        if names.len() < 2 {
            continue;
        }
        let Ok(index) = u16::try_from(i) else {
            continue;
        };
        let model = ModelIdx(index);
        let logical = resolved
            .model_names
            .get(i)
            .map_or_else(|| "?".to_owned(), Clone::clone);
        let client = TritonClient::new(resolved.endpoint_of(model));

        let mut reference: Option<(String, Signature)> = None;
        for physical in names {
            let Ok(meta) = client.model_metadata(physical).await else {
                // Nicht abrufbar ist **nicht** "gleich". Eine Variante ohne
                // geprueftbare Signatur darf nicht automatisch gewaehlt
                // werden: "keine nachgewiesene Abweichung" ist keine
                // Freigabe, und eine spaeter ladende Variante koennte jede
                // Schnittstelle haben.
                conflicts.push(SignatureConflict {
                    model,
                    logical: logical.clone(),
                    reference: (
                        "?".to_owned(),
                        Signature {
                            inputs: Vec::new(),
                            outputs: Vec::new(),
                        },
                    ),
                    divergent: (
                        (*physical).clone(),
                        Signature {
                            inputs: vec!["<Metadaten nicht abrufbar>".to_owned()],
                            outputs: Vec::new(),
                        },
                    ),
                });
                continue;
            };
            let signature = Signature::of(&meta);
            match &reference {
                None => reference = Some(((*physical).clone(), signature)),
                Some((first_name, first)) => {
                    if *first != signature {
                        conflicts.push(SignatureConflict {
                            model,
                            logical: logical.clone(),
                            reference: (first_name.clone(), first.clone()),
                            divergent: ((*physical).clone(), signature),
                        });
                    }
                }
            }
        }
    }
    conflicts
}

/// Eine Variante, die die zugesagte Schnittstelle ihres Modells nicht erfuellt.
pub(crate) struct ContractViolation {
    /// Logischer Name.
    pub logical: String,
    /// Die abweichende Variante.
    pub physical: String,
    /// Was zugesagt war.
    pub declared: Signature,
    /// Was das Backend meldet.
    pub actual: Signature,
}

/// Prueft jede Variante gegen die zugesagte Schnittstelle ihres Modells.
///
/// Der Unterschied zu [`signature_conflicts`] ist entscheidend: dort wird
/// verglichen, ob die Varianten **untereinander** gleich aussehen — das
/// erkennt Unterschiede, aber nie Gleichheit der Bedeutung. Hier hat der
/// Betreiber die Schnittstelle **zugesagt**, und der Governor prueft nur noch,
/// ob sie eingehalten wird. Die Aussage ueber die Bedeutung kommt von dem
/// Einzigen, der sie treffen kann.
///
/// Modelle ohne hinterlegte Signatur werden uebersprungen. Ein nicht
/// abrufbares Metadatum ist keine Verletzung: unbekannt ist nicht ungleich.
pub(crate) async fn contract_violations(resolved: &Resolved) -> Vec<ContractViolation> {
    let mut out = Vec::new();

    for (i, names) in resolved.backend_models.iter().enumerate() {
        let Some(Some(declared_spec)) = resolved.io_signatures.get(i) else {
            continue;
        };
        let Ok(index) = u16::try_from(i) else {
            continue;
        };
        let model = ModelIdx(index);
        let logical = resolved
            .model_names
            .get(i)
            .map_or_else(|| "?".to_owned(), Clone::clone);
        let (inputs, outputs) = declared_spec.normalised();
        let declared = Signature { inputs, outputs };
        let client = TritonClient::new(resolved.endpoint_of(model));

        for physical in names {
            let Ok(meta) = client.model_metadata(physical).await else {
                // Eine zugesagte Signatur, die nicht geprueft werden kann, ist
                // nicht erfuellt. Sonst genuegte ein voruebergehend nicht
                // erreichbares Modell, um die Zusage auszuhebeln.
                out.push(ContractViolation {
                    logical: logical.clone(),
                    physical: (*physical).clone(),
                    declared: declared.clone(),
                    actual: Signature {
                        inputs: vec!["<Metadaten nicht abrufbar>".to_owned()],
                        outputs: Vec::new(),
                    },
                });
                continue;
            };
            let actual = Signature::of(&meta);
            if actual != declared {
                out.push(ContractViolation {
                    logical: logical.clone(),
                    physical: (*physical).clone(),
                    declared: declared.clone(),
                    actual,
                });
            }
        }
    }
    out
}

/// Die Modelle, deren Profil nachweislich nicht mehr gilt.
pub(crate) fn unverified_models(checked: &[Checked]) -> Vec<ModelIdx> {
    let mut models: Vec<ModelIdx> = checked
        .iter()
        .filter(|c| c.is_mismatch())
        .map(|c| c.model)
        .collect();
    models.sort_unstable_by_key(|m| m.0);
    models.dedup_by_key(|m| m.0);
    models
}
