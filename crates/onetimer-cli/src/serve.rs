//! `onetimer serve` — startet den Governor.

#![allow(clippy::print_stdout)]

use onetimer_backend_triton::{
    CONNECTION_WINDOW_BYTES, DEFAULT_MAX_MESSAGE_BYTES, STREAM_WINDOW_BYTES, TritonClient,
};
use onetimer_config::Config;
use onetimer_gateway::{GatewayService, MonotonicClock, actor};
use onetimer_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use tonic::transport::Server;

/// Startet den Gateway und laeuft, bis ein Abbruchsignal kommt.
///
/// # Errors
///
/// Wenn die Konfiguration ungueltig ist oder die Adresse nicht gebunden werden
/// kann. Eine ungueltige Konfiguration startet den Prozess **nicht** mit
/// Defaults (Spec L-020).
pub(crate) async fn run(
    path: &Path,
    listen: &str,
    metrics: &str,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let config = Config::from_yaml(&text)?;

    let findings = config.diagnose();
    if !findings.is_empty() {
        for finding in &findings {
            tracing::error!("{finding}");
        }
        return Err("die Konfiguration ist ungueltig; `onetimer doctor` zeigt die Details".into());
    }
    let resolved = Arc::new(config.resolve()?);

    let clock = MonotonicClock::start();
    let backend = Arc::new(TritonClient::new(&resolved.backend_endpoint));
    // Was der Server kann, wird abgefragt und nicht angenommen. Insbesondere
    // der Datenpfad haengt daran: ohne Shared Memory kostet ein grosses Bild
    // ein Vielfaches (`docs/benchmark/data-plane.md`).
    if let Ok(mut raw) = backend.raw().await
        && let Ok(response) = raw
            .server_metadata(onetimer_protocol_oip::inference::ServerMetadataRequest {})
            .await
    {
        let meta = response.into_inner();
        let caps = onetimer_backend_triton::Capabilities::from_metadata(&meta);
        tracing::info!(
            server = %meta.name,
            version = %meta.version,
            shared_memory = caps.can_pass_references(),
            sequence = caps.has(onetimer_backend_triton::Extension::Sequence),
            "Backend erkannt"
        );
        if !caps.can_pass_references() {
            tracing::warn!(
                "Der Server meldet kein Shared Memory. Grosse Tensoren laufen ueber \
                 den Kopierpfad; gemessen kostet ein 6,2-MB-Bild dort +11,7 ms statt \
                 +160 us."
            );
        }
    }

    // G-010: bevor irgendetwas geplant wird, pruefen, ob die hinterlegten
    // Profile ueberhaupt noch zur laufenden Umgebung gehoeren.
    let checked = crate::verify::check(&resolved).await;
    for entry in &checked {
        match &entry.trust {
            crate::verify::Trust::Mismatch { declared, actual } => tracing::warn!(
                model = %entry.logical,
                backend_model = %entry.physical,
                %declared,
                %actual,
                "Profil gehoert zu einer anderen Umgebung; wird vorsichtiger geplant, \
                 bis eigene Messungen vorliegen (G-010). Neu profilieren mit \
                 `onetimer profile`."
            ),
            crate::verify::Trust::Missing => tracing::warn!(
                model = %entry.logical,
                backend_model = %entry.physical,
                "Profil ohne Fingerabdruck; es konnte nicht geprueft werden (G-010)."
            ),
            crate::verify::Trust::Unavailable(reason) => tracing::warn!(
                model = %entry.logical,
                %reason,
                "Profil konnte nicht geprueft werden (G-010)."
            ),
            crate::verify::Trust::Verified => {}
        }
    }
    let unverified = crate::verify::unverified_models(&checked);
    let handle = actor::spawn(Arc::clone(&resolved), &backend, clock, &unverified)?;
    let service = GatewayService::new(Arc::clone(&resolved), backend, handle.clone(), clock);

    // Der Metrik-Endpunkt laeuft auf einem eigenen Port und in einem eigenen
    // Task: er darf den Inferenzpfad weder blockieren noch mit ihm um
    // Verbindungen konkurrieren. Faellt er aus, laeuft der Governor weiter —
    // umgekehrt waere es falsch herum.
    let metrics_address = metrics.parse()?;
    let metrics_handle = handle.clone();
    tokio::spawn(async move {
        if let Err(error) = onetimer_gateway::exporter::serve(metrics_handle, metrics_address).await
        {
            tracing::error!(%error, "Metrik-Endpunkt beendet");
        }
    });
    tracing::info!(%metrics_address, "Metriken unter /metrics");

    let address = listen.parse()?;
    tracing::info!(
        %address,
        backend = %resolved.backend_endpoint,
        models = resolved.model_names.len(),
        slots = resolved.slots.len(),
        "OneTimer laeuft"
    );

    // Dieselben Transportgrenzen wie zum Backend. tonics Voreinstellungen sind
    // fuer Steuernachrichten gedacht: 4 MiB Nachrichtengrenze lehnt einen
    // gewoehnlichen Kameraframe ab, und das 64-KiB-HTTP/2-Fenster zwingt bei
    // Tensornutzlasten zu einer Kette von WINDOW_UPDATE-Runden.
    Server::builder()
        .initial_stream_window_size(STREAM_WINDOW_BYTES)
        .initial_connection_window_size(CONNECTION_WINDOW_BYTES)
        .add_service(
            GrpcInferenceServiceServer::new(service)
                .max_decoding_message_size(DEFAULT_MAX_MESSAGE_BYTES)
                .max_encoding_message_size(DEFAULT_MAX_MESSAGE_BYTES),
        )
        .serve_with_shutdown(address, async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("Abbruchsignal empfangen, fahre herunter");
        })
        .await?;

    Ok(ExitCode::SUCCESS)
}
