# NV-16 gemessen: der Prefix-Cache ist 35 Mikrosekunden je Kontexttoken wert

Datum: 10.09.2026. Maschine: RTX 3070 Laptop, Treiber 580.173.02.
Triton 2.70.0 (26.06-**vllm**-python-py3), Qwen über das vLLM-Backend,
`max_model_len: 2048`, `gpu_memory_utilization: 0.3`.

## Was hier fehlte

ADR-0031 rechnet damit, dass die erneute Prefill-Berechnung eines gewachsenen
Prompts Geld kostet, und führt `prefill_per_token_us` als Vertragsgröße ein.
Nur war die Zahl auf dieser Maschine **nicht gemessen**: der `vlm`-Strom in
den Benchmarks ist ein ResNet-50 mit Batch 48 und hat keinen Texteingang.
`prefill_per_token_us` stand deshalb in jeder Beispielkonfiguration auf null,
und das hieß dort ausdrücklich *nicht gemessen* und nicht *kostenlos*.

Dieses Repo hat ein echtes generatives Modell — `onetimer-llm/qwen`, ein
vLLM-Modell mit `enable_prefix_caching: true`. Was fehlte, war das passende
Triton-Image; das `-py3`-Image bringt kein vLLM-Backend mit.

## Die Messung

`vig calibrate` misst zwei Geraden: eine über die Zahl erzeugter Token bei
festem Prompt (die Erzeugungsrate) und eine über die **Promptlänge** bei
fester Tokenzahl (die Kontextkosten). Je drei Läufe, dasselbe Modell,
derselbe Prompt, nur `enable_prefix_caching` unterschiedlich:

| | Sockel | Rate | **Kontext** |
|---|---:|---:|---:|
| `enable_prefix_caching: true` | 6791 / 6298 / 5656 us | 258 / 255 / 255 tok/s | **0 / 3 / 5 us** |
| `enable_prefix_caching: false` | 6747 / 6712 / 5597 us | 259 / 260 / 257 us | **35 / 36 / 39 us** |

Sockel und Erzeugungsrate sind auf beiden Seiten gleich — innerhalb der
Streuung, die auch zwischen zwei Läufen derselben Konfiguration auftritt. Die
**einzige** Größe, die sich ändert, ist der Kontextterm, und er ändert sich um
den Faktor zehn.

Genau das ist die Behauptung von ADR-0031, und sie ist damit an einem echten
Backend belegt: `prefill_per_token = 0` heißt „wirksamer Prefix-Cache", und
ohne ihn kostet ein Kontexttoken 35 bis 39 Mikrosekunden.

## Was das für die Zerlegung heißt

Derselbe Vertrag — 128 Token Gesamtbudget, Quanten zu 8, Sockel 6,7 ms,
258 Token/s — durch `vig doctor`:

| | Aufschlag der Zerlegung | größter Tokenabstand |
|---|---:|---:|
| mit Cache (0 us) | 19 % | 10,6 ms |
| ohne Cache (36 us) | **26 %** | **14,9 ms** |

Und das ist die **Untergrenze**: `doctor` rechnet ohne Prompt. Mit einem
500-Token-Prompt trägt das letzte der 16 Quanten rund 628 Token Kontext, und
allein deren Prefill kostet dann 628 × 36 us = **22,6 ms** — gegen einen
Sockel von 6,7 ms. Das späte Quantum kostet mehr als das dreifache des
frühen, und genau dafür schneidet der Kern es kleiner zu.

## Was die Messung nicht sagt

**Sie gilt für dieses Modell und diesen Kontextbereich.** Der Kalibrierprompt
ist rund 276 Token lang; der Aufwand von Attention wächst superlinear im
Kontext. Die Gerade unterschätzt die Grenzkosten bei 2000 Token — in der
sicheren Richtung, denn ein unterschätzter Kontextterm ergibt ein größeres
Quantum, und ein größeres Quantum ist die kühnere Planung. Das steht so in
`calibrate.rs` und in ADR-0031.

**Sie sagt nichts über die Ausgabequalität.** Ob ein zerlegter Auftrag
dasselbe erzeugt wie ein ungeteilter, ist eine andere Frage — sie steht in
der Abnahme von NV-16 und ist hier nicht beantwortet.

**Der Prefix-Cache ist eine Einstellung des Backends, keine Zusage.** vLLM
räumt ihn unter Speicherdruck. Was heute 0 us misst, kann unter Last 36 us
sein, und dann plant der Governor mit der falschen Zahl. Wer sich darauf
verlässt, misst nach — oder trägt den gemessenen Wert **ohne** Cache ein und
plant konservativ.
