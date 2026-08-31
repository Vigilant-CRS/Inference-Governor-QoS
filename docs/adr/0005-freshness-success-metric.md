# ADR-0005: Erfolgsmetrik für supersedierbare Streams

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec §4.4 (Go/No-Go-Zielwerte), §18 (Metriken), §19.5, Anhang D Punkt 8

## Kontext

§4.4 Ziel A lautet: „mindestens 2x weniger Protected/Critical Deadline Misses
unter relevanter Überlast". Anhang D Punkt 8 wiederholt das.

Bei `LATEST` / `LATEST_PER_KEY` ist „Deadline Miss pro Request" jedoch nicht
wohldefiniert und im Vergleich systematisch verzerrt:

- **Zählt man supersedierte Requests als Miss**, verliert OneTimer per
  Konstruktion — Supersession ist ein absichtliches Verwerfen, keine
  Vertragsverletzung.
- **Zählt man sie nicht**, sinkt die Miss-Rate trivial, weil OneTimer den Nenner
  verkleinert. Eine Politik, die 90 % aller Frames supersediert, erreicht eine
  nahezu perfekte Miss-Rate, ohne dem Roboter zu nützen.

Beide Lesarten sind angreifbar. Ein Reviewer, ein Design-Partner oder ein
NVIDIA-Ingenieur wird genau hier ansetzen — und §3.5 sowie §19.1 verpflichten das
Projekt ausdrücklich darauf, sich nicht durch Messkonstruktion zu begünstigen.

## Entscheidung

Für supersedierbare Streams ist die Erfolgsmetrik **periodenbezogen**, nicht
requestbezogen.

**Primärmetrik — Fresh Result Coverage:**

```text
Zerlege die Messdauer in Fenster der Länge period_ms.
Ein Fenster gilt als ABGEDECKT, wenn in ihm mindestens ein Ergebnis geliefert
wurde, dessen Information Age (t_c - t_g) <= max_age war.

coverage = abgedeckte Fenster / alle Fenster
```

Diese Metrik ist gegen beide Manipulationen immun:

- Supersession hilft nur, wenn sie zu **mehr rechtzeitig gelieferten frischen
  Ergebnissen** führt.
- Ein leergeräumter Stream fällt sofort auf, weil unabgedeckte Fenster entstehen.

Sie entspricht außerdem der Frage, die das Robotik-System tatsächlich stellt:
*Hatte ich in diesem Regelzyklus eine hinreichend aktuelle Wahrnehmung?*

**Sekundärmetriken:** Age of Information p50/p95/p99 pro Stream;
`stale_compute_seconds_total`; Anzahl gelieferter valider Ergebnisse pro Sekunde.

**Unverändert:** Für `NEVER_DROP`- und `FIFO`-Streams bleibt die klassische
requestbezogene Deadline-Miss-Rate korrekt und wird weiter verwendet. Dort gibt
es keine Supersession und damit keine Nennerverzerrung.

## Konsequenzen

- **Ziel A wird umformuliert:** „mindestens 2x weniger unabgedeckte Perioden
  (`1 - coverage`) für Protected-Streams unter definierter Überlast."
- **Ziel B bleibt unverändert:** mindestens 30 % weniger Stale Compute.
- §4.4 und Anhang D Punkt 8 sind entsprechend zu lesen.
- Neue Metrik im Exporter: `onetimer_fresh_coverage_ratio{model,stream}` sowie
  `onetimer_uncovered_periods_total{model,stream}`.
- Der Benchmark-Report muss `period_ms` und `max_age` je Stream ausweisen, weil
  die Metrik ohne diese Parameter bedeutungslos ist.
