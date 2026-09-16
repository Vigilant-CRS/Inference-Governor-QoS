#!/usr/bin/env python3
"""Count physical source lines, without pretending to parse comments or inline tests.

Scope: rg-visible .rs/.py/.sh files under crates, backends, integrations,
tools, deploy. Runtime operational scripts are reported separately; third-party
XSched sources, models, generated build output and documentation are excluded.
Run from the project root. Output is JSON on stdout; no source files are changed.
"""

import collections
import datetime
import hashlib
import json
from pathlib import Path
import subprocess


def visible_files(root, paths):
    args = ["rg", "--files", *paths]
    for pattern in ("*.rs", "*.py", "*.sh"):
        args.extend(["-g", pattern])
    for excluded in ("target", "__pycache__", ".venv", "venv", "node_modules"):
        args.extend(["-g", f"!**/{excluded}/**"])
    return sorted(subprocess.check_output(args, cwd=root, text=True).splitlines())


def summarize(root, paths, grouping):
    groups = collections.defaultdict(lambda: {"files": 0, "physical_lines": 0, "nonblank_lines": 0})
    languages = collections.defaultdict(lambda: {"files": 0, "physical_lines": 0, "nonblank_lines": 0})
    files = []
    for name in paths:
        raw = (root / name).read_bytes()
        lines = raw.decode("utf-8").splitlines()
        item = {
            "path": name,
            "physical_lines": len(lines),
            "nonblank_lines": sum(bool(line.strip()) for line in lines),
            "sha256": hashlib.sha256(raw).hexdigest(),
        }
        files.append(item)
        for counter in (groups[grouping(Path(name))], languages[Path(name).suffix]):
            counter["files"] += 1
            counter["physical_lines"] += item["physical_lines"]
            counter["nonblank_lines"] += item["nonblank_lines"]
    total = {key: sum(group[key] for group in groups.values()) for key in ("files", "physical_lines", "nonblank_lines")}
    return {"total": total, "groups": dict(sorted(groups.items())), "languages": dict(sorted(languages.items())), "files": files}


root = Path.cwd()
runtime = root.parent / "InferenceQoS-runtime"
product_paths = visible_files(root, ["crates", "backends", "integrations", "tools", "deploy"])
runtime_paths = visible_files(runtime, ["skripte", "quiet-build.sh", "messungen/pilot/prepare-alarm.sh"])
report = {
    "captured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
    "method": "Physical lines including comments, blank lines and tests; nonblank still includes comments. No claim of production-only or logical SLOC. Working-tree files, not an atomic Git snapshot; per-file SHA-256 recorded.",
    "product": summarize(root, product_paths, lambda p: "/".join(p.parts[:2]) if p.parts[0] == "crates" else p.parts[0]),
    "runtime_operations": summarize(runtime, runtime_paths, lambda p: p.parts[0] if len(p.parts) > 1 else "root"),
}
print(json.dumps(report, ensure_ascii=False, indent=2))
