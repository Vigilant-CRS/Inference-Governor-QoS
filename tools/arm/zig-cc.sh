#!/usr/bin/env bash
# C-Compiler fuer die statischen musl-Builds von `vig`.
#
# `vig` bringt ueber tonic `tls-ring` die Krypto-Bibliothek ring mit, und ring
# enthaelt C und Assembler. rust-lld linkt, uebersetzt aber kein C; ohne
# Cross-Compiler scheitert der Build an `aarch64-linux-musl-gcc`. zig bringt
# einen clang mit musl-Zielen mit, installierbar ohne Root:
#
#   python3 -m venv ~/.cache/vig-zig && ~/.cache/vig-zig/bin/pip install ziglang==0.16.0
#
# cc-rs erkennt zig als clang und haengt das Rust-Ziel an, etwa
# `--target=aarch64-unknown-linux-musl`. zig versteht diese Schreibweise nicht;
# hier wird daraus `-target aarch64-linux-musl`.
set -euo pipefail
ZIG_PYTHON="${ZIG_PYTHON:-$HOME/.cache/vig-zig/bin/python}"
target=aarch64-linux-musl
args=()
for a in "$@"; do
  case "$a" in
    --target=*-unknown-linux-musl) target="${a#--target=}"; target="${target/-unknown-/-}" ;;
    --target=*) ;;
    *) args+=("$a") ;;
  esac
done
exec "$ZIG_PYTHON" -m ziglang cc -target "$target" "${args[@]}"
