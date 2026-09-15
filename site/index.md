<div class="held">

<h1>Stop computing the past.</h1>

<p class="zeile">Vigilant is a scheduler for AI inference on central compute:
when vision, planning and language models share one chip on a robot or a
vehicle, it keeps the results that matter fresh. It sits in front of your
inference server and decides, before every dispatch, whether a result will
still be useful when it is finished.</p>

<p class="zeile"><code>vig autotune</code> tunes it for your hardware: it
measures your models, tries the governor's settings against your contracts and
keeps only what holds up in a back-to-back rerun.</p>

<div class="marken">
<span>Qualifies itself on your hardware</span>
<span>Speaks the Open Inference Protocol</span>
<span>Runs in front of Triton</span>
<span>Written in Rust, no <code>unsafe</code></span>
<span>Source-available, BUSL-1.1</span>
</div>

<div class="knoepfe">
<a class="knopf" href="#video">Watch the video</a>
<a class="knopf leer" href="#qualify-your-own-hardware">Measure your machine: vig autotune</a>
<a class="knopf leer" href="docs/use-cases.md">Use cases</a>
<a class="knopf leer" href="docs/getting-started.md">Getting started</a>
<a class="knopf leer" href="docs/benchmark/README.md">Read the measurements</a>
<a class="knopf leer" href="https://github.com/Vigilant-CRS/Inference-Governor-QoS">GitHub</a>
</div>

</div>

<div class="video" id="video">
<video controls preload="metadata" playsinline poster="assets/video-poster.png">
<source src="assets/vigilant-inference-governor.mp4" type="video/mp4">
<track kind="subtitles" srclang="en" label="English" src="assets/vigilant-inference-governor.en.vtt">
</video>
</div>

## See it: four cameras, one GPU

A vehicle, a sidewalk robot and a humanoid each run four cameras through
RF-DETR at 30 frames per second next to a vision-language model — on one laptop
GPU, more work than the chip can do. Left, NVIDIA Triton computes every frame in
arrival order, and each result is a third of a second old when it arrives.
Right, the governor keeps the camera that matters fresh in every cycle, serves
the other cameras with what is left — and the language model waits. Each pair is
recorded back to back with the same frames; boxes are drawn where the detector
saw the objects.

### Vehicle, city driving — front camera fresh in 100 % of cycles instead of 0.2 %

<div class="video">
<video controls preload="none" playsinline poster="assets/demo-four-cameras.jpg">
<source src="assets/demo-four-cameras.mp4" type="video/mp4">
</video>
</div>

### Sidewalk robot — front camera fresh in 100 % of cycles instead of 0.2 %

<div class="video">
<video controls preload="none" playsinline poster="assets/demo-sidewalk-robot.jpg">
<source src="assets/demo-sidewalk-robot.mp4" type="video/mp4">
</video>
</div>

### Humanoid robot — head camera fresh in 99.8 % of cycles instead of 0.1 %

<div class="video">
<video controls preload="none" playsinline poster="assets/demo-humanoid-robot.jpg">
<source src="assets/demo-humanoid-robot.mp4" type="video/mp4">
</video>
</div>

Where the GPU is not overloaded, the governor does not help — one camera plus
the language model ran as well or better without it. All results, the price for
the other cameras and the language model, and the configurations:
[demo measurements](docs/benchmark/demo-2026-09-15.md).

## The problem

A camera gives you a frame every 33 ms. Your detector needs 15 ms. That fits —
until a language model, a depth network or a second camera wants the same GPU.
Now the detector waits behind a job that cannot be interrupted, and by the time
its answer arrives, the robot has already moved.

An inference server schedules requests fairly. What it cannot know is that
frame four became worthless the moment frame five existed. It will compute an
answer nobody can use any more, with GPU time the fresh frame needed.

## What the governor does

<div class="karten">
<div class="karte">
<h3>Drops superseded work</h3>
<p>A newer frame from the same camera replaces an older one that is still
waiting. The old result would have been discarded anyway — now it costs
nothing.</p>
</div>
<div class="karte">
<h3>Refuses work that would arrive too late</h3>
<p>If the answer would be stale when it is finished, it is not started, and the
client is told immediately. A late answer is not a slow answer. It is a wrong
one.</p>
</div>
<div class="karte">
<h3>Holds back background work</h3>
<p>When a protected stream is due within the next few milliseconds, a long job
does not start. This is the one place the governor deliberately leaves the GPU
idle — and the reason the camera stays fresh.</p>
</div>
<div class="karte">
<h3>Picks the variant that still fits</h3>
<p>Under pressure a smaller, faster model variant is chosen rather than missing
the deadline, and it switches back when the pressure is gone.</p>
</div>
</div>

