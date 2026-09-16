# Verifikation der erneuten Prüfung · 16. September 2026

Basis: `6e554ffe64e125980dd7f3e86553ed4948b330b8` plus die Reparaturen im Arbeitsbaum.
Dateihashes und Status: [recheck-validation.json](recheck-validation.json).

| Prüfung | Ergebnis |
|---|---|
| Workspace, alle Features, locked/offline | **990 bestanden**, 0 fehlgeschlagen, 6 bewusst ignoriert; 66 Testsuiten |
| Clippy, Workspace, alle Targets und Features, `-D warnings` | bestanden |
| `cargo fmt --all --check` | bestanden |
| `git diff --check` | bestanden |
| Neue Regressionstests | 8; davon 7 vor der jeweiligen Reparatur scheiternd beobachtet |

Befehle wurden über `InferenceQoS-runtime/quiet-build.sh` serialisiert,
Buildartefakte lagen im vorhandenen NVMe-Cache.

```sh
cargo test --workspace --all-features --locked --offline
cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
```

Protokolle: [Tests](recheck-workspace-tests.log), [Clippy](recheck-clippy.log).
Die ignorierten Prüfungen bleiben im Testprotokoll mit ihren Gründen sichtbar.
Es gab keine neue GPU-, Android-, ROS- oder Release-Qualifikation.
Die frühere Android-Hostprüfung bleibt in [VALIDATION.md](VALIDATION.md) dokumentiert.

Nach dem Workspace-Lauf wurde das parallel geänderte Beispiel (`min_tokens: 4`)
mit `cargo test -p vig-config --test cooperative_example --locked --offline`
nochmals geprüft: **3 bestanden**. Die protokollierten Quellhashes waren danach
unverändert. Die 990 Workspace-Tests werden dadurch nicht doppelt gezählt.
