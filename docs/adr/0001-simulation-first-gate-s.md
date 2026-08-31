# ADR-0001: Simulation-First-Entwicklung mit Gate S

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec §24 (Arbeitspakete), §25 (Milestones), §38 (die eine Hypothese)

## Kontext

Die Spec definiert in §38 genau eine entscheidende Hypothese und macht Milestone
M3 zum Falsifikationsgate. Die Arbeitspaketreihenfolge in §24 ist jedoch nach
Produktvollständigkeit sortiert, nicht nach Risiko: das erste belastbare Signal
zur Kernhypothese entsteht erst nach WP0–WP20, also nach Gateway, Triton-Adapter,
Profiler, Metrics und Benchmark-Harness.

Für ein Projekt, dessen Spec selbst explizite Kill-Kriterien formuliert (§19.8),
ist das sehr spät und sehr teuer.

Entscheidende Beobachtung: Die Kernvergleiche A (Freshness, §19.5) und B
(Protected vs. Best Effort, §19.6) sind vollständig im Discrete-Event-Simulator
aus WP1 darstellbar. Sie benötigen Ankunftsprozesse, Laufzeitverteilungen,
Queue-Policies und Deadlines — keine GPU, kein Triton, kein Netzwerk.

## Entscheidung

Wir führen ein zusätzliches **Gate S** (Simulation) nach WP2–WP6 ein, vor jeder
Netzwerkarbeit.

Gate S ist bestanden, wenn im Simulator gegen eine FIFO-Baseline mit **identischen
Laufzeitannahmen und identischem Ankunftsprozess** mindestens eines erreicht wird:

- Ziel A' (vgl. ADR-0005): mindestens 2x weniger unabgedeckte Perioden für
  Protected-Streams unter 125 % Offered Load; oder
- Ziel B: mindestens 30 % weniger Stale Compute.

Wird keines der beiden erreicht, wird vor Phase 2 die Positionierung überprüft.

## Begründung

Der Simulator ist die **Best-Case-Welt** für OneTimer: kein Proxy-Overhead,
perfekte Laufzeitkenntnis, kein Backend-Jitter, keine zweite Backend-Queue,
keine Profilfehler. Ein Effekt, der dort nicht groß ist, kann auf realer Hardware
nur kleiner werden.

Gate S kann die Hypothese daher **falsifizieren, aber nicht bestätigen**. Genau
das ist beabsichtigt: ein billiges Gate, das nur in eine Richtung schließt.

## Konsequenzen

**Positiv**

- Falsifikation wird um Größenordnungen billiger — Wochen statt Monate.
- Der Simulator wird kein Wegwerf-Testcode, sondern dauerhaftes Analysewerkzeug.
  Der Event-Trace-Replay aus §30.2 nutzt dieselbe Engine.
- Die Baseline-Frage aus §19.1 („kein Strohmann") wird zweimal beantwortet:
  in Simulation gegen FIFO, real gegen getunten Triton.

**Negativ / Risiko**

- Ein zu optimistisch parametrisierter Simulator könnte einen Effekt zeigen, den
  es real nicht gibt. Gegenmittel: alle Laufzeitverteilungen und
  Ankunftsprozesse für Gate S werden begründet, versioniert und im Gate-S-Report
  offengelegt; Parameter werden nicht nachträglich zugunsten des Ergebnisses
  angepasst.
- Gefahr, den Simulator als Beweis misszuverstehen. Deshalb ausdrücklich:

> **Gate S ersetzt Milestone M3 nicht.** M3 gegen eine getunte Triton-Baseline
> auf echter Hardware bleibt das produktentscheidende Gate.
