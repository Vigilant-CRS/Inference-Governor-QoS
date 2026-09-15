//! # Vigilant Scheduling Core
//!
//! Der reine, deterministische Kern des Vigilant-Inference-Governors: Zeit,
//! Requestklassifikation, Queue-Policies, Slot-Modell, Feasibility und
//! Ueberlaststeuerung.
//!
//! ## Invarianten dieses Crates
//!
//! * **Kein I/O.** Kein Netzwerk, kein Dateisystem, kein Triton, keine Uhr.
//!   Der Scheduler ruft niemals selbst `now` ab; die aktuelle Zeit wird an
//!   jedem Eintrittspunkt uebergeben. Genau das macht einen Live-Trace im
//!   Simulator exakt reproduzierbar (Spec 30.1, 30.2).
//! * **Keine Dependencies.** Der Kern haengt von nichts ausser `core`/`std` ab.
//! * **Keine Payload.** Tensoren werden nie beruehrt, nur als [`ids::PayloadRef`]
//!   weitergereicht (ADR-0003).
//! * **Keine unbeschraenkten Puffer.** Jede Queue hat eine explizite Kapazitaet
//!   (Spec L-003).
//! * **Geprueft rechnen.** Zeitarithmetik ist `checked` oder saettigend; ein
//!   Ueberlauf ist ein Eingabefehler, kein Wraparound (Spec G-012).
//!
//! ## Abweichungen von der Spezifikation
//!
//! Dieses Crate folgt zusaetzlich den Entscheidungen in `docs/adr/`. Relevant
//! fuer den Kern sind insbesondere ADR-0002 (Backend In-Flight Control),
//! ADR-0004 (Execution Slots) und ADR-0005 (Erfolgsmetrik).

pub mod arrayvec;
pub mod arrival;
pub mod contract_ext;
pub mod dag;
pub mod estimator;
pub mod feasibility;
pub mod generative;
pub mod hints;
pub mod ids;
pub mod interference;
pub mod learning;
pub mod metrics;
pub mod model;
pub mod overload;
pub mod predictor;
pub mod profile;
pub mod queue;
pub mod request;
pub mod runtime_budget;
pub mod scheduler;
pub mod semantics;
pub mod slots;
pub mod time;
pub mod variant;

pub use estimator::{MarginController, ProfileHealth, RuntimeEstimator};
pub use feasibility::{ExpectedArrival, Feasibility, GuardVerdict};
pub use ids::{ModelIdx, PayloadRef, RequestId, SlotIdx, SupersessionKey, VariantIdx};
pub use metrics::Metrics;
pub use model::{ContractError, ModelContract, Quality, QualitySource, QualityValue, Variant};
pub use overload::{OverloadConfig, OverloadController, OverloadState, PressureSample};
pub use profile::{ProfileError, RuntimeProfile, SafetyMargin, VariantProfile};
pub use queue::{DropReason, Eviction, ModelQueue, QueueConfig, QueueConfigError};
pub use request::{Criticality, OverflowPolicy, QueuePolicy, RequestDescriptor, RequestState};
pub use scheduler::{Action, ActionSink, Event, Scheduler, SchedulerError};
pub use slots::{InFlight, ModelMask, SlotError, SlotSet};
pub use time::{Duration, Instant, Slack};
pub use variant::{Resolution, VariantSelection, VariantState};
