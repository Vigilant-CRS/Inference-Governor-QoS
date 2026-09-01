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
| 2a | OIP-Gateway, Triton-Adapter, Konfiguration, CLI | **fertig** |
| 2b | Shared-Memory-Referenz-Passthrough | **fertig** |
| 3 | Online Runtime Estimator | **fertig** · Profiler und Prometheus offen |
| 4 | Benchmark-Harness, getunte Triton-Baseline — **Gate M3** | **bestanden** |

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

### Gemessener Zusatzaufwand des Governors

Gegen ein echtes gRPC-Backend, Release-Build, leere Tensoren:

| Backend-Laufzeit | direkt | über OneTimer | Zusatz |
|---:|---:|---:|---:|
| 1 ms | 2227 µs/Req | 2337 µs/Req | +110 µs |
| 5 ms | 6249 µs/Req | 6329 µs/Req | +80 µs |
| 20 ms | 21227 µs/Req | 21435 µs/Req | +208 µs |

Rund 0,1 bis 0,2 ms je Request — das ist die Steuerebene.

### Der Datenpfad entscheidet

Mit echten Tensorgrößen, beide Seiten gleich getunt:

| Nutzlast | direkt | über OneTimer | Zusatz |
|---|---:|---:|---:|
| 150 KB | 8130 µs | 8368 µs | +238 µs (2 %) |
| 1,2 MB | 9592 µs | 12151 µs | +2559 µs (26 %) |
| 6,2 MB | 13126 µs | 24818 µs | +11692 µs (**89 %**) |
| **6,2 MB als Shm-Referenz** | 6229 µs | 6389 µs | **+160 µs (2 %)** |

Der gRPC-Copy-Pfad ist für Kameraframes unbrauchbar — der Proxy verdoppelt die
Übertragungszeit. Der Shm-Referenz-Pfad kostet stattdessen 160 µs bei
demselben Tensor, **unabhängig von seiner Größe**: im Request steht nur, wo die
Daten liegen, und OneTimer berührt sie nie. Faktor 73.

Details und Methodik: [`docs/benchmark/data-plane.md`](docs/benchmark/data-plane.md).

### Bringt die Steuerung etwas? — auf dem echten Stack gemessen

Derselbe Workload zweimal durch denselben gRPC-Stack, einmal direkt zum Backend
und einmal über OneTimer. Bei 133 % geschützter Auslastung auf einem Slot:

| Strom | Abdeckung ohne | mit | AoI p95 ohne | mit |
|---|---:|---:|---:|---:|
| detector | 45 % | **99 %** | 216 ms | **15 ms** |
| pose | 46 % | **99 %** | 209 ms | **25 ms** |
| depth | 52 % | **99 %** | 225 ms | **32 ms** |

Dabei führt das Backend **mehr** aus, nicht weniger: 1112 statt 854 Inferenzen.

Ohne Konkurrenz bringt der Governor dagegen nichts — bei 30 % Auslastung auf
zwei Slots liefern beide Seiten alles. Und ein Best-Effort-Job, der länger
dauert als die kürzeste geschützte Periode, startet auf einem Slot nie; das ist
eine Eigenschaft nicht unterbrechbarer Ausführung und in
[ADR-0012](docs/adr/0012-best-effort-starvation.md) festgehalten.

Details: [`docs/benchmark/wire-bench.md`](docs/benchmark/wire-bench.md).

### Gate M3 — gegen getunten Triton, auf echter GPU

RTX 3070, Triton 2.70.0, RF-DETR 512 px als Detektor, System Shared Memory auf
beiden Seiten, Triton mit Rate Limiter und Prioritäten. Geschützte Auslastung
92 %, mit dem langen Block zusammen rund 116 %:

| Strom | Abdeckung Triton | OneTimer | AoI p95 Triton | OneTimer | Faktor |
|---|---:|---:|---:|---:|---:|
| detector (RF-DETR) | 84 % | **99 %** | 77 ms | **33 ms** | **22,4x** |
| pose | 91 % | **99 %** | 51 ms | **33 ms** | **10,6x** |
| depth | 97 % | **99 %** | 53 ms | 61 ms | **2,6x** |
| vlm | 100 % | **0 %** | 84 ms | — | — |

**Ziel A′ ist erreicht** — mindestens 2x weniger unabgedeckte Perioden für
geschützte Ströme, tatsächlich 2,6x bis 22,4x. Der Preis steht daneben: der
lange Best-Effort-Block läuft nicht, und `onetimer doctor` sagt das vor dem
Start.

Details, Grenzen und Reproduktion:
[`docs/benchmark/gate-m3.md`](docs/benchmark/gate-m3.md).

### Detektor neben Sprachmodell — das Szenario aus §1.3

RF-DETR bei 30 Hz und Qwen3-0.6B auf derselben RTX 3070, ein Slot:

| Betriebsart | Detektor-Abdeckung | AoI p95 | Generierungen |
|---|---:|---:|---:|
| direkt zu Triton | 77 % | 36 ms | 70 |
| über OneTimer | **98 %** | **33 ms** | 2 |

Ohne Governor sättigt das Sprachmodell die GPU und die Wahrnehmung bricht ein.
Mit Governor bleibt sie stabil — zum Preis, dass das Sprachmodell kaum noch
läuft.

**Die Zerlegung in kooperative Quanten (WP26) behebt das nicht.** Sie ist
umgesetzt und gemessen: das kleinstmögliche Quantum kostet 17 ms, die
Leerlauflücke zwischen zwei Detektorläufen beträgt 14 ms. Die Zusage „VLM neben
Detektor auf einer GPU" gilt deshalb bis auf Weiteres **ab zwei
Ausführungseinheiten**. Details:
[`docs/benchmark/wp26.md`](docs/benchmark/wp26.md),
[ADR-0015](docs/adr/0015-quantum-sizing-must-not-spend-the-deadline-reserve.md).

> **Diese Werte stammen von einer Maschine, einem Lastprofil und einer GPU.** Alle Zahlen in der
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
