"""Live-Probe: die Bruecke gegen einen echten Governor vor echtem Triton.

Publiziert `SECONDS` Sekunden lang synthetische Bilder mit 30 Hz auf ein
Topic, laesst die Bruecke sie an den Governor schicken und meldet am Ende,
was ankam: Ergebnisse, Ablehnungen, Transport. Kein Messlauf — eine
Funktionsprobe, dass Bruecke, Governor und Backend miteinander sprechen.

Aufruf siehe `live-smoke.sh`.
"""

import json
import os
import sys
import threading
import time

import rclpy
from rclpy.executors import MultiThreadedExecutor
from rclpy.node import Node
from rclpy.parameter import Parameter
from rclpy.qos import qos_profile_sensor_data
from sensor_msgs.msg import Image
from std_msgs.msg import String

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "vig_bridge"))
from vig_bridge.node import VigBridge  # noqa: E402

GOVERNOR = os.environ.get("VIG_GOVERNOR", "127.0.0.1:9311")
MODEL = os.environ.get("VIG_MODEL", "pose")
TRANSPORT = os.environ.get("VIG_TRANSPORT", "auto")
SECONDS = float(os.environ.get("VIG_SECONDS", "10"))
SEND_CAPTURE_ID = os.environ.get("VIG_SEND_CAPTURE_ID", "1") == "1"
TOPIC = "/smoke/camera/image"


def main() -> int:
    rclpy.init()
    bridge = VigBridge(
        parameter_overrides=[
            Parameter("governor", Parameter.Type.STRING, GOVERNOR),
            Parameter("topics", Parameter.Type.STRING_ARRAY, [TOPIC]),
            Parameter("models", Parameter.Type.STRING_ARRAY, [MODEL]),
            Parameter("capture_groups", Parameter.Type.STRING_ARRAY, ["smoke"]),
            Parameter("supersession_keys", Parameter.Type.INTEGER_ARRAY, [1]),
            Parameter("transport", Parameter.Type.STRING, TRANSPORT),
            Parameter("fill_batch", Parameter.Type.BOOL, True),
            Parameter("send_capture_id", Parameter.Type.BOOL, SEND_CAPTURE_ID),
        ]
    )
    node = Node("vig_smoke")
    publisher = node.create_publisher(Image, TOPIC, qos_profile_sensor_data)
    results, events = [], []
    node.create_subscription(String, TOPIC + "/vig/result", lambda m: results.append(json.loads(m.data)), 100)
    node.create_subscription(String, "/vig_bridge/events", lambda m: events.append(json.loads(m.data)), 100)

    executor = MultiThreadedExecutor(num_threads=4)
    executor.add_node(bridge)
    executor.add_node(node)
    threading.Thread(target=executor.spin, daemon=True).start()
    time.sleep(0.5)

    frames = 0
    end = time.monotonic() + SECONDS
    while time.monotonic() < end:
        msg = Image()
        msg.header.stamp = bridge.get_clock().now().to_msg()
        msg.width, msg.height, msg.encoding, msg.step = 640, 480, "rgb8", 640 * 3
        msg.data = bytes([frames % 256]) * (640 * 480 * 3)
        publisher.publish(msg)
        frames += 1
        time.sleep(1 / 30)
    time.sleep(1.5)

    counts = bridge.counts()
    executor.shutdown()
    bridge.shutdown()
    bridge.destroy_node()
    node.destroy_node()
    rclpy.try_shutdown()

    latencies = sorted(r["latency_us"] for r in results)
    summary = {
        "frames_published": frames,
        "results": len(results),
        "refusals": sum(1 for e in events if e["event"] == "refusal"),
        "refusal_details": sorted({f'{e["outcome"]}: {e["detail"]}' for e in events if e["event"] != "backpressure"})[:5],
        "send_capture_id": SEND_CAPTURE_ID,
        "errors": sum(1 for e in events if e["event"] == "error"),
        "counts": counts,
        "latency_us_p50": latencies[len(latencies) // 2] if latencies else None,
        "latency_us_max": latencies[-1] if latencies else None,
        "age_us_sent_max": max((r["age_us_sent"] or 0) for r in results) if results else None,
        "transport": TRANSPORT,
    }
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0 if results and not summary["errors"] else 1


if __name__ == "__main__":
    sys.exit(main())