## What it measured

One RTX 3070 Laptop, Triton 2.70, real models, against a **tuned** Triton with
priorities and the same shared-memory path — not a strawman. Coverage is the
share of control cycles in which a fresh enough result was available.

| Stream | Triton (tuned) | Vigilant | |
|---|---:|---:|---|
| detector | 84–85 % | **99 %** | [20–22× fewer uncovered cycles](docs/benchmark/gate-m3-r04.md) |
| pose | 91 % | **99 %** | [10–12× fewer](docs/benchmark/gate-m3-r03.md) |
| depth | 97–98 % | 90–100 % | no gain — and we do not count it as one |

**And what it costs, in the same run:** the background language model gets
**0 % coverage**. A 95 ms block does not fit next to a 33 ms period, with or
without a governor. The difference is that the governor decides which side
loses and says so. For models that can be split, the trade becomes visible:
[twenty times more background progress for seven points of detector
coverage](docs/benchmark/wp26.md).

<div class="karten">
<div class="karte">
<h3 class="gut">Corrected, and re-measured</h3>
<p>At exactly 100 % load the governor used to drop work that Triton still
served — our bug, not the approach: it waited for each answer before sending
the next request. With pipelining it is
<a href="docs/benchmark/messkette-2026-09-12.md">17 ‰ instead of 188 ‰</a>.</p>
</div>
<div class="karte">
<h3 class="warn">Preemption is not free</h3>
<p>With XSched, a tuned Triton alone already keeps every stream at 100 %, and
the shim costs the protected path
<a href="docs/benchmark/pilot-praemption-2026-09-12.md">17–20 % runtime</a>.
We publish that next to the benefit.</p>
</div>
<div class="karte">
<h3>A second GPU, a second backend</h3>
<p>The same governor, unchanged, in front of TFLite on a phone GPU
(<a href="docs/benchmark/android-gpu.md">Adreno 540</a>). The logic travels.
The advantage does not — where transport and CPU dominate, the backend's own
overlapping wins.</p>
</div>
</div>

## When not to use it

| Your situation | What we recommend |
|---|---|
| GPU below saturation | **No governor.** Your server is fine; we would cost you 0.8 % of control cycles. |
| A single stream | **Fifty lines in your client.** Keep only the newest frame — that gets most of the benefit. |
| The bottleneck is transport or CPU, not GPU time | **No governor.** Measured on a phone GPU: the backend overlaps better than we serialise. |
| Several streams of different importance, above saturation | **This is what it is for.** |

Break-even is between 100 % and 110 % offered load
([load ramp](docs/benchmark/load-ramp.md)).

## Qualify your own hardware

Our numbers come from our hardware — a laptop GPU and two Android devices. What
happens on yours is a measurement, and we give you the tool that takes it,
there.

> **Start it, and in half an hour it has measured your machine and tells you
> what it can carry. And if it turns out you don't need us, it says that too.**

On our laptop, the run of 15 September answered in under two minutes: above
90 % load the direct path misses **996 ‰** of the protected stream's cycles,
the governor **0 ‰** — and the lower-priority streams pay for all of it
(559 ‰ → 1000 ‰). It kept two of four measurement series, threw two away
because the power-capped GPU changed state mid-series, and therefore **refused
to sign off**. On a Pixel 2 and a Pixel 5 it ran clean, twelve of twelve series
each, and derived the same configuration structure on both. What the governor
is worth there depends on the load: with slow contracts at about 35 % planned
utilisation, **no governor needed**. With a delivery-robot load that saturates
the phone, the detector misses **293 ‰** of its cycles on the direct path and
**99 ‰** under the governor on the Pixel 2 (**497 ‰ → 208 ‰** on the Pixel 5),
paid for by the lower-priority streams — and at the highest load point on the
Pixel 5 the governor is the worse choice for the detector
([use cases](docs/use-cases.md), [report](docs/benchmark/validierung-autotune.md)).

```bash
vig autotune --endpoint 127.0.0.1:8001 -c vig.yaml -o qualification
```

One command runs the whole chain on your hardware: it reads your models from
the server you already run, measures runtimes, real backend concurrency and
directed interference, answers whether the governor is worth it on your load,
and checks the configuration it produced. What it leaves behind is a frozen
configuration and a report — Markdown and JSON — stating what was measured,
under which conditions, what was discarded and why, and what explicitly does
not hold.

