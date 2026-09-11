"""Der ROS-2-Knoten: Kamera-Topics rein, Governor-Antworten raus.

Je Kamera ein Abonnement auf `sensor_msgs/Image`. Jeder Frame geht mit
seinem **Alter** (nicht mit einem absoluten Zeitstempel, siehe `core.py`),
seiner Aufnahme und dem Supersession-Schluessel seiner Kamera an den
Governor. Die Antwort erscheint auf `<topic>/vig/result`; jede Ablehnung
auf `/vig_bridge/events` — als Information, nicht als Fehler.
"""

from __future__ import annotations

import base64
import json
import threading
from collections import Counter
from typing import Dict, List, Optional

import rclpy
from rclpy.node import Node
from rclpy.qos import qos_profile_sensor_data
from sensor_msgs.msg import Image
from std_msgs.msg import String

from .core import (
    Camera,
    CaptureIds,
    FreshnessParams,
    Outcome,
    RequestIds,
    SlotRing,
    age_of,
    image_to_tensor,
    is_local_endpoint,
    parse_cameras,
)
from .oip import GovernorClient, InferResult, ShmRegion


class _CameraState:
    def __init__(self, camera: Camera, spec, region: Optional[ShmRegion], ring: Optional[SlotRing], publisher) -> None:
        self.camera = camera
        self.spec = spec
        self.region = region
        self.ring = ring
        self.publisher = publisher


