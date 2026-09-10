//! Ein Backend, das ohne GPU antwortet (NV-07).
//!
//! ## Wofuer
//!
//! Die interessanten Fehlerpfade des Governors lassen sich auf echter
//! Hardware kaum herbeifuehren. „Der Aufruf brach unterwegs ab, und die GPU
//! rechnet vielleicht weiter" ist genau der Zustand, in dem sich entscheidet,
//! ob ein Slotkredit zu frueh zurueckkommt (NV-00) — und er tritt im Labor
//! selten und nie auf Kommando ein.
//!
//! Dieser Executor tritt ihn auf Kommando ein. Er kennt kein Netz, keine
//! Uhr und keine GPU; er gibt zurueck, was ihm vorher gesagt wurde, und
//! schreibt mit, was von ihm verlangt wurde.
//!
//! ## Warum Innenveraenderlichkeit
//!
//! Der [`Executor`](crate::executor::Executor)-Vertrag ist `&self` — ein
//! Backend veraendert den Governor nicht. Ein Fake muss trotzdem mitzaehlen,
//! also liegt sein Zustand hinter einem Mutex. Das ist Testcode, und die
//! Alternative waere gewesen, den Vertrag fuer alle Implementierungen
//! aufzuweichen.

// Ein vergifteter Mutex heisst hier: ein Test ist in einem anderen Thread
// gescheitert. Dann soll dieser auch scheitern, und zwar laut.
#![allow(
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "Testhilfe; ein vergifteter Mutex ist ein Testfehler"
)]

use crate::executor::{Capabilities, Executor, Ticket};
use std::collections::VecDeque;
use std::sync::Mutex;
use vig_backend_triton::{BackendError, Evidence};
use vig_protocol_oip::inference::{ModelInferRequest, ModelInferResponse};

/// Was der Fake auf den naechsten Aufruf antwortet.
#[derive(Debug)]
enum Programmed {
    /// Eine Antwort mit diesem Modellnamen.
    Ok(String),
    /// Dieser Fehler.
    Err(BackendError),
    /// Erst nach dieser Zeit antworten.
    ///
    /// Fuer Timeout-Pfade: der Actor beantwortet den Client nach der Frist,
    /// wartet aber weiter auf das Backend.
    Slow {
        /// Wie lange.
        delay: std::time::Duration,
        /// Was danach kommt.
        then: Box<Programmed>,
    },
}

/// Der aufgezeichnete Zustand.
#[derive(Debug, Default)]
struct Recorded {
    programmed: VecDeque<Programmed>,
    executed_models: Vec<String>,
    decoupled_calls: u64,
    evidence: u64,
    evidence_calls: u64,
}

/// Ein Backend, das ohne GPU antwortet.
#[derive(Debug)]
pub struct FakeExecutor {
    inner: Mutex<Recorded>,
    capabilities: Mutex<Capabilities>,
    /// Wenn gesetzt, meldet die Erreichbarkeitsprobe diesen Fehler.
    unreachable: Mutex<Option<BackendError>>,
}

impl Default for FakeExecutor {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Recorded::default()),
            capabilities: Mutex::new(Capabilities {
                completion_evidence: true,
                decoupled_endpoint: true,
            }),
            unreachable: Mutex::new(None),
        }
    }
}

impl FakeExecutor {
    /// Laesst die Erreichbarkeitsprobe scheitern — oder wieder gelingen.
    ///
    /// Damit laesst sich der Fall nachstellen, um den es bei R11 geht: das
    /// Backend faellt aus, der Loadbalancer nimmt den Verkehr weg, und der
    /// Governor muss sich **ohne** Inferenz wieder erholen koennen.
    pub fn set_unreachable(&self, error: Option<BackendError>) {
        *self.unreachable.lock().unwrap() = error;
    }

    /// Der naechste Aufruf gelingt und meldet diesen Modellnamen.
    pub fn expect_ok(&self, model: &str) {
        self.inner
            .lock()
            .unwrap()
            .programmed
            .push_back(Programmed::Ok(model.to_owned()));
    }

    /// Der naechste Aufruf schlaegt mit diesem Fehler fehl.
    pub fn expect_error(&self, _model: &str, error: BackendError) {
        self.inner
            .lock()
            .unwrap()
            .programmed
            .push_back(Programmed::Err(error));
    }

    /// Der naechste Aufruf antwortet erst nach dieser Zeit.
    pub fn expect_slow(&self, model: &str, delay: std::time::Duration) {
        self.inner
            .lock()
            .unwrap()
            .programmed
            .push_back(Programmed::Slow {
                delay,
                then: Box::new(Programmed::Ok(model.to_owned())),
            });
    }

