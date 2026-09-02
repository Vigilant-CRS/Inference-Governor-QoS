//! Der Triton-Backend-Adapter (Spec WP8).
//!
//! ## Was dieser Adapter nicht tut
//!
//! Er trifft **keine fachlichen Scheduling-Entscheidungen**. Er weiss nichts
//! von Frische, Deadlines, Varianten oder Ueberlast. Er fuehrt aus, was der
//! Scheduler entschieden hat, misst wie lange es gedauert hat, und meldet das
//! Ergebnis zurueck. Spec 8.4 verlangt genau diese Trennung: der Kern muss
//! ohne Triton testbar bleiben, und das geht nur, wenn keine Politik in den
//! Adapter sickert.
//!
//! ## Warum die Verbindung wiederhergestellt wird, aber der Request nicht
//!
//! Ein Verbindungsverlust wird transparent geheilt — der naechste Aufruf baut
//! neu auf. Ein **Request** wird dagegen nicht automatisch wiederholt: er ist
//! nach einem Fehlschlag meist schon zu alt, und ein blinder Retry wuerde
//! GPU-Zeit in ein bereits wertloses Ergebnis stecken. Ob wiederholt wird,
//! entscheidet der Scheduler anhand der Frische, nicht der Adapter
//! (Spec 30.4).

pub mod capabilities;
pub mod client;
pub mod error;
pub mod fingerprint;
pub mod request;

pub use capabilities::{Capabilities, Extension};
pub use fingerprint::fingerprint;
pub use request::zero_request;

pub use client::{
    CONNECTION_WINDOW_BYTES, DEFAULT_MAX_MESSAGE_BYTES, STREAM_WINDOW_BYTES, TritonClient,
    TritonHealth,
};
pub use error::BackendError;