class VigBridge(Node):
    """Bruecke zwischen ROS-2-Kameras und dem Vigilant Inference Governor."""

    def __init__(self, client: Optional[GovernorClient] = None, **node_kwargs) -> None:
        # `node_kwargs` reicht z. B. `parameter_overrides` an rclpy durch —
        # der Weg, auf dem die Funktionstests den Knoten konfigurieren.
        super().__init__("vig_bridge", **node_kwargs)
        p = self.declare_parameter
        p("governor", "127.0.0.1:9001")
        p("topics", [""])
        p("models", [""])
        p("capture_groups", [""])
        p("supersession_keys", [0])
        p("transport", "auto")  # auto | shm | copy
        p("shm_slots", 4)
        p("age_source", "stamp")  # stamp | arrival
        p("max_clock_skew_us", 10_000)
        p("capture_tolerance_us", 1_000)
        p("max_age_us", 0)  # 0: der Vertrag des Governors gilt
        p("layout", "NCHW")
        p("scale", 1.0 / 255.0)
        p("fill_batch", False)
        p("token", "")
        p("timeout_s", 5.0)
        # Ohne Fusion nach dem Governor braucht er keine Aufnahmen: jede
        # gemeldete belegt einen Knoten in seinem Graphen (NV-17).
        p("send_capture_id", True)

        get = lambda name: self.get_parameter(name).value  # noqa: E731
        self._governor: str = get("governor")
        self._cameras = parse_cameras(
            [t for t in get("topics") if t],
            [m for m in get("models") if m],
            [g for g in get("capture_groups") if g],
            list(get("supersession_keys"))[: len([t for t in get("topics") if t])],
        )
        transport = get("transport")
        if transport not in ("auto", "shm", "copy"):
            raise ValueError(f"transport={transport!r}: erlaubt sind auto, shm, copy")
        self._use_shm = transport == "shm" or (transport == "auto" and is_local_endpoint(self._governor))
        self._age_source = get("age_source")
        if self._age_source not in ("stamp", "arrival"):
            raise ValueError(f"age_source={self._age_source!r}: erlaubt sind stamp, arrival")
        if self._age_source == "stamp" and self.get_parameter("use_sim_time").value:
            self.get_logger().warning(
                "use_sim_time ist gesetzt: Alter entstehen in Simulationszeit, der "
                "Governor misst Deadlines in Echtzeit. age_source: arrival waehlen, "
                "wenn die Simulation nicht mit Echtzeit laeuft."
            )
        self._max_skew_us = int(get("max_clock_skew_us"))
        self._max_age_us = int(get("max_age_us")) or None
        self._layout = get("layout")
        self._scale = float(get("scale"))
        self._fill_batch = bool(get("fill_batch"))
        self._send_capture_id = bool(get("send_capture_id"))

        self._client = client or GovernorClient(
            self._governor, timeout_s=float(get("timeout_s")), token=get("token") or None
        )
        self._client.wait_ready()
        self._captures = CaptureIds(tolerance_ns=int(get("capture_tolerance_us")) * 1_000)
        self._ids = RequestIds()
        self._counts: Counter = Counter()
        self._lock = threading.Lock()
        self._events = self.create_publisher(String, "/vig_bridge/events", 50)
        self._diagnostics = self.create_publisher(String, "/vig_bridge/diagnostics", 5)

        self._states: List[_CameraState] = []
        for index, camera in enumerate(self._cameras):
            spec = self._client.input_spec(camera.model, self._layout)
            region = ring = None
            if self._use_shm:
                region = ShmRegion(f"vig_bridge_{index}_{camera.model}", spec.byte_size, int(get("shm_slots")))
                self._client.register_region(region)
                ring = SlotRing(region.slots)
            publisher = self.create_publisher(String, camera.topic.rstrip("/") + "/vig/result", 10)
            state = _CameraState(camera, spec, region, ring, publisher)
            self._states.append(state)
            self.create_subscription(
                Image,
                camera.topic,
                lambda msg, s=state: self._on_image(s, msg),
                qos_profile_sensor_data,
            )
        self.create_timer(1.0, self._publish_diagnostics)
        self.get_logger().info(
            f"vig_bridge: {len(self._states)} Kamera(s) an {self._governor}, "
            f"Transport {'Shared Memory' if self._use_shm else 'Kopie'}"
        )

    # ------------------------------------------------------------------

    def _on_image(self, state: _CameraState, msg: Image) -> None:
        camera = state.camera
        stamp_ns = msg.header.stamp.sec * 1_000_000_000 + msg.header.stamp.nanosec
        try:
            tensor = image_to_tensor(
                bytes(msg.data), msg.width, msg.height, msg.encoding, msg.step,
                state.spec, scale=self._scale, fill_batch=self._fill_batch,
            )
        except ValueError as error:
            self._event("error", camera, Outcome.INVALID, None, str(error))
            return

        capture = self._captures.assign(camera.group, stamp_ns)
        request_id = self._ids.next()
        slot = None
        offset = 0
        if state.ring is not None:
            slot = state.ring.acquire()
            if slot is None:
                self._count("client_backpressure")
                self._event("backpressure", camera, None, request_id, "alle Faecher belegt")
                return
            offset = state.region.write(slot, tensor.tobytes())

        # Das Alter im Moment des Absendens, nicht beim Empfang: die Zeit fuer
        # Umwandlung und Kopie gehoert dazu.
        age = None
        if self._age_source == "stamp":
            measured = age_of(stamp_ns, self.get_clock().now().nanoseconds, self._max_skew_us)
            if measured.clock_skew:
                self._count("clock_skew")
                self._event("clock_skew", camera, None, request_id, "Stempel liegt in der Zukunft")
            age = measured.micros
        params = FreshnessParams(
            age_us=age,
            capture_id=capture if self._send_capture_id else None,
            supersession_key=camera.supersession_key,
            max_age_us=self._max_age_us,
        ).as_dict()

        request = self._client.build_request(
            camera.model, state.spec, request_id, params,
            tensor=None if state.region is not None else tensor,
            region=state.region, offset=offset,
        )
        self._count("sent")
        self._client.infer_async(
            request,
            lambda result: self._on_result(state, slot, request_id, capture, stamp_ns, age, result),
        )

    def _on_result(self, state, slot, request_id, capture, stamp_ns, age, result: InferResult) -> None:
        if slot is not None:
            state.ring.release(slot)
        self._count(result.outcome.value)
        if result.outcome in (Outcome.DELIVERED, Outcome.DELIVERED_OBSOLETE):
            payload = {
                "camera": state.camera.topic,
                "model": state.camera.model,
                "request_id": request_id,
                "capture_id": capture,
                "stamp_ns": stamp_ns,
                "age_us_sent": age,
                "latency_us": result.latency_us,
                "obsolete": result.outcome is Outcome.DELIVERED_OBSOLETE,
                "outputs": {
                    name: {
                        "datatype": str(array.dtype),
                        "shape": list(array.shape),
                        "data_b64": base64.b64encode(array.tobytes()).decode("ascii"),
                    }
                    for name, array in result.outputs.items()
                },
            }
            state.publisher.publish(String(data=json.dumps(payload)))
        else:
            kind = "refusal" if result.outcome.is_refusal else "error"
            self._event(kind, state.camera, result.outcome, request_id, result.detail)

    # ------------------------------------------------------------------

    def _count(self, name: str) -> None:
        with self._lock:
            self._counts[name] += 1

    def _event(self, kind: str, camera: Camera, outcome: Optional[Outcome], request_id, detail: str) -> None:
        event = {
            "event": kind,
            "camera": camera.topic,
            "outcome": outcome.value if outcome else None,
            "request_id": request_id,
            "detail": detail,
        }
        self._events.publish(String(data=json.dumps(event)))

    def _publish_diagnostics(self) -> None:
        with self._lock:
            counts: Dict[str, int] = dict(self._counts)
        counts["transport_shm"] = int(self._use_shm)
        self._diagnostics.publish(String(data=json.dumps(counts, sort_keys=True)))

    def counts(self) -> Dict[str, int]:
        with self._lock:
            return dict(self._counts)

    def shutdown(self) -> None:
        for state in self._states:
            if state.region is not None:
                self._client.unregister_region(state.region)
                state.region.close()
        self._client.close()


def main(args=None) -> None:
    rclpy.init(args=args)
    node = VigBridge()
    try:
        rclpy.spin(node)
    except KeyboardInterrupt:
        pass
    finally:
        node.shutdown()
        node.destroy_node()
        rclpy.try_shutdown()


if __name__ == "__main__":
    main()
