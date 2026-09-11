//! `serve-latency` — dieselbe Anfrage ueber mehrere Wege, abwechselnd.
//!
//! Fuer `docs/benchmark/arm-serve.md`: der Governor laeuft auf einem anderen
//! Geraet, Triton auf dem Laptop, dazwischen USB. Die Frage ist, wie viel der
//! End-to-End-Zeit der Governor kostet und wie viel der Weg zu ihm. Deshalb
//! misst das Werkzeug mehrere Wege **abwechselnd** Request um Request —
//! direkt, ueber ein reines Relais auf dem Geraet, ueber den Governor —, damit
//! eine Drift der Maschine nicht einem Weg allein zufaellt. Die Reihenfolge
//! rotiert je Runde, damit auch kein Weg immer als erster laeuft.
//!
//! Die Nutzlast reist im Request (Copy-Pfad): Shared Memory reicht nicht ueber
//! Geraetegrenzen. Sie besteht aus Nullen in der Eingabegroesse, die das
//! Modell selbst meldet.
//!
//! ```text
//! serve-latency alternate <runs> <name=host:port/modell>...
//! serve-latency burst <parallel> <je> <host:port/modell>
//! ```
//!
//! `burst` schickt `parallel` Clients mit je `je` Anfragen gleichzeitig los
//! und zaehlt die Antworten nach Statuscode. Gegen einen Governor mit
//! LATEST-Warteschlange zeigt das, dass Frische entschieden wird: aeltere
//! Anfragen werden verdraengt, statt hinter neueren zu warten.

#![allow(
    clippy::print_stdout,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::integer_division
)]

use std::collections::BTreeMap;
use std::time::Instant;
use tonic::transport::Channel;
use vig_gateway::datapath_budget::Latency;
use vig_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient;
use vig_protocol_oip::inference::model_infer_request::{
    InferInputTensor, InferRequestedOutputTensor,
};
use vig_protocol_oip::inference::{ModelInferRequest, ModelMetadataRequest};

/// Aufwaermrunden, die nicht gezaehlt werden: Verbindungsaufbau und
/// Erstbenutzung gehoeren nicht in die Messung.
const WARMUP: usize = 20;

type Client = GrpcInferenceServiceClient<Channel>;

/// Ein Weg zum Modell: Name im Bericht, Adresse, Modellname an dieser Adresse.
struct Arm {
    name: String,
    address: String,
    model: String,
}

