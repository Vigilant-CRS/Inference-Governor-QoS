//! Die OneTimer-Erweiterungsparameter (Spec 16.2, 16.3).
//!
//! Alle Erweiterungen tragen den Prefix `onetimer_` und reisen im
//! `parameters`-Feld eines `ModelInferRequest`. Ein Client, der keine davon
//! setzt, funktioniert unveraendert weiter — das ist der Compatibility Mode
//! aus Spec 16.3 und die eigentliche Integrationszusage des Produkts.
//!
//! ## Die Uhrenfrage
//!
//! `generation_time` ist der wichtigste Wert, den ein Client liefern kann, und
//! zugleich der gefaehrlichste: er stammt aus **einer fremden Zeitbasis**.
//! Ein monotoner Zeitstempel ist nur innerhalb desselben Hosts vergleichbar;
//! zwischen zwei Rechnern ist er bedeutungslos, und eine Wall-Clock darf nach
//! Spec L-019 gar nicht erst zur Scheduling-Grundlage werden.
//!
//! Deshalb kennt OneTimer zwei Wege, und der Client waehlt den, der zu seiner
//! Topologie passt:
//!
//! * [`GenerationHint::MonotonicNanos`] — `onetimer_generation_ns`. Exakt,
//!   aber **nur gueltig, wenn Client und Governor dieselbe monotone Uhr
//!   teilen**, also auf demselben Host laufen. Das ist der Regelfall auf einem
//!   Roboter.
//! * [`GenerationHint::AgeMicros`] — `onetimer_age_us`. Der Client sagt, wie
//!   alt das Sensordatum beim Senden war. Weniger genau, weil die
//!   Uebertragungszeit fehlt, dafuer ueber Hostgrenzen hinweg gueltig.
//!
//! Ein unplausibler Wert wird nicht stillschweigend verrechnet, sondern
//! verworfen: siehe [`ExtractError`] und [`resolve_generation`].

use crate::inference::InferParameter;
use crate::inference::infer_parameter::ParameterChoice;
use onetimer_core::{Criticality, Duration, Instant};
use std::collections::HashMap;

/// Der gemeinsame Prefix aller OneTimer-Parameter (Spec 16.2).
pub const PARAM_PREFIX: &str = "onetimer_";

/// Absolute monotone Erzeugungszeit in Nanosekunden.
pub const P_GENERATION_NS: &str = "onetimer_generation_ns";
/// Alter des Sensordatums beim Senden, in Mikrosekunden.
pub const P_AGE_US: &str = "onetimer_age_us";
/// Streamkennung.
pub const P_STREAM_ID: &str = "onetimer_stream_id";
/// Freshness-Scope fuer Supersession.
pub const P_SUPERSESSION_KEY: &str = "onetimer_supersession_key";
/// Relative Deadline in Mikrosekunden.
pub const P_DEADLINE_US: &str = "onetimer_deadline_us";
/// Hoechstalter in Mikrosekunden.
pub const P_MAX_AGE_US: &str = "onetimer_max_age_us";
/// Wichtigkeitsklasse.
pub const P_CLASS: &str = "onetimer_class";

/// Groesste akzeptierte Zukunftsabweichung eines Client-Zeitstempels.
///
/// Ein Zeitstempel, der weiter in der Zukunft liegt, stammt aus einer anderen
/// Zeitbasis. Ihn zu verrechnen wuerde eine Deadline erzeugen, die nichts mit
/// der Wirklichkeit zu tun hat.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_nanos_unbounded(10_000_000);

/// Woher die Erzeugungszeit kommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationHint {
    /// Absolute monotone Zeit des Clients. Nur bei geteilter Uhr gueltig.
    MonotonicNanos(u64),
    /// Alter des Sensordatums beim Senden.
    AgeMicros(u64),
}

