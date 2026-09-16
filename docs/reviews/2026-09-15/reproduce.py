#!/usr/bin/env python3
"""Add review counterexamples to a disposable `git archive` of 90f517c.

Usage: python3 reproduce.py /tmp/inferenceqos-review-20260915/source
Tests assert the required behavior; failures demonstrate the reviewed bugs.
No production source is changed in the working repository.
"""
from pathlib import Path
import sys

root = Path(sys.argv[1]).resolve()
if not str(root).startswith('/tmp/'):
    raise SystemExit('Use a disposable source copy under /tmp')

def add(path, code, inside=False):
    target = root / path
    text = target.read_text()
    if 'review_20260915' in text:
        raise SystemExit(f'Already patched: {target}')
    if inside:
        pos = text.rfind('}')
        text = text[:pos] + code + '\n' + text[pos:]
    else:
        text += '\n' + code
    target.write_text(text)

add('crates/vig-gateway/tests/security.rs', r'''
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_20260915_strict_rejects_foreign_segment_alias() {
    let (_mock, endpoint) = backend(Duration::from_millis(1)).await;
    let service = service(&endpoint, "  trust: strict\n", 1, CONTRACT).with_tokens(tokens());
    service.system_shared_memory_register(register("alpha", "/vig_private", 0, 1024, ALPHA)).await.unwrap();
    let alias = service.system_shared_memory_register(register("beta", "/vig_private", 0, 1024, BETA)).await;
    assert!(alias.is_err(), "BETA can register ALPHA's physical key under another region name");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_20260915_parallel_registrations_obey_limit() {
    let (_mock, endpoint) = backend(Duration::from_millis(1)).await;
    let service = service(&endpoint, "  security:\n    max_shm_regions: 1\n", 1, CONTRACT).with_tokens(tokens());
    let (a,b) = tokio::join!(
        service.system_shared_memory_register(register("a", "/vig_a", 0, 1024, ALPHA)),
        service.system_shared_memory_register(register("b", "/vig_b", 0, 1024, BETA)),
    );
    assert!(service.shm_registry().len() <= 1,
        "limit=1, registered={}, a={a:?}, b={b:?}", service.shm_registry().len());
}
''')

add('crates/vig-cli/src/autotune/tune.rs', r'''
#[cfg(test)]
mod review_20260915 {
    use super::*;
    #[test]
    fn protected_stream_regression_must_not_be_hidden_by_maximum() {
        let eval = |a, b| Evaluation { points: vec![LoadPoint {
            load_percent: 110,
            streams: vec![
                StreamMiss { stream: "A".into(), protected: true, governed_permille: a, samples: 10_000 },
                StreamMiss { stream: "B".into(), protected: true, governed_permille: b, samples: 10_000 },
            ],
        }] };
        let before = eval(200, 0).objective().unwrap();
        let after = eval(100, 100).objective().unwrap();
        let decision = decide(after, before, before);
        assert!(decision.is_err(), "B regresses from 0 to 100 permille but is accepted: {decision:?}");
    }
}
''')

