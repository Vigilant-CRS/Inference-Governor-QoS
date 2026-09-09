//! Das Governor-Gateway (Spec WP7, WP9).
//!
//! Nach aussen ein gewoehnlicher Open-Inference-Protocol-Server: ein
//! Standardclient aendert nur den Zielendpunkt und inferiert weiter
//! (Spec L-001, L-002). Nach innen sitzt der Scheduling-Kern dazwischen und
//! entscheidet, was wann und mit welcher Variante laeuft.
//!
//! ## Aufteilung
//!
//! * [`clock`] — die einzige Stelle, an der eine echte Uhr abgelesen wird.
//! * [`actor`] — der Single-Owner-Scheduler-Task.
//! * [`executor`] — die Naht zum Backend; Triton ist der erste Executor.
//! * [`testing`] — ein Backend, das ohne GPU antwortet.
//! * [`service`] — der gRPC-Dienst; uebersetzt zwischen Draht und Kern.
//! * [`outcome`] — die Uebersetzung terminaler Zustaende in gRPC-Antworten.
//! * [`exporter`] — der Prometheus-Endpunkt.
//! * [`shm`] — Buchfuehrung ueber die durchgereichten Shared-Memory-Regionen.

pub mod actor;
pub mod auth;
pub mod clock;
pub mod cooperative;
pub mod executor;
pub mod exporter;
pub mod outcome;
pub mod service;
pub mod shm;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use actor::{Handle, Msg};
pub use clock::MonotonicClock;
pub use executor::{Capabilities, Executor, Ticket, TritonExecutor};
pub use exporter::render as render_metrics;
pub use service::GatewayService;
pub use shm::{Region, ShmRegistry};
