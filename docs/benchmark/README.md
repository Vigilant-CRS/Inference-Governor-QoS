# Messungen

Die Zahlen in diesem Verzeichnis stammen von einer Maschine: RTX 3070 Laptop
(8 GB), Triton 2.70.0, Ubuntu 26.04. Bis zum 10.09. lief Treiber 580.173.02,
ab dem 11.09. 580.178.04; jeder Bericht nennt seinen. Ausnahmen sind die drei
Telefonberichte (`arm-phones.md`, `arm-serve.md`, `android-gpu.md`), die ihr
Gerät selbst nennen. Alle sind reproduzierbar beschrieben und **nicht** als
allgemeingültige Produktleistung zu lesen.

| Dokument | Frage | Antwort |
|---|---|---|
| [`gate-s-report.md`](gate-s-report.md) | Trägt die Scheduling-Idee im Modell? | ja, simuliert |
| [`data-plane.md`](data-plane.md) | Was kostet der Governor auf dem Draht? | 160 µs mit Shared Memory, 11,7 ms ohne |
| [`gate-m3.md`](gate-m3.md) | Schlägt er einen getunten Triton? | ja, 22,4x weniger unabgedeckte Perioden |
| [`gate-m3-r03.md`](gate-m3-r03.md) | Hält das nach der Korrektur der Lückenrechnung? | ja — Detektor 20–22x, Pose 10–12x; eine Lückenzahl aus einem Lauf ist keine Aussage |
| [`gate-m3-r04.md`](gate-m3-r04.md) | Hält es auf dem neuen Treiber 580.178.04? | ja — Detektor 21–22x, Pose 10–12x |
| [`wire-bench.md`](wire-bench.md) | Wo wirkt er, wo nicht? | nur unter Konkurrenz |
| [`wp26.md`](wp26.md) | Lösen kooperative Quanten die Aushungerung? | **ja** — 20x mehr Best-Effort-Fortschritt für 7 Punkte Abdeckung (Neumessung 2026-09-08) |
| [`load-ramp.md`](load-ramp.md) | Ab welcher Auslastung lohnt es sich? | Knick zwischen 100 % und 110 % |
| [`diy-baseline.md`](diy-baseline.md) | Reicht Supersession im Client? | 47x gegen den Eigenbau |
| [`soak.md`](soak.md) | Haelt es acht Stunden durch? | keine Drift, kein unbegrenztes Speicherwachstum im Fenster — „kein Leck" folgt daraus nicht |
| [`portability.md`](portability.md) | Laeuft es auch vor anderen OIP-Servern? | OVMS: 150/150 ohne Codeaenderung |
| [`rfdetr-variants.md`](rfdetr-variants.md) | Belegen vier echte RF-DETR-Varianten die Variantenwahl? | **nein** — die Auflaesung bestimmt die Laufzeit, das Modell fast nicht |
| [`arm-phones.md`](arm-phones.md) | Ist der Kern auf schwacher ARM-Hardware ein Engpass? | **nein** — p99 3–20 µs je Ereignis (Pixel 2/5, A53 bis A76); eine Entscheidung kostet auf dem ältesten Kern 0,08 % einer 33-ms-Periode |
| [`arm-serve.md`](arm-serve.md) | Was kostet der ganze Governor je Request auf schwacher ARM-Hardware? | **mehr als die Laptop-Budgets erlauben** — Pixel 2: +2,1 ms je Request auf dem Shm-Pfad statt +0,17 ms, größtenteils Kernelzeit; nicht größenabhängig; `vig serve` läuft dort |
| [`android-gpu.md`](android-gpu.md) | Hängt der Vorteil an Triton und an NVIDIA? (Pixel 2, Adreno 540, zweites Backend) | **der Governor läuft unverändert, der Einbruch aus Gate M3 tritt dort aber nicht ein** — mit Rechenzeit statt Laufzeit geplant schadet er (Faktor 3–4); mit gemessenen Profilen hält er bis 138 % den Detektor bei kürzeren Lücken und bezahlt mit den anderen Strömen; bei 277 % ist ein Slot schon für den Detektor zu wenig, und er verliert überall. Mit `slots: 2` und gemessener Interferenz holt er die Hälfte zurück — über Last mehr Lieferfenster für `pose` als das Backend direkt, dafür verliert der geschützte Detektor seinen Lückenvorsprung |
| [`tensorrt.md`](tensorrt.md) | Beseitigt TensorRT den Engpass? | **nein** — Auslastung 103 % → 76 %, Vorsprung halbiert, Engpass bleibt |
| [`messkette-2026-09-11.md`](messkette-2026-09-11.md) | Was hält auf dem neuen Treiber, mit korrigierten Werkzeugen? | Überlast 21–125x und Datenpfad bestanden; **Schwächen** an der Kante bei 100 %, bei Lastspitzen und in der Variantenwahl; mit XSched Gleichstand mit Triton, das VLM läuft erstmals unter dem Governor |
| [`messkette-2026-09-12.md`](messkette-2026-09-12.md) | Was bringen die Korrekturen auf echter Hardware? | Kante bei 100 % ist die Dispatch-Luecke (188 → 17 ‰), Variantenwahl ueberall 0 ‰, Lastspitzen ohne Nachteil, gelernte Marge nuetzt bis 110 % und schadet bei 125 %, Pilot relativ besser und absolut verfehlt |
| [`abnahme-2026-09-12.md`](abnahme-2026-09-12.md) | Pipelining, Versorgungsschutz, Kalibrierung: was davon traegt? | Pipelining kostet nichts und behebt die Kante (188 → 17 ‰); der Versorgungsschutz wirkt, erschlaegt aber allein den Hintergrund (998 ‰) und gehoert mit der Kalibrierung zusammen; bei starker Ueberlast bleibt die feste Marge vorn |
| [`pilot-praemption-2026-09-12.md`](pilot-praemption-2026-09-12.md) | Bekommt der Berichtspfad mit Praemption Fortschritt, ohne dass der Alarmpfad zahlt? | **auf diesem Stapel nicht** — vLLM laedt unter dem XSched-Shim nicht; der Shim kostet am geschuetzten Pfad 17–20 % Laufzeit, und eine nur angegebene Lane bringt 30 statt 2 Berichte/min und kostet 28 % Alarmzeit bei D |
| [`scenarios.md`](scenarios.md) | Welche Lastfälle tragen die Aussagen, wogegen, mit welchem Preis? | Plan für S1–S12, darunter Detektor + LLM, zwei LLMs nebeneinander, langer Job unter Sättigung, zweite GPU |
| [`reproduce.md`](reproduce.md) | Lässt sich die Kernaussage mit frei lizenzierten Modellen nachfahren? | Aufbau mit RT-DETR R18/R50 und Qwen3-0.6B (alle Apache-2.0, Digests gepinnt), drei Lastfälle, ein Befehl — samt der zwei Stolpersteine, die dabei Zeit kosten |

