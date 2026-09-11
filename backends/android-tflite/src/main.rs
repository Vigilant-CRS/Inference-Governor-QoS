//! `vig-tflite-server` — ein OIP-Backend fuer TFLite mit GPU-Delegate.
//!
//! Das zweite echte Backend hinter der Backendnaht (ADR-0024, ADR-0039):
//! derselbe Governor, der vor Triton steht, steht hier vor der GPU eines
//! Android-Geraets. Ein eigener Prozess, weil nativer Code in den
//! Backendprozess gehoert und nicht in den Governor (ADR-0033).
//!
//! ```text
//! vig-tflite-server --lib-dir /data/local/tmp/tflite \
//!     --model detector=efficientdet_lite0.tflite \
//!     --model pose=movenet_lightning_f16.tflite \
//!     --model depth=midas_v21_small.tflite
//! vig-tflite-server ... --bench 200           # Rechenzeit je Modell, allein
//! vig-tflite-server ... --bench-together 200  # alle Modelle gleichzeitig
//! ```

#![allow(clippy::print_stdout, clippy::print_stderr)]

mod models;
mod service;
mod tflite;

use models::{ModelHandle, Runner};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tflite::Accelerator;
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceServiceServer;

const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const BENCH_WARMUP: usize = 20;

const USAGE: &str = "vig-tflite-server [--listen ADDR] [--lib-dir DIR] [--cpu] [--fp32] \
[--threads N] --model NAME=PATH [--model NAME=PATH ...] [--bench N | --bench-together N]";

#[derive(Debug)]
struct Options {
    listen: SocketAddr,
    lib_dir: PathBuf,
    accelerator: Accelerator,
    threads: i32,
    models: Vec<(String, PathBuf)>,
    bench: Option<Bench>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bench {
    Alone(usize),
    Together(usize),
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut listen: SocketAddr = "127.0.0.1:8001"
        .parse()
        .map_err(|e| format!("Voreinstellung: {e}"))?;
    let mut lib_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let mut cpu = false;
    let mut fp32 = false;
    let mut threads = 4;
    let mut models = Vec::new();
    let mut bench = None;
    let count = |v: String| v.parse::<usize>().map_err(|e| format!("Anzahl {v}: {e}"));

    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} braucht einen Wert"))
        };
        match arg.as_str() {
            "--listen" => listen = value()?.parse().map_err(|e| format!("--listen: {e}"))?,
            "--lib-dir" => lib_dir = PathBuf::from(value()?),
            "--cpu" => cpu = true,
            "--fp32" => fp32 = true,
            "--threads" => threads = value()?.parse().map_err(|e| format!("--threads: {e}"))?,
            "--model" => {
                let spec = value()?;
                let (name, path) = spec
                    .split_once('=')
                    .ok_or_else(|| format!("--model {spec}: NAME=PATH erwartet"))?;
                if models.iter().any(|(n, _): &(String, PathBuf)| n == name) {
                    return Err(format!("--model {name}: doppelt"));
                }
                models.push((name.to_owned(), PathBuf::from(path)));
            }
            "--bench" => bench = Some(Bench::Alone(count(value()?)?)),
            "--bench-together" => bench = Some(Bench::Together(count(value()?)?)),
            other => return Err(format!("unbekanntes Argument: {other}")),
        }
    }
    if models.is_empty() {
        return Err("mindestens ein --model".to_owned());
    }
    Ok(Options {
        listen,
        lib_dir,
        accelerator: if cpu {
            Accelerator::Cpu
        } else {
            Accelerator::Gpu {
                full_precision: fp32,
            }
        },
        threads,
        models,
        bench,
    })
}

fn load(options: &Options) -> Result<HashMap<String, ModelHandle>, String> {
    let api = tflite::Api::load(&options.lib_dir, options.accelerator)?;
    let mut handles = HashMap::new();
    for (name, path) in &options.models {
        let (accelerator, threads, path) = (options.accelerator, options.threads, path.clone());
        let handle = ModelHandle::spawn(name, move || {
            let (mut interpreter, info) =
                tflite::Interpreter::load(api, &path, accelerator, threads)?;
            let runner: Runner = Box::new(move |inputs: &[Vec<u8>]| interpreter.run(inputs));
            Ok((info, runner))
        })
        .map_err(|e| format!("{name}: {e}"))?;
        let describe = |tensors: &[tflite::TensorInfo]| {
            tensors
                .iter()
                .map(|t| format!("{} {} {:?}", t.name, t.datatype, t.shape))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "  {name:<10} {}  ein: {}  aus: {}",
            options.accelerator.platform(),
            describe(&handle.info.inputs),
            describe(&handle.info.outputs)
        );
        handles.insert(name.clone(), handle);
    }
    Ok(handles)
}

fn quantile_us(sorted: &[Duration], percent: usize) -> u128 {
    let index = sorted
        .len()
        .saturating_sub(1)
        .saturating_mul(percent)
        .checked_div(100)
        .unwrap_or(0);
    sorted.get(index).map_or(0, Duration::as_micros)
}