add('crates/vig-cli/src/autotune.rs', r'''
#[cfg(test)]
mod review_20260915 {
    use super::*;
    #[tokio::test]
    async fn absent_frozen_artifact_must_invalidate_completed_state() {
        let dir = std::env::temp_dir().join(format!("vig-review-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("vig.yaml");
        std::fs::write(&config, include_str!("../../../examples/gate_m3/vig.yaml")).unwrap();
        let missing = dir.join("never-written-measured.yaml");
        assert!(!missing.exists());
        let options = Options { endpoint: "127.0.0.1:1".into(), config: config.clone(),
            out_dir: dir.clone(), samples: 100, period_us: None, quick: false,
            only: None, offline: true, restart: false };
        let q = Qualification { steps: Step::ALL.iter().map(|step| StepResult {
            step: *step, outcome: Outcome::Done, seconds: 0, notes: vec![] }).collect(),
            config: Some(missing), series: SeriesCount { qualified: 1, discarded: 0, skipped_models: 0 },
            ..Qualification::default() };
        std::fs::write(dir.join("state.json"), state_json(&q, &fingerprint_of(&options.endpoint, &config))).unwrap();
        let result = run(&options, &IdentityArgs::default()).await.unwrap();
        assert_ne!(result, ExitCode::SUCCESS, "complete state is accepted although measured.yaml is absent");
    }

    #[tokio::test]
    async fn explicit_endpoint_must_not_silently_disagree_with_config() {
        let dir = std::env::temp_dir().join(format!("vig-review-endpoint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("vig.yaml");
        std::fs::write(&config, include_str!("../../../examples/gate_m3/vig.yaml")).unwrap();
        let requested = "different-machine.invalid:8001";
        let mut live = Live { endpoint: requested.into(), out_dir: dir, samples: 12,
            period_us: None, offline: true, identity: IdentityArgs::default(), fingerprint: String::new() };
        let result = live.discover(requested, &config).await;
        let parsed = vig_config::Config::from_yaml(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert!(result.is_err() || parsed.backend.grpc_endpoint == requested,
            "discovery accepts endpoint {requested}, but measurement uses {}", parsed.backend.grpc_endpoint);
    }
}
''')

add('backends/android-tflite/src/models.rs', r'''
    #[tokio::test]
    async fn review_20260915_cancelled_call_still_counts_completed_gpu_work() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let handle = Arc::new(ModelHandle::spawn("review", move || {
            let mut started = Some(started_tx);
            let runner: Runner = Box::new(move |_| {
                if let Some(tx) = started.take() {
                    let _ = tx.send(());
                    let _ = release_rx.recv();
                }
                Ok(vec![])
            });
            Ok((ModelInfo { inputs: vec![], outputs: vec![] }, runner))
        }).unwrap());
        let first = tokio::spawn({ let handle = Arc::clone(&handle); async move { handle.infer(vec![]).await } });
        started_rx.await.unwrap();
        first.abort();
        let _ = first.await;
        release_tx.send(()).unwrap();
        handle.infer(vec![]).await.unwrap();
        assert_eq!(handle.stats.success.load(Ordering::Acquire), 2,
            "both jobs completed, but cancellation removed one from backend completion evidence");
    }
''', inside=True)

add('backends/android-tflite/src/service.rs', r'''
    #[test]
    fn review_20260915_rejects_wrong_shape_even_with_same_byte_count() {
        let mut req = request(vec![0; 12]);
        req.inputs[0].shape = vec![1, 3, 2, 2];
        assert!(prepare_inputs(&detector_info(), &mut req).is_err(),
            "NCHW declared input is silently interpreted as NHWC");
    }
''', inside=True)

add('crates/vig-bench/src/bin/vig-fit.rs', r'''
#[cfg(test)]
mod review_20260915 {
    use super::*;
    #[tokio::test]
    async fn no_deliveries_must_not_be_conclusive() {
        let defs = vec![StreamDef { name: "det", model: "det", period: Duration::from_millis(10),
            max_age: Duration::from_millis(10), in_flight_cap: 1, input: None, text: None,
            pump: false, burst: None }];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        drop(listener);
        let reports = drive(&endpoint, &defs, Duration::from_millis(100), false).await;
        assert!(reports.iter().all(|r| r.delivered == 0));
        // Same report-to-row projection as run(): presence, not deliveries.
        let rows: Vec<Row> = reports.iter().map(|r| Row {
            load: 100, stream: r.name.into(), protected: true,
            direct: Some(r.coverage.consumer_uncovered_permille()),
            governed: r.coverage.consumer_uncovered_permille(), direct_gap_ms: Some(0), governed_gap_ms: 0,
            direct_samples: Some(r.coverage.total), governed_samples: r.coverage.total,
        }).collect();
        let value: serde_json::Value = serde_json::from_str(&as_json(&rows, &[100], 1, Some(0), Some(0), Arms::Both)).unwrap();
        assert_eq!(value.get("conclusive").and_then(|v| v.as_bool()), Some(false),
            "reports exist even when no request was delivered: {value}");
    }
}
''')
print(f'Counterexamples added to {root}')
