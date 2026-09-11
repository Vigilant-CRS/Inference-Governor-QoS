#!/usr/bin/env bash
# Live-Probe der ROS-2-Bruecke: vig serve vor einem laufenden Triton, die
# Bruecke im ROS-2-Container.
#
#   integrations/ros2/scripts/live-smoke.sh [shm|copy] [pfad/zu/vig]
#
# Voraussetzungen: Triton mit dem Modell `pose_main` auf 127.0.0.1:8001
# (deploy/triton/README.md), das Test-Image `vig-ros2-test`
# (integrations/ros2/docker), ein gebautes `vig`.
#
# `--net=host --ipc=host`: die Bruecke erreicht den Governor ueber Loopback,
# und ihre Shared-Memory-Region muss fuer Governor und Triton sichtbar sein.
set -euo pipefail

TRANSPORT=${1:-shm}
VIG=${2:-target/release/vig}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
CONFIG=$(mktemp --suffix=.yaml)
LOG=$(mktemp --suffix=.log)
trap 'kill ${SERVE_PID:-0} 2>/dev/null || true; rm -f "$CONFIG"' EXIT

cat > "$CONFIG" <<'YAML'
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:8001"
  slots: 1
  pipelining_depth: 0
models:
  pose:
    class: high
    queue: { policy: latest, capacity: 1 }
    contract: { period_ms: 33, deadline_ms: 33, max_age_ms: 66 }
    variants:
      - id: main
        backend_model: pose_main
        quality: { value: 1.0, source: user_declared }
        profile: { p50_us: 4082, p95_us: 5163, p99_us: 5690, samples: 200 }
YAML

"$VIG" serve -c "$CONFIG" --listen 127.0.0.1:9311 --metrics 127.0.0.1:9312 > "$LOG" 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 50); do
  (exec 3<>/dev/tcp/127.0.0.1/9311) 2>/dev/null && break
  sleep 0.2
done

docker run --rm --net=host --ipc=host \
  -e VIG_GOVERNOR=127.0.0.1:9311 -e VIG_MODEL=pose -e VIG_TRANSPORT="$TRANSPORT" \
  -e VIG_SEND_CAPTURE_ID="${VIG_SEND_CAPTURE_ID:-1}" \
  -v "$ROOT:/ws/ros2:ro" vig-ros2-test \
  python3 /ws/ros2/scripts/live_smoke.py
status=$?

echo "--- Governor-Kennzahlen"
curl -s 127.0.0.1:9312/metrics | grep -E '^vig_requests_(received|forwarded|superseded|stale|completed_valid)_total' || true
echo "--- Governor-Log (Ende)"
tail -5 "$LOG"
rm -f "$LOG"
exit $status
