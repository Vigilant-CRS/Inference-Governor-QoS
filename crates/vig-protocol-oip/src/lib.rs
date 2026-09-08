//! Open Inference Protocol v2 — Typen und Governor-Parameterabbildung.
//!
//! Zwei Aufgaben:
//!
//! 1. Die generierten Wire-Typen bereitstellen, damit ein Standardclient ohne
//!    kundenspezifisches SDK ueber Vigilant inferieren kann (Spec L-001).
//! 2. Die Governor-Erweiterungsparameter aus einem Request lesen und in einen
//!    [`vig_core::RequestDescriptor`] uebersetzen — beziehungsweise, wenn
//!    keine gesetzt sind, den Compatibility Mode aus Spec 16.3 anwenden.

pub mod inference;
pub mod params;

pub use params::{ExtractError, PARAM_PREFIX, VigParams};
