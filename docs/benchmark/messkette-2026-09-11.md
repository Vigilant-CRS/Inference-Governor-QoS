# Messkette vom 11.09.2026: was hält, was nicht

Maschine: RTX 3070 Laptop (8 GB), Treiber 580.178.04, Triton 2.70.0
(26.06), ONNX-Modelle, System Shared Memory, Messung auf `taskset -c 8-15`.
Binaries vom Stand `b0b36b5`, also mit `TCP_NODELAY` in den Werkzeugen. Die
Kette davor (`11c`) wurde nach acht Minuten abgebrochen, weil sie ohne diese
Korrektur lief ([arm-serve](arm-serve.md)).

Zwei Läufe: die Kette `11d` (15:25–16:52) und ein Nachlauf `11e`
(16:55–17:44) für die Blöcke, die der Lastwächter als verschmutzt markiert
hatte oder die ein Aufbaufehler entwertet hatte. Rohdaten, Wächterprotokoll
und Taktmitschnitt: `InferenceQoS-runtime/measure-chain-2026-09-11d/` und
`.../measure-nachlauf-2026-09-11e/`.

**Gültigkeit.** Der Wächter schreibt alle 30 s jeden fremden Prozess über
30 % CPU mit. Im Nachlauf gab es in 91 Proben keinen einzigen. In der Kette
markierte er Browserlast (Chrome bis 117 %) und Gradle-Builds einer anderen
Sitzung (Java bis 500 %). Unten steht bei jeder Tabelle, aus welchem Lauf sie
stammt; verschmutzte Blöcke sind entweder ersetzt oder als solche benannt.

## Datenpfadbudgets (NV-20): bestanden

Kette, ohne Fremdlast.

| Pfad | Nutzlast | Zusatz p50 | Anteil | Urteil |
|---|---|---:|---:|---|
| Mock, Shm | 150 KB / 1,2 MB / 6,2 MB | +223 / +236 / +210 µs | 3,2–3,6 % | PASS |
| Mock, gRPC-Kopie | 150 KB | +316 µs | 4,8 % | PASS |
| Mock, gRPC-Kopie | 1,2 / 6,2 MB | +1121 / +4771 µs | 16 / 46 % | nicht budgetiert, gehört auf den Shm-Pfad |
| Triton, Shm (Pose, 2,3 MB) | — | +159 µs | — | PASS |

Der Shm-Zusatz hängt nicht von der Nutzlast ab (ADR-0003). Die p99-Spalte der
Werkzeuge ist die Differenz zweier p99-Quantile, nicht das p99 der
paarweisen Zusatzkosten; das sagt der externe Review zu Recht, und die Tabelle
nennt sie deshalb nicht.

## Stationäre Rampe: der Kern hält, die Kante ist zu vorsichtig

Kette; zwei kurze Browserproben von rund 40 % CPU. Unabgedeckte Perioden des
Detektors (Median über drei Läufe) und des schlechtesten Stroms.

| Last | Detektor Triton | Detektor Vigilant | Faktor | alle Ströme T / V |
|---|---:|---:|---:|---:|
| 50–90 % | 0 ‰ | 0 ‰ | — | 0 / 0 ‰ |
| 100 % | 0 ‰ | 0 ‰ | — | 0 / **165 ‰** |
| 110 % | 342 ‰ | 14 ‰ | 24x | 342 / 102 ‰ |
| 125 % | 457 ‰ | 22 ‰ | 21x | 457 / 450 ‰ |
| 150 % | 500 ‰ | 4 ‰ | 125x | 500 / 996 ‰ |

Über 100 % hält der Governor den geschützten Strom und gibt die übrigen
auf — das ist gewollt, und es kostet: bei 150 % verfehlen sie fast jede
Periode. Genau bei 100 % verfehlt Triton nichts, der Governor verwirft
165 ‰ eines nachrangigen Stroms. Die erste Vermutung — er plane mit 110 %
Marge und sehe echte 100 % als 110 % — hat die Simulation derselben Last
widerlegt: Bis 105 % liefern feste 110 %, feste 100 % und eine gelernte Marge
exakt dieselbe Abdeckung, weil der Plan nur auf Look-ahead und Variantenwahl
wirkt und der Look-ahead an der Kante nicht vetoiert. Nächster Kandidat ist
die Lücke zwischen zwei Aufträgen bei `pipelining_depth: 0`: Der Governor
startet den nächsten erst nach der Antwort auf den vorigen, Triton direkt hat
bis zu acht offen. Die Messung dazu steht aus.

## Lastspitzen: kein Gewinn, im dritten Profil ein Verlust

Nachlauf, ohne Fremdlast. Die Kette zeigte dieselbe Richtung (1,3x, 1,8x,
−1,4x), ihr drittes Profil lief unter Browserlast.

