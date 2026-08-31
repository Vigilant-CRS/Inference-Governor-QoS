//! `onetimer serve` — startet den Governor.

#![allow(clippy::print_stdout)]

use onetimer_backend_triton::TritonClient;
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
pub(crate) async fn run(path: &Path, listen: &str) -> Result<ExitCode, Box<dyn std::error::Error>> {
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
    let handle = actor::spawn(Arc::clone(&resolved), Arc::clone(&backend), clock)?;
    let service = GatewayService::new(Arc::clone(&resolved), backend, handle, clock);

    let address = listen.parse()?;
    tracing::info!(
        %address,
        backend = %resolved.backend_endpoint,
        models = resolved.model_names.len(),
        slots = resolved.slots.len(),
        "OneTimer laeuft"
    );

    Server::builder()
        .add_service(GrpcInferenceServiceServer::new(service))
        .serve_with_shutdown(address, async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("Abbruchsignal empfangen, fahre herunter");
        })
        .await?;

    Ok(ExitCode::SUCCESS)
}
