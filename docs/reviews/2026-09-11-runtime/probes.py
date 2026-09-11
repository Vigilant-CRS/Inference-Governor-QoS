"""Small review probes. No real Docker/network calls or GPU workloads.

The Rust probe extracts unchanged method bodies from the current repository.
Its surrounding Actor/Scheduler types are stand-ins, not an integration test.
Run: PYTHONDONTWRITEBYTECODE=1 python3 probes.py [repository [runtime]]
Exit 0 means the reviewed defects were reproduced, not that they were fixed.
"""
import hashlib
import ast
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace

REPO = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path(__file__).resolve().parents[3]
RUNTIME = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else REPO.with_name('InferenceQoS-runtime')


def between(source, start, end):
    offset = source.index(start)
    return source[offset:source.index(end, offset)]


def run_probes():
    coop = (REPO / 'crates/vig-gateway/src/cooperative.rs').read_text()
    actor = (REPO / 'crates/vig-gateway/src/actor.rs').read_text()
    sampling = between(coop, '    fn sampling_for(', '    /// Nimmt das Ergebnis')
    strip = between(coop, 'fn strip_max_tokens(', '/// Alle Eingaben ausser')
    evidence = between(actor, '    fn on_completion_evidence<', '    /// Beantwortet eine Metrikabfrage.')
    highest = between(actor, '                        let restarted = evidence.completed < highest;', '                        // Gemeldet, nicht entschieden:')
    rust = r'''
#![allow(dead_code, unused_variables)]
use std::collections::HashMap;
type Instant = ();
type RequestId = u64;
type Action = ();
macro_rules! info { ($($t:tt)*) => {}; }
mod tracing { pub(crate) use crate::info; }
#[derive(PartialEq)] enum LeaseState { Running, TimedOut, Reconciling }
struct Lease { backend_model: String, state: LeaseState, slot: usize }
enum Event { BackendFailure { request: RequestId, slot: usize } }
#[derive(Default)] struct Scheduler { released: Vec<RequestId> }
impl Scheduler {
    fn on_event<S: FnMut(Action)>(&mut self, _: Instant, event: Event, _: &mut S) {
        let Event::BackendFailure { request, .. } = event;
        self.released.push(request);
    }
}
#[derive(Default)] struct Actor {
    reconcile_baseline: HashMap<String, u64>,
    dispatched_per_model: HashMap<String, u64>,
    leases: HashMap<RequestId, Lease>,
    reconciled: u64,
    scheduler: Scheduler,
}
impl Actor {
    fn release_permit(&mut self, _: RequestId) {}
    METHOD_EVIDENCE
}
struct Job { declared_sampling: Option<String> }
impl Job { METHOD_SAMPLING }
METHOD_STRIP
struct Evidence { completed: u64 }
fn main() {
    for text in [r#"{"max_tokens":64,"temperature":0.7}"#,
                 r#"{"temperature":0.7,"max_tokens":64}"#] {
        let job = Job { declared_sampling: Some(text.to_owned()) };
        println!("JSON {}", job.sampling_for(8));
    }
    let mut actor = Actor::default();
    actor.reconcile_baseline.insert("m".into(), 100);
    actor.dispatched_per_model.insert("m".into(), 1);
    actor.leases.insert(1, Lease { backend_model: "m".into(), state: LeaseState::Reconciling, slot: 0 });
    let mut highest = 0_u64;
    for (step, count) in [100, 0, 0].into_iter().enumerate() {
        if step == 2 {
            actor.dispatched_per_model.insert("m".into(), 2);
            actor.leases.insert(2, Lease { backend_model: "m".into(), state: LeaseState::TimedOut, slot: 0 });
        }
        let evidence = Evidence { completed: count };
        METHOD_HIGHEST
        actor.on_completion_evidence((), "m", count, restarted, &mut |_| {});
        println!("COUNTER count={count} highest={highest} restarted={restarted} released={:?}", actor.scheduler.released);
    }
    assert!(actor.scheduler.released.contains(&2), "expected reproduction of premature release");
}
'''.replace('METHOD_EVIDENCE', evidence).replace('METHOD_SAMPLING', sampling).replace('METHOD_STRIP', strip).replace('METHOD_HIGHEST', highest)
    # Namespaced no-op tracing macro for the extracted method.
    rust = rust.replace('macro_rules! info', '#[macro_export]\nmacro_rules! info')
    for name, source in [('actor.rs', actor), ('cooperative.rs', coop)]:
        print(f'SOURCE {name} sha256={hashlib.sha256(source.encode()).hexdigest()}')
    with tempfile.TemporaryDirectory(prefix='vig-review-probe-') as directory:
        root = Path(directory)
        src = root / 'probe.rs'
        src.write_text(rust)
        binary = root / 'probe'
        rustc = shutil.which('rustc') or str(Path.home() / '.cargo/bin/rustc')
        subprocess.run([rustc, '--edition=2024', str(src), '-o', str(binary)], check=True, timeout=30)
        result = subprocess.run([str(binary)], capture_output=True, text=True, check=True, timeout=5)
        print('EXTRACTED RUST METHODS (stand-in Actor/Scheduler):')
        print(result.stdout, end='')
        for line in result.stdout.splitlines():
            if line.startswith('JSON '):
                try:
                    json.loads(line[5:])
                    verdict = 'VALID'
                except json.JSONDecodeError:
                    verdict = 'INVALID'
                print(f'{verdict}: {line[5:]}')

        stub = r'''
docker() { printf '%s\n' "$*" >> "$PROBE_LOG"; return 0; }
curl() { return "$PROBE_READY"; }
sleep() { :; }
seq() { printf '1\n'; }
export -f docker curl sleep seq
exec bash "$PROBE_SCRIPT" up xsched TSG
'''
        for script in [RUNTIME / 'xsched-triton-alt.sh', REPO / 'deploy/xsched/triton-two-process.sh']:
            for ready in [0, 1]:
                log = root / f'{script.name}-{ready}.log'
                env = dict(os.environ, PROBE_LOG=str(log), PROBE_READY=str(ready),
                           PROBE_SCRIPT=str(script), MODELS=str(RUNTIME / 'onetimer-vision'),
                           XSCHED_DIR=str(RUNTIME / 'xsched'))
                env.pop('LEVEL', None)
                p = subprocess.run(['bash', '--noprofile', '--norc', '-c', stub], env=env,
                                   capture_output=True, text=True, timeout=5)
                commands = log.read_text()
                print(f'SHELL {script.name}: health_ok={ready == 0}, exit={p.returncode}, '
                      f'level2={"XSCHED_AUTO_XQUEUE_LEVEL=2" in commands}, stdout={p.stdout.strip()!r}')
                assert p.returncode == 0, 'expected reproduction on current source'
                assert 'XSCHED_AUTO_XQUEUE_LEVEL=2' in commands

    # Sequence counterexample for the pilot's modulo allocation, NOT a real GPU run.
    capacity, regions = 4, 5
    live = {0: 'A'}  # A stays in flight; subsequent requests finish quickly.
    collision = False
    for seq in range(1, 6):
        assert len(live) < capacity
        slot = seq % regions
        if slot in live:
            print(f'SHM MODEL: request seq={seq} overwrites region={slot} still owned by {live[slot]}')
            collision = True
        else:
            live[slot] = str(seq)
            del live[slot]
    assert collision
    # Execute the unchanged ROS result callback with a fake node/state, without ROS.
    bridge = REPO / 'integrations/ros2/vig_bridge/vig_bridge'
    spec = importlib.util.spec_from_file_location('review_bridge_core', bridge / 'core.py')
    core = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = core
    spec.loader.exec_module(core)
    tree = ast.parse((bridge / 'node.py').read_text())
    callback = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == '_on_result')
    namespace = {'InferResult': object, 'Outcome': core.Outcome}
    exec(compile(ast.Module(body=[callback], type_ignores=[]), 'extracted_node_callback', 'exec'), namespace)
    ring = core.SlotRing(1)
    held = ring.acquire()
    node = SimpleNamespace(_count=lambda *_: None, _event=lambda *_: None)
    state = SimpleNamespace(ring=ring, camera=object())
    result = SimpleNamespace(outcome=core.Outcome.BACKEND_TIMEOUT, detail='probe')
    assert ring.acquire() is None
    namespace['_on_result'](node, state, held, 1, 1, 0, 0, result)
    reused = ring.acquire()
    print(f'ROS EXTRACTED CALLBACK: BACKEND_TIMEOUT released slot={reused}, original slot={held}')
    assert reused == held
    minimum_seconds = 4 * 3 * 3 * 2 * 60
    print(f'PILOT MATRIX: minimum={minimum_seconds}s, runner timeout=1800s, reference/setup excluded')


if __name__ == '__main__':
    run_probes()