## Regeln, die für jede Zahl hier gelten

**Die Vergleichsseite wird getunt, nicht geschwächt.** Triton läuft mit Rate
Limiter und Prioritäten, mit denselben Modellen, derselben
Instance-Group-Konfiguration und demselben Shared-Memory-Datenpfad. Beide Seiten
werden über mehrere Client-Puffertiefen gefahren; je Strom zählt das jeweils
bessere Ergebnis (Spec 19.1).

**Die Systemlast steht im Protokoll.** Ein Lauf dieses Benchmarks lieferte
einmal 52 % statt 98 % auf unverändertem Code, nur weil andere Prozesse
mitliefen. Messungen laufen auf reservierten Kernen (`taskset -c 8-15`), und
Punkte mit Wirkung auf Produktaussagen werden wiederholt.

**Was gemessen wird, ist die Abdeckung, nicht der Durchsatz.** Anteil der
Perioden, in denen ein Ergebnis vorlag, dessen Alter unter `max_age` war
(ADR-0005). Eine Politik, die alles verwirft, fällt damit sofort auf — eine
Deadline-Miss-Rate pro Request täte das nicht.

**Verworfene Messläufe werden dokumentiert.** In `wp26.md` und in den ADRs
stehen die Läufe, die plausibel aussahen und falsch waren, mitsamt Ursache.
Wer nur die guten Zahlen zeigt, hat nicht gemessen, sondern ausgewählt.
