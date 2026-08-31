# Gate-S-Report

> Simulierte Falsifikation nach ADR-0001. **Kein Messwert gegen echte Hardware.** Der Simulator ist die Best-Case-Welt fuer OneTimer: keine Proxy-Kosten, keine zweite Backend-Queue, keine Profilfehler. Ein Effekt, der hier nicht gross ist, kann real nur kleiner werden. Gate S kann die Produkthypothese daher **falsifizieren, aber nicht bestaetigen** — Milestone M3 gegen eine getunte Triton-Baseline bleibt das entscheidende Gate.

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
| 100 % | 128 ‰ | 167 ‰ (d=1) | **1.30x** | 47 ‰ | 103 ‰ | **2.19x** | 845 / 803 |
| 110 % | 202 ‰ | 386 ‰ (d=1) | **1.91x** | 98 ‰ | 331 ‰ | **3.38x** | 740 / 569 |
| 125 % | 327 ‰ | 692 ‰ (d=1) | **2.12x** | 131 ‰ | 639 ‰ | **4.88x** | 616 / 282 |
| 150 % | 515 ‰ | 941 ‰ (d=1) | **1.83x** | 198 ‰ | 922 ‰ | **4.66x** | 442 / 54 |

## Szenario `B-protected-vs-best-effort`

Slots: 1 · Pipelining: 0 · Messdauer: 30000 ms · Streams: detector, depth, pose, vlm

| Last | unabged. Perioden OT | Baseline (beste Tiefe) | Faktor | stale compute OT | Baseline | Faktor | gueltige Ergebnisse OT / Baseline |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 % | 3 ‰ | 195 ‰ (d=1) | **65.00x** | 0 ‰ | 30 ‰ | **∞** | 2270 / 1963 |
| 75 % | 5 ‰ | 307 ‰ (d=1) | **61.40x** | 0 ‰ | 46 ‰ | **∞** | 2268 / 1688 |
| 90 % | 30 ‰ | 408 ‰ (d=1) | **13.60x** | 0 ‰ | 57 ‰ | **∞** | 2265 / 1532 |
| 100 % | 78 ‰ | 488 ‰ (d=1) | **6.26x** | 0 ‰ | 63 ‰ | **∞** | 2265 / 1431 |
| 110 % | 134 ‰ | 582 ‰ (d=1) | **4.34x** | 0 ‰ | 68 ‰ | **∞** | 2262 / 1324 |
| 125 % | 257 ‰ | 683 ‰ (d=1) | **2.66x** | 0 ‰ | 71 ‰ | **∞** | 2246 / 1166 |
| 150 % | 520 ‰ | 761 ‰ (d=1) | **1.46x** | 0 ‰ | 82 ‰ | **∞** | 1852 / 879 |

## Bewertung

### `A-freshness`

- Lebendigkeit (mind. 95 % der nuetzlichen Ergebnisse der Baseline, an jedem Lastpunkt): **erfuellt**
- Ziel A' (mind. 2x weniger unabgedeckte Perioden bei >= 110 % Last): **erreicht**
- Ziel B (mind. 30 % weniger stale compute bei >= 110 % Last): **erreicht**


### `B-protected-vs-best-effort`

- Lebendigkeit (mind. 95 % der nuetzlichen Ergebnisse der Baseline, an jedem Lastpunkt): **erfuellt**
- Ziel A' (mind. 2x weniger unabgedeckte Perioden bei >= 110 % Last): **erreicht**
- Ziel B (mind. 30 % weniger stale compute bei >= 110 % Last): **erreicht**



**Gate S: bestanden**
