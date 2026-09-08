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
enforces this itself: every profile carries a fingerprint of the environment,
and it plans more conservatively when the fingerprint no longer matches.

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
vig calibrate -c your.yaml -o measured.yaml --samples 200
```

Measures solo runtime, mutual interference between model pairs, and — for
decomposable models — the fixed per-request cost. **Do not copy profiles
between machines.** The one time we let a stale number stand, a generation rate
was off by a factor of 4.4 and the look-ahead planned accordingly wrong for
weeks.

Run it twice. If p95 differs by more than about 10 % between runs, something
else was using the GPU.

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
