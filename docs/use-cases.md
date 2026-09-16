# Use cases

A result of the governor only means something against a contract that means
something. "The detector misses 99 ‰ instead of 293 ‰" is a statement
about *these* periods and freshness limits — so every measured load below
starts from the application: what moves, how fast, and how old an answer may
be before acting on it is wrong.

Each use case names its streams, derives the contract, says which models ran
where, and what was measured. Where a model is a stand-in, it says so.

## How to derive a contract

1. **Freshness from motion.** `max_age_ms` is the time the system may act on an
   old answer: *allowed blind travel ÷ speed*. A robot at 0.5 m/s that may move
   at most 37 cm on a stale obstacle detection gets `max_age_ms: 740`.
2. **Period from freshness.** With `period_ms` at half of `max_age_ms`, one
   missed cycle is survivable and two are a miss. That is the convention in
   every example here.
3. **Class from consequence.** `protected` is what stops the machine or keeps
   people safe. `high` is what the task needs to go on. `best_effort` is what
   may wait for a quiet moment — scene descriptions, reports, logging.
4. **Check it.** `vig doctor -c vig.yaml` says whether the protected contracts
   fit the measured runtimes at all (`NOT_READY` means no scheduler can serve
   them — change the contract or the hardware, not the governor). Then
   `vig autotune` measures, tunes and says whether the governor pays off on
   that load.

## 1. Mobile robot or humanoid: perception next to a vision-language model

*One GPU, camera at 30 fps, a long non-interruptible model on the same chip.*

| Stream | Model | Class | Contract | Why |
|---|---|---|---|---|
| detector | RF-DETR, 512 px (real) | protected | period 33 ms, max age 66 ms | one camera frame; at 1 m/s the machine moves ≤ 6.6 cm on a stale detection |
| pose | pose model | high | period 33 ms, max age 66 ms | tracking people near the arm |
| depth | ResNet-50, batch 4 | high | period 66 ms, max age 132 ms | free-space update every other frame |
| vlm | **stand-in:** ResNet-50, batch 48, ~100 ms non-interruptible | best effort | deadline 800 ms | behaves on the GPU like a VLM prefill block; a real VLM would be longer |

Config: [`examples/gate_m3/vig.yaml`](../examples/gate_m3/vig.yaml).
Measured on an RTX 3070 Laptop GPU against a tuned Triton: the detector answers
in time in **99 %** of control cycles instead of 84–85 %, pose 99 % instead of
91 % ([acceptance run](benchmark/abnahme-2026-09-12.md)). `vig autotune` on
this load: the hand-tuned settings were already the best of the settings tried.

Seen as a video — head, chest and two wrist cameras plus a vision-language model
on one laptop GPU: the head camera stays fresh in 100 % of cycles instead of
0.1 %, while the language model keeps answering from its runtime budget ([humanoid demo](benchmark/demo-2026-09-15.md)).

## 2. Indoor service or delivery robot on a phone-class SoC

*Two GPU slots on an Adreno GPU, three vision models, walking pace near people.*

| Stream | Model | Class | Contract | Why |
|---|---|---|---|---|
| detector | EfficientDet-Lite0 with NMS (real) | protected | period 370 ms, max age 740 ms | people and obstacles; at 0.5 m/s the robot moves ≤ 37 cm on a stale detection |
| pose | pose landmarks (real) | high | period 185 ms, max age 370 ms | a stop or wave gesture is acted on within a third of a second |
| depth | MiDaS v2.1 small (real) | high | period 740 ms, max age 1480 ms | free-space map no more than 74 cm of travel old |

Config: [`examples/android_gpu/vig-slots2-saturated.yaml`](../examples/android_gpu/vig-slots2-saturated.yaml)
(about 95 % protected utilisation — saturated, but schedulable). Models run in
TensorFlow Lite on the phone's GPU; the measuring tool sends zero tensors of the
right shape, so what is measured is supply — which answer arrives in time —,
not recognition quality.

Measured on 15 September 2026 with `vig autotune` on two phones. Every
measuring window holds at least 200 detector cycles (83 s per arm and load
point); the load points scale the periods from 90 % to 125 % of the contract.

| | Pixel 2 (Adreno 540) | Pixel 5 (Adreno 620) |
|---|---|---|
| detector cycles missed at 90 % load, direct → governor | **293 ‰ → 99 ‰** | **497 ‰ → 208 ‰** |
| detector at 100 % and 110 % | 4 ‰ both ways | 4 ‰ both ways |
| detector at 125 % | 3 ‰ both ways | 3 ‰ direct, **32 ‰ governed** |
| price: pose at 90 % | 272 → 363 ‰ | 222 → 413 ‰ |
| price: depth at 125 % | 0 → 328 ‰ | 0 → 571 ‰ |
| tuning | none of five settings beat the measured one | none of five settings beat the measured one |

