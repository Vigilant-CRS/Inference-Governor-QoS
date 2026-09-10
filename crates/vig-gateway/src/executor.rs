//! Die Naht zwischen Governor und Backend (NV-07).
//!
//! ## Warum eine Naht
//!
//! Der Actor kannte bisher genau einen Backendtyp: `TritonClient`. Das ist
//! solange bequem, wie es nur Triton gibt, und kostet an drei Stellen:
//!
//! * **Fehlerpfade sind ohne GPU nicht pruefbar.** Ein abgebrochener Aufruf,
//!   ein Timeout gefolgt von `Unavailable`, ein Backend, das nach dem Timeout
//!   doch noch antwortet — das sind genau die Faelle, in denen sich
//!   entscheidet, ob ein Slotkredit zu frueh zurueckgegeben wird (NV-00). Sie
//!   auf echter Hardware herbeizufuehren ist muehsam und unzuverlaessig.
//! * **Ein zweites Backend waere ein zweiter Actor.** TensorRT direkt, ein
//!   anderer Server, ein Simulator: jedes davon haette denselben
//!   Lebenszyklus und eine andere API.
//! * **Protokolldetails wandern nach oben.** Was OIP-spezifisch ist, gehoert
//!   in den Adapter und nicht in die Ablaufsteuerung.
//!
//! ## Was diese Naht **nicht** aendert
//!
//! Die Ressourcenverwaltung bleibt vollstaendig beim Actor. Ein Executor
//! fuehrt aus und berichtet; er haelt keine Slots, vergibt keine Kredite und
//! entscheidet nichts. Genau ein Besitzer je Ressource — sonst gaebe es zwei
//! Stellen, die einen Kredit freigeben koennen, und die Frage „wer hat ihn
//! zuletzt gehalten" waere nicht mehr beantwortbar.
//!
//! ## Die Nutzlast bleibt, wo sie ist
//!
//! Ein [`Ticket`] traegt den Request weiterhin als OIP-Nachricht. Ihn hier
//! schon in eine backendneutrale Darstellung zu uebersetzen hiesse, ihn
//! einmal mehr zu kopieren — bei 28 MB Nutzlast je VLM-Anfrage ist das keine
//! Abstraktion, sondern Bandbreite (ADR-0003). Die Uebersetzung gehoert
//! dorthin, wo ein zweiter Executor sie tatsaechlich braucht.

use std::sync::Arc;
use vig_backend_triton::{BackendError, Evidence, TritonClient};
use vig_protocol_oip::inference::{ModelInferRequest, ModelInferResponse};

/// Ein Ausfuehrungsauftrag an ein Backend.
///
/// Neutral in dem, was die Ablaufsteuerung angeht: welches Modell, ob der
/// Endpunkt entkoppelt antwortet, und die Nutzlast. Alles Weitere — Slot,
/// Kredit, Generation, Deadline — bleibt beim Actor.
#[derive(Debug)]
pub struct Ticket {
    /// Der Backendmodellname.
    pub model: String,
    /// Ob das Modell nur ueber den Stream-Endpunkt antwortet.
    pub decoupled: bool,
    /// Die Nutzlast.
    pub request: Box<ModelInferRequest>,
}

/// Was ein Backend ueber sich kann.
///
/// Bewusst klein: hier steht, was die Ablaufsteuerung **anders macht**, wenn
/// es fehlt. Eine Faehigkeitsliste, die niemand abfragt, ist Dokumentation
/// am falschen Ort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Ob das Backend belegen kann, dass eine Ausfuehrung beendet ist.
    ///
    /// Ohne diesen Nachweis bleibt ein Slotkredit nach einem Timeout
    /// dauerhaft gehalten (NV-00). Das ist die richtige Antwort — aber der
    /// Betreiber soll wissen, dass sein Backend sie erzwingt.
    pub completion_evidence: bool,
    /// Ob das Backend einen Stream-Endpunkt anbietet.
    pub decoupled_endpoint: bool,
}

