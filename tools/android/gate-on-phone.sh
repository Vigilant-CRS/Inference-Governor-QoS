#!/usr/bin/env bash
# Gate M3 auf der GPU eines Telefons (docs/benchmark/android-gpu.md):
# Backend und Governor laufen beide auf dem Geraet, der Laptop schiebt nur
# Dateien und holt Ergebnisse ab.
#
#   bench      Rechenzeit je Modell allein und alle gleichzeitig (Profile)
#   calibrate  `vig calibrate` auf dem Telefon gegen den TFLite-Server, je
#              Konfiguration in CONFIGS; die gemessene Datei landet als
#              <name>-measured.yaml in OUT (Paare nur bei `slots` > 1)
#   gate       gate-m3 im Kopiermodus, je Konfiguration RUNS Laeufe mit
#              Abkuehlpause dazwischen; Temperaturen vor jedem Lauf
#
#   SKIP_BUILD=1 STEPS=bench tools/android/gate-on-phone.sh
#   STEPS=gate CONFIGS="examples/android_gpu/vig.yaml examples/android_gpu/vig-overload.yaml" \
#     tools/android/gate-on-phone.sh
#   STEPS=calibrate CONFIGS=examples/android_gpu/vig-slots2.yaml tools/android/gate-on-phone.sh
#
# FRAMES=<datei.rgb> schickt statt Nullen echte Bilder (RGB24, quadratisch,
# FRAME_SIZE Pixel, Voreinstellung 512; gate-m3 `VIG_GATE_FRAMES`). Die Datei
# wird aufs Telefon geschoben; sie sollte klein sein (16 Bilder, 12 MB).
#
# Voraussetzungen: tools/android/fetch-assets.sh, NDK (build-backend.sh),
# zig fuer ring im musl-Build von gate-m3 (tools/arm/serve-on-phone.sh).
#
# Umgebung (alle optional): ADB, SERIAL (Pixel 2), STEPS ("bench gate"),
# CONFIGS, RUNS (3), COOLDOWN (120 s), SECONDS_PER_RUN (30), OUT, QUIET,
# GATE_MASK (f0: gate-m3 auf den Gold-Kernen des Pixel 2)
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
RUNTIME=${RUNTIME:-/run/media/dd/USB_4028/Projekte/InferenceQoS-runtime}
ADB=${ADB:-$HOME/Android/Sdk/platform-tools/adb}
QUIET=${QUIET:-$RUNTIME/quiet-build.sh}
SERIAL=${SERIAL:-FA82P1A01294}
STEPS=${STEPS:-"bench gate"}
CONFIGS=${CONFIGS:-"examples/android_gpu/vig.yaml examples/android_gpu/vig-overload.yaml"}
RUNS=${RUNS:-3}
COOLDOWN=${COOLDOWN:-120}
SECONDS_PER_RUN=${SECONDS_PER_RUN:-30}
GATE_MASK=${GATE_MASK:-f0}
OUT=${OUT:-$RUNTIME/android-gpu-$(date +%Y-%m-%d)/$(date +%H%M%S)}
PHONE=/data/local/tmp/vigtfl
# Logischer Name = Datei in $RUNTIME/android-models. Der Detektor mit NMS im
# Graphen antwortet mit 25 Boxen statt 19 206 Ankern (7 MB) — auf dem
# Kopierpfad eines Telefons ist das der Unterschied zwischen einer Messung
# des Schedulings und einer des Transports.
read -r -a MODELS <<< "${MODEL_FILES:-detector=efficientdet_lite0_nms.tflite pose=pose_landmarks_detector.tflite depth=midas_v21_small.tflite}"

case "$REPO" in
  */.claude/worktrees/*) TARGET=${CARGO_TARGET_DIR:-$HOME/.cache/vig-target/$(basename "$REPO")} ;;
  *) TARGET=${CARGO_TARGET_DIR:-$REPO/target} ;;
esac

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
export CC_aarch64_unknown_linux_musl=$REPO/tools/arm/zig-cc.sh
export AR_aarch64_unknown_linux_musl=$REPO/tools/arm/zig-ar.sh

mkdir -p "$OUT"
cd "$REPO"
adb() { "$ADB" -s "$SERIAL" "$@"; }
thermal() { # Temperaturen laut Thermal-HAL; sysfs ist ohne Root gesperrt
  adb shell dumpsys thermalservice |
    sed -n '/Current temperatures from HAL/,/Current cooling/p' | grep mValue |
    sed 's/.*mValue=\([0-9.]*\).*mName=\([^,]*\).*/\2=\1/' | tr '\n' ' '
  echo
}