| Profil (Grundlast → Spitze, Dauer/Abstand) | Detektor Triton | Detektor Vigilant | Faktor | längste Lücke T / V | alle Ströme T / V |
|---|---:|---:|---:|---:|---:|
| 90 → 150 %, 200/2000 ms | 90 ‰ | 95 ‰ | −1,1x | 21 / 14 ms | 90 / 105 ‰ |
| 90 → 150 %, 500/2000 ms | 102 ‰ | 82 ‰ | 1,2x | 22 / 14 ms | 102 / 117 ‰ |
| 75 → 150 %, 1000/4000 ms | 11 ‰ | 41 ‰ | **−3,7x** | 22 / 12 ms | 11 / 70 ‰ |

Der Governor hält die längste Lücke kürzer (12–14 gegen 21–22 ms), verfehlt
aber mehr Perioden, und bei langen Spitzen über niedriger Grundlast deutlich
mehr. Zwei Dinge sind offen: die Ursache (eine Analyse läuft) und das
Szenario selbst — hier heißt eine Spitze, dass dieselbe Kamera schneller
liefert, als ihr Vertrag sagt. Realistischer ist eine zusätzliche Kamera oder
eine Häufung von Anfragen nach einem Alarm (S7).

**Nachtrag, Analyse ([bursts-and-frontier](../analysis/bursts-and-frontier.md)).**
Die Zahlen oben sind die Fenstersicht. Im Simulator mit denselben Strömen,
Verträgen und Aufnahmezeitpunkten verfehlen Governor und FIFO dort
gleichermaßen 59–95 ‰ der Detektorfenster und in der Verbrauchersicht keine
einzige Abtastung; eine halbe Millisekunde mehr Transport verschiebt die
Fenstersicht um 20 ‰. Der Unterschied 41 gegen 11 ‰ ist deshalb kein
belegter Unterschied in der Versorgung. `load-ramp` zeigt ab jetzt beide
Sichten; die Messung ist zu wiederholen.

## Variantenwahl (Frontier): stark bei Überlast, eine Anomalie bei 90 %

Nachlauf, ohne Fremdlast. Ein Detektor allein, große und kleine Variante.

| Last (effektiv) | nur groß | nur klein | auto | auto ohne Dwell |
|---|---:|---:|---:|---:|
| 50 / 75 % | 0 ‰ | 0 ‰ | 0 ‰ | 0 ‰ |
| 90 % | 105 ‰ | 0 ‰ | **143 ‰** | 170 ‰ |
| 100 % (97 %) | 1 ‰ | 0 ‰ | 2 ‰ | 1 ‰ |
| 110 % (106 %) | 74 ‰ | 0 ‰ | 66 ‰ | 162 ‰ |
| 125 % (127 %) | 310 ‰ | 0 ‰ | 84 ‰ | 384 ‰ |
| 150 % (159 %) | 501 ‰ | 0 ‰ | 2 ‰ | 0 ‰ |

Bei 150 % hält die automatische Wahl den Strom, wo die große Variante die
Hälfte verfehlt. Aber bei 90 % verfehlt schon die große Variante allein mehr
als bei 100 %, und die automatische Wahl bleibt dort vollständig auf ihr,
statt herunterzuschalten. Bei 110–125 % schaltet sie zu spät. Das hat sich in
beiden Läufen gezeigt, in der Kette mit 126/162 ‰ bei 90 %. Die Hysterese
(Dwell) hilft: ohne sie ist es bei 110–125 % schlechter. Eine Analyse läuft;
die Vermutung ist Aliasing zwischen Periode und Laufzeit bei 90 % und eine
Wahl, die auf die geplante Laufzeit statt auf verfehlte Perioden schaut.

**Nachtrag, Analyse ([bursts-and-frontier](../analysis/bursts-and-frontier.md)).**
Beides trifft zu, mit einer genaueren Ursache. Der Vertrag (`deadline =
1,5 P`, `max_age = 2 P`) lässt zu, dass ein Frame seine Deadline hält und der
Verbraucher trotzdem eine Lücke hat: das vorige Ergebnis läuft schon `P` nach
dieser Aufnahme ab. Die Variantenwahl prüfte nur die Deadline und blieb
deshalb auf der großen Variante, auch wo deren Laufzeit an die Periode
reichte. Der Simulator trifft `auto` bei 110 und 125 % mit 71 und 84 ‰; seit
der Korrektur wählt sie die beste Variante, die vor `Aufnahme + max_age − P`
fertig wird, und verfehlt dort 0 ‰ wie die kleine. Die 1–2 ‰ bei 100 % sind
die zu gute Fenstersicht einer gesättigten Variante: im Simulator fehlt dort
bei der Hälfte der Abtastungen ein brauchbares Ergebnis. `frontier` zeigt ab
jetzt beide Sichten.

## XSched und Präemption (NV-15)

Zwei Tritonprozesse: A mit Detektor, Pose, Tiefe (Priorität hoch), B mit dem
VLM (niedrig). Abdeckung Triton / Vigilant, drei Läufe je Zeile.

