#!/usr/bin/env bash
# Der ganze Gateway auf einem Handy (docs/benchmark/arm-serve.md):
#
#   1. `vig` und die end_to_end-Tests statisch fuer aarch64 bauen,
#   2. das Datenpfadbudget (`datapath_budgets_hold`) je Kerngruppe auf dem
#      Handy und als Bezug auf dem Laptop messen,
#   3. `vig serve` auf dem Handy vor Triton auf dem Laptop starten und die
#      End-to-End-Zeit direkt / ueber ein reines Relais / ueber den Governor
#      abwechselnd messen, dazu Frische unter Last und den Metrikendpunkt.
#
#   tools/arm/serve-on-phone.sh                          # bauen, Budget
#   SKIP_BUILD=1 STEPS=offline tools/arm/serve-on-phone.sh  # vig serve ohne Backend
#   SKIP_BUILD=1 STEPS=profile tools/arm/serve-on-phone.sh  # Profile ueber USB
#   SKIP_BUILD=1 STEPS=serve tools/arm/serve-on-phone.sh    # Governor, Latenz
#
# `profile` gibt die Profile aus, wie der Governor auf dem Handy sie sieht;
# sie gehoeren von Hand in `tools/arm/serve-phone.yaml`, bevor `serve` laeuft.
#
# Voraussetzungen, ohne Root:
#   - Triton auf dem Laptop, gRPC auf 127.0.0.1:8001 (deploy/triton/README.md)
#   - zig als C-Compiler fuer ring (ueber tonic `tls-ring`):
#       python3 -m venv ~/.cache/vig-zig
#       ~/.cache/vig-zig/bin/pip install ziglang==0.16.0
#     `tools/arm/zig-cc.sh` und `zig-ar.sh` rufen es auf.
#
# Umgebung (alle optional):
#   ADB, QUIET, SERIAL, OUT, RUNS (300),
#   STEPS ("budget serve"; einzeln: budget, offline, profile, serve),
#   CORE_GROUPS ("gold:f0 silver:0f"), SERVE_MASK (f0)
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
RUNTIME=/run/media/dd/USB_4028/Projekte/InferenceQoS-runtime
ADB=${ADB:-$RUNTIME/tools/platform-tools/adb}
QUIET=${QUIET:-$RUNTIME/quiet-build.sh}
SERIAL=${SERIAL:-FA82P1A01294}
STEPS=${STEPS:-"budget serve"}
RUNS=${RUNS:-300}
# Pixel 2: 0-3 Silver (A53-Klasse, 1,9 GHz), 4-7 Gold (A73-Klasse, 2,46 GHz).
CORE_GROUPS=${CORE_GROUPS:-"gold:f0 silver:0f"}
SERVE_MASK=${SERVE_MASK:-f0}
OUT=${OUT:-$REPO/target/arm-serve/$(date +%Y%m%d-%H%M%S)}
PHONE=/data/local/tmp