/// Die aus einem Request gelesenen OneTimer-Parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OneTimerParams {
    /// Herkunft der Erzeugungszeit, falls angegeben.
    pub generation: Option<GenerationHint>,
    /// Streamkennung.
    pub stream_id: Option<u64>,
    /// Freshness-Scope.
    pub supersession_key: Option<u64>,
    /// Relative Deadline.
    pub deadline: Option<Duration>,
    /// Hoechstalter.
    pub max_age: Option<Duration>,
    /// Wichtigkeitsklasse.
    pub class: Option<Criticality>,
}

impl OneTimerParams {
    /// Wahr, wenn der Client keinen einzigen OneTimer-Parameter gesetzt hat.
    ///
    /// Dann gilt der Compatibility Mode: Vertrag und Policy kommen vollstaendig
    /// aus der Serverkonfiguration (Spec 16.3).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Warum ein Parameter nicht uebernommen werden konnte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    /// Der Parameter hat den falschen Wertetyp.
    WrongType {
        /// Der Parametername.
        name: String,
        /// Der erwartete Typ.
        expected: &'static str,
    },
    /// Der Wert liegt ausserhalb des zulaessigen Bereichs.
    OutOfRange {
        /// Der Parametername.
        name: String,
    },
    /// Die Wichtigkeitsklasse ist unbekannt.
    UnknownClass {
        /// Der uebergebene Wert.
        value: String,
    },
    /// Sowohl absolute Erzeugungszeit als auch Alter wurden gesetzt.
    ///
    /// Beide gleichzeitig zu akzeptieren hiesse, sich fuer eine der beiden
    /// stillschweigend zu entscheiden. Spec L-020 verlangt das Gegenteil.
    ConflictingGeneration,
    /// Ein Parameter mit `onetimer_`-Prefix ist unbekannt.
    ///
    /// Ein Tippfehler in einem Frische-Parameter waere sonst folgenlos — der
    /// Request liefe ohne die Semantik, die der Entwickler gemeint hat.
    UnknownParameter {
        /// Der Parametername.
        name: String,
    },
}

impl core::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::WrongType { name, expected } => {
                write!(f, "Parameter {name} muss vom Typ {expected} sein")
            }
            Self::OutOfRange { name } => {
                write!(f, "Parameter {name} liegt ausserhalb des Bereichs")
            }
            Self::UnknownClass { value } => write!(
                f,
                "unbekannte Klasse {value:?}; zulaessig sind protected, high, normal, best_effort"
            ),
            Self::ConflictingGeneration => write!(
                f,
                "{P_GENERATION_NS} und {P_AGE_US} schliessen einander aus"
            ),
            Self::UnknownParameter { name } => {
                write!(f, "unbekannter OneTimer-Parameter {name}")
            }
        }
    }
}

impl core::error::Error for ExtractError {}

fn as_u64(name: &str, p: &InferParameter) -> Result<u64, ExtractError> {
    match p.parameter_choice {
        Some(ParameterChoice::Int64Param(v)) => {
            u64::try_from(v).map_err(|_| ExtractError::OutOfRange {
                name: name.to_owned(),
            })
        }
        Some(ParameterChoice::Uint64Param(v)) => Ok(v),
        _ => Err(ExtractError::WrongType {
            name: name.to_owned(),
            expected: "int64",
        }),
    }
}

fn as_duration_from_micros(name: &str, p: &InferParameter) -> Result<Duration, ExtractError> {
    let micros = as_u64(name, p)?;
    Duration::from_micros(micros).ok_or(ExtractError::OutOfRange {
        name: name.to_owned(),
    })
}

fn as_class(p: &InferParameter) -> Result<Criticality, ExtractError> {
    let Some(ParameterChoice::StringParam(ref s)) = p.parameter_choice else {
        return Err(ExtractError::WrongType {
            name: P_CLASS.to_owned(),
            expected: "string",
        });
    };
    match s.as_str() {
        "protected" => Ok(Criticality::Protected),
        "high" => Ok(Criticality::High),
        "normal" => Ok(Criticality::Normal),
        "best_effort" => Ok(Criticality::BestEffort),
        other => Err(ExtractError::UnknownClass {
            value: other.to_owned(),
        }),
    }
}

