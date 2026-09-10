# Four RF-DETR variants, honestly declared

Four detection models from a real deployment, in one Triton model repository,
declared with the semantics NV-10 introduced. The point of this example is not
to demonstrate variant selection. **It is not possible here, and that is the
finding.**

## What the four models are

Shapes measured with `tools/onnx-signature.py` from the files themselves; class
lists taken from the `*.classmap.json` next to them.

| Model | Input | `dets` | `labels` | Classes |
|---|---|---|---|---|
| `rfdetr_512` | `1×3×512×512` | `1×300×4` | `1×300×10` | 9 + no-object |
| `rfdetr_768` | `1×3×768×768` | `1×300×4` | `1×300×10` | 9 + no-object, **same order** |
| `rfdetr_nano_512` | `1×3×512×512` | `1×300×4` | `1×300×8` | 7 + no-object, order unknown |
| `rfdetr_23cls_768` | `1×3×768×768` | `1×300×4` | `1×300×24` | 23 + no-object |
| `rfdetr_28cls_768` | `1×3×768×768` | `1×300×4` | `1×300×29` | 28 + no-object, **superset of v4** |

`rfdetr_512` is byte-identical to the detector every published Gate-M3 number
was measured with — same SHA-256. The measurement and this example describe the
same artifact.

## What `doctor` says, and why

```
WARN detector_resolution: rfdetr_768 und rfdetr_512 sind fachlich nicht
     austauschbar (verschiedene Eingabevertraege; eine andere Vorverarbeitung
     ist kein Ersatz).
WARN detector_classes: rfdetr_28cls_768 und rfdetr_23cls_768 sind fachlich nicht
     austauschbar (Ausgabe 1: verschiedene Labels oder verschiedene
     Labelreihenfolge — gleiche Form, andere Bedeutung).
```

**Case 1 — same classes, different resolution.** The output semantics are
character-for-character identical. Only the input differs. That is not a
substitute, it is a different preprocessing — and it costs time, which is why
`preprocess_us` exists.

**Case 2 — same resolution, superset of classes.** The unpleasant one. v5 knows
all 23 of v4's classes in the same order plus five more, so a detection with
class id ≤ 22 means the same thing in both. They are still not interchangeable:
the governor picks per request, and v4 instead of v5 silently loses five
classes. Interchangeability has to hold in both directions.

**Case 3 — a variant whose meaning nobody wrote down.** No class map ships with
`rfdetr_nano_512`. The class count follows from the logit width; the **order**
does not — and the order is the meaning. So it carries no semantics block and
no invented label list. Not described is an honest statement; a guessed list
would not be.

## Where the semantics declarations come from

None of it is convention or guesswork:

| Field | Source |
|---|---|
| Input and output shapes | `tools/onnx-signature.py`, measured from the files |
| Class lists and order | `*.classmap.json` next to each model |
| `coordinates: normalized`, `layout: cxcywh` | the deployment's own decode code, `dem Dekodiercode des Anwenders:267` |
| `unit: logits` | same file, lines 266 and 301 — the values are pre-sigmoid |
| `normalization: imagenet_after_square_bilinear_no_letterbox` | same file, lines 208 and 232 |

Reading that code corrected two assumptions we had written down first: the class
output carries **logits, not probabilities**, and `dets` and `labels` are two
outputs with different meanings rather than one. A client that treats logits as
probabilities thresholds against the wrong scale — and no tensor shape shows it.

## What this example does *not* prove

Both conflicts here would **also** have been caught by the older I/O-signature
check, because the declared shapes differ. For these four models the semantics
check adds precision — it names the field and the output index — not coverage.

The case where it adds coverage is the one where an operator declares dynamic
dimensions (`dims: [-1, 300, -1]`) to make two models look alike to Triton. The
signature check then goes blind and the semantics check still catches it. That
case is covered by a test rather than a config here, because shipping a
deliberately misleading `config.pbtxt` invites copying it:
`the_same_shape_with_permuted_labels_switches_off_auto_selection` in
`crates/vig-config/tests/schema.rs`.

## Running it

The model repository lives outside this git repository — model weights do not
belong in version control. See `PROVENANCE.txt` in each model directory for
where it came from and its digest.

Start a **second** Triton instance on its own ports so a running measurement
setup stays untouched:

```bash
export VARIANTS=/path/to/vig-variants

docker run -d --name vig-triton-variants --device nvidia.com/gpu=all \
  -p 8100:8000 -p 8101:8001 -p 8102:8002 \
  -v "$VARIANTS:/models:ro" --ipc=host \
  nvcr.io/nvidia/tritonserver:26.06-py3 \
  tritonserver --model-repository=/models --allow-client-shm=true
```

`--device nvidia.com/gpu=all` and not `--gpus all`; `--allow-client-shm=true`
is mandatory from Triton 26; `--ipc=host` is required for the shared-memory
data path. The reasons are in [`deploy/triton/README.md`](../../deploy/triton/README.md).

Then:

```bash
vig doctor -c examples/rfdetr_variants/vig.yaml

vig calibrate -c examples/rfdetr_variants/vig.yaml -o measured.yaml \
  --model-repository "$VARIANTS" \
  --library-version "onnxruntime via Triton 2.70.0" \
  --partition exclusive --instances 1 --rate-limiter disabled \
  --independent-runs 2 --valid-up-to-occupancy-pct 92
```

`--periodic-us` belongs to `vig profile`, not to `calibrate` — `calibrate`
measures back to back plus under concurrent load, which is what its interference
step needs.

`--rate-limiter disabled` and not `off`: YAML 1.1 tools (PyYAML, most
inspection scripts) read a bare `off` as the boolean `false`. Our parser follows
YAML 1.2 and reads the string correctly, but a file that means two different
things to two readers is a trap waiting for whoever edits it next.

Device name, compute capability, driver and memory are filled in from
`nvidia-smi`; `vig` says what it filled in. If the card is throttled during the
measurement you get a warning **before** the numbers — a profile measured under
a power cap describes the card under that power cap.

## The profiles in `vig.yaml` are placeholders

They exist so `doctor` runs at all. They carry no fingerprint and no manifest,
so `doctor` reports them as unverifiable itself. Only `rfdetr_512`'s numbers are
measured; every other variant repeats them, and the 768 px models certainly need
more. How much is unmeasured, which makes `doctor`'s utilisation figure too
optimistic. Replace the file with `calibrate`'s output before drawing any
conclusion from it.
