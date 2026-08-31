//! Open Inference Protocol v2 — Typen und OneTimer-Parameterabbildung.
//!
//! Zwei Aufgaben:
//!
//! 1. Die generierten Wire-Typen bereitstellen, damit ein Standardclient ohne
//!    kundenspezifisches SDK ueber OneTimer inferieren kann (Spec L-001).
//! 2. Die OneTimer-Erweiterungsparameter aus einem Request lesen und in einen
//!    [`onetimer_core::RequestDescriptor`] uebersetzen — beziehungsweise, wenn
//!    keine gesetzt sind, den Compatibility Mode aus Spec 16.3 anwenden.

pub mod inference;
pub mod params;

pub use params::{ExtractError, OneTimerParams, PARAM_PREFIX};