**It cannot issue a qualification, only refuse one or leave it open.** That is
deliberate. A discarded measurement series stays discarded and the value it
would have produced stays unset; a step that ran while something else used the
machine is marked contaminated; nothing that was not measured is made to look
measured. And "the governor brings you nothing on this load" is one of its
normal results, printed as the headline rather than buried
([ADR-0044](docs/adr/0044-qualification-happens-at-the-users-site.md)).

It runs wherever the governor runs, including `aarch64` in front of a TFLite
backend. Where there is no `nvidia-smi` there is no clock to observe, and the
report says so instead of failing or quietly measuring something else.

The individual steps remain, for anyone who wants to drive them:

```bash
vig doctor  -c vig.yaml                 # check without starting anything
vig profile -c vig.yaml -o measured.yaml # measure this machine
vig serve   -c measured.yaml             # the client only changes its address
```

Full walkthrough: [getting started](docs/getting-started.md). What is
qualified, what is merely built: [support matrix](docs/support-matrix.md).
What is still missing: [status](docs/STATUS.md).

## Reproduce it on your machine

Every number above comes from models you do not have: an in-house detector and
ResNet stand-ins chosen for their shapes and runtimes. That is defensible for a
scheduling claim — the scheduler sees runtimes, not weights — and useless if
you want to check the claim yourself.

So there is a second path that uses only models anyone can download:

```bash
cargo build --release --workspace
tools/repro/run.sh
```

RT-DETR R18 and R50 as the detector pair, Qwen3-0.6B as the language model —
all Apache-2.0, every digest pinned, and the script refuses to measure if one
of them does not match. It **measures the runtime profiles on your machine**
instead of reusing ours, because a profile is only valid for the hardware it
was taken on.

What it costs you: one command and about fifteen minutes of measuring for the
detector cases, roughly twenty-two with the language model. The first run also
pays for the downloads, and those dominate: 258 MB of models, 1.5 GB more for
the language model, and the container images — 22.7 GB for Triton, another
35.3 GB for vLLM. Our own quarter of an hour was measured on a machine that
already had all of them, which is exactly the kind of number you should not
have to discover for yourself. What it gives you: the three load cases, the
consumer-side view next to the window view, and the same verdict tool an
evaluation would use — including the verdict "you do not need this", which is
one of its normal answers.

**And here is what it found, so that your own machine is not the first to tell
you.** RTX 3070 laptop, three runs per case, 14.09.2026: with a detector beside
a language model the direct path won outright — 100 % against 33 % detector
coverage, *and* more background reports. With two detector sizes side by side
the direct path won as well (99–100 % against 89–94 %). Four cameras on one
detector could not be calibrated on that card at all. `vig-fit` printed its
verdict "you do not need this" in two of three runs.

That does not contradict the table at the top of this page — it marks its edge,
and the edge is the honest part. These public pairs land at 47 % and 91 % of
one GPU slot; Gate M3 measures at 103 %, and the knee sits between 100 and
110 %. Below saturation there is nothing to arbitrate, so the governor only
costs its own overhead, and the counters say so plainly: nothing superseded,
nothing stale, nothing held back.

So what the reproduction actually proves is narrower than we would like, and
still worth something: **the tools run on models we did not choose, and they
report their own limit instead of flattering us.** What stays open is the
claim above saturation with public models — for that we still need a pair that
pushes an 8 GB laptop card past 100 % without the profile measurement failing
on a wandering clock. The gap is named in the walkthrough, not hidden in it.

Details, and the two pitfalls that cost us an afternoon:
[reproduce it on your machine](docs/benchmark/reproduce.md).

## Licence and contact

Free, without a time limit, for evaluation, development, testing,
benchmarking, research, teaching and CI — including inside a company and on
production hardware. Free in production on **up to three devices**. Beyond
that, or inside a product you ship to someone else, a commercial licence
applies; every version converts to Apache-2.0 four years after its release.
The details are in [licensing](LICENSING.md).

Questions, a pilot, or an unsure case: **info@vigilant-crs.de** —
[Vigilant e.K., Stuttgart](IMPRINT.md).

## Honest about what this is not

This is not a hard-real-time runtime and not a safety-certified system. It is a
component — you integrate it, you qualify it on your hardware, the way you
would with any other infrastructure part. Every performance figure on this site
comes from one machine, and the reports name the runs we threw away. There is
no pilot customer yet. Read the
[status page](docs/STATUS.md) before you believe the benchmark table.
