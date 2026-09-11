# Getting started

What you need, what to run, and how to tell whether it is doing anything.

## What you need

- An existing **NVIDIA Triton** installation (or any Open Inference Protocol
  server — we also test against OpenVINO Model Server).
- At least **two models competing for one GPU**, at least one of which is
  time-critical. Below saturation this software has nothing to offer; see the
  table in the [README](../README.md#when-this-helps--and-when-it-does-not).
- Rust 1.90 or newer to build.

```bash
cargo build --release
# binary at target/release/vig
```

## Step 1 — describe what your robot needs

The configuration answers two kinds of question. **Requirements** — what has to
be fresh, what matters more — only you can answer. **Hardware facts** — how
long a model takes — are measured in step 2, so leave the profiles out for now.

```yaml
version: 1

backend:
  type: triton
  grpc_endpoint: "127.0.0.1:8001"
  slots: 1                # how many inferences your GPU runs at once
  pipelining_depth: 0     # extra credits per slot; 0 is the safe start
  trust: strict           # refuse anything that would bypass the governor

models:
  detector:
    class: protected                          # must not be starved
    queue: { policy: latest, capacity: 1 }    # only the newest frame matters
    contract:
      period_ms: 33       # you send one every 33 ms
      deadline_ms: 33     # it is worthless if it takes longer
      max_age_ms: 66      # ...and worthless if the input was older than this
    variants:
      - id: rfdetr
        backend_model: rfdetr                 # the name in your Triton repo
        quality: { value: 1.0, source: measured }

  summariser:
    class: best_effort    # may use spare capacity, must not endanger detector
    queue: { policy: fifo, capacity: 4, overflow: backpressure_client }
    contract: { deadline_ms: 800, max_age_ms: 1500 }
    variants:
      - id: main
        backend_model: vlm_main
        quality: { value: 1.0, source: measured }
```

**`slots` is the one number to get right.** It must match your Triton instance
groups. Too high and a second, invisible queue forms behind the governor,
reordering everything it decided. Too low and capacity sits unused.

### The four classes

| Class | Meaning |
|---|---|
| `protected` | Time-critical perception. Everything else yields to it. |
| `high` | Important, but yields to `protected`. |
| `normal` | Ordinary work, no special guarantee. |
| `best_effort` | Uses leftover capacity. Never knowingly endangers protected work. |

### The four queue policies

| Policy | Use it when |
|---|---|
| `latest` | Only the newest input matters — a camera. |
| `latest_per_key` | Several independent sources share one model — several cameras. |
| `fifo` | Order matters and nothing may be dropped for freshness. |
| `never_drop` | Nothing may be dropped at all; overflow pushes back on the client. |

## Step 2 — measure the machine

```bash
vig doctor -c vig.yaml
```

Checks the configuration, reaches the backend, confirms every model exists, and
tells you whether shared memory is available. It starts nothing.

```bash
vig calibrate -c vig.yaml -o measured.yaml
```

Measures how long each model takes alone, how much models slow each other down,
and — for decomposable models — the fixed cost per request. It writes a
complete configuration with the profiles filled in.

It writes to a **new file**, never over your template: YAML written by a
program loses your comments, and your comments are the reasons.

`calibrate` never touches contracts. Deadlines are requirements, not
measurements.

## Step 3 — run it

```bash
vig serve -c measured.yaml
```

Listens on `127.0.0.1:9001` by default, with metrics on `127.0.0.1:9090`.

**Loopback is deliberate.** By default there is no authentication and no TLS,
so `vig serve` refuses any other address unless something checks **who** is
calling — mTLS (`client_ca`) or tokens (`token_file`). TLS alone encrypts but
checks nobody and is refused too. The one explicit way past this is
`--insecure-open`, meant for a container whose port is published only on the
host's loopback (that is what the shipped `docker-compose.yml` does):

```bash
vig serve -c measured.yaml --listen 0.0.0.0:9001 --insecure-open   # container only
```

### Opening the endpoint safely

If it has to be reachable from elsewhere, turn on one of the two mechanisms —
`vig doctor` warns until you do, and reports `trust: strict` without an
identity check as not ready:

```yaml
backend:
  trust: strict                    # no bypassing the governor, no self-promotion
  security:
    tls_cert:  /etc/vig/server.pem
    tls_key:   /etc/vig/server.key
    client_ca: /etc/vig/clients-ca.pem   # mTLS: clients need a certificate
    # or, without certificate management:
    token_file: /etc/vig/tokens
    # model load/unload, tracing, log level, CUDA memory: closed without this
    admin_token_file: /etc/vig/admin.tokens
```

mTLS is the stronger one: a private key does not leave the device, a token
travels in every request. The token file holds one token per line, optionally
named — `robot:<token>`; `#` starts a comment. A token must be at least 16
characters. With either mechanism enabled, **every gRPC request** requires it —
including the shared-memory registration, which is the one that matters most.
The metrics port (`--metrics`, default loopback) is not authenticated; keep it
on loopback or behind something that is.

The administration endpoints (model load/unload, trace and log settings, CUDA
shared memory) stay closed in every mode until `admin_token_file` is set, and
then accept only a token from that file.

Shared-memory registrations through the governor are checked: the key must
start with `shm_key_prefix` (default `/vig_`), the region must have a size and
must not overflow, a region belongs to the caller that registered it, and at
most `max_shm_regions` (default 256) exist. Under `trust: strict` an inference
may only name regions its own caller registered.

Half a configuration is refused rather than half applied: a certificate without
a key looks like protection and is none. What this protects and what it does
not is listed in [security.md](security.md).

## Step 4 — point your client at it

The API is unchanged. Only the address differs.

```python
# before
client = grpcclient.InferenceServerClient("triton:8001")
# after
client = grpcclient.InferenceServerClient("governor:9001")
```

To get the freshness behaviour, tell the governor how old your data is:

```python
client.infer("detector", inputs, parameters={
    "vig_age_us":     12_000,   # captured 12 ms ago
    "vig_max_age_us": 66_000,   # useless beyond 66 ms
})
```

| Parameter | Type | Meaning |
|---|---|---|
| `vig_age_us` | int64 | how old the input is, in microseconds |
| `vig_generation_ns` | int64 | absolute monotonic capture timestamp — only valid if client and governor share one clock, i.e. run on the same host. Mutually exclusive with `vig_age_us` |
| `vig_max_age_us` | int64 | beyond this age the result is worthless |
| `vig_deadline_us` | int64 | this request's own deadline, overriding the contract |
| `vig_stream_id` | int64 | which sensor stream this request comes from |
| `vig_supersession_key` | int64 | which stream this belongs to for replacement (camera id, object id) |
| `vig_class` | string | requested importance class: `protected`, `high`, `normal`, `best_effort`. Under `trust: strict` a client can only lower its class below the configured one, never raise it |
| `vig_capture_id` | int64 | the capture this request belongs to — see [results from the same capture](#results-from-the-same-capture) |
| `vig_depends_on` | string | comma-separated request `id`s whose results this request combines |
| `vig_hint_elevated_max_age_us` | int64 | "I need this stream fresher right now" — see [application hints](#application-hints) |
| `vig_hint_action_horizon_us` | int64 | "I do not need this stream fresher for this long" |
| `vig_hint_mode` | int64 | an operating mode the operator has named |
| `vig_hint_ttl_us` | int64 | how long the hint holds; a hint without it is not accepted |

Without any of these the request still works — it simply uses the contract's
defaults and counts its age from arrival instead of capture.

A misspelled `vig_` parameter is **refused**, not ignored: a typo in a
freshness parameter would otherwise run the request without the meaning you
intended. Parameters without the prefix pass through untouched.

### Results from the same capture

Two results can both be fresh and still come from **different** captures — a
depth map from frame 41 and detections from frame 42. Freshness alone cannot
see that. If your pipeline fuses results, say which capture each request
belongs to:

```python
# depth and detector on the same frame
client.infer("depth",    depth_in,  request_id="4101", parameters={"vig_capture_id": 41})
client.infer("detector", det_in,    request_id="4102", parameters={"vig_capture_id": 41})
# the fusion step names both
client.infer("fusion",   fusion_in, request_id="4103", parameters={
    "vig_capture_id": 41,
    "vig_depends_on": "4101,4102",
})
```

A fusion across two captures is refused with `FAILED_PRECONDITION` **before**
it runs — the backend never sees it. The request `id` must be a number when
`vig_capture_id` is set, otherwise no later request could name it; at most
eight parents per request. Without `vig_capture_id` nothing changes and the
governor plans by freshness alone ([ADR-0028](adr/0028-a-fusion-needs-a-common-capture.md)).

### Application hints

Hints let the application say what it needs **right now**, within bounds the
operator set. They are off until `backend.hints` is configured, and a hint is
only accepted from a caller that authenticates with a **named** bearer token
(`robot:<token>` in the token file). The authority is a number derived from the
name, not from the token; `vig serve` prints it at startup for every named
token. An unnamed token authenticates but never sends hints.

```yaml
backend:
  hints:
    authority: 1234567890123     # the number `vig serve` printed for the name "robot"
    allow_loosening: false       # action horizons weaken a promise; off by default
    approved_modes: [1, 2]       # modes the operator has named
    min_max_age_ms: 20           # how far "fresher" may go
    max_action_horizon_ms: 200   # how long "not fresher" may last
    max_ttl_ms: 60000            # longest hint lifetime, 1 ms – 1 h; longer hints are dropped (default 60 s)
```

A hint may **tighten** a promise, never silently loosen it: an elevated
freshness requirement is accepted within `min_max_age_ms`; an action horizon
only with `allow_loosening: true`. A stale, contradictory or unauthorised hint
leaves the operator's contract in force
([ADR-0029](adr/0029-a-hint-may-tighten-never-loosen.md)).

### Preemptible background work

A long background job — a language model, a 90 ms block — never starts under
load next to a 33 ms camera: it would hold up the next frame. If your backend
can **interrupt** that job when protected work arrives — two Triton processes
on one GPU under a preemption layer such as XSched, the background one at low
priority — tell the governor, and it plans with what the interruption
actually costs:

```yaml
backend:
  grpc_endpoint: "127.0.0.1:9201"      # the high-priority process
  slots: 1
  preemptible_lanes: 1                 # a lane for interruptible work

models:
  summariser:
    class: best_effort
    backend_endpoint: "127.0.0.1:9101" # the low-priority process
    preemptible: { residual_blocking_us: 14000, source: measured }
```

The background job then runs on its own lane instead of the protected slot.
While it runs, protected work plans with the **residual blocking** — how much
later it can finish because the interruption is not instantaneous — and the
look-ahead only starts the job if that residual fits into the protected
slack. It no longer needs to fit its whole runtime into a gap.

The residual is a measurement of your setup, not a setting:
`vig calibrate` measures it (p99 of protected latency with the background job
running, minus p99 alone) and writes `source: measured`. `vig doctor` warns
while it says `declared`, and warns when the residual does not fit into a
protected deadline — then the lane is configured and useless. The
configuration refuses a preemptible model in the same process as protected
models (priority is per process), a protected class marked preemptible, and a
`no_corun` rule between the two
([ADR-0035](adr/0035-preemption-is-a-measured-backend-property.md)).

The governor never triggers preemption itself; the backend does
([ADR-0033](adr/0033-native-code-lives-in-the-backend-process.md)).

### Two GPUs

A slot is one execution unit on one GPU. If your machine has two GPUs,
describe the second one as a **resource domain** — a GPU with its own
scheduler — instead of adding slots:

```yaml
backend:
  grpc_endpoint: "127.0.0.1:8001"      # GPU 0: the domain `default`
  slots: 1
  domains:
    gpu1:
      gpu_index: 1                     # as nvidia-smi counts it
      grpc_endpoint: "127.0.0.1:8101"  # a Triton that only sees GPU 1
      slots: 1

models:
  detector:                            # no `domain:` — runs on GPU 0
    class: protected
    # ...
  vlm:
    class: best_effort
    domain: gpu1                       # runs on GPU 1, always
    # ...
```

Each domain plans on its own: slots, credits, quarantine, safety margin,
look-ahead and miss budget. An expected detector frame on GPU 0 does not hold
back the language model on GPU 1, and a hung backend on GPU 1 takes no credit
from GPU 0. Shared are what belongs to the process: the payload budget, access
control, hints and timeouts. The assignment is fixed — a model never moves to
another GPU at runtime, and there is no automatic failover
([ADR-0037](adr/0037-a-domain-is-a-gpu-with-one-owner.md)). A complete
example is in [`examples/domains/`](../examples/domains/vig.yaml).

The configuration refuses two domains on the same GPU, one endpoint used by
two domains, a domain without models, a model naming a domain that does not
exist, and `no_corun` or interference pairs across domains. `/readyz` is red
while any domain is not ready and names it (`domain gpu1: …`); requests to the
healthy domains keep running. `/metrics` keeps its series as a total and adds
`vig_domain_*{domain="…"}`.

Three things to know before you rely on it: it is **reachable, not
qualified** — the logic is tested with fake backends, not on two GPUs; a
fusion (`vig_depends_on`) must run in the domain of its parents; and shared
memory registered through the governor reaches only `backend.grpc_endpoint`,
so models on other GPUs use the copy path for now.

## Step 5 — see what it decided

```bash
curl localhost:9090/metrics
```

The numbers worth watching:

| Metric | Reads as |
|---|---|
| `vig_requests_superseded_total` | work replaced by newer data — **this is the product working**, not an error |
| `vig_requests_stale_total` | work dropped because it would have arrived too late |
| `vig_stale_compute_ratio` | share of GPU time that went into results already obsolete on arrival |
| `vig_deferred_for_protected_total` | how often the governor deliberately idled |
| `vig_best_effort_starved_total` | background work that never ran — **watch this one** |
| `vig_preemptible_dispatched_total` | background jobs that ran on a preemptible lane |
| `vig_protected_overlapped_total` | protected jobs started while a preemptible job was running |
| `vig_protected_overlap_extra_us_total` | what those overlaps actually cost against the solo profile; divided by the count above, compare it with the calibrated residual |
| `vig_protected_deadline_misses_total` | the number that should stay at zero |
| `vig_quarantined_slots` | slot credits held because the execution end is not yet proven |
| `vig_execution_reconciled_total` | ends the governor proved via the backend's own statistics rather than a timer |
| `vig_generative_prefill_us_total` | work spent re-computing the prompt of a split job — this produces **no tokens** |
| `vig_generative_decode_us_total` | work that actually produced tokens; only the ratio of the two says whether splitting still pays |
| `vig_generative_context_tokens` | longest context a continuation carried; if it grows far past what you calibrated, the sizing is no longer backed by a measurement |
| `vig_decomposition_refused_total` | jobs that ran undivided because splitting them would have cost more work than it saved |
| `vig_variant_selected_total` | how often each variant index ran, **summed over all models** — index 0 is every model's best variant |
| `vig_variant_upgrades_total` / `vig_variant_downgrades_total` | variant switches per model, by direction. Downgrades act at once, upgrades only after `variant_dwell_ms`; a stream whose upgrades climb as fast as its downgrades is oscillating — raise the dwell time |

`/healthz` answers "is the process alive". `/readyz` answers "can it currently
do anything" — they are different questions with different consequences.

## When a request is refused

Every rejection carries a machine-readable reason in the `vig-reason` gRPC
metadata header:

| Reason | gRPC code | What to do |
|---|---|---|
| `superseded` | `Aborted` | Nothing. Send the next frame. |
| `stale` | `Aborted` | Nothing. The result would have been worthless. |
| `infeasible` | `ResourceExhausted` | Retry after load drops, or lower your requirements. |
| `backend_timeout` | `DeadlineExceeded` | Check the backend; the governor is still holding that slot. |
| `execution_unknown` | `Unavailable` | The call aborted mid-flight. Your request **may or may not** have run — if it is not idempotent, treat it as possibly executed. |
| `backend_failed` | `Unavailable` | Retry may help. |
| `cancelled` | `Cancelled` | You disconnected. |

## Shutting down

`SIGTERM` (what `docker stop` and Kubernetes send) stops new traffic, answers
accepted work, and lets running inferences finish — up to a 20-second drain
deadline. An exit code other than zero means the drain did not complete, which
usually means the backend stopped answering.

## If something looks wrong

1. `vig doctor -c your.yaml` — it names the problem and the file position.
2. Check `vig_best_effort_starved_total`. If it is climbing, your protected
   streams have no headroom left; `doctor` warns about this before you start.
3. Check `vig_quarantined_slots`. If it equals your slot count, the backend has
   stopped answering and nothing can start.
4. Re-run `vig calibrate` after any driver, backend or hardware change. Profiles
   carry a manifest of the environment they were measured in — artifact digest,
   runtime, device, partitioning — and the governor names the field that no
   longer matches. Set `backend.model_repository` if the model files are
   visible to the governor; it is the only way to notice a weight file replaced
   under the same version number.

For a symptom that is not in this list, the [runbook](runbook.md) covers every
state the system reports about itself, and what to do about each.

## Still stuck?

Open a GitHub issue for bugs and questions about the software. For a commercial
licence, a supervised pilot, or anything that needs a person rather than a
tracker: **info@vigilant-crs.de** ([who we are](../IMPRINT.md)).
