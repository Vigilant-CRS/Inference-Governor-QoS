//! Der Metrik-Endpunkt haelt hoechstens eine feste Zahl Verbindungen
//! (Security-Review N4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::serve::Listener as _;
use std::time::Duration;
use vig_gateway::exporter::LimitedListener;

/// Ohne Grenze verbrauchen Leerlaufverbindungen die Dateideskriptoren des
/// Prozesses — und dann nimmt auch der Inferenzendpunkt nichts mehr an. Mit
/// Grenze wartet die naechste Verbindung im Backlog, bis eine endet.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_metrics_listener_holds_at_most_its_limit() {
    let inner = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = inner.local_addr().unwrap();
    let mut listener = LimitedListener::new(inner, 1);

    let _first_client = tokio::net::TcpStream::connect(address).await.unwrap();
    let (first, _) = listener.accept().await;

    let _second_client = tokio::net::TcpStream::connect(address).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err(),
        "die zweite Verbindung wartet, solange die erste gehalten wird"
    );

    drop(first);
    tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("nach dem Ende der ersten wird die zweite angenommen");
}
