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

`vig-core` ist ein reiner, deterministischer Zustandsautomat:

- kein I/O, keine Uhr, kein Netzwerk, kein Triton
- keine Dependencies
- keine Allokation im Entscheidungspfad, soweit praktisch erreichbar
- `now` wird uebergeben, nie abgerufen
- jede Zeitarithmetik geht ueber `vig_core::time` (dort und nur dort sind
  die Arithmetiklints lokal ausgesetzt, mit Begruendung im Modulkopf)

Wer eine dieser Regeln brechen will, schreibt zuerst ein ADR.

## Tests

Jeder Task braucht mindestens einen negativen Test (Spec 26.3). Scheduler-
Semantik wird zuerst als deterministischer Test formuliert, dann implementiert.

Der Golden-Test-Katalog aus Spec 28 (G-001 bis G-012) ist die Mindestabdeckung
fuer Phase 1.

## Build auf langsamen Dateisystemen

Hier stand einmal die Empfehlung, `CARGO_TARGET_DIR` auf `~/.cache` zu legen,
wenn das Repository auf einem FUSE-/NTFS-Mount liegt. Der Rat kostet mehr, als
er bringt, und wird hier ausdruecklich zurueckgenommen.

**Gemessen:** ein vollstaendiger Release-Build des Workspace auf einem
ntfs-3g-Mount dauert 1 min 17 s. Das ist kein Grund, irgendetwas zu verlegen.

**Was der Override kostet:** 6,6 GB Build-Artefakte wandern still auf die
Systemplatte. Auf einer Maschine, deren Systemlaufwerk bei 96 % steht, ist das
kein Detail — und es faellt erst auf, wenn nichts mehr geht.

Der Cargo-Standard ist `<workspace>/target` und damit von sich aus auf
demselben Laufwerk wie das Projekt. Ihn zu setzen ist nur dann richtig, wenn
man vorher nachgesehen hat, wieviel Platz das Ziellaufwerk hat.
