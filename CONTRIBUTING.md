# Contributing

## Vor jedem Commit

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check licenses bans advisories sources
```

Diese vier Kommandos sind die Definition of Done aus Spec 26.3. Ein Task ist
nicht fertig, solange eines davon rot ist.

## Reihenfolge der Arbeit

Es wird immer nur am aktuell zugewiesenen Arbeitspaket gearbeitet. Der
verbindliche Phasenplan steht in [ADR-0008](docs/adr/0008-risk-driven-phase-order.md),
nicht in Kapitel 24 der Spezifikation.

Keine CUDA-/Native-Executor-Arbeit vor bestandenem Gate M3.

## Regeln fuer den Scheduling-Kern

`onetimer-core` ist ein reiner, deterministischer Zustandsautomat:

- kein I/O, keine Uhr, kein Netzwerk, kein Triton
- keine Dependencies
- keine Allokation im Entscheidungspfad, soweit praktisch erreichbar
- `now` wird uebergeben, nie abgerufen
- jede Zeitarithmetik geht ueber `onetimer_core::time` (dort und nur dort sind
  die Arithmetiklints lokal ausgesetzt, mit Begruendung im Modulkopf)

Wer eine dieser Regeln brechen will, schreibt zuerst ein ADR.

## Tests

Jeder Task braucht mindestens einen negativen Test (Spec 26.3). Scheduler-
Semantik wird zuerst als deterministischer Test formuliert, dann implementiert.

Der Golden-Test-Katalog aus Spec 28 (G-001 bis G-012) ist die Mindestabdeckung
fuer Phase 1.

## Build auf langsamen Dateisystemen

Liegt das Repository auf einem FUSE-/NTFS-Mount, sollte das Build-Verzeichnis
auf ein natives Dateisystem zeigen:

```bash
export CARGO_TARGET_DIR="$HOME/.cache/onetimer-target"
```
