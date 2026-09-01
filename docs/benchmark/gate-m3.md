# Gate M3 — OneTimer gegen Triton

Stand: 2026-09-01 · **erster Vergleich gegen ein konkurrierendes Produkt**

## Aufbau

| | |
|---|---|
| Hardware | NVIDIA RTX 3070 Laptop, 8 GB, Treiber 580.173.02 |
| Backend | Triton 2.70.0 (`nvcr.io/nvidia/tritonserver:26.06-py3`), onnxruntime |
| Detektor | RF-DETR 512 px — das tatsächliche Modell des Anwenders, kein Stellvertreter |
| Pose / Tiefe | ResNet-18 Batch 4 / ResNet-50 Batch 4 |
| Langer Block | ResNet-50 Batch 48, rund 95 ms nicht unterbrechbar |
| Datenpfad | System Shared Memory auf beiden Seiten |
| Messdauer | 30 s je Lauf und Puffertiefe, Puffertiefen 1 und 8 |
| Kerne | 8–15 reserviert (`taskset`) |

Gemessene Laufzeitprofile (`onetimer profile`, 120 Messläufe):

| Modell | p50 | p95 | p99 |
|---|---:|---:|---:|
| rfdetr (RF-DETR 512) | 14,9 ms | 17,1 ms | 17,5 ms |
| pose_main | 4,0 ms | 4,6 ms | 5,5 ms |
| depth_main | 7,9 ms | 9,4 ms | 9,5 ms |
| vlm_main | 94,5 ms | 102,1 ms | 109,8 ms |

Daraus: geschützte serialisierte Auslastung **92 %**, mit dem VLM zusammen rund
116 % — echte Überlast auf einem Slot.

## Was gleich gehalten wird

Dieselbe GPU, dasselbe Triton, dieselben Modelle, dieselbe
Instance-Group-Konfiguration, derselbe Shared-Memory-Datenpfad, derselbe
Client, dieselben Frames. Dynamisches Batching ist auf beiden Seiten aus — bei
periodischer Einzelbildlast macht ein Batcher die Baseline nicht schneller,
sondern nur träger, und er würde hinter dem Governor eine zweite Warteschlange
erzeugen (ADR-0002).

Beide Seiten werden mit den Client-Puffertiefen 1 und 8 gefahren; je Strom
zählt das bessere Ergebnis.

## Ergebnis: Triton ohne Rate Limiter

| Strom | Abdeckung Triton | OneTimer | AoI p95 Triton | OneTimer | Faktor |
|---|---:|---:|---:|---:|---:|
| detector (RF-DETR) | 83 % | **99 %** | 76 ms | **33 ms** | **24,1x** |
| pose | 91 % | **99 %** | 51 ms | **33 ms** | **10,4x** |
| depth | 97 % | 96 % | 51 ms | 48 ms | −1,4x |
| vlm | 100 % | **0 %** | 85 ms | — | — |

Governor: 3707 angenommen, 3627 weitergereicht, 78 wegen Überalterung
verworfen, 3848 Veto-Ereignisse des Look-ahead, 78 Best-Effort-Requests nie
ausgeführt.

## Ergebnis: Triton mit Rate Limiter und Prioritäten

Triton gestartet mit `--rate-limit=execution_count`, je Instance-Group
`rate_limiter { priority: N }` — RF-DETR auf 1 (höchste), Pose und Tiefe auf 2,
der lange Block auf 5. Das ist das Werkzeug, das Triton für dieses Problem
anbietet (Spec 3.1).

| Strom | Abdeckung Triton | OneTimer | AoI p95 Triton | OneTimer | Faktor |
|---|---:|---:|---:|---:|---:|
| detector (RF-DETR) | 84 % | **99 %** | 77 ms | **33 ms** | **22,4x** |
| pose | 91 % | **99 %** | 51 ms | **33 ms** | **10,6x** |
| depth | 97 % | **99 %** | 53 ms | 61 ms | **2,6x** |
| vlm | 100 % | **0 %** | 84 ms | — | — |

Governor: 3720 angenommen, 3640 weitergereicht, 78 wegen Überalterung
verworfen, 3506 Veto-Ereignisse, 78 Best-Effort-Requests nie ausgeführt.

**Der Rate Limiter hilft der Baseline kaum** — 83 % auf 84 % beim Detektor.
Das war zu erwarten und bestätigt die Analyse aus Spec 3.1: der Rate Limiter
ordnet *wartende* Arbeit, kann aber eine bereits laufende 95-ms-Inferenz nicht
zurückholen. Und er kennt weder Frische noch Deadlines, entscheidet also nicht
darüber, ob ein Request überhaupt noch ausgeführt werden sollte.

## Bewertung

**Ziel A′ (ADR-0005): erreicht.** Gefordert waren mindestens 2x weniger
unabgedeckte Perioden für geschützte Ströme unter Überlast. Gemessen gegen die
getunte Baseline:

| Strom | unabgedeckt Triton | OneTimer | Faktor |
|---|---:|---:|---:|
| detector | 160 ‰ | 7 ‰ | **22,4x** |
| pose | 90 ‰ | 8 ‰ | **10,6x** |
| depth | 31 ‰ | 12 ‰ | **2,6x** |

Alle drei geschützten Ströme übertreffen die Schwelle. Die Age of Information
des Detektors halbiert sich von 77 ms auf 33 ms — also von rund zweieinhalb
Kameraperioden auf genau eine.

**Der Preis ist real und war vorhergesagt.** Der lange Block läuft unter
OneTimer nicht: 78 Best-Effort-Requests erreichten einen terminalen Zustand,
ohne je ausgeführt zu werden. `onetimer doctor` meldet das **vor** dem Start:

```text
WARN vlm: konservative Laufzeit 120.770ms uebersteigt die kuerzeste geschuetzte
     Periode 33.000ms (detector). Auf einem Slot wird das Modell unter Last nie
     starten. Abhilfe: mehr Slots, kuerzere Quanten oder eine hoehere Klasse.
```

Das ist keine Überraschung, sondern ADR-0012 auf echter Hardware. Ein nicht
unterbrechbarer 95-ms-Block und eine 33-ms-Periode passen auf einer
Ausführungseinheit nicht zusammen — mit oder ohne Governor. Der Unterschied
ist, dass OneTimer entscheidet, *welche* Seite verliert, und es sagt.

## Was dieses Ergebnis nicht zeigt

- **Nur eine GPU, nur ein Betriebspunkt.** Eine RTX 3070 Laptop ist keine
  Jetson und kein Serverbeschleuniger. Spec WP22 fordert den Jetson-Port
  gesondert.
- **Nur ein Lastprofil.** Die Bursts und die Lastrampe aus Spec 19.4 fehlen.
- **Kein Vergleich gegen Holoscan.** Dessen Async-Buffer-Semantik ist der
  nächstliegende Wettbewerber für die Frische-Frage.
- **Keine Variantenwahl im Spiel.** Für den Detektor liegt nur eine Variante
  vor; die Qualitäts-Deadline-Frontier aus Spec 19.7 ist unbelegt.
- **Der Best-Effort-Fall ist ungelöst**, nicht nur ungemessen (ADR-0012).

## Reproduzieren

```bash
# Umgebung: deploy/triton/README.md
onetimer doctor  -c examples/gate_m3/onetimer.yaml
onetimer profile -c examples/gate_m3/onetimer.yaml
taskset -c 8-15 target/release/gate-m3 examples/gate_m3/onetimer.yaml
```

