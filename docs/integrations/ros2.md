# ROS 2 bridge

`vig_bridge` connects ROS 2 camera topics to the governor. It subscribes to
`sensor_msgs/Image`, turns each frame into the model's input tensor, and sends
it over the Open Inference Protocol with the freshness information the
governor needs: how old the frame is, which capture it belongs to, and which
camera it replaces. Results come back on a topic next to the camera; every
decision the governor makes about a frame is published as an event.

It is a separate Python process (`rclpy` + gRPC). Nothing native is linked
into the governor ([ADR-0033](../adr/0033-native-code-lives-in-the-backend-process.md)).

Package: [`integrations/ros2/vig_bridge`](../../integrations/ros2/vig_bridge)
(ament_python, ROS 2 Jazzy; Humble uses the same API).

## Setup

```bash
# in your ROS 2 workspace
cp -r integrations/ros2/vig_bridge src/
pip install "tritonclient[grpc]"          # gRPC stubs for the Open Inference Protocol
colcon build --packages-select vig_bridge
source install/setup.bash

ros2 run vig_bridge vig_bridge --ros-args \
  -p governor:=127.0.0.1:9001 \
  -p topics:="['/front/image_raw', '/rear/image_raw']" \
  -p models:="['detector', 'detector']" \
  -p capture_groups:="['front', 'rear']" \
  -p supersession_keys:="[1, 2]"
```

The governor must list the models under the names you give in `models` — the
bridge asks it for their input shape at start-up.

## Parameters

| Parameter | Default | Meaning |
|---|---|---|
| `governor` | `127.0.0.1:9001` | the governor's gRPC endpoint |
| `topics` | — | camera topics (`sensor_msgs/Image`) |
| `models` | — | the governor model for each topic |
| `capture_groups` | — | topics in the same group that fire together share a capture id |
| `supersession_keys` | — | one key per camera: a newer frame replaces an older one with the same key |
| `transport` | `auto` | `shm`, `copy`, or `auto` (shared memory when the governor is on loopback) |
| `shm_slots` | `4` | shared-memory slots per camera; a slot is reused only after its request is answered |
| `age_source` | `stamp` | `stamp`: age from `header.stamp`; `arrival`: send no age, the governor counts from arrival |
| `max_clock_skew_us` | `10000` | a stamp further in the future than this is reported as clock skew |
| `capture_tolerance_us` | `1000` | how close two stamps in one group must be to count as one capture |
| `max_age_us` | `0` | per-request maximum age; `0` leaves the governor's contract in force |
| `layout` | `NCHW` | tensor layout of the model input (`NCHW` or `NHWC`) |
| `scale` | `1/255` | pixel scaling for float inputs |
| `fill_batch` | `false` | repeat the frame if the model has a fixed batch size above one |
| `token` | — | bearer token, if the governor requires one (`backend.security`) |
| `send_capture_id` | `true` | send `vig_capture_id`; switch it off if nothing downstream fuses results (see the known issue below) |

`topics`, `models`, `capture_groups` and `supersession_keys` are parallel lists
and must have the same length. Unequal lists are refused, not padded.

## What gets published

| Topic | Type | Content |
|---|---|---|
| `<topic>/vig/result` | `std_msgs/String` (JSON) | request id, capture id, the age that was sent, round-trip latency, whether the result was already obsolete on completion, and the output tensors (base64) |
| `/vig_bridge/events` | `std_msgs/String` (JSON) | one event per refusal (`superseded`, `stale`, `infeasible`, `capacity`, `fusion_refused`), error, back-pressure or clock skew |
| `/vig_bridge/diagnostics` | `std_msgs/String` (JSON) | counters once per second, and which transport is in use |

**A refusal is information, not an error.** A frame that was superseded by a
newer one, or that would have been too old when finished, is the governor doing
its job. The bridge publishes it as an event of kind `refusal` and does not
resend that frame — the next one is fresher. Only a transport error (the
governor did not answer at all) leaves open whether a frame arrived.

A result the governor completed but marks as obsolete (`vig_obsolete`) is still
published, with `"obsolete": true`. Whether to use it is the application's
decision.

The JSON result message is deliberately generic: output decoding is
model-specific and belongs to the application. A production consumer would
decode the outputs into a typed message such as `vision_msgs/Detection2DArray`.

