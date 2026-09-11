//! # Vigilant Discrete-Event Simulator
//!
//! Testumgebung fuer den Scheduling-Kern ohne GPU, ohne Triton, ohne Netzwerk
//! (Spec WP1). Zwei Aufgaben:
//!
//! 1. **Verifikation.** Die Golden Tests aus Spec 28 und die Property-Tests der
//!    Scheduler-Invarianten laufen hier gegen eine virtuelle Uhr.
//! 2. **Falsifikation.** Gate S (ADR-0001) fuehrt die Kernvergleiche A und B
//!    aus Spec 19.5/19.6 vollstaendig simuliert aus, bevor Netzwerkarbeit
//!    beginnt.
//!
//! Der Simulator ist die **Best-Case-Welt** fuer Vigilant: perfekte
//! Laufzeitkenntnis, kein Proxy-Overhead, keine zweite Backend-Queue, kein
//! Profilfehler. Ein Effekt, der hier nicht gross ist, kann real nur kleiner
//! werden. Deshalb kann Gate S die Produkthypothese falsifizieren, aber nicht
//! bestaetigen — Milestone M3 gegen eine getunte Triton-Baseline bleibt das
//! entscheidende Gate.

// Der Simulator ist ausdruecklich nicht der Hot Path. Er rechnet in `f64`,
// zieht Verteilungen und aggregiert Statistiken; die strengen Arithmetiklints
// des Workspace waeren hier reines Rauschen. Die Invarianten, die sie schuetzen
// sollen, gelten fuer `vig-core` und bleiben dort in Kraft.
// Die Casts sind durchweg durch vorangehende `clamp`-Aufrufe gedeckt: die
// Werte liegen konstruktionsbedingt in einem nichtnegativen, darstellbaren
// Bereich.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::integer_division
)]

pub mod baseline;
pub mod bounded;
pub mod coverage;
pub mod event;
pub mod harness;
pub mod rng;
pub mod scenario;
pub mod trace;
pub mod workload;

pub use coverage::{Coverage, CoverageTracker};
pub use event::{ScheduleError, SimClock, SimEvent};
pub use rng::Pcg32;
pub use trace::TraceDigest;
pub use workload::{PeriodicStream, RuntimeDistribution};
