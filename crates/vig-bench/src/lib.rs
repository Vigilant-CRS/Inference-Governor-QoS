//! Wire-Level-Benchmark: bringt der Governor auf dem Draht etwas?
//!
//! Gate S beantwortet diese Frage im Simulator. Hier laeuft dieselbe Frage
//! durch den echten Stack: echter gRPC-Transport, echter Scheduler-Actor,
//! echte Nebenlaeufigkeit, echte Backend-Kapazitaetsgrenze.
//!
//! ## Was verglichen wird
//!
//! * **ohne Governor** — die Clients sprechen direkt mit dem Backend. Das ist
//!   FIFO: das Backend arbeitet ab, was ankommt.
//! * **mit Governor** — dieselben Clients, dieselbe Last, dieselben Frames,
//!   nur ueber Vigilant.
//!
//! Beide Laeufe teilen sich Backend, Kapazitaet, Laufzeitverteilung und
//! Frame-Nummern. Die Laufzeit eines Frames haengt an seiner Nummer und nicht
//! am Fortschritt eines Zufallsstroms — derselbe Frame kostet in beiden
//! Laeufen dasselbe, gleichgueltig in welcher Reihenfolge er ausgefuehrt wird.
//!
//! ## Was das nicht ist
//!
//! Kein Vergleich gegen Triton. Das Backend hier ist ein Modell mit
//! begrenzter Ausfuehrungskapazitaet, kein Inferenzserver. Der Vergleich gegen
//! einen **getunten** Triton auf echter Hardware ist Gate M3 und steht aus.

// Ein Benchmark, der eine kaputte Umgebung stillschweigend umgeht, misst
// etwas anderes als beabsichtigt. Ein fehlender Port oder eine ungueltige
// Szenariokonfiguration soll den Lauf abbrechen, nicht ein Ergebnis liefern,
// das zu keiner dokumentierten Konfiguration gehoert.
#![allow(clippy::expect_used)]

pub mod backend;
pub mod pilot;
pub mod service;
pub mod shm;
pub mod workload;

/// Eingang fuer einen Governor oder ein Mock-Backend im Prozess, mit
/// `TCP_NODELAY` wie `vig serve` und Triton.
///
/// `serve_with_incoming` uebergeht die Einstellung des Builders. Ohne sie
/// wartet eine Antwort gelegentlich auf das verzoegerte ACK des Clients, rund
/// 40 ms — ein Ausreisser, der dem Governor angelastet wuerde, aber nur dem
/// Messaufbau gehoert (docs/benchmark/arm-serve.md).
#[must_use]
pub fn incoming(listener: tokio::net::TcpListener) -> tonic::transport::server::TcpIncoming {
    tonic::transport::server::TcpIncoming::from(listener).with_nodelay(Some(true))
}

pub use backend::Backend;
pub use workload::{StreamDef, StreamReport, drive};
