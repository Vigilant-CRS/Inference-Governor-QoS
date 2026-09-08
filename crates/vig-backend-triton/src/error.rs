//! Fehler des Backend-Adapters.

/// Warum ein Backendaufruf fehlschlug.
#[derive(Debug, Clone)]
pub enum BackendError {
    /// Die Verbindung zum Backend liess sich nicht aufbauen.
    Unreachable {
        /// Der Endpunkt.
        endpoint: String,
        /// Die Ursache.
        cause: String,
    },
    /// Das Backend hat den Aufruf mit einem Statuscode abgelehnt.
    Rejected {
        /// Der gRPC-Statuscode.
        code: tonic::Code,
        /// Die Meldung des Backends.
        message: String,
    },
    /// Das Backend kennt das Modell nicht.
    ///
    /// Eigener Fall, weil er fast immer ein Konfigurationsfehler ist:
    /// `backend_model` in der Governor-Konfiguration passt nicht zum
    /// Modellrepository. `vig doctor` prueft das vor dem Start.
    UnknownModel {
        /// Der angefragte Modellname.
        model: String,
    },
    /// Die Antwort verletzt das Protokoll.
    Malformed {
        /// Was fehlte oder unstimmig war.
        detail: String,
    },
}

impl core::fmt::Display for BackendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreachable { endpoint, cause } => {
                write!(f, "Backend {endpoint} nicht erreichbar: {cause}")
            }
            Self::Rejected { code, message } => write!(f, "Backend meldet {code:?}: {message}"),
            Self::UnknownModel { model } => write!(
                f,
                "das Backend kennt das Modell {model:?} nicht; \
                 stimmt `backend_model` in der Konfiguration?"
            ),
            Self::Malformed { detail } => write!(f, "unerwartete Backendantwort: {detail}"),
        }
    }
}

impl core::error::Error for BackendError {}

impl BackendError {
    /// Wahr, wenn ein erneuter Verbindungsaufbau sinnvoll ist.
    ///
    /// Ob ein **Request** wiederholt wird, entscheidet der Scheduler anhand
    /// der Frische — nicht dieser Adapter.
    #[must_use]
    pub const fn is_transport_failure(&self) -> bool {
        match self {
            Self::Unreachable { .. } => true,
            Self::Rejected { code, .. } => matches!(
                code,
                tonic::Code::Unavailable | tonic::Code::DeadlineExceeded | tonic::Code::Aborted
            ),
            Self::UnknownModel { .. } | Self::Malformed { .. } => false,
        }
    }
}

impl From<tonic::Status> for BackendError {
    fn from(status: tonic::Status) -> Self {
        if status.code() == tonic::Code::NotFound {
            return Self::UnknownModel {
                model: status.message().to_owned(),
            };
        }
        Self::Rejected {
            code: status.code(),
            message: status.message().to_owned(),
        }
    }
}
