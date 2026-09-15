//! Das Kommandozeilenwerkzeug des Governors.
//!
//! Der Nutzerworkflow aus Spec 6.3:
//!
//! ```text
//! vig doctor    -c vig.yaml    # prueft Konfiguration und Backend
//! vig calibrate -c vig.yaml    # misst die Hardware aus
//! vig serve     -c vig.yaml    # startet den Governor
//! ```
//!
//! Die Hilfetexte sind englisch, die Kommentare deutsch: das erste liest ein
//! Anwender, das zweite jemand, der hier weiterarbeitet.

// Ein CLI ist genau die Stelle, an der auf die Standardausgabe geschrieben
// werden muss. Der Workspace verbietet das sonst, damit keine Bibliothek
// heimlich druckt.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod artifact;
mod autotune;
mod calibrate;
mod doctor;
mod identity;
mod init;
mod profile;
mod runloop;
mod serve;
mod verify;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

/// Vigilant Inference Governor — inference QoS for edge robotics.
#[derive(Debug, Parser)]
#[command(name = "vig", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Qualify your own hardware: measure it, freeze the result, report it.
    ///
    /// Start it, and in half an hour it has measured your machine and tells
    /// you what it can carry. And if it turns out you do not need us, it says
    /// that too.
    ///
    /// It runs the existing steps in order — read the models from the backend,
    /// measure runtimes and interference, tune the governor's own settings
    /// for this load (never your contracts, never the slots), ask whether the
    /// tuned governor is worth it here, check the result — and writes a frozen
    /// configuration plus a report in Markdown and JSON.
    ///
    /// What it will not do: issue a release. A discarded measurement series
    /// stays discarded and its value stays unset; a run under foreign load is
    /// marked as such; nothing that was not measured is made to look measured.
    Autotune {
        /// Address of the inference server to measure against.
        ///
        /// Default: `backend.grpc_endpoint` of the configuration if it exists,
        /// otherwise 127.0.0.1:8001 for the draft. If both are given and
        /// differ, autotune refuses instead of reporting one server and
        /// measuring another.
        #[arg(short, long)]
        endpoint: Option<String>,
        /// The configuration with your contracts. Written as a draft if absent.
        #[arg(short, long, value_name = "FILE", default_value = "vig.yaml")]
        config: PathBuf,
        /// Directory for the frozen configuration and the report.
        #[arg(short, long, value_name = "DIR", default_value = "qualification")]
        out: PathBuf,
        /// Number of measurement runs per step.
        #[arg(long, default_value_t = profile::DEFAULT_SAMPLES)]
        samples: usize,
        /// Release period in microseconds. Without it, back to back.
        #[arg(long, value_name = "US")]
        periodic_us: Option<u64>,
        /// Measure a smaller matrix: faster, and less precise about it.
        #[arg(long)]
        quick: bool,
        /// Run a single step instead of all five.
        #[arg(long, value_enum, value_name = "STEP")]
        only: Option<autotune::Step>,
        /// Check the configuration only; do not contact the backend for it.
        #[arg(long)]
        offline: bool,
        /// Ignore a previous run's progress and start over.
        #[arg(long)]
        restart: bool,
        /// Where the numbers come from and what they are claimed valid for.
        #[command(flatten)]
        identity: identity::IdentityArgs,
    },
    /// Write a starting configuration from a running inference server.
    ///
    /// Fills in what the machine knows — models, tensor names, shapes, data
    /// types — and leaves what only you know as an explicit placeholder:
    /// period, deadline, maximum age, importance. The file deliberately does
    /// not load until you have filled those in.
    Init {
        /// Address of the inference server to read the models from.
        #[arg(short, long, default_value = "127.0.0.1:8001")]
        endpoint: String,
        /// Target file. Without one the template goes to stdout.
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Check the configuration and the backend without starting anything.
    Doctor {
        /// Path to the configuration file.
        #[arg(short, long, value_name = "FILE")]
        config: PathBuf,
        /// Check the file only; do not contact the backend.
        #[arg(long)]
        offline: bool,
    },
    /// Measure runtime profiles at the backend and print them ready to paste.
    Profile {
        /// Path to the configuration file.
        #[arg(short, long, value_name = "FILE")]
        config: PathBuf,
        /// Number of measurement runs per variant.
        #[arg(long, default_value_t = profile::DEFAULT_SAMPLES)]
        samples: usize,
        /// Release on an absolute grid with this period, in microseconds.
        ///
        /// This is the number that belongs to a contract. Without it, the tool
        /// measures back to back, which is a statement about capacity, not
        /// about behaviour under a cycle. An overrun never shifts the grid;
        /// the missed release points are counted.
        #[arg(long, value_name = "US")]
        periodic_us: Option<u64>,
        /// Where the numbers come from and what they are claimed valid for.
        #[command(flatten)]
        identity: identity::IdentityArgs,
    },
    /// Measure runtime **and** mutual interference, and write a complete
    /// configuration.
    ///
    /// Unlike `profile`, this also measures under concurrent load and finds
    /// model pairs that slow each other down too much. It never touches
    /// contracts: what has to be fresh is a requirement, not a measurement.
    Calibrate {
        /// Path to the configuration file.
        #[arg(short, long, value_name = "FILE")]
        config: PathBuf,
        /// Number of measurement runs per step.
        #[arg(long, default_value_t = profile::DEFAULT_SAMPLES)]
        samples: usize,
        /// Release period in microseconds. Without it, back to back.
        ///
        /// Same switch, same meaning, same measurement core as `profile`
        /// (review R08). Back to back is the weaker measurement: waiting one
        /// period after each answer measures less often when answers are
        /// slow, and so goes easy on the run exactly when it would get hard.
        #[arg(long, value_name = "US")]
        periodic_us: Option<u64>,
        /// Target file. Without one the result goes to stdout.
        ///
        /// Never the template itself: YAML written by a program loses your
        /// comments, and your comments are the reasons.
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Where the numbers come from and what they are claimed valid for.
        #[command(flatten)]
        identity: identity::IdentityArgs,
    },
    /// Start the governor.
    Serve {
        /// Path to the configuration file.
        #[arg(short, long, value_name = "FILE")]
        config: PathBuf,
        /// Address the inference endpoint listens on.
        ///
        /// Defaults to **loopback**, not `0.0.0.0`. The governor has neither
        /// authentication nor TLS; a default start that listens on every
        /// interface hands control of the whole GPU to anyone on the network.
        /// A non-loopback address is refused unless `backend.security` checks
        /// identity (mTLS via `client_ca`, or `token_file`); see
        /// `--insecure-open` for the one exception.
        #[arg(short, long, default_value = "127.0.0.1:9001")]
        listen: String,
        /// Address for the metrics and health endpoints
        /// (`/metrics`, `/healthz`, `/readyz`). Loopback as well.
        #[arg(long, default_value = "127.0.0.1:9090")]
        metrics: String,
        /// Listen outside loopback **without** any identity check.
        ///
        /// Refused by default: with neither `client_ca` (mTLS) nor
        /// `token_file`, anyone who reaches the endpoint controls the GPU —
        /// TLS alone encrypts but checks nobody. The one legitimate use is a
        /// container whose port is published on the host's loopback only, as
        /// in the compose quickstart.
        #[arg(long)]
        insecure_open: bool,
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
        Command::Autotune {
            endpoint,
            config,
            out,
            samples,
            periodic_us,
            quick,
            only,
            offline,
            restart,
            identity,
        } => {
            let options = autotune::Options {
                endpoint,
                config,
                out_dir: out,
                samples,
                period_us: periodic_us,
                quick,
                only,
                offline,
                restart,
            };
            Box::pin(autotune::run(&options, &identity)).await
        }
        Command::Init {
            endpoint,
            out,
            force,
        } => Box::pin(init::run(&endpoint, out.as_deref(), force)).await,
        Command::Doctor { config, offline } => Box::pin(doctor::run(&config, offline)).await,
        Command::Profile {
            config,
            samples,
            periodic_us,
            identity,
        } => Box::pin(profile::run(&config, samples, periodic_us, &identity)).await,
        Command::Calibrate {
            config,
            samples,
            periodic_us,
            out,
            identity,
        } => {
            Box::pin(calibrate::run(
                &config,
                samples,
                periodic_us,
                out.as_deref(),
                &identity,
            ))
            .await
        }
        Command::Serve {
            config,
            listen,
            metrics,
            insecure_open,
        } => Box::pin(serve::run(&config, &listen, &metrics, insecure_open)).await,
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("FEHLER {error}");
            ExitCode::FAILURE
        }
    }
}
