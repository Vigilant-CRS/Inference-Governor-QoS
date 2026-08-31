# Architecture Decision Records

Diese ADRs dokumentieren Entscheidungen, die von der
`Vigilant_OneTimer_Product_Specification_v1.0.md` abweichen oder sie präzisieren.

Die Spezifikation v1.0 bleibt unverändert als Baseline erhalten. Wo ein ADR und
die Spec sich widersprechen, gilt das ADR — jedes ADR benennt die betroffene
Spec-Stelle explizit.

| ADR | Titel | Betrifft Spec | Status |
|---|---|---|---|
| [0001](0001-simulation-first-gate-s.md) | Simulation-First-Entwicklung mit Gate S | §24, §25, §38 | Akzeptiert |
| [0002](0002-backend-in-flight-control.md) | Backend In-Flight Control (L-021) | §3.1, §7.1, §10.3 | Akzeptiert |
| [0003](0003-shared-memory-passthrough.md) | Shm-Referenz-Passthrough als primärer Datenpfad | §17.3, §4.4, §19.8 | Akzeptiert |
| [0004](0004-execution-slots.md) | Backend als explizite Execution Slots | §10.4, §10.7, §10.10 | Akzeptiert |
| [0005](0005-freshness-success-metric.md) | Erfolgsmetrik für LATEST-Streams | §4.4, Anhang D | Akzeptiert |
| [0006](0006-defer-interference-profiler.md) | Interferenzprofiler hinter Gate M3 | §13.4, WP12 | Akzeptiert |
| [0007](0007-variant-quality-provenance.md) | Herkunft der Varianten-Qualitätswerte | §12.2, §12.3 | Akzeptiert |
| [0008](0008-risk-driven-phase-order.md) | Risikogetriebene Phasenordnung | §24, §25 | Akzeptiert |
| [0009](0009-infeasibility-does-not-mean-worthless.md) | Verworfen wird, was wertlos ist — nicht, was zu spaet kommt | §10.3, §10.6 | Akzeptiert |
| [0010](0010-pessimistic-promises-optimistic-discards.md) | Pessimistisch versprechen, optimistisch verwerfen | §10.3, §13.2 | Akzeptiert |
| [0011](0011-client-clock-domains.md) | Die Erzeugungszeit kommt aus einer fremden Uhr | §16.2, §10.2, L-019 | Akzeptiert |
| [0012](0012-best-effort-starvation.md) | Aushungerung ist ein Befund, kein Nebeneffekt | §1.3, §10.6, §15 | Akzeptiert |
| [0013](0013-margin-corrects-forecasts-not-contracts.md) | Die Marge korrigiert Prognosefehler, nicht Vertragsverletzungen | §13.3 | Akzeptiert |

## Format

Kontext → Entscheidung → Konsequenzen. Kurz halten. Ein ADR beschreibt *eine*
Entscheidung und die Begründung, die zum Zeitpunkt der Entscheidung galt.
ADRs werden nicht rückwirkend umgeschrieben; sie werden durch neue ADRs abgelöst.
