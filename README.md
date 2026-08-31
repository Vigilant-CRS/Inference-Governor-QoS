# Vigilant OneTimer

**Adaptive Inference Governor for Edge AI**

> Keep your models. Keep Triton. Tell OneTimer what must be fresh and what must
> be fast; OneTimer decides what should run now, what should wait, what should
> be replaced by newer work and which model variant is still feasible.

Auf einem Roboter ist ein Teil der Rechenarbeit **zeitlich verderblich**. Ein
Kamerabild kann veraltet sein, bevor seine Inferenz ueberhaupt beginnt. Ein
generischer Inference Server arbeitet die Warteschlange trotzdem ab — effizient,
aber am Weltzustand vorbei.

OneTimer sitzt als protokollkompatibler Governor vor einer vorhandenen
NVIDIA-Triton-Installation, entfernt ueberholte Requests **bevor** sie GPU-Zeit
verbrauchen, laesst Arbeit nur zu, wenn sie noch sinnvoll abschliessbar ist,
schuetzt zeitkritische Wahrnehmung vor langsamer Hintergrundlast und waehlt die
beste noch rechtzeitig ausfuehrbare Modellvariante.

---

## Status

**Phase 1 von 5.** Der Scheduling-Kern und sein Simulator stehen; es gibt noch
kein Gateway und keine Triton-Anbindung.

| Phase | Inhalt | Stand |
|---|---|---|
| 0 | Workspace, CI, Lizenzbasis, Kerntypen, Sim-Clock | **fertig** |
| 1 | Queue-Policies, Deadline/Slack, Slot-Look-ahead, Varianten, Ueberlast-FSM | **fertig** |
| 1b | Simulierte Kernvergleiche — **Gate S** | **bestanden** |
| 2 | OIP-Gateway, Triton-Adapter, Shared-Memory-Passthrough | offen |
| 3 | Profiler, Online Estimator, Metrics | offen |
| 4 | Benchmark-Harness, getunte Triton-Baseline — **Gate M3** | offen |

### Gate S — simulierte Falsifikation

Gegen eine FIFO-Baseline mit modellübergreifender Priorität (= Triton mit Rate
Limiter), identischem Ankunftsprozess und identischen Laufzeitprofilen, bei
125 % Angebotslast:

| Szenario | unabgedeckte Perioden | stale compute | nützliche Ergebnisse |
|---|---|---|---|
| A — Freshness | 2,12x weniger | 4,88x weniger | 616 statt 282 |
| B — Protected vs. Best Effort | 2,66x weniger | messbar null statt 71 ‰ | 2246 statt 1166 |

Vollständiger Report samt aller Parameter:
[`docs/benchmark/gate-s-report.md`](docs/benchmark/gate-s-report.md),
reproduzierbar mit `cargo run --release -p onetimer-sim --bin gate-s`.

Der Simulator ist die Best-Case-Welt für OneTimer — kein Proxy-Overhead, keine
zweite Backend-Queue, keine Profilfehler. **Gate S kann die Hypothese
falsifizieren, aber nicht bestätigen.**

> **Es liegen keine Messwerte gegen echte Hardware vor.** Alle Zahlen in der
> Spezifikation sind Zielwerte, Rechenbeispiele oder Validierungsschwellen. Die
> Produkthypothese ist unbewiesen, bis Gate M3 sie gegen eine **getunte**
> Triton-Baseline bestaetigt oder widerlegt.

## Was OneTimer nicht ist

- Keine Hard-Realtime-Runtime und kein Safety-zertifiziertes System.
- Kein GPU-Preemptor. Eine laufende Inferenz wird nicht zurueckgeholt; deshalb
  wird ueberholte Arbeit **vor** dem Dispatch entfernt.
- Kein Ersatz fuer Triton, Holoscan oder TensorRT.
- Die Latest-Frame-Semantik ist kein Alleinstellungsmerkmal; Holoscan-Async-
  Buffer kennen sie ebenfalls. Der Unterschied liegt in der Kombination aus
  Frische, deadline-bewusster Zulassung, Variantenwahl und Protokoll-
  kompatibilitaet.

## Aufbau

```
crates/onetimer-core/   Scheduling-Kern: rein, deterministisch, ohne Dependencies
crates/onetimer-sim/    Discrete-Event-Simulator, Baseline und Gate S
docs/benchmark/         Gate-S-Report
docs/adr/               Architekturentscheidungen (Abweichungen von der Spec)
Vigilant_OneTimer_Product_Specification_v1.0.md   Die Spezifikation
```

Der Kern kennt kein I/O, keine Uhr und keine Payload. Er bekommt `now` an jedem
Eintrittspunkt uebergeben — deshalb laeuft derselbe Code im Simulator und im
spaeteren Gateway, und ein Live-Trace ist offline exakt reproduzierbar.

## Bauen und pruefen

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check licenses bans advisories sources
```

Alle vier muessen gruen sein — das ist die Definition of Done fuer jeden Task.
Liegt das Repository auf einem langsamen Dateisystem, hilft
`export CARGO_TARGET_DIR="$HOME/.cache/onetimer-target"`.

## Wo die Entscheidungen stehen

Die Spezifikation v1.0 ist die Baseline und bleibt unveraendert. Wo die
Umsetzung davon abweicht, steht der Grund in [`docs/adr/`](docs/adr/) — unter
anderem: warum das Backend als Slot-Menge und nicht als serielle Ressource
modelliert wird, warum die Erfolgsmetrik fuer Latest-Streams periodenbezogen
sein muss, und warum der Interferenzprofiler hinter das Falsifikationsgate
verschoben wurde.

## Lizenz

Apache-2.0. Siehe [LICENSE](LICENSE), [NOTICE](NOTICE) und
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

NVIDIA Triton und die NVIDIA-Containerimages werden **nicht** mitgeliefert.
