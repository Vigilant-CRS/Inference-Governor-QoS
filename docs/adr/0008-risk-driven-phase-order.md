# ADR-0008: Risikogetriebene Phasenordnung

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec §24 (Arbeitspakete), §25 (Milestones), §26.2 (Commit-Reihenfolge)

## Kontext

§24 listet WP0–WP30 in Produktvollständigkeitsreihenfolge; §25 gruppiert sie zu
M0–M5. §38 stellt fest, dass es genau **eine** entscheidende Hypothese gibt und
dass sie durch M3 bewiesen oder falsifiziert wird.

Zwischen diesen beiden Aussagen besteht eine Spannung: die WP-Reihenfolge ist
nach Vollständigkeit sortiert, die Projektlogik verlangt Sortierung nach Risiko.

## Entscheidung

Verbindlicher Phasenplan. Die Ausführungsreihenfolge orientiert sich am Risiko,
nicht an der Vollständigkeit.

| Phase | Inhalt | WP | Gate |
|---|---|---|---|
| 0 | Workspace, CI, Lizenzbasis, Kerntypen, Sim-Clock | WP0, WP1 | Build/Test/Lint/Lizenz grün |
| 1 | Queue-Policies, Deadline/Slack, Slot-Look-ahead, Variant Resolver, Overload-FSM | WP2–WP6 | Golden G-001…G-012 grün |
| 1b | Simulierte Kernvergleiche A und B | *(neu)* | **Gate S** — ADR-0001 |
| 2 | OIP-Gateway, Triton-Adapter mit In-Flight-Credits, Shm-Passthrough | WP7–WP9, WP14 | End-to-End-Inferenz; Data-Plane-Overhead **getrennt** gemessen |
| 3 | Profiler, Online Estimator, Metrics | WP10, WP11, WP13 | reproduzierbare Profile |
| 4 | Benchmark-Harness, getunte Triton-Baseline, Vergleiche A/B/C | WP15–WP19 | **Gate M3** |
| ≥5 | alles Übrige | WP12, WP20–WP30 | erst nach M3 |

### Änderungen gegenüber §25

- **WP14** (Shared Memory) von M2 nach Phase 2 — Begründung in ADR-0003.
- **WP12** (Interferenzprofiler) von M2 nach ≥5 — Begründung in ADR-0006.
- **WP20** (Stress/Property/Fuzz) läuft ab Phase 1 **kontinuierlich** mit statt
  als Block vor M3. Property-Tests, die erst nach dem Gateway entstehen, hätten
  die Scheduler-Invarianten nicht während ihrer Entstehung abgesichert.
- **Neues Gate S** vor Phase 2.

### Unverändert

- Commit-/PR-Reihenfolge §26.2 **innerhalb** der Phasen.
- Definition of Done §26.3.
- Coding-Regeln §26.4.
- Verbot nativer CUDA-/Executor-Arbeit vor bestandenem Gate.

## Konsequenzen

- Phase 0 und 1 benötigen **keine GPU** und kein Triton. Sie sind vollständig auf
  jeder Entwicklungsmaschine ausführbar, was parallele Arbeit und CI vereinfacht.
- Das erste inhaltliche Signal zur Kernhypothese entsteht am Ende von Phase 1b
  statt nach Phase 4.
- Die Wettbewerbsmatrix §3.4 ist bis nach M3 nur teilweise eingelöst; das ist
  gemäß §3.5 offen zu kommunizieren.
