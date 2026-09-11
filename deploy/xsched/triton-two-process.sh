#!/usr/bin/env bash
# Zwei Triton-Prozesse auf einer GPU, wahlweise unter XSched (NV-15).
#
#   A: geschuetzte Modelle   gRPC 9201  HTTP 9200  Metriken 9202  Prioritaet 1
#   B: Hintergrundmodell     gRPC 9101  HTTP 9100  Metriken 9102  Prioritaet 0
#
#   XSCHED_DIR=… MODELS=… triton-two-process.sh up [xsched|plain] [LV2|TSG|TRAP]
#   triton-two-process.sh down
#
# Die entscheidende Zeile ist CUXTRA_CUDA_LIB (siehe README.md): ohne sie
# laedt cuxtra eine zweite libcuda in den Prozess.
set -euo pipefail

XSCHED_DIR=${XSCHED_DIR:-./xsched}
MODELS=${MODELS:?Modellrepository angeben (MODELS=/pfad)}
MODELS_A=${MODELS_A:-rfdetr pose_main depth_main}
MODELS_B=${MODELS_B:-vlm_main}
IMG=${IMG:-nvcr.io/nvidia/tritonserver:26.06-py3}
REAL_CUDA=${REAL_CUDA:-/usr/local/cuda/compat/lib.real/libcuda.so.1}

# --ipc/--pid host: der Shim spricht mit xserver ueber Shared Memory und
# meldet Prozesskennungen; --net host: feste Ports ohne Portweiterleitung.
COMMON=(--device nvidia.com/gpu=all --ipc=host --pid=host --net=host
        -v "$(cd "$MODELS" && pwd):/models:ro"
        -v "$(cd "$XSCHED_DIR" && pwd)/output:/xsched:ro")

# Die Implementierung bestimmt das Level: LV2 ist Level 2, TSG und TRAP sind
# Unterbrechungen und wirken erst ab Level 3 (XSched ruft Interrupt() erst
# ab kPreemptLevelInterrupt). Ein gesetztes LEVEL, das nicht passt, wird
# abgelehnt statt ueberstimmt.
level_for() {
  local want
  case "$1" in
    LV2) want=2 ;;
    TSG|TRAP) want=3 ;;
    *) echo "unbekannte Implementierung: $1" >&2; return 1 ;;
  esac
  if [ -n "${LEVEL:-}" ] && [ "$LEVEL" != "$want" ]; then
    echo "LEVEL=$LEVEL passt nicht zu $1 (braucht $want)" >&2
    return 1
  fi
  echo "$want"
}

shim_env() { # Prioritaet Schwelle Batch Implementierung Level
  local impl=""
  [ "$4" != "TRAP" ] && impl="XSCHED_CUDA_LV3_IMPL=$4"
  echo "export XSCHED_SCHEDULER=GLB XSCHED_AUTO_XQUEUE=ON XSCHED_AUTO_XQUEUE_LEVEL=$5 \
XSCHED_AUTO_XQUEUE_PRIORITY=$1 XSCHED_AUTO_XQUEUE_THRESHOLD=$2 XSCHED_AUTO_XQUEUE_BATCH_SIZE=$3 \
$impl XSCHED_CUDA_LIB=$REAL_CUDA CUXTRA_CUDA_LIB=$REAL_CUDA LD_LIBRARY_PATH=/xsched/lib:\${LD_LIBRARY_PATH:-};"
}

load_flags() { local f=""; for m in $1; do f="$f --load-model=$m"; done; echo "$f"; }

# Je eine Zeile: in `bash -c` beendet ein Zeilenumbruch den Befehl.
triton_a="tritonserver --model-repository=/models --allow-client-shm=true --model-control-mode=explicit$(load_flags "$MODELS_A") --grpc-port=9201 --http-port=9200 --metrics-port=9202"
triton_b="tritonserver --model-repository=/models --allow-client-shm=true --model-control-mode=explicit$(load_flags "$MODELS_B") --grpc-port=9101 --http-port=9100 --metrics-port=9102"

wait_ready() {
  for _ in $(seq 1 90); do
    curl -sf "localhost:$1/v2/health/ready" >/dev/null && return 0
    sleep 2
  done
  echo "Port $1 nicht bereit" >&2
  return 1
}

case "${1:-}" in
  up)
    mode=${2:-xsched}; impl=${3:-LV2}
    level=$(level_for "$impl") || exit 2
    docker rm -f xs-server xs-triton-a xs-triton-b >/dev/null 2>&1 || true
    pre_a=""; pre_b=""
    if [ "$mode" = xsched ]; then
      docker run -d --name xs-server "${COMMON[@]}" "$IMG" \
        bash -c "/xsched/bin/xserver HPF 50000" >/dev/null
      sleep 2
      pre_a=$(shim_env 1 "${THRESH_A:-16}" "${BATCH_A:-8}" "$impl" "$level")
      pre_b=$(shim_env 0 "${THRESH_B:-4}" "${BATCH_B:-2}" "$impl" "$level")
    fi
    docker run -d --name xs-triton-a "${COMMON[@]}" "$IMG" bash -c "$pre_a exec $triton_a" >/dev/null
    docker run -d --name xs-triton-b "${COMMON[@]}" "$IMG" bash -c "$pre_b exec $triton_b" >/dev/null
    # Unter `set -e` beendet ein `a && b` nichts: scheitert a, laeuft die
    # naechste Zeile trotzdem und meldet Erfolg.
    wait_ready 9200 || exit 1
    wait_ready 9100 || exit 1
    if [ "$mode" = xsched ]; then
      echo "bereit: $mode $impl level=$level"
    else
      echo "bereit: $mode"
    fi
    ;;
  down)
    docker rm -f xs-server xs-triton-a xs-triton-b >/dev/null 2>&1 || true
    ;;
  *)
    echo "usage: $0 up [xsched|plain] [LV2|TSG|TRAP] | down" >&2
    exit 2
    ;;
esac