## Clocks

The governor works on its own monotonic clock. A ROS stamp comes from another
time base, so the bridge never sends an absolute time. It sends an **age**:
`now − header.stamp`, both read from the bridge node's ROS clock, at the moment
the request is sent — conversion and copying are included.

That age is only right if two things hold:

- **Camera and bridge share a clock, or their clocks are synchronised.** If
  the camera driver runs on another machine without PTP or chrony, every age
  is off by the offset between the two clocks.
- **The ROS clock runs at real time.** With `use_sim_time`, ages come out in
  simulated seconds, while the governor measures deadlines in real ones. A
  simulation that runs slower, faster or pauses produces ages that mean
  nothing. The bridge warns at start-up; set `age_source: arrival` in that case
  — the governor then counts from arrival, which is honestly wrong instead of
  quietly wrong.

A stamp in the future is clamped to age zero. Beyond `max_clock_skew_us` it is
also reported as a `clock_skew` event: a negative age would promise the
governor a fresher frame than exists.

## Shared memory

When the governor is reachable on loopback, the bridge writes each frame into
a POSIX shared-memory region and sends only a reference; the governor and the
backend read it in place ([ADR-0003](../adr/0003-shared-memory-passthrough.md)).
For camera frames this is the path the product is measured on — the copy path
costs a multiple ([datapath budgets](../datapath-budgets.md)).

Each camera gets one region with `shm_slots` slots. A slot is reused only once
the request that uses it has been answered; with one slot, a new frame would
overwrite the one the backend may still be reading. If every slot is busy, the
new frame is not sent and counted as `client_backpressure`.

In containers, bridge, governor and backend must share `/dev/shm`
(`--ipc=host` or a common IPC namespace). An address that only looks local —
a port forward into another container without shared IPC — needs
`transport: copy`.

## Fusion across cameras

Frames from the same `capture_group` whose stamps lie within
`capture_tolerance_us` get the same `vig_capture_id`, and every request gets a
numeric id. Both are in the result message. A fusion node that combines them
sends its own request with `vig_capture_id` and `vig_depends_on`; the governor
refuses a fusion across captures before it computes
([results from the same capture](../getting-started.md#results-from-the-same-capture)).

### Known issue: the capture graph fills up

The governor keeps one node per announced capture in a bounded graph of 256
nodes ([`MAX_NODES`](../../crates/vig-core/src/dag.rs)). As of this release it
never moves a node out of the open state once its request has finished, so the
graph is only cleaned of nodes that were never there: after 256 captures in
the lifetime of a governor process, every request carrying `vig_capture_id` is
refused with `FAILED_PRECONDITION` — "der Graph fasst hoechstens 256 Knoten".
At 30 Hz that is about eight seconds.

The bridge reports these as `fusion_refused` events, and a live run against
`vig serve` shows exactly this: 256 frames delivered, every later one refused.
Until the governor retires finished capture nodes, run the bridge with
`send_capture_id: false` unless a downstream node actually fuses results.
Freshness, supersession and the shared-memory path do not depend on it.

## What the bridge does not do

- **No preprocessing beyond resize and scale.** Nearest-neighbour resize,
  channel order, scaling. A model that needs letterboxing or normalisation
  with mean and standard deviation needs its own preprocessing.
- **No output decoding.** It publishes tensors, not detections.
- **No retries of refused frames**, by design.
- **No clock synchronisation.** It reports skew; it cannot fix it.
- **No compressed images.** `sensor_msgs/CompressedImage` would need a decoder
  in the bridge.

## Tests

```bash
docker build -t vig-ros2-test integrations/ros2/docker
docker run --rm -v "$PWD/integrations/ros2/vig_bridge:/ws/vig_bridge" vig-ros2-test
```

Unit tests for the logic (age, capture and request ids, parameters, refusal
classification, image conversion, slot reuse), transport tests against a stub
governor that reads the shared-memory region and answers with `vig-reason`,
and a function test that runs the real ROS 2 node with two cameras of one
capture group over both transports.

Against a real governor in front of Triton (`pose_main` on `127.0.0.1:8001`):

```bash
integrations/ros2/scripts/live-smoke.sh shm target/release/vig
integrations/ros2/scripts/live-smoke.sh copy target/release/vig
```
