"""Funktionstest: der echte ROS-2-Knoten gegen einen Stub-Governor.

Zwei Kameras einer Aufnahmegruppe publizieren synthetische Bilder mit
gemeinsamen Stempeln. Geprueft wird, was der Governor zu sehen bekommt —
Alter, Aufnahme, Supersession-Schluessel, numerische Kennung,
Shared-Memory-Pfad — und was die Anwendung zuruecksieht: Ergebnisse auf
`<topic>/vig/result`, Ablehnungen als Ereignis und nicht als Fehler.

Braucht rclpy; laeuft im Test-Image (`integrations/ros2/docker`).
"""

import json
import threading
import time

import pytest

rclpy = pytest.importorskip("rclpy")
pytest.importorskip("tritonclient.grpc")

from rclpy.executors import MultiThreadedExecutor  # noqa: E402
from rclpy.node import Node  # noqa: E402
from rclpy.parameter import Parameter  # noqa: E402
from rclpy.qos import qos_profile_sensor_data  # noqa: E402
from sensor_msgs.msg import Image  # noqa: E402
from std_msgs.msg import String  # noqa: E402

from vig_bridge.node import VigBridge  # noqa: E402

from stub_governor import start  # noqa: E402

FRAMES = 12


def _image(stamp_ns: int, value: int) -> Image:
    msg = Image()
    msg.header.stamp.sec = stamp_ns // 1_000_000_000
    msg.header.stamp.nanosec = stamp_ns % 1_000_000_000
    msg.header.frame_id = "cam"
    msg.width, msg.height, msg.encoding, msg.step = 16, 12, "rgb8", 48
    msg.data = bytes([value]) * (16 * 12 * 3)
    return msg


class Harness(Node):
    def __init__(self) -> None:
        super().__init__("vig_bridge_harness")
        self.left = self.create_publisher(Image, "/stereo/left/image", qos_profile_sensor_data)
        self.right = self.create_publisher(Image, "/stereo/right/image", qos_profile_sensor_data)
        self.results = []
        self.events = []
        self.diagnostics = []
        for topic in ("/stereo/left/image/vig/result", "/stereo/right/image/vig/result"):
            self.create_subscription(String, topic, lambda m: self.results.append(json.loads(m.data)), 50)
        self.create_subscription(String, "/vig_bridge/events", lambda m: self.events.append(json.loads(m.data)), 50)
        self.create_subscription(String, "/vig_bridge/diagnostics", lambda m: self.diagnostics.append(json.loads(m.data)), 5)


@pytest.mark.parametrize("transport", ["shm", "copy"])
def test_the_bridge_sends_what_the_governor_needs_and_reports_refusals(transport):
    # Jeder dritte Request der rechten Kamera wird als veraltet abgelehnt.
    def decide(recorded):
        if recorded.params.get("vig_supersession_key") == 2 and int(recorded.id) % 3 == 0:
            return "stale"
        return None

    server, stub, endpoint = start(decide)
    rclpy.init()
    try:
        bridge = VigBridge(
            parameter_overrides=[
                Parameter("governor", Parameter.Type.STRING, endpoint),
                Parameter("topics", Parameter.Type.STRING_ARRAY, ["/stereo/left/image", "/stereo/right/image"]),
                Parameter("models", Parameter.Type.STRING_ARRAY, ["det", "det"]),
                Parameter("capture_groups", Parameter.Type.STRING_ARRAY, ["stereo", "stereo"]),
                Parameter("supersession_keys", Parameter.Type.INTEGER_ARRAY, [1, 2]),
                Parameter("transport", Parameter.Type.STRING, transport),
                Parameter("shm_slots", Parameter.Type.INTEGER, 4),
            ]
        )
        harness = Harness()
        executor = MultiThreadedExecutor(num_threads=4)
        executor.add_node(bridge)
        executor.add_node(harness)
        spinner = threading.Thread(target=executor.spin, daemon=True)
        spinner.start()
        time.sleep(0.5)  # Abonnements finden einander

        stamps = []
        for i in range(FRAMES):
            now = bridge.get_clock().now().nanoseconds
            stamp = now - 5_000_000  # 5 ms alt beim Publizieren
            stamps.append(stamp)
            harness.left.publish(_image(stamp, 10 + i))
            harness.right.publish(_image(stamp + 200_000, 10 + i))  # 0,2 ms Ausloeseversatz
            time.sleep(0.05)

        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline and len(stub.requests) < 2 * FRAMES:
            time.sleep(0.05)
        time.sleep(1.2)  # eine Diagnoserunde abwarten

        executor.shutdown()
        bridge.shutdown()
        bridge.destroy_node()
        harness.destroy_node()
    finally:
        rclpy.try_shutdown()
        server.stop(0)
        stub.close()

    requests = stub.requests
    assert len(requests) == 2 * FRAMES, f"{len(requests)} Requests statt {2 * FRAMES}"

    # Was der Governor sieht.
    for r in requests:
        assert r.id.isdigit(), "numerische Kennung (NV-17)"
        assert 5_000 <= r.params["vig_age_us"] < 2_000_000, r.params
        assert r.params["vig_supersession_key"] in (1, 2)
        assert r.via_shm is (transport == "shm")
    by_capture = {}
    for r in requests:
        by_capture.setdefault(r.params["vig_capture_id"], set()).add(r.params["vig_supersession_key"])
    assert len(by_capture) == FRAMES, "je Ausloesung genau eine Aufnahme"
    assert all(keys == {1, 2} for keys in by_capture.values()), "beide Kameras einer Ausloesung teilen die Aufnahme"
    assert len({r.id for r in requests}) == 2 * FRAMES

    # Was die Anwendung sieht.
    refused = [r for r in requests if decide(r) == "stale"]
    assert refused, "der Test muss mindestens eine Ablehnung erzeugen"
    stale_events = [e for e in harness.events if e["outcome"] == "stale"]
    assert len(stale_events) == len(refused)
    assert all(e["event"] == "refusal" for e in stale_events), "eine Ablehnung ist Information, kein Fehler"
    assert len(harness.results) == 2 * FRAMES - len(refused)
    first = harness.results[0]
    assert {"capture_id", "request_id", "age_us_sent", "outputs"} <= set(first)
    assert harness.diagnostics and harness.diagnostics[-1]["transport_shm"] == int(transport == "shm")
