# Messungen

Alle Zahlen in diesem Verzeichnis stammen von einer Maschine: RTX 3070 Laptop
(8 GB), Treiber 580.173.02, Triton 2.70.0, Ubuntu 26.04. Sie sind reproduzierbar
beschrieben und **nicht** als allgemeingültige Produktleistung zu lesen.

| Dokument | Frage | Antwort |
|---|---|---|
| [`gate-s-report.md`](gate-s-report.md) | Trägt die Scheduling-Idee im Modell? | ja, simuliert |
| [`data-plane.md`](data-plane.md) | Was kostet der Governor auf dem Draht? | 160 µs mit Shared Memory, 11,7 ms ohne |
| [`gate-m3.md`](gate-m3.md) | Schlägt er einen getunten Triton? | ja, 22,4x weniger unabgedeckte Perioden |
| [`wire-bench.md`](wire-bench.md) | Wo wirkt er, wo nicht? | nur unter Konkurrenz |
| [`wp26.md`](wp26.md) | Lösen kooperative Quanten die Aushungerung? | nein, gemessen und begründet |
| [`load-ramp.md`](load-ramp.md) | Ab welcher Auslastung lohnt es sich? | Knick zwischen 100 % und 110 % |
| [`diy-baseline.md`](diy-baseline.md) | Reicht Supersession im Client? | 47x gegen den Eigenbau |
| [`soak.md`](soak.md) | Haelt es acht Stunden durch? | keine Drift, kein Leck |

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
