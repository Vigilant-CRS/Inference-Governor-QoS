//! `shm-latency` — was kostet der Transport, und was das Backend?
//!
//! Die Messung, die NV-09 offen liess (siehe
//! `docs/spikes/nv09-tensorrt-direct.md`). Ein direkter TensorRT-Aufruf im
//! Prozess dauert fuer `pose_main` 1795 us; ueber gRPC ohne Shared Memory
//! misst der Client 5050 us. Die Frage ist, wie viel davon der **Transport**
//! ist — denn den hat dieses Projekt mit ADR-0003 laengst adressiert, und der
//! Rest waere der Gewinn eines eigenen Executors.
//!
//! Deshalb hier derselbe Aufruf ueber **System Shared Memory**: der Tensor
//! reist als Referenz, der Request traegt wenige hundert Byte. Verglichen wird
//! gegen Tritons eigene Statistik, die sagt, was serverseitig anfiel.
//!
//! Bewusst ein eigenes Werkzeug und keine Option an `vig profile`: `profile`
//! schreibt Vertragsprofile, und ein Profil, das ueber Shared Memory gemessen
//! wurde, gilt nur fuer Clients, die Shared Memory benutzen. Das ist eine
//! Aussage ueber den Aufbau und gehoert nicht unbemerkt in eine
//! Konfiguration.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::integer_division
)]

use std::time::Instant;
use vig_backend_triton::TritonClient;
use vig_bench::shm::Region;
use vig_protocol_oip::inference::model_infer_request::{
    InferInputTensor, InferRequestedOutputTensor,
};
use vig_protocol_oip::inference::{
    InferParameter, ModelInferRequest, SystemSharedMemoryRegisterRequest,
    SystemSharedMemoryUnregisterRequest, infer_parameter,
};

/// Aufwaermlaeufe, die nicht gezaehlt werden.
const WARMUP: usize = 30;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8001".to_owned());
    let model = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "pose_main".to_owned());
    let runs: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);

    let client = TritonClient::new(&endpoint);
    let metadata = client
        .model_metadata(&model)
        .await
        .expect("Modellmetadaten");
    let input = metadata.inputs.first().expect("ein Eingang").clone();
    let shape: Vec<i64> = input.shape.iter().map(|d| (*d).max(1)).collect();
    let elements: i64 = shape.iter().product();
    let bytes = usize::try_from(elements).unwrap_or(0).saturating_mul(4);

    let region = Region::create(
        &format!("vig_shmlat_{model}"),
        u64::try_from(bytes).unwrap_or(0),
    )
    .expect("Shm-Region");
    let raw = client.raw().await.expect("Backend erreichbar");
    let _ = raw
        .clone()
        .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
            name: region.name.clone(),
        })
        .await;
    raw.clone()
        .system_shared_memory_register(SystemSharedMemoryRegisterRequest {
            name: region.name.clone(),
            key: region.key.clone(),
            offset: 0,
            byte_size: u64::try_from(bytes).unwrap_or(0),
        })
        .await
        .expect("Shm-Region registrieren");

    let request = ModelInferRequest {
        model_name: model.clone(),
        inputs: vec![InferInputTensor {
            name: input.name.clone(),
            datatype: input.datatype.clone(),
            shape,
            parameters: shm_parameters(&region.name, bytes),
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
        ..Default::default()
    };

    for _ in 0..WARMUP {
        let _ = client.infer(request.clone()).await;
    }

    let mut samples: Vec<u64> = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        if client.infer(request.clone()).await.is_err() {
            continue;
        }
        samples.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
    samples.sort_unstable();

    let pick = |q: usize| -> u64 {
        if samples.is_empty() {
            return 0;
        }
        let i = samples
            .len()
            .saturating_mul(q)
            .checked_div(100)
            .unwrap_or(0)
            .min(samples.len().saturating_sub(1));
        samples.get(i).copied().unwrap_or(0)
    };

    println!(
        "{model} ueber System Shared Memory, {} Laeufe:",
        samples.len()
    );
    println!(
        "  p50 {} us | p95 {} us | p99 {} us",
        pick(50),
        pick(95),
        pick(99)
    );
    println!("  Nutzlast {} KB, als Referenz uebertragen", bytes / 1024);

    let _ = raw
        .clone()
        .system_shared_memory_unregister(SystemSharedMemoryUnregisterRequest {
            name: region.name.clone(),
        })
        .await;
}

/// Die Parameter, mit denen ein Tensor als Shm-Referenz reist.
fn shm_parameters(name: &str, bytes: usize) -> std::collections::HashMap<String, InferParameter> {
    let mut map = std::collections::HashMap::new();
    map.insert(
        "shared_memory_region".to_owned(),
        InferParameter {
            parameter_choice: Some(infer_parameter::ParameterChoice::StringParam(
                name.to_owned(),
            )),
        },
    );
    map.insert(
        "shared_memory_offset".to_owned(),
        InferParameter {
            parameter_choice: Some(infer_parameter::ParameterChoice::Int64Param(0)),
        },
    );
    map.insert(
        "shared_memory_byte_size".to_owned(),
        InferParameter {
            parameter_choice: Some(infer_parameter::ParameterChoice::Int64Param(
                i64::try_from(bytes).unwrap_or(0),
            )),
        },
    );
    map
}