# quiet-build legt die Artefakte von Worktrees auf die NVMe; dasselbe hier.
case "$REPO" in
  */.claude/worktrees/*) TARGET=${CARGO_TARGET_DIR:-$HOME/.cache/vig-target/$(basename "$REPO")} ;;
  *) TARGET=${CARGO_TARGET_DIR:-$REPO/target} ;;
esac

export PATH="$HOME/.cargo/bin:$PATH"
# Linken wie decision-bench (rust-lld, mitgelieferte musl-Laufzeit); das C
# von ring uebersetzt zig.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
export CC_aarch64_unknown_linux_musl=$REPO/tools/arm/zig-cc.sh
export AR_aarch64_unknown_linux_musl=$REPO/tools/arm/zig-ar.sh

mkdir -p "$OUT"
cd "$REPO"
adb() { "$ADB" -s "$SERIAL" "$@"; }

e2e_binary() { # Protokoll eines `cargo test --no-run`
  grep -o 'Executable tests/end_to_end.rs ([^)]*)' "$1" | tail -1 | sed 's/.*(\(.*\))/\1/'
}

if [ "${SKIP_BUILD:-0}" != 1 ]; then
  echo "== Bauen"
  "$QUIET" cargo build --release -p vig-cli --target aarch64-unknown-linux-musl
  "$QUIET" cargo test --release -p vig-gateway --test end_to_end \
    --target aarch64-unknown-linux-musl --no-run 2>&1 | tee "$OUT/build-arm.log"
  "$QUIET" cargo build --release -p vig-bench --bin serve-latency
  "$QUIET" cargo test --release -p vig-gateway --test end_to_end --no-run 2>&1 | tee "$OUT/build-x86.log"
  adb push "$TARGET/aarch64-unknown-linux-musl/release/vig" $PHONE/vig >/dev/null
  adb push "$(e2e_binary "$OUT/build-arm.log")" $PHONE/vig-e2e >/dev/null
fi
adb push tools/arm/serve-phone.yaml $PHONE/serve-phone.yaml >/dev/null

# Wach halten: im Doze-Zustand misst man das Energiesparen des Handys, nicht
# den Governor (siehe arm-serve.md).
adb shell svc power stayon usb
adb shell input keyevent KEYCODE_WAKEUP
state() { adb shell dumpsys battery | grep -m1 temperature; adb shell dumpsys power | grep -m1 mWakefulness=; }

# Direkt aufgerufen fehlt den Testbinaries das `[env]` aus
# `.cargo/config.toml`. `datapath_budgets_hold` laeuft mit den 2 MiB eines
# Testthreads auch im Release-Build ueber, auf dem Laptop wie auf dem Handy.
STACK=RUST_MIN_STACK=16777216

# Was auf dem Handy gestartet wird, endet mit dem Skript. SIGTERM, damit der
# Governor seinen Drain protokolliert.
cleanup() {
  adb shell pkill -TERM -f "$PHONE/vig serve" 2>/dev/null || true
  adb shell pkill -f "nc -p 9002" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
trap 'cleanup; exit 130' TERM INT
wait_ready() {
  for _ in $(seq 30); do curl -sf http://127.0.0.1:9090/readyz >/dev/null && return 0; sleep 1; done
  echo "vig serve wurde nicht bereit" >&2
  return 1
}

if [[ " $STEPS " == *" budget "* ]]; then
  echo "== Funktionssuite auf dem Handy"
  adb shell $STACK taskset f0 $PHONE/vig-e2e | tee "$OUT/phone-suite.txt"
  for group in $CORE_GROUPS; do
    name=${group%%:*}; mask=${group##*:}
    for rep in 1 2 3; do
      echo "== Budget $name $rep"
      { state; adb shell $STACK taskset "$mask" $PHONE/vig-e2e --ignored --nocapture \
          --exact datapath_budgets_hold; state; } > "$OUT/phone-budget-$name-$rep.txt" 2>&1 || true
    done
  done
  echo "== Budget Laptop"
  x86=$(e2e_binary "$OUT/build-x86.log")
  while [ -e "$RUNTIME/measure-pending" ]; do sleep 30; done
  for rep in 1 2 3; do
    # Last und Fremdprozesse innerhalb der Sperre notieren, nicht davor.
    # shellcheck disable=SC2016
    flock -x "$RUNTIME/quiet.lock" bash -c '
      uptime; ps -eo pcpu,comm --sort=-pcpu | head -6
      '"$STACK"' taskset -c 8-15 "$0" --ignored --nocapture --exact datapath_budgets_hold
      uptime' "$x86" > "$OUT/laptop-budget-$rep.txt" 2>&1 || true
  done
fi

# `vig serve` ohne erreichbares Backend: Start, Endpunkte, ein Request durch
# den Scheduler, ein Schwall auf das LATEST-Modell, SIGTERM. Keine Inferenz und
# keine Last auf dem Host — laeuft auch, waehrend der Laptop fuer GPU-Messungen
# reserviert ist. Die Requests baut curl von Hand: ein gRPC-Rahmen mit
# ModelInferRequest{model_name: "pose"}.
if [[ " $STEPS " == *" offline "* ]]; then
  echo "== vig serve ohne Backend"
  adb reverse --remove tcp:8001 2>/dev/null || true
  adb forward tcp:9001 tcp:9001 >/dev/null
  adb forward tcp:9090 tcp:9090 >/dev/null
  cleanup
  frame=$OUT/infer-pose.bin
  printf '\x00\x00\x00\x00\x06\x0a\x04pose' > "$frame"
  adb shell "RUST_LOG=info taskset $SERVE_MASK $PHONE/vig serve -c $PHONE/serve-phone.yaml" \
    > "$OUT/serve-offline.log" 2>&1 &
  for _ in $(seq 30); do curl -sf http://127.0.0.1:9090/healthz >/dev/null && break; sleep 1; done
  {
    echo "== /healthz"; curl -s -w ' [%{http_code}]\n' http://127.0.0.1:9090/healthz
    echo "== /readyz"; curl -s -w ' [%{http_code}]\n' http://127.0.0.1:9090/readyz
    echo "== ein Request"
    curl -s -o /dev/null -D - --http2-prior-knowledge -X POST \
      -H 'content-type: application/grpc' -H 'te: trailers' --data-binary @"$frame" \
      http://127.0.0.1:9001/inference.GRPCInferenceService/ModelInfer | grep -i -E 'grpc-status|grpc-message'
    echo "== 40 Requests, 20 gleichzeitig"
    seq 40 | xargs -P 20 -I{} curl -s -o /dev/null -D - --http2-prior-knowledge -X POST \
      -H 'content-type: application/grpc' -H 'te: trailers' --data-binary @"$frame" \
      http://127.0.0.1:9001/inference.GRPCInferenceService/ModelInfer \
      | grep -i -o 'grpc-message: [^ ]*' | sort | uniq -c
    echo "== /metrics"
    curl -s http://127.0.0.1:9090/metrics | grep -E '^vig_requests_|^vig_backend_failures'
  } | tee "$OUT/serve-offline.txt"
  cleanup
fi

if [[ " $STEPS " == *" profile "* || " $STEPS " == *" serve "* ]]; then
  adb reverse tcp:8001 tcp:8001      # Handy 127.0.0.1:8001 -> Triton auf dem Laptop
  adb forward tcp:9001 tcp:9001      # Laptop 9001 -> Governor auf dem Handy
  adb forward tcp:9090 tcp:9090      # Metriken
  adb forward tcp:9002 tcp:9002      # reines Relais, derselbe USB-Weg ohne Governor
fi

# Die Profile, wie der Governor sie sieht: mit dem USB-Weg. Die Ausgabe gehoert
# von Hand in `serve-phone.yaml`, danach die Perioden pruefen (`doctor`).
if [[ " $STEPS " == *" profile "* ]]; then
  echo "== vig profile auf dem Handy"
  adb shell taskset "$SERVE_MASK" $PHONE/vig profile -c $PHONE/serve-phone.yaml --samples 100 \
    | tee "$OUT/profile.txt"
fi

if [[ " $STEPS " == *" serve "* ]]; then
  echo "== vig serve auf dem Handy"
  adb shell pkill -f "$PHONE/vig serve" || true
  adb shell pkill -f "nc -p 9002" || true
  adb shell taskset "$SERVE_MASK" $PHONE/vig doctor -c $PHONE/serve-phone.yaml | tee "$OUT/doctor.txt" || true

  adb shell "toybox nc -p 9002 -L toybox nc 127.0.0.1 8001" &
  adb shell "RUST_LOG=info taskset $SERVE_MASK $PHONE/vig serve -c $PHONE/serve-phone.yaml" \
    > "$OUT/serve.log" 2>&1 &
  wait_ready

  SL=$TARGET/release/serve-latency
  while [ -e "$RUNTIME/measure-pending" ]; do sleep 30; done
  { uptime; state; } > "$OUT/latency.txt"
  for pair in pose:pose_main depth:depth_main detector:detector_small; do
    logical=${pair%%:*}; physical=${pair##*:}
    echo "== $logical" | tee -a "$OUT/latency.txt"
    flock -x "$RUNTIME/quiet.lock" taskset -c 8-15 "$SL" alternate "$RUNS" \
      "direkt=127.0.0.1:8001/$physical" \
      "relais=127.0.0.1:9002/$physical" \
      "vigilant=127.0.0.1:9001/$logical" | tee -a "$OUT/latency.txt"
  done
  { uptime; state; } >> "$OUT/latency.txt"

  echo "== Frische unter Last"
  curl -s http://127.0.0.1:9090/metrics > "$OUT/metrics-before.txt"
  "$SL" burst 4 25 127.0.0.1:9001/pose | tee "$OUT/burst.txt"
  curl -s http://127.0.0.1:9090/metrics > "$OUT/metrics-after.txt"
  grep -E '^vig_requests_|^vig_useful|^vig_longest_gap' "$OUT/metrics-after.txt" || true
  cleanup
fi

echo "Ergebnisse: $OUT"
