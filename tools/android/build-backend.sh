#!/usr/bin/env bash
# Baut vig-tflite-server fuer das Telefon: aarch64-linux-android, API 30,
# gelinkt mit dem Clang des NDK (bionic, damit die Systembibliotheken
# libEGL/libGLESv3 und die TFLite-.so aus den AARs ladbar sind).
#
# Voraussetzungen, ohne Root:
#   rustup target add aarch64-linux-android
#   ~/Android/Sdk/cmdline-tools/latest/bin/sdkmanager --install "ndk;27.2.12479018"
#
#   tools/android/build-backend.sh           # gibt den Pfad des Binaries aus
#   ANDROID_NDK_HOME=/pfad tools/android/build-backend.sh
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
NDK=${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/27.2.12479018}
API=${ANDROID_API:-30}
QUIET=${QUIET:-/run/media/dd/USB_4028/Projekte/InferenceQoS-runtime/quiet-build.sh}
CLANG=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android$API-clang
if [ ! -x "$CLANG" ]; then
  echo "NDK-Clang nicht gefunden: $CLANG (ANDROID_NDK_HOME setzen)" >&2
  exit 1
fi

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$CLANG
# Nicht aufs NTFS-Laufwerk (MFT voll, siehe quiet-build.sh).
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/vig-target/android-tflite}

"$QUIET" cargo build --release --target aarch64-linux-android \
  --manifest-path "$REPO/backends/android-tflite/Cargo.toml" >&2
echo "$CARGO_TARGET_DIR/aarch64-linux-android/release/vig-tflite-server"
