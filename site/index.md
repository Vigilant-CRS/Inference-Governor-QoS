<div class="held">

<h1>One GPU. Several models. Only one of them can run now.</h1>

<p class="zeile">Vigilant is a scheduler that sits in front of your inference
server and decides, before every dispatch, whether a result will still be
useful when it is finished. Stale work is dropped before it costs GPU time.
Your models stay where they are. Your client changes one line: the address.</p>

<div class="marken">
<span>Speaks the Open Inference Protocol</span>
<span>Runs in front of Triton</span>
<span>Written in Rust, no <code>unsafe</code></span>
<span>Source-available, BUSL-1.1</span>
</div>

<div class="knoepfe">
<a class="knopf" href="docs/getting-started.md">Try it in 30 minutes</a>
<a class="knopf leer" href="docs/benchmark/README.md">Read the measurements</a>
<a class="knopf leer" href="https://github.com/Vigilant-CRS/Inference-Governor-QoS">GitHub</a>
</div>

</div>

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

Every number on this page comes from one laptop. We cannot tell you what
happens on your machine — so we give you the tool that finds out, there.

> **Start it, and in half an hour it has measured your machine and tells you
> what it can carry. And if it turns out you don't need us, it says that too.**

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
