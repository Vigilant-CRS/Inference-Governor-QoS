# Lastrampe — ab welcher Auslastung lohnt sich der Governor?

Stand: 2026-09-01 · RTX 3070, Triton 2.70.0, RF-DETR + Pose + Tiefe

## Warum diese Kurve

Punktmessungen beantworten die Frage nicht, die ein Anwender zuerst stellt. Er
will nicht wissen, ob OneTimer bei 116 % Auslastung hilft — er will wissen, **ab
wann** es sich lohnt und **ob es bei geringer Last schadet**. Das zweite ist die
unangenehmere Hälfte der Frage und die, die über einen Einsatz entscheidet.

## Aufbau

| | |
|---|---|
| Modelle | RF-DETR 512 px (`protected`), Pose (`high`), Tiefe (`high`) |
| Kapazität | ein Slot, Shared Memory auf beiden Seiten |
| Baseline | Triton mit Rate Limiter und Prioritäten |
| Laständerung | über die Sensorperioden — die Bildrate steigt, die Modelle bleiben |
| Wiederholungen | 3 je Punkt, berichtet werden Median und Spannweite |
| Kerne | 8–15 reserviert, Systemlast im Protokoll |

Beide Seiten laufen über die Client-Puffertiefen 1 und 8; je Strom zählt das
bessere Ergebnis.

## Die Kalibrierung ist der schwierige Teil

Die erste Fassung dieses Benchmarks kalibrierte die Perioden gegen die
**konservative** Planungsgröße `p99 × Marge`. Ausgeführt wird aber mit der
tatsächlichen Laufzeit, und die liegt beim Median. Folge: bei nominell 150 %
Angebotslast betrug die reale Auslastung rund 95 %, die Rampe hat nie
gesättigt, und Triton zeigte über die gesamte Kurve null unabgedeckte Perioden.

Das sah nach einem Ergebnis aus und war keines — die Messung hatte nur
festgestellt, dass nichts gemessen wurde. Kalibriert wird deshalb gegen die
gemessenen Mediane:

```text
RF-DETR 14,9 ms · Pose 4,0 ms · Tiefe 7,9 ms
Periode P für Detektor und Pose, 2P für Tiefe
U = (14,9 + 4,0 + 3,95) / P   ->   U = 1 bei P = 23 ms
```

## Ergebnis

Unabgedeckte Perioden des **geschützten** Stroms (RF-DETR), Median aus drei
Wiederholungen, Spannweite in Klammern:

| Angebotslast | Triton | OneTimer | Faktor |
|---:|---:|---:|---:|
| 50 % | 0 ‰ | 7 ‰ [7–7] | −7,0x |
| 75 % | 0 ‰ | 8 ‰ [8–8] | −8,0x |
| 90 % | 0 ‰ | 7 ‰ [7–9] | −7,0x |
| 100 % | 2 ‰ [0–2] | 7 ‰ [7–7] | −3,5x |
| **110 %** | **299 ‰** [287–322] | **15 ‰** [10–59] | **19,9x** |
| **125 %** | **476 ‰** [433–501] | **17 ‰** [7–81] | **28,0x** |
| **150 %** | **500 ‰** [500–500] | **24 ‰** [21–64] | **20,8x** |

### Der Knick liegt zwischen 100 % und 110 %

Das ist die Antwort auf die Ausgangsfrage, und sie ist unerwartet scharf.

**Unterhalb der Sättigung ist Triton perfekt** — null bis zwei unabgedeckte
Perioden von tausend. Es gibt dort nichts zu verbessern, und OneTimer kostet
rund 7 ‰, also 0,7 % der Regelzyklen. Der Governor ist in diesem Bereich reiner
Aufwand.

**Oberhalb bricht Triton zusammen.** Bei 110 % hat schon knapp ein Drittel der
Regelzyklen kein hinreichend frisches Ergebnis, bei 150 % die Hälfte. OneTimer
bleibt bei 15 bis 24 ‰ — der Detektor liefert also weiter in 97,6 bis 98,5 %
aller Perioden.

Der Übergang ist kein sanfter Anstieg, sondern eine Kante. Das passt zur
Theorie: unterhalb der Sättigung leert sich jede Warteschlange wieder,
oberhalb wächst sie, und der Rückstand altert die Ergebnisse.

### Was der Schutz kostet

Dieselbe Messung, aber der schlechteste aller Ströme statt nur des geschützten:

| Angebotslast | Triton | OneTimer |
|---:|---:|---:|
| 50 % | 0 ‰ | 221 ‰ |
| 100 % | 2 ‰ | 85 ‰ |
| 125 % | 476 ‰ | 544 ‰ |
| 150 % | 500 ‰ | 998 ‰ |

Bei 150 % werden Pose und Tiefe praktisch nicht mehr bedient. Das ist kein
Defekt, sondern die lexikographische Zielordnung aus Spec 10.6: geschützte
Arbeit wird nicht gegen geringerwertige verrechnet. Wer alle drei Ströme
gleich behandelt haben will, konfiguriert sie gleich — dann verteilt sich der
Mangel, statt konzentriert zu werden.

**Das ist die eigentliche Entscheidung, die OneTimer trifft:** nicht *ob*
etwas verloren geht, sondern *was*. Bei 150 % Angebotslast auf einer
Ausführungseinheit geht zwangsläufig ein Drittel der Arbeit verloren. Triton
verteilt den Verlust gleichmäßig über alle Ströme, OneTimer konzentriert ihn
auf die, die als verzichtbar deklariert wurden.

### Age of Information

Triton liegt über die gesamte Kurve bei 18–22 ms, OneTimer bei 29–65 ms. Das
klingt zunächst wie ein Nachteil und ist keiner: Triton liefert **schnell, aber
selten** — bei 150 % fehlt in der Hälfte aller Fenster jedes Ergebnis, und die
gemessene AoI bezieht sich nur auf die gelieferten. OneTimer liefert
gleichmäßiger und deshalb mit etwas höherer, aber verlässlicher AoI.

Für ein Regelsystem ist das der bessere Handel: eine vorhersagbare
Aktualisierung alle 20 bis 30 ms schlägt eine sehr frische Aktualisierung, die
in jedem zweiten Zyklus ausbleibt.

## Grenzen dieser Kurve

- **Eine Maschine, eine GPU, ein Modellsatz.** Ein Jetson hat ein anderes
  Verhältnis von Rechenleistung zu Speicherbandbreite; die Kante kann dort
  woanders liegen.
- **Stationäre Last.** Bursts fehlen. Ein System, das im Mittel bei 90 % läuft
  und Spitzen bis 150 % hat, ist hier nicht abgebildet — und genau das ist der
  realistische Fall.
- **Drei Wiederholungen.** Die Spannweite bei 125 % ([7–81] ‰) zeigt, dass
  einzelne Läufe deutlich streuen. Für eine Veröffentlichung wären mehr
  Wiederholungen und eine ruhigere Maschine nötig.
- **Ein Slot.** Mit zwei Ausführungseinheiten verschiebt sich die Kante nach
  rechts, und der Best-Effort-Fall aus ADR-0012 entschärft sich.

## Reproduzieren

```bash
# Umgebung: ../triton/README.md
taskset -c 8-15 target/release/load-ramp
```

Nicht neben einem laufenden Build. Ein früherer Lauf dieses Benchmarks zeigte
52 % statt 98 % auf unverändertem Code, weil parallel kompiliert wurde.
