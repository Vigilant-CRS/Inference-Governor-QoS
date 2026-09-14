# Gate-S-Report

> Simulierte Falsifikation nach ADR-0001. **Kein Messwert gegen echte Hardware.** Der Simulator ist die Best-Case-Welt fuer Vigilant: keine Proxy-Kosten, keine zweite Backend-Queue, keine Profilfehler. Ein Effekt, der hier nicht gross ist, kann real nur kleiner werden. Gate S kann die Produkthypothese daher **falsifizieren, aber nicht bestaetigen** — Milestone M3 gegen eine getunte Triton-Baseline bleibt das entscheidende Gate.

## Versuchsaufbau

Beide Governoren bekommen denselben Ankunftsprozess, dieselben Slots, dieselben Vertraege, dieselben Laufzeitprofile und dieselben Weckrufe (1 ms). Die tatsaechliche Backendlaufzeit eines Frames wird aus `(seed, request_id, variant)` gezogen und ist damit unabhaengig von der Reihenfolge, in der ein Governor Arbeit startet.

| Parameter | Wert |
|---|---|
| Laufzeitverteilung | lognormal, an p50/p99 kalibriert, geklemmt auf `[p50/2, p99*3]` |
| Planungsgrundlage des Schedulers | `p99 * 1.10` (Spec 13.2) |
| Angebotslast | [500, 750, 900, 1000, 1100, 1250, 1500] Promille der Slot-Kapazitaet |
| Baseline-Queue-Tiefen | [1, 4, 16], beste je Lastpunkt gewertet |
| Seeds | [5eed0001, 5eed0002, 5eed0003, 5eed0004, 5eed0005], Median berichtet |
| Baseline-Politik | bounded FIFO mit modelluebergreifender Prioritaet (= Triton + Rate Limiter) |
| Erfolgsmetrik | unabgedeckte Perioden nach ADR-0005, nicht Deadline-Misses pro Request |
| Nebenbedingung | Lebendigkeit nach ADR-0009: mind. 95 % der nuetzlichen Ergebnisse der Baseline |

## Szenario `A-freshness`

Slots: 1 · Pipelining: 0 · Messdauer: 30000 ms · Streams: detector

| Last | unabged. Perioden OT | Baseline (beste Tiefe) | Faktor | stale compute OT | Baseline | Faktor | gueltige Ergebnisse OT / Baseline |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 % | 0 ‰ | 0 ‰ (d=1) | **—** | 0 ‰ | 0 ‰ | **—** | 910 / 910 |
| 75 % | 117 ‰ | 117 ‰ (d=1) | **1.00x** | 0 ‰ | 0 ‰ | **—** | 910 / 910 |
| 90 % | 184 ‰ | 184 ‰ (d=1) | **1.00x** | 2 ‰ | 2 ‰ | **1.00x** | 908 / 908 |
| 100 % | 201 ‰ | 167 ‰ (d=1) | **0.83x** | 8 ‰ | 103 ‰ | **12.88x** | 794 / 803 |
| 110 % | 379 ‰ | 386 ‰ (d=1) | **1.02x** | 6 ‰ | 331 ‰ | **55.17x** | 578 / 569 |
| 125 % | 479 ‰ | 692 ‰ (d=1) | **1.44x** | 23 ‰ | 639 ‰ | **27.78x** | 476 / 282 |
| 150 % | 548 ‰ | 941 ‰ (d=1) | **1.72x** | 108 ‰ | 922 ‰ | **8.54x** | 412 / 54 |

## Szenario `B-protected-vs-best-effort`

Slots: 1 · Pipelining: 0 · Messdauer: 30000 ms · Streams: detector, depth, pose, vlm

| Last | unabged. Perioden OT | Baseline (beste Tiefe) | Faktor | stale compute OT | Baseline | Faktor | gueltige Ergebnisse OT / Baseline |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 % | 3 ‰ | 195 ‰ (d=1) | **65.00x** | 0 ‰ | 30 ‰ | **∞** | 2270 / 1963 |
| 75 % | 5 ‰ | 307 ‰ (d=1) | **61.40x** | 0 ‰ | 46 ‰ | **∞** | 2268 / 1688 |
| 90 % | 30 ‰ | 408 ‰ (d=1) | **13.60x** | 0 ‰ | 57 ‰ | **∞** | 2265 / 1532 |
| 100 % | 78 ‰ | 488 ‰ (d=1) | **6.26x** | 0 ‰ | 63 ‰ | **∞** | 2265 / 1431 |
| 110 % | 138 ‰ | 582 ‰ (d=1) | **4.22x** | 0 ‰ | 68 ‰ | **∞** | 2262 / 1324 |
| 125 % | 321 ‰ | 683 ‰ (d=1) | **2.13x** | 0 ‰ | 71 ‰ | **∞** | 2181 / 1166 |
| 150 % | 395 ‰ | 761 ‰ (d=1) | **1.93x** | 0 ‰ | 82 ‰ | **∞** | 1985 / 879 |

## Bewertung

### `A-freshness`

- Lebendigkeit (mind. 95 % der nuetzlichen Ergebnisse der Baseline, an jedem Lastpunkt): **erfuellt**
- Ziel A' (mind. 2x weniger unabgedeckte Perioden bei >= 110 % Last): **nicht erreicht**
- Ziel B (mind. 30 % weniger stale compute bei >= 110 % Last): **erreicht**


### `B-protected-vs-best-effort`

- Lebendigkeit (mind. 95 % der nuetzlichen Ergebnisse der Baseline, an jedem Lastpunkt): **erfuellt**
- Ziel A' (mind. 2x weniger unabgedeckte Perioden bei >= 110 % Last): **erreicht**
- Ziel B (mind. 30 % weniger stale compute bei >= 110 % Last): **erreicht**



**Gate S: bestanden**