/// Liest die OneTimer-Parameter aus einem Request.
///
/// Parameter ohne den Prefix werden ignoriert und bleiben unangetastet: Spec
/// 6.1 verlangt ausdruecklich, dass unbekannte OIP-Felder nicht zerstoert
/// werden.
///
/// # Errors
///
/// Siehe [`ExtractError`]. Ein fehlerhafter Parameter fuehrt zur Ablehnung des
/// Requests, nicht zu einem stillen Default — sonst liefe ein Request mit
/// vertippter Frischeangabe ohne die Semantik, die gemeint war (Spec L-020).
pub fn extract<S: core::hash::BuildHasher>(
    parameters: &HashMap<String, InferParameter, S>,
) -> Result<OneTimerParams, ExtractError> {
    let mut out = OneTimerParams::default();
    let mut saw_generation_ns = false;
    let mut saw_age = false;

    for (name, value) in parameters {
        if !name.starts_with(PARAM_PREFIX) {
            continue;
        }
        match name.as_str() {
            P_GENERATION_NS => {
                saw_generation_ns = true;
                out.generation = Some(GenerationHint::MonotonicNanos(as_u64(name, value)?));
            }
            P_AGE_US => {
                saw_age = true;
                out.generation = Some(GenerationHint::AgeMicros(as_u64(name, value)?));
            }
            P_STREAM_ID => out.stream_id = Some(as_u64(name, value)?),
            P_SUPERSESSION_KEY => out.supersession_key = Some(as_u64(name, value)?),
            P_DEADLINE_US => out.deadline = Some(as_duration_from_micros(name, value)?),
            P_MAX_AGE_US => out.max_age = Some(as_duration_from_micros(name, value)?),
            P_CLASS => out.class = Some(as_class(value)?),
            other => {
                return Err(ExtractError::UnknownParameter {
                    name: other.to_owned(),
                });
            }
        }
    }

    if saw_generation_ns && saw_age {
        return Err(ExtractError::ConflictingGeneration);
    }
    Ok(out)
}

/// Wie die Erzeugungszeit eines Requests zustande kam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationSource {
    /// Aus einem plausiblen absoluten Zeitstempel des Clients.
    ClientMonotonic,
    /// Aus einer vom Client gemeldeten Altersangabe.
    ClientAge,
    /// Fallback auf die Ankunftszeit (Spec 16.3).
    ///
    /// Entweder hat der Client nichts geliefert, oder sein Wert war
    /// unplausibel. Der zweite Fall ist ein Konfigurations- oder
    /// Topologiefehler und muss als Metrik sichtbar werden.
    ArrivalFallback {
        /// Wahr, wenn ein Clientwert vorlag, aber verworfen wurde.
        rejected_client_value: bool,
    },
}

