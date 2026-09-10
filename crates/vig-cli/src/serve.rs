//! `vig serve` — startet den Governor.

#![allow(clippy::print_stdout)]

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use tonic::transport::Server;
use vig_backend_triton::{
    CONNECTION_WINDOW_BYTES, DEFAULT_MAX_MESSAGE_BYTES, STREAM_WINDOW_BYTES, TritonClient,
};
use vig_config::Config;
use vig_gateway::{GatewayService, MonotonicClock, actor};
use vig_platform::{
    Actuation, ActuationPolicy, ClockMhz, Collector as _, NvidiaSmi, NvidiaSmiActuator,
};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;

/// Startet den Gateway und laeuft, bis ein Abbruchsignal kommt.
///
/// # Errors
///
/// Wenn die Konfiguration ungueltig ist oder die Adresse nicht gebunden werden
/// kann. Eine ungueltige Konfiguration startet den Prozess **nicht** mit
/// Defaults (Spec L-020).
// Der Startablauf ist eine Kette von Bedingungen, die alle erfuellt sein
// muessen, bevor der erste Request angenommen wird: Konfiguration, Backend,
// Profile, Signaturen, Zugang, Transport. Sie aufzuteilen versteckte genau
// die Reihenfolge, auf die es ankommt.
#[expect(clippy::too_many_lines, reason = "eine zusammenhaengende Startkette")]
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
        return Err("die Konfiguration ist ungueltig; `vig doctor` zeigt die Details".into());
    }
    // Bewusst noch nicht in einen `Arc`: die Startpruefungen duerfen
    // Konsequenzen haben, und eine Konsequenz, die den Vertrag nicht mehr
    // aendern kann, ist eine Warnung.
    let mut resolved = config.resolve()?;

    // NV-13: der Taktregler, falls der Betreiber ihn eingeschaltet hat. Vor
    // dieser Zeile war er gebaut, getestet und durch keine Konfiguration
    // erreichbar (Review R09). Er bleibt bis zum Ende dieser Funktion am
    // Leben: sein `Drop` gibt den Takt zurueck.
    let mut actuation = start_actuation(resolved.actuation.as_ref());

    let clock = MonotonicClock::start();
    let backend = Arc::new(TritonClient::new(&resolved.backend_endpoint));
    report_backend_capabilities(&backend).await;

    let unverified = match startup_checks(&mut resolved).await {
        Ok(u) => u,
        Err(violations) => {
            return Err(format!(
                "{} Variante(n) erfuellen die zugesagte io_signature nicht; \
                 die Details stehen im Protokoll",
                violations.len()
            )
            .into());
        }
    };

    let resolved = Arc::new(resolved);
    let handle = actor::spawn(Arc::clone(&resolved), &backend, clock, &unverified)?;
    let mut service = GatewayService::new(Arc::clone(&resolved), backend, handle.clone(), clock);

    // Zugangspruefung: die Dateien werden **hier** geladen, nicht im Dienst.
    // Ein Ladefehler soll den Start verhindern, bevor irgendetwas lauscht —
    // nicht bei der ersten Anfrage auffallen.
    if let Some(path) = &resolved.security.token_file {
        let tokens = vig_gateway::auth::Tokens::load(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        tracing::info!(tokens = tokens.len(), "Bearer-Token-Pruefung aktiv");
        // Die Kennungen, die diese Token belegen (NV-18). Der Betreiber
        // braucht die Zahl fuer `backend.hints.authority`; ohne sie waere die
        // Hinweispolicy zwar konfigurierbar, aber nicht ausfuellbar. Die
        // Token selbst stehen hier nicht.
        for authority in tokens.authorities() {
            tracing::info!(
                authority = authority.0,
                "Hinweis-Kennung eines hinterlegten Tokens"
            );
        }
        service = service.with_tokens(tokens);
    }

    // Der Metrik-Endpunkt laeuft auf einem eigenen Port und in einem eigenen
    // Task: er darf den Inferenzpfad weder blockieren noch mit ihm um
    // Verbindungen konkurrieren. Faellt er aus, laeuft der Governor weiter —
    // umgekehrt waere es falsch herum.
    let metrics_address = metrics.parse()?;
    let metrics_handle = handle.clone();
    tokio::spawn(async move {
        if let Err(error) = vig_gateway::exporter::serve(metrics_handle, metrics_address).await {
            tracing::error!(%error, "Metrik-Endpunkt beendet");
        }
    });
    tracing::info!(%metrics_address, "Metriken unter /metrics");

    // Transportsicherung. Ein halb konfiguriertes TLS lehnt bereits die
    // Konfigurationspruefung ab; hier gibt es nur noch an oder aus.
    let tls = tls_config(&resolved.security)?;
    let address: std::net::SocketAddr = listen.parse()?;
    let exposed = !address.ip().is_loopback();
    match (&tls, exposed) {
        (Some(_), _) => tracing::info!(mtls = resolved.security.client_ca.is_some(), "TLS aktiv"),
        (None, true) => tracing::warn!(
            %address,
            "Der Endpunkt lauscht ausserhalb von Loopback **ohne TLS**. Jeder im \
             Netz kann damit die Steuerung der GPU uebernehmen. Entweder \
             `backend.security` einrichten oder eine authentifizierende \
             Instanz davorstellen."
        ),
        (None, false) => {}
    }
    tracing::info!(
        %address,
        backend = %resolved.backend_endpoint,
        models = resolved.model_names.len(),
        slots = resolved.slots.len(),
        "Vigilant laeuft"
    );

    // Dieselben Transportgrenzen wie zum Backend. tonics Voreinstellungen sind
    // fuer Steuernachrichten gedacht: 4 MiB Nachrichtengrenze lehnt einen
    // gewoehnlichen Kameraframe ab, und das 64-KiB-HTTP/2-Fenster zwingt bei
    // Tensornutzlasten zu einer Kette von WINDOW_UPDATE-Runden.
    let mut builder = Server::builder();
    if let Some(tls) = tls {
        builder = builder.tls_config(tls)?;
    }
    // Die Frist laeuft ab dem Signal, nicht ab dem Ende des Servers. Sonst
    // stuende vor der 20-Sekunden-Frist ein unbegrenztes Warten darauf, dass
    // tonic alle Verbindungen schliesst — und die zugesagte Gesamtfrist waere
    // keine.
    let (signalled_tx, signalled_rx) = tokio::sync::oneshot::channel();
    let shutdown = async move {
        shutdown_signal().await;
        let _ = signalled_tx.send(std::time::Instant::now());
    };

    // Der Server laeuft in einem eigenen Task, damit sein Ende **begrenzbar**
    // ist. Ihn direkt abzuwarten hiesse: schliesst tonic seine Verbindungen
    // nicht, steht davor ein unbegrenztes Warten, und die zugesagte
    // Gesamtfrist ab SIGTERM waere keine.
    let mut server = tokio::spawn(
        builder
            .initial_stream_window_size(STREAM_WINDOW_BYTES)
            .initial_connection_window_size(CONNECTION_WINDOW_BYTES)
            .add_service(
                GrpcInferenceServiceServer::new(service)
                    .max_decoding_message_size(DEFAULT_MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(DEFAULT_MAX_MESSAGE_BYTES),
            )
            .serve_with_shutdown(address, shutdown),
    );

    // Entweder der Server endet von selbst — dann ist etwas kaputt und die
    // Frist irrelevant — oder das Signal kommt und die Uhr laeuft.
    let signalled = tokio::select! {
        finished = &mut server => {
            finished??;
            tracing::warn!("der Server endete ohne Abbruchsignal");
            return Ok(ExitCode::FAILURE);
        }
        at = signalled_rx => at.ok(),
    };
    let elapsed = || signalled.map_or(std::time::Duration::ZERO, |at| at.elapsed());

    // Ab hier ist alles begrenzt. Zuerst der Server: laufende Anfragen sollen
    // zu Ende gehen, aber nicht unbegrenzt.
    let closing = DRAIN_DEADLINE.saturating_sub(elapsed());
    if tokio::time::timeout(closing, &mut server).await.is_err() {
        tracing::warn!(
            "der gRPC-Server hat innerhalb der Frist nicht geschlossen; er wird \
             abgebrochen"
        );
        server.abort();
    }

    // Was von der Frist uebrig ist, gehoert dem Drain.
    let remaining = DRAIN_DEADLINE.saturating_sub(elapsed());

    // Der Server nimmt nichts Neues mehr an. Jetzt die angenommene Arbeit zu
    // Ende bringen: wartende Requests beantworten, laufende Backendaufrufe
    // auslaufen lassen. Ein Abbruch mitten in einer Inferenz liesse den
    // Client ohne Antwort und die GPU trotzdem rechnen.
    tracing::info!(
        remaining_ms = remaining.as_millis(),
        "kein neuer Verkehr; laufende Arbeit wird abgeschlossen"
    );
    // Der Takt gehoert zurueck, bevor der Prozess endet — und zwar auch
    // dann, wenn der Drain scheitert.
    if let Some(actuation) = actuation.as_mut() {
        match actuation.restore() {
            Ok(()) => tracing::info!("GPU-Takt zurueckgegeben"),
            Err(error) => tracing::warn!(%error, "der GPU-Takt liess sich nicht zurueckgeben"),
        }
    }

    match handle.drain(remaining).await {
        Ok(true) => {
            tracing::info!("alle Requests beantwortet, Governor beendet");
            Ok(ExitCode::SUCCESS)
        }
        Ok(false) => {
            // Ehrlich melden statt still beenden: ein Exitcode ungleich null
            // ist die einzige Spur, die im Orchestrator uebrig bleibt.
            tracing::warn!(
                "Drain-Frist abgelaufen, waehrend noch Arbeit offen war; \
                 wahrscheinlich antwortet das Backend nicht"
            );
            Ok(ExitCode::FAILURE)
        }
        Err(error) => {
            tracing::error!(%error, "der Scheduler war beim Herunterfahren nicht erreichbar");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Wie lange **ab dem Signal** insgesamt heruntergefahren werden darf.
///
/// Kubernetes gibt einem Container per Voreinstellung 30 s zwischen SIGTERM
/// und SIGKILL. Die Frist liegt bewusst darunter, damit der Governor sein
/// Ende selbst protokolliert, statt getoetet zu werden.
const DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Wartet auf SIGTERM oder SIGINT.
///
/// SIGTERM ist im Container der Normalfall — `docker stop`, ein
/// Kubernetes-Pod-Ende und systemd senden es. Nur auf Ctrl-C zu hoeren hiess:
/// im Betrieb wird der Governor immer hart getoetet, und jede Drain-Logik ist
/// wirkungslos.
/// Schaltet den Taktregler ein, wenn der Betreiber ihn konfiguriert hat
/// (NV-13, ADR-0030).
///
/// Gibt `None` zurueck, wenn keine Aktuation konfiguriert ist — der
/// Normalfall — oder wenn die Stellbefugnis nicht zu bekommen war. Beides ist
/// **kein** Startfehler: ein Governor ohne Stellbefugnis ist ein
/// funktionierender Governor, und ADR-0021 bleibt die Regel.
///
/// Was hier passiert, ist ein Betriebspunkt und keine Regelung: der Takt wird
/// einmal angefordert und beim Beenden zurueckgegeben. Wann eine Anhebung
/// sich waehrend des Betriebs lohnt, ist eine Messfrage und braucht eine
/// Installation, auf der das Stellen ueberhaupt erlaubt ist.
fn start_actuation(config: Option<&vig_config::schema::ActuationConfig>) -> Option<Actuation> {
    let config = config?;
    let policy = ActuationPolicy {
        platform_min: ClockMhz(config.platform_min_mhz),
        platform_max: ClockMhz(config.platform_max_mhz),
        promised_floor: ClockMhz(config.promised_floor_mhz),
        dwell_ms: config.dwell_ms,
        tolerance_mhz: config.tolerance_mhz,
        settle_ms: config.settle_ms,
    };
    let mut actuation = Actuation::disabled(policy);

    let actuator = match NvidiaSmiActuator::acquire(config.gpu_index) {
        Ok(actuator) => actuator,
        Err(error) => {
            tracing::warn!(
                %error,
                "keine Stellbefugnis fuer die GPU; der Governor laeuft ohne \
                 Aktuation weiter"
            );
            return None;
        }
    };
    actuation.enable(Box::new(actuator));

    let gpu = config.gpu_index;
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    };
    // Frisch beobachten, nicht auf einen Zustand von vorhin schauen: die
    // Bestaetigung soll die Karte **jetzt** zeigen.
    let observe = || NvidiaSmi::default().snapshot().ok();
    match actuation.request(ClockMhz(config.hold_mhz), now_ms(), &observe, gpu) {
        Ok(confirmed) => tracing::info!(
            requested = config.hold_mhz,
            confirmed = confirmed.0,
            "GPU-Takt gestellt und beobachtet"
        ),
        Err(error) => tracing::warn!(
            %error,
            requested = config.hold_mhz,
            "der GPU-Takt liess sich nicht bestaetigen; geplant wird weiter \
             mit dem beobachteten Zustand"
        ),
    }
    Some(actuation)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(error) => {
                tracing::error!(%error, "SIGTERM nicht abonnierbar; nur Ctrl-C wirkt");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM empfangen"),
            _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT empfangen"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("Abbruchsignal empfangen");
    }
}

/// Die Pruefungen, die vor dem ersten Dispatch stattfinden.
///
/// Gibt die Modelle zurueck, deren Profil nachweislich nicht mehr gilt. Zieht
/// nebenbei die Konsequenz aus nicht austauschbaren Varianten — eine Pruefung,
/// die nur meldet, aber nichts aendert, ist eine Meinung.
async fn startup_checks(
    resolved: &mut vig_config::schema::Resolved,
) -> Result<Vec<vig_core::ModelIdx>, Vec<crate::verify::ContractViolation>> {
    // G-010: bevor irgendetwas geplant wird, pruefen, ob die hinterlegten
    // Profile ueberhaupt noch zur laufenden Umgebung gehoeren.
    let checked = crate::verify::check(resolved).await;
    for entry in &checked {
        match &entry.trust {
            crate::verify::Trust::Mismatch { declared, actual } => tracing::warn!(
                model = %entry.logical,
                backend_model = %entry.physical,
                %declared,
                %actual,
                "Profil gehoert zu einer anderen Umgebung; wird vorsichtiger geplant, \
                 bis eigene Messungen vorliegen (G-010). Neu profilieren mit \
                 `vig profile`."
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

        // NV-03: was der Metadaten-Fingerabdruck nicht sehen kann.
        let Some(comparison) = entry.manifest.as_ref() else {
            continue;
        };
        for field in comparison.divergences() {
            if let vig_config::manifest::FieldVerdict::Divergent { declared, actual } =
                &field.verdict
            {
                tracing::warn!(
                    model = %entry.logical,
                    backend_model = %entry.physical,
                    field = %field.field,
                    %declared,
                    %actual,
                    identity_bearing = field.identity_bearing,
                    "Profilmanifest weicht von der laufenden Umgebung ab (NV-03)."
                );
            }
        }
    }
    let unverified = crate::verify::unverified_models(&checked);

    // Eine zugesagte Schnittstelle ist eine Zusage des Betreibers, keine
    // Vermutung des Werkzeugs. Wird sie verletzt, ist die Konfiguration
    // falsch — und der Governor startet nicht. Ein Client, der sich auf die
    // Zusage verlaesst, bekaeme sonst nach einem Variantenwechsel einen Tensor
    // mit anderer Bedeutung, und das faellt erst im Feld auf.
    let violations = crate::verify::contract_violations(resolved).await;
    if !violations.is_empty() {
        for v in &violations {
            tracing::error!(
                model = %v.logical,
                variant = %v.physical,
                declared = %v.declared.describe(),
                actual = %v.actual.describe(),
                "Variante erfuellt die zugesagte io_signature nicht"
            );
        }
        return Err(violations);
    }

    // Austauschbarkeit der Varianten: der Governor waehlt sie je Request und
    // sagt es dem Client nicht. Unterscheiden sich die Schnittstellen, ist
    // diese Freiheit ein Fehler mit Ansage — sie wird deshalb abgeschaltet,
    // nicht nur bemaengelt. Das Modell laeuft weiter, aber nur auf seiner
    // besten Variante.
    for conflict in crate::verify::signature_conflicts(resolved).await {
        tracing::warn!(
            model = %conflict.logical,
            reference = %conflict.reference.0,
            reference_signature = %conflict.reference.1.describe(),
            divergent = %conflict.divergent.0,
            divergent_signature = %conflict.divergent.1.describe(),
            "Varianten haben verschiedene I/O-Signaturen; die automatische \
             Variantenwahl ist fuer dieses Modell abgeschaltet"
        );
        if let Some(contract) = resolved.contracts.get_mut(conflict.model.get()) {
            contract.variants_interchangeable = false;
        }
    }

    Ok(unverified)
}

/// Baut die TLS-Konfiguration des Servers, wenn eine hinterlegt ist.
///
/// `None` heisst Klartext. Das ist fuer Loopback die richtige Voreinstellung —
/// TLS gegen sich selbst schuetzt nichts und kostet bei grossen Tensoren
/// messbar Zeit. Fuer alles andere warnt `serve` beim Start und `doctor` schon
/// davor.
///
/// # Errors
///
/// Wenn eine der Dateien nicht lesbar ist.
fn tls_config(
    security: &vig_config::schema::SecurityConfig,
) -> Result<Option<tonic::transport::ServerTlsConfig>, Box<dyn std::error::Error>> {
    let (Some(cert_path), Some(key_path)) = (&security.tls_cert, &security.tls_key) else {
        return Ok(None);
    };
    let cert = std::fs::read(cert_path).map_err(|e| format!("{}: {e}", cert_path.display()))?;
    let key = std::fs::read(key_path).map_err(|e| format!("{}: {e}", key_path.display()))?;
    let identity = tonic::transport::Identity::from_pem(cert, key);
    let mut config = tonic::transport::ServerTlsConfig::new().identity(identity);

    // Mit einer Client-CA wird aus TLS mTLS: ohne gueltiges Clientzertifikat
    // kommt keine Verbindung zustande. Das ist die belastbare Variante — ein
    // Token reist in jeder Anfrage mit und kann kopiert werden, ein privater
    // Schluessel nicht.
    if let Some(ca_path) = &security.client_ca {
        let ca = std::fs::read(ca_path).map_err(|e| format!("{}: {e}", ca_path.display()))?;
        config = config.client_ca_root(tonic::transport::Certificate::from_pem(ca));
    }
    Ok(Some(config))
}

/// Fragt ab, was der Server kann, statt es anzunehmen.
///
/// Insbesondere der Datenpfad haengt daran: ohne Shared Memory kostet ein
/// grosses Bild ein Vielfaches (`docs/benchmark/data-plane.md`).
async fn report_backend_capabilities(backend: &Arc<TritonClient>) {
    if let Ok(mut raw) = backend.raw().await
        && let Ok(response) = raw
            .server_metadata(vig_protocol_oip::inference::ServerMetadataRequest {})
            .await
    {
        let meta = response.into_inner();
        let caps = vig_backend_triton::Capabilities::from_metadata(&meta);
        tracing::info!(
            server = %meta.name,
            version = %meta.version,
            shared_memory = caps.can_pass_references(),
            sequence = caps.has(vig_backend_triton::Extension::Sequence),
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
}
