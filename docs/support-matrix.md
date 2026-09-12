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
| RTX 3070 Laptop (8 GB), driver 580.173.02 and 580.178.04 | **Qualified** | The machine every published number comes from. Most numbers were taken under 580.173.02, where the card ran power-capped at 1830 of 2100 MHz. Gate M3 was repeated under 580.178.04 on 11 September and holds ([report](benchmark/gate-m3-r04.md)); `vig doctor` reports the current clock state. |
| Other consumer Ampere (RTX 30xx desktop) | Untested | Same architecture, different power and memory behaviour. Expect the logic to hold and the numbers not to. |
| RTX A2000 / professional Ampere | Untested | — |
| Ada / Blackwell consumer or professional | Untested | — |
| Jetson Orin, Xavier | Untested | The scheduling core is built and unit-tested for `aarch64` under emulation on every push. Emulation says nothing about kernel runtime or interference. See [hardware qualification](hardware-qualification.md). |
| Adreno 540 (Pixel 2, Android 11) through `vig-tflite-server` | Measured, **not a target** | A second, isolated GPU for the logic, not a platform: a 2017 phone with passive cooling. Gate-M3 analogue on the device, backend direct against the governor ([android-gpu.md](benchmark/android-gpu.md), [ADR-0039](adr/0039-a-second-backend-proves-the-seam.md)). Its numbers transfer to no other device |
| Datacenter accelerators (A100, L4, H100) | Untested | MIG in particular changes the model: a MIG instance *is* an independent execution unit, which the current slot model does not represent. |
| Multi-GPU | Reachable, **not qualified** | One scheduler per GPU (`backend.domains`, [ADR-0037](adr/0037-a-domain-is-a-gpu-with-one-owner.md)). The logic is tested with fake backends; no second GPU has been measured, and whether two GPUs slow each other through PCIe, host memory or a shared power budget is unknown. |
| CPU-only inference | **Not supported** | Nothing prevents it technically; nothing about it is measured, and the freshness argument assumes a contended accelerator. |

## Software

