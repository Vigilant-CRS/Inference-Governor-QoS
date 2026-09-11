#!/usr/bin/env bash
# Holt XSched auf dem gepinnten Stand, wendet den sm86-Patch an und baut die
# CUDA-Plattform im Triton-Image — mit dessen CUDA und GCC, damit Shim und
# Laufzeit zusammenpassen. Braucht keine GPU.
#
#   deploy/xsched/build-xsched.sh <zielverzeichnis>
#
# Ergebnis: <zielverzeichnis>/output/{lib,bin} mit libshimcuda.so, xserver, xcli.
set -euo pipefail

DEST=${1:?Zielverzeichnis fuer den XSched-Checkout angeben}
PIN=f49289f0220931df78de948ed841ecbaf960a919
IMG=${IMG:-nvcr.io/nvidia/tritonserver:26.06-py3}
HERE=$(cd "$(dirname "$0")" && pwd)

if [ ! -d "$DEST/.git" ]; then
  git clone -q https://github.com/XpuOS/xsched.git "$DEST"
fi
git -C "$DEST" checkout -q "$PIN"
git -C "$DEST" submodule update --init --recursive -q

# Der Patch ist idempotent anzuwenden: schon angewendet heisst fertig.
if git -C "$DEST" apply --check "$HERE/xsched-sm86-lv2.patch" 2>/dev/null; then
  git -C "$DEST" apply "$HERE/xsched-sm86-lv2.patch"
else
  git -C "$DEST" apply --reverse --check "$HERE/xsched-sm86-lv2.patch" \
    || { echo "Patch passt nicht auf $PIN" >&2; exit 1; }
fi

docker run --rm -v "$(cd "$DEST" && pwd):/xsched" "$IMG" \
  bash -c 'pip install -q cmake && cd /xsched && make cuda'

ls "$DEST/output/lib/libshimcuda.so" "$DEST/output/bin/xserver"
