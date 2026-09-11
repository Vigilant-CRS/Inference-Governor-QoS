#!/usr/bin/env bash
# llama.cpp fuer das Telefon (aarch64, Android API 28), nur CPU: der Fall
# "Detektor plus kleines Sprachmodell" in docs/benchmark/android-gpu.md.
# Das Sprachmodell laeuft NICHT hinter dem Governor; es ist ein Nachbar, der
# um CPU, Speicherbandbreite und Waermebudget konkurriert, nicht um GPU-Zeit.
#
#   tools/android/build-llama.sh      # gibt das bin-Verzeichnis aus
set -euo pipefail

SRC=${LLAMA_SRC:-$HOME/.cache/vig-llama.cpp}
COMMIT=${LLAMA_COMMIT:-8172e65}
NDK=${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/27.2.12479018}
QUIET=${QUIET:-/run/media/dd/USB_4028/Projekte/InferenceQoS-runtime/quiet-build.sh}

if [ ! -d "$SRC/.git" ]; then
  git clone https://github.com/ggml-org/llama.cpp "$SRC"
fi
git -C "$SRC" fetch --depth 1 origin "$COMMIT" 2>/dev/null || true
git -C "$SRC" checkout -q "$COMMIT"

"$QUIET" taskset -c 0-7 nice -n 10 cmake -S "$SRC" -B "$SRC/build-android" \
  -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
  -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=android-28 \
  -DCMAKE_C_FLAGS=-march=armv8-a -DCMAKE_CXX_FLAGS=-march=armv8-a \
  -DGGML_OPENMP=OFF -DGGML_LLAMAFILE=OFF -DLLAMA_CURL=OFF \
  -DBUILD_SHARED_LIBS=OFF -DCMAKE_BUILD_TYPE=Release >&2
"$QUIET" taskset -c 0-7 nice -n 10 cmake --build "$SRC/build-android" --config Release -j8 \
  --target llama-cli llama-bench >&2
echo "$SRC/build-android/bin"
