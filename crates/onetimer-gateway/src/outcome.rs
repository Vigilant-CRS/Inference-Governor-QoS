//! Die Uebersetzung terminaler Requestzustaende in gRPC-Antworten.

use onetimer_core::RequestState;
use onetimer_protocol_oip::inference::infer_parameter::ParameterChoice;
use onetimer_protocol_oip::inference::{InferParameter, ModelInferResponse};
use tonic::{Code, Status};

/// Metadatenschluessel, unter dem OneTimer den Grund einer Ablehnung nennt.
///
/// Ein Client soll programmatisch unterscheiden koennen, ob sein Request
/// ueberholt wurde, zu alt war oder gegen Backpressure lief. Ein reiner
/// Fehlertext waere dafuer nicht belastbar.
pub const REASON_HEADER: &str = "onetimer-reason";

/// Response-Parameter, der ein bei Fertigstellung veraltetes Ergebnis markiert.
pub const OBSOLETE_PARAM: &str = "onetimer_obsolete";

/// Uebersetzt einen terminalen Zustand in einen gRPC-Status.
///
/// Die Codewahl folgt der Frage, was der Client tun soll:
///
/// * `Aborted` — der Request wurde absichtlich verworfen. Ein Retry desselben
///   Frames ist sinnlos; der Client soll den naechsten schicken.
/// * `ResourceExhausted` — es fehlte Kapazitaet. Ein Retry ist sinnvoll, aber
///   erst nach Entlastung. Das ist die uebliche Backpressure-Semantik.
/// * `Unavailable` — das Backend hat versagt. Ein Retry kann helfen.
#[must_use]
pub fn status_for(state: RequestState) -> Option<Status> {
    let (code, reason, message) = match state {
        RequestState::Superseded => (
            Code::Aborted,
            "superseded",
            "durch einen neueren Request desselben Streams ersetzt",
        ),
        RequestState::Stale => (
            Code::Aborted,
            "stale",
            "das Ergebnis waere bei Fertigstellung zu alt gewesen",
        ),
        RequestState::RejectedInfeasible => (
            Code::ResourceExhausted,
            "infeasible",
            "nicht mehr rechtzeitig ausfuehrbar oder Queue erschoepft",
        ),
        RequestState::Failed => (Code::Unavailable, "backend_failed", "Backendfehler"),
        // Fertiggestellt: die Antwort kommt aus dem Backend, nicht aus einem
        // Status. Nicht-terminale Zustaende erreichen diese Funktion nicht,
        // werden aber gleich behandelt, statt zu panisch zu werden.
        RequestState::CompletedValid
        | RequestState::CompletedObsolete
        | RequestState::Received
        | RequestState::Queued
        | RequestState::Admitted
        | RequestState::Forwarded => return None,
    };
    let mut status = Status::new(code, message);
    if let Ok(value) = reason.parse() {
        status.metadata_mut().insert(REASON_HEADER, value);
    }
    Some(status)
}

/// Markiert eine Antwort als bei Fertigstellung veraltet (Spec 10.3 Stufe C).
///
/// Das Ergebnis wird trotzdem geliefert. Es zu verschweigen waere eine
/// Verhaltensaenderung gegenueber einem direkten Triton und wuerde Clients
/// brechen, die mit einer Antwort rechnen. Der Parameter macht den Zustand
/// sichtbar; die Entscheidung darueber trifft die Anwendung.
pub fn mark_obsolete(response: &mut ModelInferResponse) {
    response.parameters.insert(
        OBSOLETE_PARAM.to_owned(),
        InferParameter {
            parameter_choice: Some(ParameterChoice::BoolParam(true)),
        },
    );
}
