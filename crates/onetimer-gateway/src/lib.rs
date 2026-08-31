//! Das OneTimer-Gateway (Spec WP7, WP9).
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
//! * [`service`] — der gRPC-Dienst; uebersetzt zwischen Draht und Kern.
//! * [`outcome`] — die Uebersetzung terminaler Zustaende in gRPC-Antworten.
//! * [`shm`] — Buchfuehrung ueber die durchgereichten Shared-Memory-Regionen.

pub mod actor;
pub mod clock;
pub mod outcome;
pub mod service;
pub mod shm;

pub use actor::{Handle, Msg};
pub use clock::MonotonicClock;
pub use service::GatewayService;
pub use shm::{Region, ShmRegistry};
