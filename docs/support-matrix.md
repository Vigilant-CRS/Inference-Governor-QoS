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

Not everything in the code is meant to be switched on. Three things are built,
tested, and deliberately inert:

| Feature | State | Default |
|---|---|---|
| Freshness-aware admission, supersession, look-ahead | **Qualified** | on |
| Variant selection by quality | **Qualified** | on where variants are interchangeable |
| Cooperative decomposition of generative jobs | **Qualified** | on where `cooperative:` is configured |
| Slot credits with proof-based release | **Qualified** | on |
| Weakly-hard monitoring (M/K/L) | Built and tested | on where a `miss_budget` is configured |
| Weakly-hard **policy** — the budget influencing dispatch | Built and tested | **off** ([ADR-0027](adr/0027-a-miss-budget-that-decides-not-only-observes.md)) |
| State-aware runtime prediction | Built and tested | **shadow only** ([ADR-0023](adr/0023-state-aware-prediction-runs-in-the-shadow-first.md)) |
| Directed interference table | Built and tested | **not connected** — the table is empty until a measurement campaign fills it ([ADR-0026](adr/0026-interference-is-directed-and-not-additive.md)) |
| Validity-aware dependency graph | Built and tested | **not connected** — needs a protocol extension ([ADR-0028](adr/0028-a-fusion-needs-a-common-capture.md)) |
| Hardware observation | **Qualified** | on where `nvidia-smi` exists; read-only, no root |
| Bearer-token authentication, mTLS | Built and tested | off — `backend.security` |
| `trust: strict` | Built and tested | off; the default is `open` (Spec L-002) |

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
