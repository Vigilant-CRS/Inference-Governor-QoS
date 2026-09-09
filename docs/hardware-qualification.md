# Hardware qualification

Every performance number in this repository comes from **one machine**: an
RTX 3070 Laptop (8 GB), driver 580.173.02, Triton 2.70, Ubuntu 26.04. That is
enough to show the mechanism works. It is not enough to promise anything about
your hardware.

This document says what we know, what we do not, and what you have to run
before trusting the governor on a device we have never seen.

## What is already portable, and what is not

| | Status |
|---|---|
| Scheduling decisions on `aarch64` | **Verified in CI.** The core has no clock, no I/O and no floating point in its decision path; it is built and unit-tested for `aarch64-unknown-linux-gnu` under emulation on every push. It decides the same things. |
| Runtime, throughput, interference on `aarch64` | **Unknown.** Emulation says nothing about how fast a kernel runs or how two models slow each other down. |
| Jetson (Orin, Xavier) | **Untested.** No measurement exists. Treat every number here as inapplicable. |
| Datacenter accelerators (A100, L4, H100) | **Untested.** MIG in particular changes the slot model: a MIG instance *is* an independent execution unit, which the current model does not represent. |
| Multi-GPU | **Not supported.** The slot set models one execution unit. |

The distinction matters more than it looks: **the logic is portable, the
numbers are not.** A profile is a measurement under conditions. Change the
conditions and it is not wrong — it is no longer authoritative. The governor
enforces this itself: every profile carries a manifest of the environment —
artifact digest, runtime, device, partitioning, measurement conditions — and it
plans more conservatively when that manifest no longer matches what it sees.

## The procedure

Budget half a day per hardware configuration.

### 1. Establish the environment

Record, and keep with the results:

```bash
nvidia-smi --query-gpu=name,driver_version,memory.total,power.limit --format=csv
uname -a
docker inspect <triton> --format '{{.Config.Image}}'
```

Power mode matters on Jetson and is easy to forget:

```bash
sudo nvpmodel -q          # which power mode
sudo jetson_clocks --show # are clocks locked
```

**Lock the clocks before measuring.** An unlocked Jetson changes its clock rate
under thermal load, and a runtime profile measured across a clock change
describes neither state.

### 2. Check that the configuration fits at all

```bash
vig doctor -c your.yaml
```

It reaches the backend, confirms every model exists, checks that variants have
the same I/O signature, and warns when protected load leaves no headroom. It
starts nothing.

### 3. Measure this machine

```bash
vig calibrate -c your.yaml -o measured.yaml --samples 200 \
  --model-repository /models \
  --device "NVIDIA RTX A2000" --compute-capability 8.6 \
  --driver 560.35.03 --library-version "TensorRT 10.3.0" \
  --partition exclusive --instances 1 --rate-limiter off \
  --independent-runs 2 --valid-up-to-occupancy-pct 92
```

Measures solo runtime, mutual interference between model pairs, and — for
decomposable models — the fixed per-request cost. **Do not copy profiles
between machines.** The one time we let a stale number stand, a generation rate
was off by a factor of 4.4 and the look-ahead planned accordingly wrong for
weeks.

The flags after `--samples` are the part the inference protocol cannot tell us.
The server knows its own name and version; it does not know which GPU it runs
on, which driver, how the card is partitioned, or which bytes it loaded. What
you do not state stays `unknown` — which is honest, and different from
`verified`. `--model-repository` is what makes the artifact digest possible;
without it, a weight file swapped under the same version number stays
invisible.

Run it twice. If p95 differs by more than about 10 % between runs, something
else was using the GPU. When you merge two runs into one profile, say so with
`--independent-runs 2`: two hundred samples from one process start are not the
same evidence as two hundred from two.

### 4. Establish the baseline

Before comparing anything, measure your streams against your backend **without**
the governor. That is the number you have to beat, and it is the one that moves
most between machines.