SERVER=$RUNTIME/android-tflite/vig-tflite-server
if [ "${SKIP_BUILD:-0}" != 1 ]; then
  echo "== Bauen"
  cp "$(QUIET=$QUIET tools/android/build-backend.sh)" "$SERVER"
  "$QUIET" cargo build --release -p vig-bench --bin gate-m3 --target aarch64-unknown-linux-musl
fi
GATE=$TARGET/aarch64-unknown-linux-musl/release/gate-m3

echo "== Schieben"
adb shell mkdir -p $PHONE
adb push "$SERVER" "$GATE" "$RUNTIME"/android-tflite/arm64/*.so $PHONE/ >/dev/null
for m in "${MODELS[@]}"; do
  adb push "$RUNTIME/android-models/${m#*=}" $PHONE/ >/dev/null
done
for c in $CONFIGS; do adb push "$c" "$PHONE/$(basename "$c")" >/dev/null; done
adb shell chmod 755 $PHONE/vig-tflite-server $PHONE/gate-m3
frames_env=""
if [ -n "${FRAMES:-}" ]; then
  adb push "$FRAMES" $PHONE/frames.rgb >/dev/null
  frames_env="VIG_GATE_FRAMES=$PHONE/frames.rgb VIG_GATE_FRAME_SIZE=${FRAME_SIZE:-512}"
fi
model_args=""
for m in "${MODELS[@]}"; do model_args="$model_args --model $m"; done

server() { # Argumente an vig-tflite-server
  adb shell "cd $PHONE && ./vig-tflite-server --lib-dir $PHONE $model_args $*"
}
start_server() {
  adb shell "cd $PHONE && (nohup ./vig-tflite-server --lib-dir $PHONE $model_args > server.log 2>&1 & echo \$! > server.pid)"
  for _ in $(seq 1 60); do
    adb shell "grep -q bereit $PHONE/server.log" 2>/dev/null && break
    sleep 2
  done
  adb shell "cat $PHONE/server.log" | tee "$OUT/server-start.txt"
}
stop_server() {
  adb shell "kill \$(cat $PHONE/server.pid)" || true
  adb shell "cat $PHONE/server.log" > "$OUT/server.log" || true
}

for step in $STEPS; do
  case $step in
    bench)
      echo "== Profile auf der GPU"
      adb logcat -c
      { echo "vorher: $(thermal)"
        server --bench 200
        echo "nach allein: $(thermal)"
        sleep "$COOLDOWN"
        server --bench-together 100
        echo "nachher: $(thermal)"
      } 2>&1 | tee "$OUT/bench.txt"
      # Welcher Anteil des Graphen auf dem Delegate liegt, meldet TFLite nur
      # ins Log.
      adb logcat -d -s tflite 2>&1 | grep -E 'Replacing|delegate|GPU' | tee "$OUT/delegation.txt" || true
      ;;
    calibrate)
      adb shell chmod 755 $PHONE/vig
      start_server
      for c in $CONFIGS; do
        name=$(basename "$c" .yaml)
        echo "== calibrate $name ($(date +%T))"
        echo "$(date +%T) calibrate $name vorher $(thermal)" | tee -a "$OUT/thermal.txt"
        adb shell "cd $PHONE && taskset $GATE_MASK ./vig calibrate -c $(basename "$c") \
          -o $name-measured.yaml" > "$OUT/calibrate-$name.txt" 2>&1 || true
        echo "$(date +%T) calibrate $name nachher $(thermal)" | tee -a "$OUT/thermal.txt"
        adb pull "$PHONE/$name-measured.yaml" "$OUT/$name-measured.yaml" >/dev/null 2>&1 || true
        tail -30 "$OUT/calibrate-$name.txt"
        sleep "$COOLDOWN"
      done
      stop_server
      ;;
    gate)
      start_server
      for c in $CONFIGS; do
        name=$(basename "$c" .yaml)
        for r in $(seq 1 "$RUNS"); do
          echo "== $name Lauf $r ($(date +%T))"
          echo "$(date +%T) $name r$r $(thermal)" | tee -a "$OUT/thermal.txt"
          adb shell "cd $PHONE && VIG_GATE_COPY=1 VIG_GATE_NO_HARDWARE=1 VIG_GATE_SECONDS=$SECONDS_PER_RUN \
            $frames_env taskset $GATE_MASK ./gate-m3 $(basename "$c")" > "$OUT/gate-$name-r$r.txt" 2>&1 || true
          tail -20 "$OUT/gate-$name-r$r.txt"
          sleep "$COOLDOWN"
        done
      done
      echo "$(date +%T) Ende $(thermal)" | tee -a "$OUT/thermal.txt"
      stop_server
      ;;
    *) echo "unbekannter Schritt: $step" >&2; exit 1 ;;
  esac
done
echo "Ergebnisse: $OUT"
