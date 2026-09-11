# Datapath budgets

How much time the governor may add to a single request, and how to check that
before a release. A release that exceeds a budget is not released
([NV-20](roadmap/2026-09-09-vnext/05-arbeitspakete.md), Gate G4).

## What a budget is

The governor sits between client and backend. Every request crosses it twice,
and that costs time that has nothing to do with scheduling. A **datapath
budget** caps that cost: the governor's overhead compared with a direct call
to the same backend, per data path and payload size.

Three limits per point, each of which must hold on its own:

| Limit | Meaning |
|---|---|
| p50 overhead | median governed latency minus median direct latency |
| p99 overhead | 99th percentile governed minus 99th percentile direct |
| share of the direct call | p50 overhead divided by the direct p50 — judged only when the direct call takes at least 6 ms |

The overhead is a difference of percentiles of two series, not a per-request
difference: a request runs either directly or through the governor, never
both. If the governed side happens to be faster, the overhead counts as zero,
not as a credit.

## The budgets

The numbers live in exactly one place,
[`crates/vig-gateway/src/datapath_budget.rs`](../crates/vig-gateway/src/datapath_budget.rs).
Both checks read them from there. If this page and the code disagree, the
code is right and this page is out of date.

| Path | Payload | p50 overhead | p99 overhead | Share | Basis |
|---|---|---:|---:|---:|---|
| Shared-memory reference | 150 KB, 1.2 MB, 6.2 MB | ≤ 300 µs | ≤ 1,000 µs\* | ≤ 5 % | measured +160 µs at 6.2 MB, 2 % ([data-plane.md](benchmark/data-plane.md)) |
| gRPC copy | 150 KB | ≤ 500 µs | ≤ 1,500 µs\* | ≤ 5 % | measured +238 µs, 2 % |
| gRPC copy | 1.2 MB | not budgeted | | | measured +2,559 µs, 26 % |
| gRPC copy | 6.2 MB | not budgeted | | | measured +11,692 µs, 89 % |

\* **The p99 limits are assumptions.** The data-plane measurement reported
means, not tails. The first qualification run confirms or corrects them — and
a correction is a code change with a reason, not a release-day decision.

**Why 5 %.** The specification sets less than 3–5 % end-to-end regression as
the product gate and more than 5 % as a kill criterion (Spec 4.4, 19.8).

**Why the share is only judged from 6 ms.** The kill criterion is about the
end-to-end regression of a pipeline, not about a single very short call. For a
2 ms model, 160 µs is already 8 % — a statement about the model, not a
regression of the governor. At 6 ms the absolute limit of 300 µs and the 5 %
coincide; below that the absolute limit decides alone.

**Why the shared-memory budget is the same for every size.** The promise of
[ADR-0003](adr/0003-shared-memory-passthrough.md) is that the governor never touches tensor data, so its cost
does not grow with the frame. A governor that copies after all costs about
2.5 ms at 1.2 MB and 11.7 ms at 6.2 MB and fails the budget by far. At
150 KB a copy would cost only about 240 µs and stay under the limit — timing
cannot see it there. That case is caught by a structural test instead, which
runs in every test run: the backend must never receive payload bytes on the
shared-memory path.

**Why camera frames on the copy path have no budget.** A budget that allowed
89 % overhead would be a statement that 89 % is acceptable. It is not: camera
frames belong on the shared-memory path. The copy path at these sizes is
still measured and printed, so that it does not get worse unobserved — it
just cannot pass or fail.

## Running the checks

Build first, then run on a **quiet machine** — not next to a build and not on
a shared CI runner. A latency measurement next to a compiler measures the
compiler. Pin to reserved cores.

**1. Structural check — every test run, no GPU:**

```bash
cargo test -p vig-gateway --test end_to_end the_shm_path_never_carries_the_payload
```

**2. Budget check against a mock gRPC backend — release qualification, no GPU:**

```bash
taskset -c 8-15 cargo test --release -p vig-gateway --test end_to_end \
  datapath_budgets_hold -- --ignored --nocapture
```

It is `#[ignore]` because timing on a shared runner is noise. It measures
every row of the table against a backend with 5 ms compute time, prints
direct and governed p50/p99, the overhead and a verdict per row, and fails if
any budgeted row fails.

**3. Budget check against real Triton — on the release machine:**

```bash
taskset -c 8-15 target/release/shm-latency 127.0.0.1:8001 pose_main 300
```

Arguments: Triton endpoint, model, number of paired runs. It measures the
model's input over shared memory, directly and through an in-process
governor, alternating request by request, and judges the result against the
shared-memory budget. Exit code 1 means FAIL.

## What a FAIL means

A FAIL blocks the release until it is explained.

1. **Re-run it on a quiet machine**, three times. A single FAIL next to other
   work proves nothing; a FAIL that reproduces in two of three runs is real.
2. **A real FAIL is a regression** until shown otherwise. Look first at what
   changed on the data path: an extra copy, a new allocation per request, a
   lock on the request path, logging per request.
3. **Raising a budget** is a code change in `datapath_budget.rs`, with the
   measurement that justifies it in the comment next to the number. It is not
   done to get a release out.

## Scope

These numbers are for this machine class: an x86 laptop (RTX 3070 Laptop
class), loopback transport, governor and backend on the same host. They do
not carry over to a Jetson, to a governor on another host than the backend, or
to a network path. There, measure with the same two checks and set budgets
for that configuration — do not reuse these.