What this says, and what it does not:

- **Where the phone loses the detector, the governor cuts the loss** — to a
  third on the Pixel 2, to less than half on the Pixel 5 — and pays with pose
  and depth, which is what their class allows.
- **The direct path loses the detector mainly at the 90 % point** and hardly
  at all from 100 % to 125 %. That is not explained yet; the arrival pattern of
  the three periods against a detector that takes about 220 ms is the suspect.
  A result that depends this much on timing is a reason to measure your own
  load, not to quote ours.
- **At 125 % on the Pixel 5 the governor is worse for the detector** than the
  direct path (32 against 3 ‰). The Pixel 5 GPU is the slower one, and this
  contract is at the edge of what it carries.
- **Tuning kept nothing.** With windows long enough to count single cycles, the
  measured configuration was already the best of six on both phones.

The same robot with four cameras on a laptop GPU, as a video: the front camera
stays fresh in 100 % of cycles instead of 0.2 % ([sidewalk demo](benchmark/demo-2026-09-15.md)).

## 3. Industrial safety camera with event analysis (pilot proposal, not measured)

*A person-in-zone detector that may trigger a warning, and a slower
interpretation of the event on the same edge GPU.*

| Stream | Class | Contract | Why |
|---|---|---|---|
| zone detector | protected | agreed with the partner: the event deadline of the warning | a late detection is a missed warning |
| event analysis (VLM) | best effort, **unless** the interpretation itself is needed to recognise the hazard | deadline of the report | a report may wait; a hazard may not |

This is the pipeline we propose to pilot partners
([pilot notes, German](pilot/2026-09-11-use-cases-und-pilotpartner.md)). It is
not measured: the contract comes from the partner's warning deadline, and the
first step of a pilot is to measure whether the problem exists on their
hardware at all.

## 4. One camera and a language model on a single execution unit

*The hardest layout: one GPU slot, a 33 ms camera, and a language model whose
call takes 194 ms. A started call runs to the end — there is no preemption.*

| Stream | Model | Class | Contract | Why |
|---|---|---|---|---|
| camera | RF-DETR Medium, 576 px (real) | protected | period 33 ms, max age 100 ms | one camera frame |
| assistant | Qwen3-0.6B on vLLM (real) | best effort | deadline 2 s, `cooperative:` | answers a question about the scene; may wait, must not block |

Config: [`examples/cooperative_llm/vig.yaml`](../examples/cooperative_llm/vig.yaml).
The language model is declared **splittable**: it runs in quanta sized from the
time actually free, and between two of them the slot is open. Its state travels
in the prompt, so nothing is lost and nothing is recomputed — provided the
backend has a working prefix cache, which is the precondition
([ADR-0031](adr/0031-a-re-prefill-is-not-free-progress.md)).

Measured on 16 September, RTX 4070 Laptop, 30 s per arm, 48 tokens per answer:

| | two slots, **no** splitting | two slots, split | one slot, split |
|---|---:|---:|---:|
| camera frames served | 804 of 910 | **908** | 782 |
| uncovered cycles | 1 ‰ | 1 ‰ | 7 ‰ |
| longest gap | 65 ms | 57 ms | 231 ms |
| answers delivered | 51 | 67 | 72 |
| **refused as unkeepable** | **608** | **0** | **0** |
| characters generated | 15045 | 14202 | 15262 |

**Splitting is not a trade here, it is better on both sides.** Without it the
governor refuses 608 requests because they would miss their deadline — the
client gets errors, not answers. With it, none are refused *and* the camera
serves 104 more frames, at the same text output.

On a single slot it still works — no refusals, 72 answers — but the camera pays:
the longest gap grows to 231 ms, past its 100 ms promise, because that one slot
is busy 100 % of the time. **The honest reading: splitting buys you a working
service on one unit, a second unit buys you the promise.** What it is not is a
substitute for capacity.

The four numbers in the contract are measured, not guessed, and the file says
how: a token series (8/16/32/48, 40 runs each) and `vig calibrate` independently
give 6100 vs 6277 µs fixed cost and 256 vs 258 tokens/s. A guessed fixed cost is
the expensive mistake here — if it is as large as the gap to the next camera
frame, no quantum fits, however small you cut it.
