//! Fehler des Backend-Adapters.

/// Was ein Fehler ueber die Ausfuehrung auf dem Backend aussagt.
///
/// Diese Unterscheidung entscheidet ueber den Slotkredit — und damit darueber,
/// ob der Governor eine zweite Inferenz auf eine womoeglich noch rechnende
/// Recheneinheit legt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionState {
    /// Der Aufruf hat das Backend nie erreicht.
    NotStarted,
    /// Der Aufruf war unterwegs; ob die Recheneinheit noch arbeitet, ist
    /// unbekannt.
    Unknown,
    /// Das Backend hat geantwortet; die Ausfuehrung ist beendet.
    Finished,
}

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
    /// Was dieser Fehler ueber die **Ausfuehrung** auf dem Backend aussagt.
    ///
    /// Der Unterschied entscheidet ueber den Slotkredit. Ein Fehler beim
    /// Verbindungsaufbau heisst: der Request hat die Recheneinheit nie
    /// erreicht, der Kredit gehoert sofort zurueck. Ein Abbruch **waehrend**
    /// des Aufrufs heisst gar nichts — die GPU rechnet moeglicherweise
    /// weiter, und den Kredit dann zurueckzugeben hiesse, eine zweite
    /// Ausfuehrung auf dieselbe Einheit zu legen.
    #[must_use]
    pub const fn execution_state(&self) -> ExecutionState {
        match self {
            // `Unreachable` entsteht ausschliesslich beim Kanalaufbau.
            Self::Unreachable { .. } => ExecutionState::NotStarted,
            Self::Rejected { code, .. } => match code {
                // Der Aufruf war unterwegs, als er abbrach.
                tonic::Code::Unavailable
                | tonic::Code::DeadlineExceeded
                | tonic::Code::Aborted
                | tonic::Code::Cancelled
                | tonic::Code::Unknown => ExecutionState::Unknown,
                // Das Backend hat geantwortet, wenn auch ablehnend.
                _ => ExecutionState::Finished,
            },
            Self::UnknownModel { .. } | Self::Malformed { .. } => ExecutionState::Finished,
        }
    }

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