| Aufbau | Detektor | Pose | Tiefe | VLM | Lauf |
|---|---|---|---|---|---|
| ohne XSched | 93 / 100 % | 100 / 100 % | 100 / 100 % | 100 / **0 %** | Kette, sauber |
| XSched Level 2 | 100 / 100 % | 100 / 99–100 % | 100 / 100 % | 100 / 0 % | Kette, Lauf 1 verschmutzt |
| XSched TSG, Level 3 | 100 / 100 % | 100 / 100 % | 100 / 100 % | 100 / 0 % | Nachlauf, sauber |
| Level 2, Vigilant mit Lane, R = 14 ms (angegeben) | 95–99 / 100 % | 100 / 99 % | 100 / 100 % | 100 / 83–98 % | Kette |
| **Level 2, Vigilant mit Lane, R = 4 ms** | 100 / 100 % | 100 / 100 % | 100 / 100 % | **100 / 100 %** | Nachlauf, sauber |

**Mit Präemption löst Triton allein, was ohne sie nur der Governor konnte:**
Die geschützten Ströme liegen bei 100 %, und das VLM läuft trotzdem, mit
einem Antwortalter p95 von rund 200 ms statt 111 ms ohne XSched.

**Der Governor mit Lane und passendem R zieht gleich.** Alle Ströme liegen
bei 100 %. Die Detektorantworten sind frischer (p95 18–21 gegen 24–25 ms),
Pose und Tiefe etwas älter (21–32 gegen 13–19 ms), das VLM etwas älter
(218–290 gegen 195–200 ms). Mehr bringt er in diesem Aufbau nicht. Sein
Vorteil liegt dort, wo Präemption fehlt, und bei Überlast, die Präemption
nicht beseitigt, weil sie keine Rechenzeit schafft.

**R ist nicht gemessen.** `vig calibrate` verwarf alle vier Messreihen, weil
der Takt während jeder Reihe wanderte (Leistungslimit, Fremdlast); die
Schwelle wird dafür nicht gelockert. Die 4 ms sind eine Schätzung aus den
Läufen mit XSched: Das Antwortalter p95 des Detektors liegt dort rund 3,5 ms
über seiner Laufzeit. Mit den angegebenen 14 ms warnt `vig doctor` zu Recht,
dass Laufzeit plus R die Frist sprengt; das VLM kommt dann seltener und
später dran (83–98 %, p95 bis 870 ms). R gehört gemessen — mit festem Takt
oder, ohne Rechte dafür, online aus dem laufenden Betrieb (ADR-0038).

**Die Zeile „TSG“ der Kette gilt nicht.** Das Startskript setzte Level 2 auch
für TSG; TSG wirkt erst ab Level 3 (Review R06, behoben in `64c9d06`). Die
Zeile oben stammt aus dem Nachlauf mit Level 3.

**Was XSched kostet, steht erst seit dem 12.09. in Zahlen.** Der Shim
verlangsamt den geschützten Pfad selbst: derselbe Referenztest auf derselben
Karte, nur mit Shim, braucht beim Detektor p50 32,5 statt 27,7 ms und p99
34,4 statt 28,7 ms — 17 bis 20 % mehr Laufzeit
([Pilot mit Präemption](pilot-praemption-2026-09-12.md)). In den Tabellen
oben ist davon nichts zu sehen, weil die Abdeckung dort ohnehin am Anschlag
liegt; auf einer Last, die enger liegt, ist es der Preis der Präemption.

## Was daraus folgt

| Befund | Stand | Nächster Schritt |
|---|---|---|
| Stationäre Überlast: 21–125x beim Detektor | hält | — |
| Datenpfad: +159–236 µs, alle Budgets | bestanden | Budgets je Plattform (ARM, [arm-serve](arm-serve.md)) |
| Kante bei 100 %: 165 ‰ Verlust eines nachrangigen Stroms | Schwäche, Ursache offen (nicht die Marge) | Messung mit Pipelining |
| Lastspitzen: kein Gewinn, einmal −3,7x | Fenstersicht misst dort Phase, Versorgung unbelegt ([Analyse](../analysis/bursts-and-frontier.md)) | Neu messen mit Verbrauchersicht; Szenario S7 statt schnellerer Kamera |
| Variantenwahl: stark bei 150 %, falsch bei 90 %, spät bei 110–125 % | Ursache belegt, behoben im Simulator ([Analyse](../analysis/bursts-and-frontier.md)) | `frontier` auf dem neuen Stand |
| Präemption: Triton + XSched ≈ Vigilant + Lane | Gleichstand, VLM erstmals 100 % unter dem Governor | R messen (ADR-0038 oder fester Takt) |
| Pilot (Aufgabenmetriken) | nicht gelaufen | erst nach den Pufferfehlern aus dem Review (R03, R07) |
