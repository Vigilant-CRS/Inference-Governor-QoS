//! Abgleich hinterlegter Profile mit der laufenden Umgebung (G-010).
//!
//! Ein Laufzeitprofil ist eine Messung unter Bedingungen. Aendern sich die
//! Bedingungen, ist die Messung nicht falsch, sondern unzustaendig — und die
//! Spezifikation verlangt, dass sie dann nicht stillschweigend weiterbenutzt
//! wird.
//!
//! Beide Werkzeuge brauchen denselben Abgleich: `doctor` meldet ihn, `serve`
//! zieht Konsequenzen daraus. Deshalb steht er hier und nicht zweimal.

use onetimer_backend_triton::TritonClient;
use onetimer_config::schema::Resolved;
use onetimer_core::ModelIdx;
use onetimer_protocol_oip::inference::{ServerMetadataRequest, ServerMetadataResponse};
use std::collections::HashMap;

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
                        let actual = onetimer_backend_triton::fingerprint(server, &meta);
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
