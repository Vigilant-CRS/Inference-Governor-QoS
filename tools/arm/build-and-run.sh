#!/usr/bin/env bash
# decision-bench auf echten ARM-Kernen: statisch fuer aarch64 bauen, auf die
# angeschlossenen Handys schieben, je Kerngruppe messen, Ergebnisse holen —
# und dieselbe Messung auf dem Laptop als Bezug.
#
#   tools/arm/build-and-run.sh
#
# Keine Root-Rechte noetig: das Binary laeuft aus /data/local/tmp.
# Umgebung (alle optional):
#   ADB      Pfad zu adb
#   QUIET    Pfad zu quiet-build.sh (Builds warten auf GPU-Messungen)
#   DEVICES  "seriennummer:label:kernmaske ..." (Maske hex fuer taskset)
#   SIM_SECONDS, REPEATS, OUT
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
RUNTIME=/run/media/dd/USB_4028/Projekte/InferenceQoS-runtime
ADB=${ADB:-$RUNTIME/tools/platform-tools/adb}
QUIET=${QUIET:-$RUNTIME/quiet-build.sh}
SIM_SECONDS=${SIM_SECONDS:-300}
REPEATS=${REPEATS:-3}
OUT=${OUT:-$REPO/target/arm-results/$(date +%Y%m%d-%H%M%S)}
# Pixel 2: 0-3 Silver, 4-7 Gold (A73-Klasse). Pixel 5: 0-5 Silver, 6 Gold,
# 7 Prime (A76-Klasse).
DEVICES=${DEVICES:-"FA82P1A01294:pixel2-gold:f0 FA82P1A01294:pixel2-silver:0f 13201FDD4003E2:pixel5-prime:80 13201FDD4003E2:pixel5-silver:3f"}

export PATH="$HOME/.cargo/bin:$PATH"
# rust-lld mit der mitgelieferten musl-Laufzeit: statisch, ohne NDK, ohne
# Cross-GCC. Android fuehrt statische Linux-Binaries ohne Weiteres aus.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld

mkdir -p "$OUT"
cd "$REPO"

# SKIP_BUILD=1 misst mit den vorhandenen Binaries — etwa waehrend einer
# GPU-Messung, in der quiet-build jeden Build zurueckhaelt.
if [ "${SKIP_BUILD:-0}" != 1 ]; then
  echo "== Bauen (aarch64-unknown-linux-musl, x86_64)"
  "$QUIET" cargo build --release -p vig-sim --bin decision-bench --target aarch64-unknown-linux-musl
  "$QUIET" cargo build --release -p vig-sim --bin decision-bench
fi
BIN_ARM=$REPO/target/aarch64-unknown-linux-musl/release/decision-bench
BIN_X86=$REPO/target/release/decision-bench

device_state() { # seriennummer
  "$ADB" -s "$1" shell dumpsys battery | grep -E 'temperature|level|AC powered|USB powered' || true
  "$ADB" -s "$1" shell dumpsys power | grep -m1 'mWakefulness=' || true
}

pushed=""
for entry in $DEVICES; do
  IFS=: read -r serial label mask <<<"$entry"
  if [[ " $pushed " != *" $serial "* ]]; then
    "$ADB" -s "$serial" push "$BIN_ARM" /data/local/tmp/decision-bench >/dev/null
    "$ADB" -s "$serial" shell chmod 755 /data/local/tmp/decision-bench
    "$ADB" -s "$serial" shell getprop ro.product.model > "$OUT/$serial.model"
    pushed="$pushed $serial"
  fi
  echo "== $label ($serial, Maske $mask)"
  { echo "vorher:"; device_state "$serial"; } > "$OUT/$label.state"
  "$ADB" -s "$serial" shell taskset "$mask" /data/local/tmp/decision-bench \
    --seconds "$SIM_SECONDS" --repeats "$REPEATS" --label "$label" | tee "$OUT/$label.txt"
  { echo "nachher:"; device_state "$serial"; } >> "$OUT/$label.state"
done

# Der Laptop als Bezug. Kein geteiltes quiet-build: eine Zeitmessung neben
# einem Build misst den Build. Deshalb warten, bis keine GPU-Messung ansteht,
# und dann die Sperre exklusiv halten.
echo "== laptop"
while [ -e "$RUNTIME/measure-pending" ]; do sleep 30; done
flock -x "$RUNTIME/quiet.lock" taskset -c 8-15 "$BIN_X86" \
  --seconds "$SIM_SECONDS" --repeats "$REPEATS" --label laptop | tee "$OUT/laptop.txt"
cut -d' ' -f1-3 /proc/loadavg > "$OUT/laptop.load"

echo "Ergebnisse: $OUT"
