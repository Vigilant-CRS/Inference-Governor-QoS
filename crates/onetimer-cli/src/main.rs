//! Das OneTimer-Kommandozeilenwerkzeug.
//!
//! Der Nutzerworkflow aus Spec 6.3:
//!
//! ```text
//! onetimer doctor -c onetimer.yaml    # prueft Konfiguration und Backend
//! onetimer serve  -c onetimer.yaml    # startet den Governor
//! ```

// Ein CLI ist genau die Stelle, an der auf die Standardausgabe geschrieben
// werden muss. Der Workspace verbietet das sonst, damit keine Bibliothek
// heimlich druckt.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod doctor;
mod profile;
mod serve;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

/// Vigilant OneTimer — adaptive inference governor for edge AI.
#[derive(Debug, Parser)]
#[command(name = "onetimer", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Prueft Konfiguration und Backend, ohne etwas zu starten.
    Doctor {
        /// Pfad zur Konfigurationsdatei.
        #[arg(short, long, value_name = "DATEI")]
        config: PathBuf,
        /// Nur die Datei pruefen, das Backend nicht kontaktieren.
        #[arg(long)]
        offline: bool,
    },
    /// Misst Laufzeitprofile am Backend und gibt sie einfuegefertig aus.
    Profile {
        /// Pfad zur Konfigurationsdatei.
        #[arg(short, long, value_name = "DATEI")]
        config: PathBuf,
        /// Anzahl der Messlaeufe je Variante.
        #[arg(long, default_value_t = profile::DEFAULT_SAMPLES)]
        samples: usize,
    },
    /// Startet den Governor.
    Serve {
        /// Pfad zur Konfigurationsdatei.
        #[arg(short, long, value_name = "DATEI")]
        config: PathBuf,
        /// Adresse, auf der das Gateway lauscht.
        #[arg(short, long, default_value = "0.0.0.0:9001")]
        listen: String,
        /// Adresse fuer den Prometheus-Endpunkt (`/metrics`, `/healthz`).
        #[arg(long, default_value = "0.0.0.0:9090")]
        metrics: String,
    },
}

/// Stackgroesse der Tokio-Worker.
///
/// Der Default von 2 MiB reicht im Release-Build, laesst aber keinen Spielraum:
/// die Aufrufkette durch tonic, hyper und tower ist tief, und ein
/// Stack-Ueberlauf bricht den Prozess ohne verwertbare Fehlermeldung ab. Fuer
/// eine dauerhaft laufende Infrastrukturkomponente ist das der falsche
/// Ausfallmodus.
const WORKER_STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(WORKER_STACK_SIZE)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("FEHLER Tokio-Runtime konnte nicht gestartet werden: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run())
}

async fn run() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    // Die generierten gRPC-Typen sind gross; ohne `Box::pin` landet ein
    // 90-KB-Future auf dem Stack des Aufrufers.
    let result = match cli.command {
        Command::Doctor { config, offline } => Box::pin(doctor::run(&config, offline)).await,
        Command::Profile { config, samples } => Box::pin(profile::run(&config, samples)).await,
        Command::Serve {
            config,
            listen,
            metrics,
        } => Box::pin(serve::run(&config, &listen, &metrics)).await,
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("FEHLER {error}");
            ExitCode::FAILURE
        }
    }
}
