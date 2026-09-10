# Support matrix

What is qualified, what is built but unmeasured, and what is not supported at
all. Read this before deciding whether Vigilant fits your deployment.

Three levels are used throughout, and the difference matters:

| Level | What it means |
|---|---|
| **Qualified** | Measured on this configuration. Numbers exist and are published. |
| **Built and tested** | Compiles and passes its tests here. Nothing is claimed about runtime, throughput or interference. |
| **Untested** | No measurement, no test run. Treat every number in this repository as inapplicable. |
| **Not supported** | The model in the code does not represent this case. Do not deploy. |

## Hardware

| Configuration | Level | Notes |
|---|---|---|
| RTX 3070 Laptop (8 GB), driver 580.173.02 | **Qualified** | The machine every published number comes from. It runs power-capped at 1830 of 2100 MHz; `vig doctor` reports this. |
| Other consumer Ampere (RTX 30xx desktop) | Untested | Same architecture, different power and memory behaviour. Expect the logic to hold and the numbers not to. |
| RTX A2000 / professional Ampere | Untested | — |
| Ada / Blackwell consumer or professional | Untested | — |
| Jetson Orin, Xavier | Untested | The scheduling core is built and unit-tested for `aarch64` under emulation on every push. Emulation says nothing about kernel runtime or interference. See [hardware qualification](hardware-qualification.md). |
| Datacenter accelerators (A100, L4, H100) | Untested | MIG in particular changes the model: a MIG instance *is* an independent execution unit, which the current slot model does not represent. |
| Multi-GPU | **Not supported** | The slot set models one execution unit. |
| CPU-only inference | **Not supported** | Nothing prevents it technically; nothing about it is measured, and the freshness argument assumes a contended accelerator. |

## Software

| Component | Level | Version |
|---|---|---|
| NVIDIA Triton Inference Server | **Qualified** | 2.70.0 (26.06-py3), gRPC, system shared memory |
| Triton, other 2.x versions | Untested | The protocol is stable; the statistics endpoint and the rate limiter are not guaranteed to behave identically. |
| ONNX Runtime backend in Triton | **Qualified** | The measured configuration. |
| TensorRT backend in Triton | Untested | Planned as NV-08. The profile manifest already distinguishes it (`runtime.platform`). |
| TensorRT direct, without Triton | **Not supported** | Planned as NV-09. No code today. |
| Any other inference server | **Not supported** | The backend seam exists ([ADR-0024](adr/0024-the-backend-is-a-seam-not-a-type.md)) and has exactly one implementation. |
| Linux, glibc | **Qualified** | `x86_64-unknown-linux-gnu` |
| Linux, `aarch64` | Built and tested | Cross-built and unit-tested under emulation in CI. |
| musl, Windows, macOS | Untested | No target in the release workflow. |
| Rust toolchain | — | 1.98 or newer, edition 2024 |

## Features

Four states, not two. The review of 10 September 2026 found the difference
mattering: several features were described as "off by default" when the truth
was that no documented configuration step could reach them at all
([ADR-0032](adr/0032-four-promises-that-fell-apart-between-components.md)).

| State | What it means |
|---|---|
| **Built** | Compiles, has unit tests. No product code calls it. |
| **Reachable** | A documented configuration step switches it on, and a test proves that step changes a real decision. |
| **Connected** | On by default, or on wherever its configuration block appears. Part of the normal path. |
| **Qualified** | Measured on this configuration. Numbers exist and are published. |