impl Arm {
    fn parse(spec: &str) -> Self {
        let (name, rest) = spec.split_once('=').unwrap_or(("", spec));
        let (address, model) = rest
            .split_once('/')
            .expect("ein Weg lautet name=host:port/modell");
        Self {
            name: if name.is_empty() { address } else { name }.to_owned(),
            address: address.to_owned(),
            model: model.to_owned(),
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("alternate") if args.len() >= 3 => {
            let runs = args[1].parse().expect("<runs> ist eine Zahl");
            let arms: Vec<Arm> = args[2..].iter().map(|s| Arm::parse(s)).collect();
            alternate(runs, &arms).await;
        }
        Some("burst") if args.len() == 4 => {
            let parallel = args[1].parse().expect("<parallel> ist eine Zahl");
            let each = args[2].parse().expect("<je> ist eine Zahl");
            burst(parallel, each, &Arm::parse(&args[3])).await;
        }
        _ => {
            println!(
                "serve-latency alternate <runs> <name=host:port/modell>...\n\
                 serve-latency burst <parallel> <je> <host:port/modell>"
            );
            std::process::exit(2);
        }
    }
}

/// Misst alle Wege abwechselnd und berichtet Median und p99 je Weg.
async fn alternate(runs: usize, arms: &[Arm]) {
    let mut clients = Vec::with_capacity(arms.len());
    let mut requests = Vec::with_capacity(arms.len());
    for arm in arms {
        let mut client = connect(&arm.address).await;
        let (request, bytes) = request_for(&mut client, &arm.model).await;
        println!(
            "  {:<10} {} / {}: Nutzlast {} KB",
            arm.name,
            arm.address,
            arm.model,
            bytes / 1024
        );
        clients.push(client);
        requests.push(request);
    }

    let mut samples = vec![Vec::with_capacity(runs); arms.len()];
    let mut errors = vec![0_usize; arms.len()];
    for round in 0..WARMUP + runs {
        for step in 0..arms.len() {
            let i = (round + step) % arms.len();
            let request = requests[i].clone();
            let started = Instant::now();
            let outcome = clients[i].model_infer(request).await;
            let us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            if round < WARMUP {
                continue;
            }
            match outcome {
                Ok(_) => samples[i].push(us),
                Err(_) => errors[i] += 1,
            }
        }
    }

    println!("\n  {runs} Runden, abwechselnd, Reihenfolge rotierend; in us:");
    println!("  Weg        |   min |   p50 |   p99 |   max | Fehler");
    println!("  -----------|-------|-------|-------|-------|-------");
    for (i, arm) in arms.iter().enumerate() {
        let min = samples[i].iter().min().copied().unwrap_or(0);
        let max = samples[i].iter().max().copied().unwrap_or(0);
        let latency = Latency::from_samples(&mut samples[i]);
        let (p50, p99) = latency.map_or((0, 0), |l| (l.p50_us, l.p99_us));
        println!(
            "  {:<10} | {min:>5} | {p50:>5} | {p99:>5} | {max:>5} | {}",
            arm.name, errors[i]
        );
    }
}

/// Viele Clients zugleich gegen einen Weg; zaehlt die Antworten nach Status.
async fn burst(parallel: usize, each: usize, arm: &Arm) {
    let mut probe = connect(&arm.address).await;
    let (request, bytes) = request_for(&mut probe, &arm.model).await;
    println!(
        "  {} / {}: {parallel} Clients x {each} Anfragen, Nutzlast {} KB",
        arm.address,
        arm.model,
        bytes / 1024
    );

    let mut tasks = Vec::with_capacity(parallel);
    for _ in 0..parallel {
        let mut client = connect(&arm.address).await;
        let request = request.clone();
        tasks.push(tokio::spawn(async move {
            let mut seen: BTreeMap<String, (usize, String)> = BTreeMap::new();
            for _ in 0..each {
                let (code, message) = match client.model_infer(request.clone()).await {
                    Ok(_) => ("Ok".to_owned(), String::new()),
                    Err(status) => (format!("{:?}", status.code()), status.message().to_owned()),
                };
                let entry = seen.entry(code).or_insert((0, message));
                entry.0 += 1;
            }
            seen
        }));
    }

    let mut total: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for task in tasks {
        for (code, (count, message)) in task.await.expect("Client-Task") {
            let entry = total.entry(code).or_insert((0, message));
            entry.0 += count;
        }
    }
    for (code, (count, message)) in &total {
        let first_line = message.lines().next().unwrap_or("");
        println!("  {code:<18} {count:>5}  {first_line}");
    }
}

/// Ein Client mit denselben Transportgrenzen wie der Governor (Spec 19.1).
async fn connect(address: &str) -> Client {
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))
        .expect("Adresse")
        .initial_stream_window_size(vig_backend_triton::STREAM_WINDOW_BYTES)
        .initial_connection_window_size(vig_backend_triton::CONNECTION_WINDOW_BYTES)
        .tcp_nodelay(true)
        .connect()
        .await
        .expect("Endpunkt erreichbar");
    GrpcInferenceServiceClient::new(channel)
        .max_decoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
        .max_encoding_message_size(vig_backend_triton::DEFAULT_MAX_MESSAGE_BYTES)
}

/// Ein Request mit Nullen in der Eingabegroesse, die das Modell meldet.
///
/// Ein Governor beantwortet die Metadaten unter dem logischen Namen; so kann
/// jeder Weg seinen Request selbst bauen.
async fn request_for(client: &mut Client, model: &str) -> (ModelInferRequest, usize) {
    let metadata = client
        .model_metadata(ModelMetadataRequest {
            name: model.to_owned(),
            version: String::new(),
        })
        .await
        .expect("Modellmetadaten")
        .into_inner();
    let input = metadata.inputs.first().expect("ein Eingang");
    let shape: Vec<i64> = input.shape.iter().map(|d| (*d).max(1)).collect();
    let elements = usize::try_from(shape.iter().product::<i64>()).unwrap_or(0);
    let bytes = elements.saturating_mul(element_bytes(&input.datatype));
    let request = ModelInferRequest {
        model_name: model.to_owned(),
        inputs: vec![InferInputTensor {
            name: input.name.clone(),
            datatype: input.datatype.clone(),
            shape,
            parameters: std::collections::HashMap::new(),
            contents: None,
        }],
        outputs: metadata
            .outputs
            .iter()
            .map(|o| InferRequestedOutputTensor {
                name: o.name.clone(),
                parameters: std::collections::HashMap::new(),
            })
            .collect(),
        raw_input_contents: vec![vec![0_u8; bytes]],
        ..Default::default()
    };
    (request, bytes)
}

/// Bytes je Element eines OIP-Datentyps.
fn element_bytes(datatype: &str) -> usize {
    match datatype {
        "BOOL" | "UINT8" | "INT8" => 1,
        "FP16" | "BF16" | "UINT16" | "INT16" => 2,
        "FP64" | "UINT64" | "INT64" => 8,
        _ => 4,
    }
}