### 5. Run the comparison

```bash
taskset -c <reserved cores> gate-m3 measured.yaml
```

Rules that make the result mean something:

- **Reserve cores.** A latency benchmark that shares cores with a compiler
  measures the compiler. One of our runs produced 52 % instead of 98 % on
  unchanged code for exactly this reason.
- **Machine at rest.** Start with a load average below 2.
- **Repeat anything that affects a claim.** Once is an anecdote.
- **Tune the baseline, do not weaken it.** Same models, same instance groups,
  same data path, rate limiter enabled. If the governor only wins against a
  badly configured server, it does not win.
- **Report the losers.** Our own headline table contains a stream at 0 %
  coverage. Leaving it out would have made the result look better and been
  worth less.

### 6. Soak

```bash
soak measured.yaml    # eight hours, alternating load
python3 tools/soak-report.py <output dir>
```

What you are looking for: RSS growth, drift in the learned safety margins,
coverage degrading over time. Ours grew 109 kB/h after the first hour and the
margins returned to their starting values.

### 7. Write down what you found

Including the runs you discarded and why. `docs/benchmark/` contains ours,
mistakes included — that is what makes the rest of it credible.

## What would make us call a platform qualified

1. Runtime profiles measured on that platform, with locked clocks and a
   recorded power mode.
2. A tuned baseline measured on the same machine on the same day.
3. Two independent gate-m3 runs agreeing within 10 %.
4. An eight-hour soak with no memory growth after the first hour and no margin
   drift.
5. A written record of the environment, and of every discarded run.

Until those five exist for a platform, that platform is untested — and we will
say so rather than extrapolate.

## Before you configure variants: check they are interchangeable

The governor picks the variant per request and does not tell the client. That
freedom requires every variant to serve the same interface. Check before you
configure, not after a switch breaks a client in the field:

```bash
tools/onnx-signature.py model_a.onnx model_b.onnx
```

It reads the ONNX graph directly — no `onnx` or `onnxruntime` needed, and the
weights are never read, so a 133 MB model costs milliseconds. Exit code 0 means
the signatures match, 1 means they do not.

Two real examples from our own model sets, both of which would have silently
broken a client:

```
detector_large/1/model.onnx   out  resnetv17_dense0_fwd:FP32[?,1000]
detector_small/1/model.onnx   out  resnetv15_dense0_fwd:FP32[?,1000]
```

Identical shape, different output name — a client that asks for the large
variant's output by name gets a backend error after a switch.

```
rfdetr.onnx            in input:FP32[1,3,512,512]   out labels:FP32[1,300,10]
rfdetr_768.onnx        in input:FP32[1,3,768,768]   out labels:FP32[1,300,10]
detector_23cls.onnx   in input:FP32[1,3,768,768]   out labels:FP32[1,300,24]
rfdetr_28cls_4_large.onnx in input:FP32[1,3,768,768]   out labels:FP32[1,300,29]
```

Four generations of the same detector: different input resolutions and 10, 24
and 29 classes. None of them are interchangeable, and the governor switches
automatic selection off for such a set — correctly.

Note what the tool **cannot** tell you. If two of those models had the same
class count in a different order, the shapes would match and the meaning would
not. That gap closes only with a declared `io_signature` and your own
statement that the variants mean the same thing.

## Known model limits

- **The slot set models one execution unit.** MIG instances and multiple GPUs
  are genuinely independent units; representing them needs a slot set per unit
  and a routing decision above it. Neither exists yet.
- **`no_corun` is a declaration, not a measurement.** `vig calibrate` proposes
  pairs from measured mutual slowdown, but the threshold is a judgement call.
- **Unified memory changes the data-path arithmetic.** On Jetson, host and
  device share physical memory; the 89 % transport overhead we measured for a
  6.2 MB tensor over gRPC will look different, probably better. Nobody has
  measured it.