    /// Der naechste Aufruf bricht nach dieser Zeit mit diesem Fehler ab.
    pub fn expect_slow_error(&self, delay: std::time::Duration, error: BackendError) {
        self.inner
            .lock()
            .unwrap()
            .programmed
            .push_back(Programmed::Slow {
                delay,
                then: Box::new(Programmed::Err(error)),
            });
    }

    /// Setzt den Endnachweiszaehler, den das Backend meldet.
    pub fn set_evidence(&self, completed: u64) {
        self.inner.lock().unwrap().evidence = completed;
    }

    /// Setzt die Faehigkeiten.
    pub fn set_capabilities(&self, capabilities: Capabilities) {
        *self.capabilities.lock().unwrap() = capabilities;
    }

    /// Wie viele Auftraege ausgefuehrt wurden.
    #[must_use]
    pub fn executed(&self) -> usize {
        self.inner.lock().unwrap().executed_models.len()
    }

    /// Welche Modelle, in Aufrufreihenfolge.
    #[must_use]
    pub fn executed_models(&self) -> Vec<String> {
        self.inner.lock().unwrap().executed_models.clone()
    }

    /// Wie viele Aufrufe den entkoppelten Endpunkt verlangten.
    #[must_use]
    pub fn decoupled_calls(&self) -> u64 {
        self.inner.lock().unwrap().decoupled_calls
    }

    /// Wie oft nach einem Endnachweis gefragt wurde.
    #[must_use]
    pub fn evidence_calls(&self) -> u64 {
        self.inner.lock().unwrap().evidence_calls
    }

    /// Wie viele programmierte Antworten noch offen sind.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.inner.lock().unwrap().programmed.len()
    }

    /// Nimmt die naechste programmierte Antwort.
    ///
    /// Ohne Programmierung antwortet der Fake mit einem Fehler und nicht mit
    /// Erfolg: ein Test, der eine Antwort erwartet, ohne sie zu bestellen,
    /// soll auffallen.
    fn next(&self, ticket: &Ticket) -> Programmed {
        let mut inner = self.inner.lock().unwrap();
        inner.executed_models.push(ticket.model.clone());
        if ticket.decoupled {
            inner.decoupled_calls = inner.decoupled_calls.saturating_add(1);
        }
        inner.programmed.pop_front().unwrap_or_else(|| {
            Programmed::Err(BackendError::Malformed {
                detail: "FakeExecutor ohne programmierte Antwort".to_owned(),
            })
        })
    }
}

fn respond(
    programmed: Programmed,
) -> std::pin::Pin<Box<dyn Future<Output = Result<ModelInferResponse, BackendError>> + Send>> {
    Box::pin(async move {
        let mut step = programmed;
        loop {
            match step {
                Programmed::Ok(model) => {
                    return Ok(ModelInferResponse {
                        model_name: model,
                        ..ModelInferResponse::default()
                    });
                }
                Programmed::Err(error) => return Err(error),
                Programmed::Slow { delay, then } => {
                    tokio::time::sleep(delay).await;
                    step = *then;
                }
            }
        }
    })
}

impl Executor for FakeExecutor {
    fn execute(
        &self,
        ticket: Ticket,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ModelInferResponse, BackendError>> + Send + '_>>
    {
        respond(self.next(&ticket))
    }

    fn completion_evidence(
        &self,
        model: &str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Evidence, BackendError>> + Send + '_>> {
        let supported = self.capabilities.lock().unwrap().completion_evidence;
        let mut inner = self.inner.lock().unwrap();
        inner.evidence_calls = inner.evidence_calls.saturating_add(1);
        let completed = inner.evidence;
        let model = model.to_owned();
        drop(inner);
        Box::pin(async move {
            if supported {
                Ok(Evidence {
                    completed,
                    last_inference_ms: 0,
                })
            } else {
                Err(BackendError::UnknownModel { model })
            }
        })
    }

    fn reachable(
        &self,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send + '_>> {
        let error = self.unreachable.lock().unwrap().clone();
        Box::pin(async move { error.map_or(Ok(()), Err) })
    }

    fn capabilities(&self) -> Capabilities {
        *self.capabilities.lock().unwrap()
    }
}

/// Ein Nulltensor-Request auf ein Modell, fuer Tests.
#[must_use]
pub fn request_for(model: &str) -> ModelInferRequest {
    ModelInferRequest {
        model_name: model.to_owned(),
        ..ModelInferRequest::default()
    }
}
