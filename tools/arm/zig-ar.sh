#!/usr/bin/env bash
# Archivierer zu `zig-cc.sh`: ein Symbolindex fuer aarch64-Objekte, den das
# `ar` des Hosts nicht zuverlaessig schreibt.
set -euo pipefail
ZIG_PYTHON="${ZIG_PYTHON:-$HOME/.cache/vig-zig/bin/python}"
exec "$ZIG_PYTHON" -m ziglang ar "$@"