/// Ein Backend, das Auftraege ausfuehrt.
///
/// Ausfuehren und berichten — mehr nicht. Kein Executor haelt Ressourcen des
/// Governors.
pub trait Executor: Send + Sync + std::fmt::Debug {
    /// Fuehrt einen Auftrag aus.
    ///
    /// # Errors
    ///
    /// Siehe [`BackendError`]. Der Unterschied zwischen „nie gestartet" und
    /// „unbekannt" entscheidet ueber den Slotkredit und wird deshalb vom
    /// Fehler selbst getragen, nicht vom Aufrufer erraten.
    fn execute(
        &self,
        ticket: Ticket,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ModelInferResponse, BackendError>> + Send + '_>>;

    /// Der Endnachweis fuer ein Modell.
    ///
    /// # Errors
    ///
    /// Wenn das Backend keine Statistik liefert. Kein Nachweis heisst: der
    /// Kredit bleibt gehalten.
    fn completion_evidence(
        &self,
        model: &str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Evidence, BackendError>> + Send + '_>>;

    /// Ob dieses Backend gerade erreichbar ist.
    ///
    /// Eine **aktive** Probe, unabhaengig vom Verkehr. Ohne sie haengt die
    /// Bereitschaftsaussage am letzten Inferenzfehler: nimmt ein
    /// Loadbalancer daraufhin allen Verkehr weg, fehlt der Ausloeser zur
    /// Erholung, und der Governor bleibt rot, obwohl das Backend laengst
    /// wieder da ist (Review R11).
    ///
    /// # Errors
    ///
    /// [`BackendError`], wenn das Backend nicht antwortet.
    fn reachable(
        &self,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + '_>>;

    /// Was dieses Backend kann.
    fn capabilities(&self) -> Capabilities;
}

/// Der Triton-Executor.
///
/// Alles OIP-Spezifische bleibt hier: welcher Endpunkt fuer entkoppelte
/// Modelle, wie die Statistik gelesen wird, wie ein Status auf einen
/// Ausfuehrungszustand abgebildet wird.
#[derive(Debug, Clone)]
pub struct TritonExecutor {
    client: Arc<TritonClient>,
}

impl TritonExecutor {
    /// Ein Executor um einen bestehenden Client.
    #[must_use]
    pub const fn new(client: Arc<TritonClient>) -> Self {
        Self { client }
    }

    /// Der zugrunde liegende Client.
    ///
    /// Fuer Pfade, die es noch nicht ueber die Naht geschafft haben —
    /// Metadaten, Bereitschaft, Signaturpruefung. Sie stehen in `doctor` und
    /// `verify`, nicht in der Ablaufsteuerung.
    #[must_use]
    pub fn client(&self) -> &Arc<TritonClient> {
        &self.client
    }
}

impl Executor for TritonExecutor {
    fn execute(
        &self,
        ticket: Ticket,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ModelInferResponse, BackendError>> + Send + '_>>
    {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            if ticket.decoupled {
                client.infer_decoupled(*ticket.request).await
            } else {
                client.infer(*ticket.request).await
            }
        })
    }

    fn completion_evidence(
        &self,
        model: &str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Evidence, BackendError>> + Send + '_>> {
        let client = Arc::clone(&self.client);
        let model = model.to_owned();
        Box::pin(async move { client.completion_evidence(&model).await })
    }

    fn reachable(
        &self,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + '_>> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            // `live` und nicht `ready`: gefragt ist, ob der Server antwortet.
            // Ob ein einzelnes Modell geladen ist, ist eine andere Frage und
            // gehoert nicht in die Erreichbarkeit des Endpunkts.
            client.health().await.map(|_| ())
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            completion_evidence: true,
            decoupled_endpoint: true,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::testing::FakeExecutor;

    #[tokio::test]
    async fn a_fake_executor_answers_without_a_gpu() {
        let fake = FakeExecutor::default();
        fake.expect_ok("rfdetr");
        let response = fake
            .execute(Ticket {
                model: "rfdetr".to_owned(),
                decoupled: false,
                request: Box::default(),
            })
            .await
            .unwrap();
        assert_eq!(response.model_name, "rfdetr");
        assert_eq!(fake.executed(), 1);
    }

    #[tokio::test]
    async fn a_fake_executor_reproduces_the_states_that_decide_a_credit() {
        // Der Unterschied, der ueber den Slotkredit entscheidet (NV-00): ein
        // Aufruf, der das Backend nie erreicht hat, gegen einen, der
        // unterwegs abbrach. Auf echter Hardware ist das kaum herbeizufuehren.
        use vig_backend_triton::ExecutionState;

        let fake = FakeExecutor::default();
        fake.expect_error(
            "rfdetr",
            BackendError::Unreachable {
                endpoint: "127.0.0.1:8001".to_owned(),
                cause: "connection refused".to_owned(),
            },
        );
        let error = fake
            .execute(Ticket {
                model: "rfdetr".to_owned(),
                decoupled: false,
                request: Box::default(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.execution_state(), ExecutionState::NotStarted);

        fake.expect_error(
            "rfdetr",
            BackendError::Rejected {
                code: tonic::Code::Unavailable,
                message: "transport closed".to_owned(),
            },
        );
        let error = fake
            .execute(Ticket {
                model: "rfdetr".to_owned(),
                decoupled: false,
                request: Box::default(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.execution_state(),
            ExecutionState::Unknown,
            "unterwegs abgebrochen heisst nicht, dass die GPU fertig ist"
        );
    }

    #[tokio::test]
    async fn a_backend_without_evidence_is_a_capability_not_a_crash() {
        let fake = FakeExecutor::default();
        fake.set_capabilities(Capabilities {
            completion_evidence: false,
            decoupled_endpoint: false,
        });
        assert!(!fake.capabilities().completion_evidence);
        assert!(fake.completion_evidence("rfdetr").await.is_err());
    }

    #[tokio::test]
    async fn the_fake_records_what_it_was_asked_to_run() {
        let fake = FakeExecutor::default();
        fake.expect_ok("a");
        fake.expect_ok("b");
        let _ = fake
            .execute(Ticket {
                model: "a".to_owned(),
                decoupled: false,
                request: Box::default(),
            })
            .await;
        let _ = fake
            .execute(Ticket {
                model: "b".to_owned(),
                decoupled: true,
                request: Box::default(),
            })
            .await;
        assert_eq!(fake.executed_models(), vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(fake.decoupled_calls(), 1);
    }
}