/// Bestimmt die Erzeugungszeit in der lokalen monotonen Zeitbasis.
///
/// `arrival` ist die lokale monotone Ankunftszeit. Ein absoluter Clientwert
/// wird nur uebernommen, wenn er plausibel ist: nicht weiter als
/// [`MAX_CLOCK_SKEW`] in der Zukunft und nicht aelter als `max_plausible_age`.
/// Andernfalls faellt OneTimer auf die Ankunftszeit zurueck und meldet das,
/// statt eine sinnlose Deadline auszurechnen.
#[must_use]
pub fn resolve_generation(
    params: &OneTimerParams,
    arrival: Instant,
    max_plausible_age: Duration,
) -> (Instant, GenerationSource) {
    match params.generation {
        None => (
            arrival,
            GenerationSource::ArrivalFallback {
                rejected_client_value: false,
            },
        ),
        Some(GenerationHint::AgeMicros(micros)) => match Duration::from_micros(micros) {
            Some(age) if age <= max_plausible_age => {
                (arrival.saturating_sub(age), GenerationSource::ClientAge)
            }
            _ => (
                arrival,
                GenerationSource::ArrivalFallback {
                    rejected_client_value: true,
                },
            ),
        },
        Some(GenerationHint::MonotonicNanos(nanos)) => {
            let claimed = Instant::from_nanos(nanos);
            let in_future = claimed.saturating_since(arrival);
            let age = arrival.saturating_since(claimed);
            if in_future > MAX_CLOCK_SKEW || age > max_plausible_age {
                // Der Wert stammt erkennbar aus einer anderen Zeitbasis.
                (
                    arrival,
                    GenerationSource::ArrivalFallback {
                        rejected_client_value: true,
                    },
                )
            } else {
                // Ein leicht zukuenftiger Zeitstempel wird auf jetzt geklemmt:
                // negative Alter gibt es nicht.
                (claimed.min(arrival), GenerationSource::ClientMonotonic)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;

    fn int(v: i64) -> InferParameter {
        InferParameter {
            parameter_choice: Some(ParameterChoice::Int64Param(v)),
        }
    }

    fn text(v: &str) -> InferParameter {
        InferParameter {
            parameter_choice: Some(ParameterChoice::StringParam(v.to_owned())),
        }
    }

    fn flag(v: bool) -> InferParameter {
        InferParameter {
            parameter_choice: Some(ParameterChoice::BoolParam(v)),
        }
    }

    fn map(pairs: &[(&str, InferParameter)]) -> HashMap<String, InferParameter> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    fn at(v: u64) -> Instant {
        Instant::ZERO.checked_add(ms(v)).unwrap()
    }

    /// Spec 16.3: ein Client ohne OneTimer-Parameter laeuft unveraendert weiter.
    #[test]
    fn a_plain_request_is_accepted_unchanged() {
        let params = extract(&map(&[("some_vendor_param", int(1))])).unwrap();
        assert!(params.is_empty(), "fremde Parameter aendern nichts");
    }

    #[test]
    fn all_documented_parameters_are_read() {
        let params = extract(&map(&[
            (P_GENERATION_NS, int(1_000_000)),
            (P_STREAM_ID, int(7)),
            (P_SUPERSESSION_KEY, int(42)),
            (P_DEADLINE_US, int(30_000)),
            (P_MAX_AGE_US, int(66_000)),
            (P_CLASS, text("protected")),
        ]))
        .unwrap();

        assert_eq!(
            params.generation,
            Some(GenerationHint::MonotonicNanos(1_000_000))
        );
        assert_eq!(params.stream_id, Some(7));
        assert_eq!(params.supersession_key, Some(42));
        assert_eq!(params.deadline, Some(ms(30)));
        assert_eq!(params.max_age, Some(ms(66)));
        assert_eq!(params.class, Some(Criticality::Protected));
    }

    /// Ein Tippfehler in einem Frische-Parameter darf nicht folgenlos bleiben:
    /// der Request liefe sonst ohne die Semantik, die gemeint war.
    #[test]
    fn a_misspelled_onetimer_parameter_is_rejected() {
        let err = extract(&map(&[("onetimer_deadline_ms", int(30))])).unwrap_err();
        assert_eq!(
            err,
            ExtractError::UnknownParameter {
                name: "onetimer_deadline_ms".to_owned()
            }
        );
    }

    #[test]
    fn wrong_types_are_rejected() {
        assert!(matches!(
            extract(&map(&[(P_DEADLINE_US, flag(true))])).unwrap_err(),
            ExtractError::WrongType { .. }
        ));
        assert!(matches!(
            extract(&map(&[(P_CLASS, int(1))])).unwrap_err(),
            ExtractError::WrongType { .. }
        ));
        assert!(matches!(
            extract(&map(&[(P_CLASS, text("kritisch"))])).unwrap_err(),
            ExtractError::UnknownClass { .. }
        ));
    }

    /// Spec G-012 und 8.3: fremd kontrollierte Groessen duerfen keinen
    /// Ueberlauf und keine absurde Deadline erzeugen.
    #[test]
    fn hostile_values_cannot_produce_an_absurd_contract() {
        assert!(matches!(
            extract(&map(&[(P_DEADLINE_US, int(-1))])).unwrap_err(),
            ExtractError::OutOfRange { .. }
        ));
        assert!(matches!(
            extract(&map(&[(P_MAX_AGE_US, int(i64::MAX))])).unwrap_err(),
            ExtractError::OutOfRange { .. }
        ));
    }

    #[test]
    fn generation_and_age_are_mutually_exclusive() {
        let err = extract(&map(&[(P_GENERATION_NS, int(1)), (P_AGE_US, int(1))])).unwrap_err();
        assert_eq!(err, ExtractError::ConflictingGeneration);
    }

    // -----------------------------------------------------------------------
    // Uhrenbehandlung
    // -----------------------------------------------------------------------

    #[test]
    fn without_a_hint_the_arrival_time_is_used() {
        let (generation, source) =
            resolve_generation(&OneTimerParams::default(), at(100), ms(1_000));
        assert_eq!(generation, at(100));
        assert_eq!(
            source,
            GenerationSource::ArrivalFallback {
                rejected_client_value: false
            }
        );
    }

    #[test]
    fn a_reported_age_is_subtracted_from_the_arrival_time() {
        let params = OneTimerParams {
            generation: Some(GenerationHint::AgeMicros(25_000)),
            ..OneTimerParams::default()
        };
        let (generation, source) = resolve_generation(&params, at(100), ms(1_000));
        assert_eq!(generation, at(75));
        assert_eq!(source, GenerationSource::ClientAge);
    }

    #[test]
    fn a_shared_monotonic_timestamp_is_taken_as_is() {
        let params = OneTimerParams {
            generation: Some(GenerationHint::MonotonicNanos(at(70).as_nanos())),
            ..OneTimerParams::default()
        };
        let (generation, source) = resolve_generation(&params, at(100), ms(1_000));
        assert_eq!(generation, at(70));
        assert_eq!(source, GenerationSource::ClientMonotonic);
    }

    /// Der wichtigste Fall: ein Zeitstempel aus einer fremden Zeitbasis darf
    /// keine sinnlose Deadline erzeugen. Er wird verworfen und der Fallback
    /// gemeldet, statt still verrechnet zu werden.
    #[test]
    fn a_timestamp_from_another_clock_domain_is_rejected() {
        // Weit in der Zukunft: typisch fuer eine andere Epoche.
        let far_future = OneTimerParams {
            generation: Some(GenerationHint::MonotonicNanos(at(100_000).as_nanos())),
            ..OneTimerParams::default()
        };
        let (generation, source) = resolve_generation(&far_future, at(100), ms(1_000));
        assert_eq!(generation, at(100));
        assert_eq!(
            source,
            GenerationSource::ArrivalFallback {
                rejected_client_value: true
            }
        );

        // Absurd alt: ebenfalls eine andere Zeitbasis, kein echtes Sensordatum.
        let ancient = OneTimerParams {
            generation: Some(GenerationHint::MonotonicNanos(0)),
            ..OneTimerParams::default()
        };
        let (generation, source) = resolve_generation(&ancient, at(100_000), ms(1_000));
        assert_eq!(generation, at(100_000));
        assert_eq!(
            source,
            GenerationSource::ArrivalFallback {
                rejected_client_value: true
            }
        );
    }

    /// Kleiner Uhrenversatz ist normal und darf nicht zum Fallback fuehren;
    /// ein negatives Alter darf dabei aber nicht entstehen.
    #[test]
    fn small_skew_is_tolerated_and_clamped() {
        let params = OneTimerParams {
            generation: Some(GenerationHint::MonotonicNanos(at(105).as_nanos())),
            ..OneTimerParams::default()
        };
        let (generation, source) = resolve_generation(&params, at(100), ms(1_000));
        assert_eq!(generation, at(100), "kein negatives Alter");
        assert_eq!(source, GenerationSource::ClientMonotonic);
    }
}