| Feature | State | How to switch it on |
|---|---|---|
| Freshness-aware admission, supersession, look-ahead | **Qualified** | always on |
| Variant selection by quality | **Qualified** | on where variants are interchangeable |
| Cooperative decomposition of generative jobs | **Qualified** | `cooperative:` on the model |
| Context-dependent progress cost for generative jobs | Connected | `cooperative.prefill_per_token_us`; **not measured on this machine** ([ADR-0031](adr/0031-a-re-prefill-is-not-free-progress.md)) |
| Slot credits with proof-based release | **Qualified** | always on |
| Active per-backend readiness probe | Connected | always on |
| Payload budget bound to execution, not to the client | Connected | `backend.max_inflight_mib` |
| Weakly-hard monitoring (M/K/L) | Connected | `miss_budget` on the contract |
| Minimum background progress | Reachable | `minimum_background_progress_pct` plus `consumer_period_ms` and `observation_window` |
| Weakly-hard **policy** — the budget influencing dispatch | Reachable | `backend.miss_aware_policy: true` ([ADR-0027](adr/0027-a-miss-budget-that-decides-not-only-observes.md)) |
| Application hints (action horizon, elevated, mode) | Reachable | `backend.hints:` plus a bearer token; the authority is derived from the token and printed at startup ([ADR-0029](adr/0029-a-hint-may-tighten-never-loosen.md)) |
| Clock actuation | Reachable | `backend.actuation:`; needs the permission to set clocks, which this machine does not have ([ADR-0030](adr/0030-actuation-is-an-exception-and-must-be-observed.md)) |
| State-aware runtime prediction | Built | shadow only; no configuration switches it to deciding ([ADR-0023](adr/0023-state-aware-prediction-runs-in-the-shadow-first.md)) |
| Directed interference table | Reachable | `backend.interference:`, written by `vig calibrate`; a measured pair adds its surcharge to the planned runtime, an unmeasured one adds nothing ([ADR-0026](adr/0026-interference-is-directed-and-not-additive.md)) |
| Validity-aware dependency graph | Built | needs a protocol extension ([ADR-0028](adr/0028-a-fusion-needs-a-common-capture.md)) |
| Hardware observation | **Qualified** | on where `nvidia-smi` exists; read-only, no root |
| Bearer-token authentication, mTLS | Reachable | `backend.security` |
| `trust: strict` | Reachable | `backend.trust: strict`; the default is `open` (Spec L-002) |

**Reachable is not qualified.** A feature in that column has a test proving
that its configuration changes a decision — and no measurement saying the
change is an improvement on your workload. That measurement is yours to make.

## Permissions and administration

**The governor needs no privileges.**

| Needs | Why |
|---|---|
| Network access to the backend's gRPC port | Inference and statistics |
| Read access to `/dev/shm` if shared memory is used | Passing tensors through without copying |
| Read access to the model repository, if `backend.model_repository` is set | The artifact digest ([ADR-0019](adr/0019-profile-identity-beyond-a-metadata-hash.md)) |
| `nvidia-smi` on `PATH`, optional | Hardware state ([ADR-0021](adr/0021-hardware-is-read-never-set.md)) |

**No root. No device access. No CUDA. Nothing is set:** no clocks, no
persistence mode, no power limits, no `nvpmodel`. A governor that changes the
hardware needs privileges a governor should not have — and turns every
measurement the operator makes into a measurement of the governor.

**Models are administered in the backend, not here.** Vigilant does not load,
unload, convert or version models. It reads what the backend reports and what
the operator declares. Adding a model means adding it to the backend's
repository and naming it in `vig.yaml`; the governor then refuses to start if
the two disagree (`vig doctor` says how).

## Offline operation

Everything runs air-gapped.

- **No telemetry, no phone-home, no license server.** The binary makes exactly
  two kinds of outbound connection: gRPC to the configured backends, and
  nothing else.
- **Metrics are pulled, not pushed.** `/metrics` on the configured address.
- **Release verification works offline** if you carry the artifacts and the
  signature bundle with you. `cosign verify-blob` needs network access to
  check the Fulcio/Rekor chain; for an air-gapped install, verify at the
  boundary and transfer verified artifacts inwards.
- **`cargo auditable` embeds the dependency list in the binary.** A
  vulnerability check needs no network access to the build machine —
  `cargo audit bin vig` works on the binary alone.

## What is explicitly not claimed

- **Not functionally safe.** No claim of ISO 26262 or IEC 61508 conformance,
  no safety case, no qualification evidence. Do not make a safety function
  depend on this governor.
- **No hard real-time guarantee.** The scheduler plans against measured
  profiles and a safety margin. A margin is not a bound, and a bound would
  require execution-time guarantees the underlying stack does not give.
- **Contracts are never derived from measurements.** What has to be fresh is a
  statement about what the robot needs; only the operator can make it.
- **Numbers are per configuration.** The logic is portable, the numbers are
  not. That sentence is the whole reason this page exists.

For a configuration that is not in this table and that you need qualified:
**info@vigilant-crs.de** ([who we are](../IMPRINT.md)).