| Component | Level | Version |
|---|---|---|
| NVIDIA Triton Inference Server | **Qualified** | 2.70.0 (26.06-py3), gRPC, system shared memory |
| Triton, other 2.x versions | Untested | The protocol is stable; the statistics endpoint and the rate limiter are not guaranteed to behave identically. |
| ONNX Runtime backend in Triton | **Qualified** | The measured configuration. |
| TensorRT backend in Triton | **Qualified** | Measured as NV-08 with the same weights as TensorRT engines: detector runtime 15.6 → 11.5 ms, the governor's advantage halves (24.7× → 13.3×) because the baseline improves ([tensorrt.md](benchmark/tensorrt.md)). No code path of its own. |
| TensorRT direct, without Triton | **Not supported** | NV-09 spike measured: 500–730 µs gained per inference, almost entirely Triton's I/O copies. Not built into the product — see [ADR-0033](adr/0033-native-code-lives-in-the-backend-process.md). |
| XSched preemption under Triton (Level 2; TSG on Level 3) | **Qualified** on this machine | Runs under Triton 26.06 (CUDA 13.3) on sm86 with `CUXTRA_CUDA_LIB` and a nine-line patch — setup and pitfalls in [deploy/xsched](../deploy/xsched/README.md). With two Triton processes it keeps the protected streams at 100 % and lets the background VLM run ([measurement](benchmark/messkette-2026-09-11.md#xsched-und-präemption-nv-15)). TSG needs Level 3; the start script derives the level from the implementation since `64c9d06` |
| TFLite with the GPU delegate (Android), `vig-tflite-server` | Built and tested, measured on one device | A second backend behind the unchanged governor: its own process and workspace (`backends/android-tflite`), TFLite 2.16.1, GPU delegate V2 over GLES, copy path only, `backend.type: oip` ([ADR-0039](adr/0039-a-second-backend-proves-the-seam.md), [measurement](benchmark/android-gpu.md)). `vig calibrate` runs against it and measures the directed interference; the number of slots must match the backend's concurrency — one thread per model here, so `slots: 1` gives away the overlap the backend uses ([second operating point](benchmark/android-gpu.md#der-zweite-betriebspunkt-zwei-slots)) |
| OpenVINO Model Server | Built and tested | Answered 150 of 150 requests through the governor without a code change ([portability.md](benchmark/portability.md)); not measured under load |
| Any other inference server | Untested | Anything that answers the OIP subset the governor uses (live, ready, metadata, infer, statistics) is a backend ([ADR-0024](adr/0024-the-backend-is-a-seam-not-a-type.md), [ADR-0039](adr/0039-a-second-backend-proves-the-seam.md)); nothing else has been tried |
| Linux, glibc | **Qualified** | `x86_64-unknown-linux-gnu` |
| Linux, `aarch64` | Built and tested | Cross-built and unit-tested under emulation in CI. The decision path was also measured on real ARM cores (Pixel 2 and Pixel 5, A53 to A76 class): p99 3–20 µs per scheduling event for the Gate M3 model set ([arm-phones.md](benchmark/arm-phones.md)). Inference on ARM was measured once, on the Pixel 2's GPU with the governor running on the phone ([android-gpu.md](benchmark/android-gpu.md)). |
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
| Freshness-aware admission, supersession, look-ahead | **Qualified** | always on. Since [ADR-0036](adr/0036-the-look-ahead-counts-from-the-capture.md) the look-ahead counts deadlines from the capture, sees the next arrival of every guarded stream however far away, and keeps a late frame pending within the contract's `release_jitter_ms`. On the Gate M3 load (no jitter envelope, periods ≤ 66 ms) that moves each protected deadline earlier by the observed time from capture to arrival (on the shared-memory path alone ~160 µs) and changes nothing else; the qualifying measurements ran before the change |
| Variant selection by quality | **Qualified** | on where variants are interchangeable |
| Cooperative decomposition of generative jobs | **Qualified** | `cooperative:` on the model |
| Context-dependent progress cost for generative jobs | **Qualified** | `cooperative.prefill_per_token_us`; measured against vLLM (Qwen): 0–5 µs per context token with prefix caching, 35–39 µs without ([measurement](benchmark/nv16-prefill.md), [ADR-0031](adr/0031-a-re-prefill-is-not-free-progress.md)) |
| Slot credits with proof-based release | **Qualified** | always on |
| Active per-backend readiness probe | Connected | always on |
| Payload budget bound to execution, not to the client | Connected | `backend.max_inflight_mib` |
| Weakly-hard monitoring (M/K/L) | Connected | `miss_budget` on the contract |
| Minimum background progress | Reachable | `minimum_background_progress_pct` plus `consumer_period_ms` and `observation_window` |
| Weakly-hard **policy** — the budget influencing dispatch | Reachable | `backend.miss_aware_policy: true` ([ADR-0027](adr/0027-a-miss-budget-that-decides-not-only-observes.md)) |
| Look-ahead protecting the **supply**, not only the deadline | Reachable | `backend.protect_supply: true` ([ADR-0041](adr/0041-the-look-ahead-protects-the-supply-not-only-the-deadline.md)). Holds background work back while the previous result would expire before the next protected frame lands. Costs background progress; reproduced in the simulator, not yet measured on the GPU |
| Application hints (action horizon, elevated, mode) | Reachable | `backend.hints:` plus a bearer token; the authority is derived from the token and printed at startup ([ADR-0029](adr/0029-a-hint-may-tighten-never-loosen.md)) |
| Clock actuation | Reachable | `backend.actuation:`; needs the permission to set clocks, which this machine does not have ([ADR-0030](adr/0030-actuation-is-an-exception-and-must-be-observed.md)) |
| State-aware runtime prediction | Reachable | `backend.prediction: active`; the default is `shadow`. Needs hardware observation, which `vig serve` starts — without it every cell falls back to the profile ([ADR-0023](adr/0023-state-aware-prediction-runs-in-the-shadow-first.md)). **Do not switch it on from a release before `cebb582`:** it planned without the safety margin and, on the Gate M3 load, broke the protected streams' coverage ([measurement](benchmark/nv06-ab.md)) |
| Self-calibrating plan | Reachable | `backend.margin_learning: {}` ([ADR-0038](adr/0038-the-plan-calibrates-to-the-card.md)). Learns a factor per GPU, times a residual per model, between profile p99 and measured runtime, aiming at 1 % overruns; it may go below 100 % when the profile is pessimistic, never below the observed median or `min_factor_percent`. Refused together with `prediction: active`. Not measured on hardware yet, and the learned factor does not survive a restart |
| Directed interference table | Reachable | `backend.interference:`, written by `vig calibrate`; a measured pair adds its surcharge to the planned runtime, an unmeasured one adds nothing ([ADR-0026](adr/0026-interference-is-directed-and-not-additive.md)) |
| Validity-aware dependency graph | Reachable | the client sends `vig_capture_id` and `vig_depends_on` ([how](getting-started.md#results-from-the-same-capture)); a fusion across captures is refused before it runs ([ADR-0028](adr/0028-a-fusion-needs-a-common-capture.md)). Every terminal path closes its node; a completed result is kept for its model's `max_age` (100 ms – 5 s). Rejections carry `vig-reason` (`graph_full`, `capture_mismatch`, …). Releases up to 0.2.0 refused every request with `vig_capture_id` after 256 captures per process (found live by the [ROS 2 bridge](integrations/ros2.md), [fixed](security.md#not-a-security-finding-nv-17)); do not send `vig_capture_id` to them |
| Preemptible background lane | Reachable | `backend.preemptible_lanes` plus `preemptible: { residual_blocking_us, source }` on a model served by its own low-priority backend process ([ADR-0035](adr/0035-preemption-is-a-measured-backend-property.md)). Needs a preemption layer in the backend such as [XSched](../deploy/xsched/README.md); the residual blocking R must be measured with `vig calibrate`, and `vig doctor` warns while it is declared. Measured on the GPU with a **declared** R of 4 ms: every stream at 100 %, including the VLM, level with Triton + XSched ([measurement](benchmark/messkette-2026-09-11.md#xsched-und-präemption-nv-15)). `vig calibrate` could not measure R on this power-capped laptop (the clock wandered during every series) |
| Multiple resource domains (GPUs) | Reachable | `backend.domains.<name>` with `gpu_index`, `grpc_endpoint`, `slots`; `domain:` on a model ([how](getting-started.md#two-gpus), [ADR-0037](adr/0037-a-domain-is-a-gpu-with-one-owner.md)). One scheduler per GPU, fixed assignment, no failover. Not qualified: needs a second GPU. Fusions must stay within one domain; shared memory registered through the governor reaches only `backend.grpc_endpoint` |
| Hardware observation | **Qualified** | started explicitly by `vig serve`, not by the scheduler; read-only, no root. Without it the governor plans without device state ([ADR-0022](adr/0022-measurement-is-a-method-not-a-loop.md)) |
| Bearer-token authentication, mTLS | Reachable | `backend.security`; threat model, deployment checklist and the review findings in [security.md](security.md). `vig serve` refuses a non-loopback address without mTLS or tokens; administration endpoints stay closed until `admin_token_file` is set |
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
