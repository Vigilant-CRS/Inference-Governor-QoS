# Gegenproben zum Review vom 10.09.2026

Die Dateien sind Integrationstests für die **Review-Kopie**, keine neuen
Produktfunktionen. Sie formulieren gewünschte Invarianten und schlagen auf
dem geprüften Stand absichtlich fehl. Der Hauptbericht erklärt Voraussetzungen
und Grenzen jedes Beispiels.

- [Gateway/Core/Plattform: sieben Gegenproben](gateway_current_review.rs)
- [Benchmarktracker: eine Gegenprobe](sim_current_review.rs)
- [Review](../CURRENT-STATE-REVIEW.md)

Die eingefrorene Kopie liegt in `/tmp/vig-review-20260910.F8O51m` und kann vom
System später entfernt werden. Die Testquellen bleiben hier erhalten. In
einer separaten Arbeitskopie mit dem damaligen Produktstand installieren als:

```text
gateway_current_review.rs -> crates/vig-gateway/tests/current_review.rs
sim_current_review.rs     -> crates/vig-sim/tests/current_review.rs
```

Dort ausführen:

```bash
cargo test --offline --locked -p vig-gateway -p vig-sim \
  --test current_review --no-fail-fast -- --nocapture
```

Benötigt weder echten Triton noch GPU-Inferenz. Der Actor kann seinen normalen
lesenden Hardwareprobe starten. Der Aktuationstest verwendet ausschließlich
ein Fake-Stellglied und verändert keinen Takt.

Ergebnis des Reviews: sieben fehlgeschlagene Gateway/Core/Plattform-Assertions
und eine fehlgeschlagene Benchmark-Assertion. R01 verwendet einen exklusiven
Fake-Executor mit absichtlich unabhängig gesetztem Abschlusszähler. R06 setzt
synthetisch vier Ein-Byte-Token voraus; das ist keine Messung eines bestimmten
LLM-Tokenizers. Nach Korrekturen sollen die jeweils geltenden Invarianten in
die normale Regression aufgenommen werden.
