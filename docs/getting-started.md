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

**Loopback is deliberate.** There is no authentication and no TLS. If you need
it reachable from elsewhere, say so explicitly and put something in front of it
that checks identity:

```bash
vig serve -c measured.yaml --listen 0.0.0.0:9001
```

### Opening the endpoint safely

If it has to be reachable from elsewhere, turn on one of the two mechanisms —
`vig doctor` warns until you do:

```yaml
backend:
  trust: strict                    # no bypassing the governor, no self-promotion
  security:
    tls_cert:  /etc/vig/server.pem
    tls_key:   /etc/vig/server.key
    client_ca: /etc/vig/clients-ca.pem   # mTLS: clients need a certificate
    # or, without certificate management:
    token_file: /etc/vig/tokens
```

mTLS is the stronger one: a private key does not leave the device, a token
travels in every request. The token file holds one token per line; `#` starts a
comment. With either enabled, **every** endpoint requires it — including the
shared-memory registration, which is the one that matters most.

Half a configuration is refused rather than half applied: a certificate without
a key looks like protection and is none.

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

| Parameter | Meaning |
|---|---|
| `vig_age_us` | how old the input is, in microseconds |
| `vig_generation_ns` | absolute monotonic capture timestamp (alternative to the above) |
| `vig_max_age_us` | beyond this age the result is worthless |
| `vig_deadline_us` | this request's own deadline, overriding the contract |
| `vig_supersession_key` | which stream this belongs to (camera id, object id) |
| `vig_class` | requested importance class |

Without any of these the request still works — it simply uses the contract's
defaults and counts its age from arrival instead of capture.

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
| `vig_protected_deadline_misses_total` | the number that should stay at zero |
| `vig_quarantined_slots` | slot credits held because the backend stopped answering |

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
   carry a fingerprint of the environment they were measured in, and the
   governor will tell you when it no longer matches.
