#!/usr/bin/env bash
# "Detektor plus kleines Sprachmodell" auf dem Telefon
# (docs/benchmark/android-gpu.md):
#
#   - der Detektor auf der GPU, gemessen mit gate-m3 (direkt gegen Governor),
#   - daneben Qwen2.5-0.5B-Instruct (Q4_K_M, Apache-2.0) in llama.cpp auf
#     der CPU, NICHT hinter dem Governor.
#
# Die Konkurrenz ist damit keine GPU-Zeit, sondern CPU, Speicherbandbreite
# und Waermebudget. Der Governor sieht das Sprachmodell nicht; er sieht nur,
# ob die Laufzeiten des Detektors ihren Plan halten.
#
#   tools/android/llm-neighbour.sh
#
# Umgebung (alle optional): ADB, SERIAL, RUNS (3), COOLDOWN (120), OUT,
# CONFIG (examples/android_gpu/vig-llm.yaml), LLM_MASK (f0, Gold-Kerne),
# GATE_MASK (0f, Silver-Kerne), LLM_THREADS (4), LLM_REPS (30)
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
RUNTIME=${RUNTIME:-/run/media/dd/USB_4028/Projekte/InferenceQoS-runtime}
ADB=${ADB:-$HOME/Android/Sdk/platform-tools/adb}
SERIAL=${SERIAL:-FA82P1A01294}
RUNS=${RUNS:-3}
COOLDOWN=${COOLDOWN:-120}
CONFIG=${CONFIG:-examples/android_gpu/vig-llm.yaml}
LLM_MASK=${LLM_MASK:-f0}
GATE_MASK=${GATE_MASK:-0f}
LLM_THREADS=${LLM_THREADS:-4}
LLM_REPS=${LLM_REPS:-30}
OUT=${OUT:-$RUNTIME/android-gpu-$(date +%Y-%m-%d)/llm-$(date +%H%M%S)}
PHONE=/data/local/tmp/vigtfl
GGUF=qwen2.5-0.5b-instruct-q4_k_m.gguf

mkdir -p "$OUT"
cd "$REPO"
adb() { "$ADB" -s "$SERIAL" "$@"; }
thermal() {
  adb shell dumpsys thermalservice |
    sed -n '/Current temperatures from HAL/,/Current cooling/p' | grep mValue |
    sed 's/.*mValue=\([0-9.]*\).*mName=\([^,]*\).*/\2=\1/' | tr '\n' ' '
  echo
}
llm() { # Wiederholungen -> CSV von llama-bench (nur Generierung, 64 Token)
  adb shell "cd $PHONE && taskset $LLM_MASK ./llama-bench -m $GGUF -p 0 -n 64 -r $1 -t $LLM_THREADS -o csv"
}

echo "== Schieben"
adb push "$RUNTIME/android-llm/bin/llama-bench" "$RUNTIME/android-llm/$GGUF" $PHONE/ >/dev/null
adb push "$CONFIG" "$PHONE/$(basename "$CONFIG")" >/dev/null
adb shell chmod 755 $PHONE/llama-bench

echo "== Sprachmodell allein"
echo "$(date +%T) llm-allein $(thermal)" | tee -a "$OUT/thermal.txt"
llm 10 | tee "$OUT/llm-alone.csv"
sleep "$COOLDOWN"

adb shell "cd $PHONE && (nohup ./vig-tflite-server --lib-dir $PHONE --model detector=efficientdet_lite0_nms.tflite > server-llm.log 2>&1 & echo \$! > server.pid)"
for _ in $(seq 1 60); do
  adb shell "grep -q bereit $PHONE/server-llm.log" 2>/dev/null && break
  sleep 2
done

gate() { # Ausgabedatei
  adb shell "cd $PHONE && VIG_GATE_COPY=1 VIG_GATE_NO_HARDWARE=1 taskset $GATE_MASK ./gate-m3 $(basename "$CONFIG")" > "$1" 2>&1 || true
}

for r in $(seq 1 "$RUNS"); do
  echo "== ohne Sprachmodell, Lauf $r"
  echo "$(date +%T) ohne r$r $(thermal)" | tee -a "$OUT/thermal.txt"
  gate "$OUT/gate-alone-r$r.txt"
  tail -12 "$OUT/gate-alone-r$r.txt"
  sleep "$COOLDOWN"

  echo "== mit Sprachmodell, Lauf $r"
  echo "$(date +%T) mit r$r $(thermal)" | tee -a "$OUT/thermal.txt"
  llm "$LLM_REPS" > "$OUT/llm-during-r$r.csv" 2>&1 &
  llm_pid=$!
  sleep 5
  gate "$OUT/gate-llm-r$r.txt"
  tail -12 "$OUT/gate-llm-r$r.txt"
  wait "$llm_pid" || true
  echo "$(date +%T) nach mit r$r $(thermal)" | tee -a "$OUT/thermal.txt"
  sleep "$COOLDOWN"
done

adb shell "kill \$(cat $PHONE/server.pid)" || true
echo "Ergebnisse: $OUT"