async fn bench_one(name: String, handle: Arc<ModelHandle>, runs: usize) -> Result<String, String> {
    let zeros: Vec<Vec<u8>> = handle
        .info
        .inputs
        .iter()
        .map(|t| vec![0; t.byte_size])
        .collect();
    for _ in 0..BENCH_WARMUP {
        handle.infer(zeros.clone()).await?;
    }
    let mut compute = Vec::with_capacity(runs);
    let mut total = Vec::with_capacity(runs);
    for _ in 0..runs {
        let start = std::time::Instant::now();
        let done = handle.infer(zeros.clone()).await?;
        total.push(start.elapsed());
        compute.push(done.compute);
    }
    compute.sort();
    total.sort();
    Ok(format!(
        "  {name:<10} n={runs}  Rechenzeit p50 {} p95 {} p99 {} us  inkl. Schlange p50 {} p99 {} us",
        quantile_us(&compute, 50),
        quantile_us(&compute, 95),
        quantile_us(&compute, 99),
        quantile_us(&total, 50),
        quantile_us(&total, 99),
    ))
}

async fn bench(models: HashMap<String, ModelHandle>, mode: Bench) -> Result<(), String> {
    let mut names: Vec<String> = models.keys().cloned().collect();
    names.sort();
    let models: HashMap<String, Arc<ModelHandle>> =
        models.into_iter().map(|(n, h)| (n, Arc::new(h))).collect();
    let handle = |name: &str| {
        models
            .get(name)
            .map(Arc::clone)
            .ok_or_else(|| format!("{name} fehlt"))
    };
    match mode {
        Bench::Alone(runs) => {
            println!("\nallein, nacheinander ({BENCH_WARMUP} Warmlaeufe):");
            for name in names {
                let line = bench_one(name.clone(), handle(&name)?, runs).await?;
                println!("{line}");
            }
        }
        Bench::Together(runs) => {
            println!("\nalle Modelle gleichzeitig, je ein Auftrag in Folge:");
            let mut tasks = Vec::new();
            for name in names {
                tasks.push(tokio::spawn(bench_one(name.clone(), handle(&name)?, runs)));
            }
            for task in tasks {
                let line = task.await.map_err(|e| e.to_string())??;
                println!("{line}");
            }
        }
    }
    Ok(())
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = async {
            match term.as_mut() {
                Some(t) => { t.recv().await; }
                None => std::future::pending::<()>().await,
            }
        } => {}
    }
    eprintln!("vig-tflite-server: beende");
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(error) => {
            eprintln!("{error}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "vig-tflite-server {} · {} · Bibliotheken {}",
        env!("CARGO_PKG_VERSION"),
        options.accelerator.platform(),
        options.lib_dir.display()
    );
    let models = match load(&options) {
        Ok(m) => m,
        Err(error) => {
            eprintln!("vig-tflite-server: {error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(error) => {
            eprintln!("vig-tflite-server: Runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(mode) = options.bench {
        return match runtime.block_on(bench(models, mode)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("vig-tflite-server: {error}");
                ExitCode::FAILURE
            }
        };
    }

    let service = service::TfliteService::new(models, options.accelerator.platform());
    println!("bereit auf {}", options.listen);
    let served = runtime.block_on(
        tonic::transport::Server::builder()
            .add_service(
                GrpcInferenceServiceServer::new(service)
                    .max_decoding_message_size(MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_MESSAGE_BYTES),
            )
            // `serve_with_shutdown` mit Adresse: TCP_NODELAY per Voreinstellung.
            .serve_with_shutdown(options.listen, shutdown()),
    );
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vig-tflite-server: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_gpu_is_the_default_and_the_cpu_must_be_asked_for() {
        let o = parse(args(&["--model", "d=a.tflite"])).unwrap();
        assert_eq!(
            o.accelerator,
            Accelerator::Gpu {
                full_precision: false
            }
        );
        let o = parse(args(&["--model", "d=a.tflite", "--cpu"])).unwrap();
        assert_eq!(o.accelerator, Accelerator::Cpu);
    }

    #[test]
    fn models_are_named_and_unique() {
        assert!(parse(args(&["--model", "a.tflite"])).is_err());
        assert!(parse(args(&["--model", "d=a", "--model", "d=b"])).is_err());
        assert!(parse(args(&[])).is_err());
        let o = parse(args(&["--model", "d=a", "--model", "p=b", "--bench", "5"])).unwrap();
        assert_eq!(o.models.len(), 2);
        assert_eq!(o.bench, Some(Bench::Alone(5)));
    }

    #[test]
    fn quantiles_come_from_the_sorted_series() {
        let series: Vec<Duration> = (1..=100).map(Duration::from_micros).collect();
        assert_eq!(quantile_us(&series, 50), 50);
        assert_eq!(quantile_us(&series, 99), 99);
        assert_eq!(quantile_us(&[], 50), 0);
    }
}
