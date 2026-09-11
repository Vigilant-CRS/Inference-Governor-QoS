# Lastspitzen und Frontier: zwei Schwächen, was davon echt ist

Stand 11.09.2026. Grundlage sind die Läufe ohne Fremdlast aus
`InferenceQoS-runtime/measure-nachlauf-2026-09-11e/` (Stand `b201c57`),
dazu die verschmutzten Erstläufe aus `measure-chain-2026-09-11d/`. Die
Nachstellung liegt in `crates/vig-sim/tests/bursts_and_frontier.rs`; die
Tabellen unten druckt

```bash
cargo test -p vig-sim --test bursts_and_frontier -- --ignored --nocapture
```

## Kurz

| Befund | Ursache | Belegt durch | Folge |
|---|---|---|---|
| Frontier bei 110/125 %: `auto` verfehlt 66/84 ‰, `klein` 0 ‰ | Die Variantenwahl prüfte je Frame die Deadline (`1,5 P`), nicht die Versorgung (`max_age − P = P`) | Simulator trifft 71/84 ‰ mit denselben Profilen und Verträgen; nach der Korrektur 0 ‰ | **Behoben** in `vig-core/src/variant.rs` |
| Frontier bei 90 %: `gross` 105 ‰, `auto` 143 ‰, bei 100 % nur 1–2 ‰ | Laufzeit der großen Variante ≈ Periode; der Vertrag lässt dann Lücken zu, und die Fenstersicht kippt an den Fenstergrenzen | Simulator: `gross` bei 13 ms Periode 125–148 ‰ ohne einen einzigen verworfenen Frame | `auto` weicht jetzt aus; `gross` bleibt so — das ist die Physik dieses Vertrags |
| Lastspitzen „75 → 150 %, 1 s alle 4 s“: Vigilant 41 ‰, Triton 11 ‰ | Die gemeldete Zahl ist die Fenstersicht; unter Spitzen misst sie die Phase zwischen Aufnahme und Fenstergrenze, nicht die Versorgung | Simulator: Governor **und** FIFO 59–95 ‰ in der Fenstersicht, 0 ‰ in der Verbrauchersicht | Werkzeug zeigt jetzt beide Sichten; der Vergleich braucht einen neuen Lauf |
| Kante bei 100 % (Rampe): Vigilant verwirft 165 ‰ eines `high`-Stroms | Nicht die Marge (ADR-0038). Kandidat: die Lücke zwischen zwei Aufträgen bei `pipelining_depth: 0` | Simulator mit 0,3–0,6 ms Lücke nur auf Governorseite: bei 95–100 % deutlich schlechter als FIFO, ohne Lücke gleichauf | Ungeprüft auf der GPU; Messung mit `VIG_RAMP_PIPELINING=1` |

## Zwei Sichten auf dieselbe Versorgung

Alle Zahlen in `frontier` und `load-ramp` waren bisher die **Fenstersicht**
(ADR-0005): ein Fenster der Länge `P` gilt als abgedeckt, wenn *in* ihm ein
Ergebnis mit Alter unter `max_age` ankam. Seit NV-01 gibt es daneben die
**Verbrauchersicht**: am Ende jedes Fensters wird gefragt, ob ein Ergebnis
unter `max_age` *vorliegt* — auch eines aus dem vorigen Fenster.

Beide stimmen überein, solange Lieferungen weit von den Fenstergrenzen
entfernt liegen. Sie fallen auseinander, wenn die Laufzeit an die Periode
heranreicht:

- **Fenstersicht zu schlecht.** Eine Lieferung, die knapp hinter die Grenze
  rutscht, lässt ein Fenster leer und füllt das nächste doppelt. Dem
  Verbraucher fehlte nichts.
- **Fenstersicht zu gut.** Unter Sättigung kommt in fast jedes Fenster eine
  Lieferung; beim Abtasten ist sie aber schon älter als `max_age`.

Frontier im Simulator, feste große Variante, Fenster/Verbraucher in ‰:

| Periode | 14 ms | 13 ms | 12 ms | 10 ms |
|---|---:|---:|---:|---:|
| `gross` | 0 / 0 | 125–148 / 89–105 | 69–70 / 517–534 | 296–299 / 622–638 |

Bei 12 ms meldet die Fenstersicht 7 %, die Verbrauchersicht 53 %. Eine
Rampe, die nur die Fenstersicht zeigt, beschreibt die Sättigung zu gut.

## Frontier

### Die Anomalie bei 90 %

Mit Periode `P`, Deadline `1,5 P` und `max_age = 2 P` läuft das Ergebnis
eines Frames `2 P` nach seiner Aufnahme ab — das ist `P` nach der Aufnahme
des nächsten. Kommt das nächste Ergebnis später als `P` nach seiner
Aufnahme, hat der Verbraucher eine Lücke, **obwohl dieser Frame seine
Deadline hält**. Der Vertrag lässt also zu, was die Versorgung nicht verträgt.

Mit der großen Variante trifft das genau dann, wenn ihre Laufzeit an die
Periode heranreicht. Im Simulator ist das bei 13 ms Periode der Fall
(Median 12,72 ms): 125–148 ‰ Fenster, 89–105 ‰ Abtastungen, und weder ein
verworfener noch ein verdrängter Frame. Auf der GPU lag die Kante eine Stufe
tiefer, bei 90 % Last (14 ms): der Weg über den Governor kostet rund 0,2 ms
mehr als die Kalibrierung direkt gegen Triton (`shm-latency`, +159 µs), und
die Karte lief am Leistungslimit. Im Taktmitschnitt stehen die Fenster der
großen Variante bei 90 % auf 1755–1800 MHz, bei 100 % auf 1710–1740 MHz,
jeweils mit `SwPowerCap` und rund 129 W; die Proben mit 1560 MHz und 50–60 W
sind die Fenster der kleinen Variante, keine Einbrüche mitten in der großen.

Bei 100 % Last (13 ms) ist die große Variante dagegen gesättigt: es wartet
fast immer ein neuerer Frame, der Governor verdrängt den älteren, und die
Fenstersicht sieht in jedem Fenster eine Lieferung. Deshalb 1–2 ‰ dort und
105 ‰ bei 90 % — die Zahl bei 100 % ist die zu gute Fenstersicht, nicht eine
bessere Versorgung.

### Warum `auto` nicht abwertete

`variant::resolve` wählte in Qualitätsreihenfolge die erste Variante, deren
geplante Fertigstellung vor der **Deadline** des Frames liegt. Die große
Variante plant mit 14,69 ms (p99 13,35 ms × 110 %) und hält die Deadline von
21 ms (90 %) bzw. 18 ms (110 %) immer, sobald der Slot frei ist. Also blieb
sie die Wahl — auch dort, wo sie den Verbraucher nicht versorgen konnte. Bei
110 und 125 % Last wechselte `auto` darum ständig zwischen beiden Varianten
(324–496 Wechsel je Minute im Messlauf) und verfehlte 66 bzw. 84 ‰, während
`klein` allein 0 ‰ schaffte. Der Simulator trifft beide Zahlen mit denselben
Profilen und Verträgen: 71–72 ‰ und 84 ‰.

### Die Korrektur: die Versorgungsfrist

Eine Variante versorgt, wenn ihr Ergebnis vor `Aufnahme + max_age − P`
vorliegt. `resolve` wählt jetzt die beste Variante, die das schafft; erst
wenn keine es schafft, gilt wie bisher die beste, die ihre Deadline hält.
Nennt der Vertrag keine Periode oder kein `max_age` über der Periode, ändert
sich nichts. Die Aufnahmezeit folgt aus absoluter Deadline minus
Vertragsdeadline; hat ein Hinweis die Deadline verschärft, liegt die
Versorgungsfrist dadurch früher, also in der vorsichtigen Richtung.

Simulator, Fenster/Verbraucher in ‰, jeweils drei Seeds:

| Periode (frontier-Last) | `klein` | `auto` vorher | `auto` nachher | Anteil groß nachher |
|---|---|---|---|---|
| 25 ms (50 %) | 0 / 0 | 0 / 0 | 0 / 0 | 100 % |
| 17 ms (75 %) | 0 / 0 | 0 / 0 | 0 / 0 | 100 % |
| 14 ms (90 %) | 0 / 0 | 0 / 0 | 0 / 0 | 0 % |
| 13 ms (100 %) | 0 / 0 | 125–148 / 89–105 | 0 / 0 | 0 % |
| 12 ms (110 %) | 0 / 0 | 71–72 / 186–195 | 0 / 0 | 0 % |
| 10 ms (125 %) | 0 / 0 | 84 / 40–46 | 0 / 0 | 0 % |

Der Preis steht in der 90-%-Zeile: dort hätte die große Variante im
Simulator gerade noch gereicht (Median 12,7 ms bei 14 ms Periode), aber die
Planung mit p99 × 110 % sagt 14,69 ms, und die Wahl weicht aus. Das ist
dieselbe Vorsicht wie an der Kante bei 100 % — ob die gelernte Marge aus
ADR-0038 die große Variante dort zurückholt, ist eine Frage an die nächste
Messung. Auf der GPU war die Vorsicht bei 90 % berechtigt: dort verfehlte die
große Variante 105 ‰.

Tests: `crates/vig-core/tests/variant_supply.rs` (die Regel, sechs Fälle),
`crates/vig-sim/tests/bursts_and_frontier.rs` (`auto` versorgt unter
Überlast wie `klein`; die große Variante bleibt, wo sie versorgt; die feste
große Variante kann an der Kante nicht versorgen).

## Lastspitzen

### Was der Messlauf meldete

| Profil | Triton | Vigilant | alle Ströme T/O |
|---|---:|---:|---:|
| 90 → 150 %, 200/2000 ms | 90 ‰ | 95 ‰ | 90 / 105 ‰ |
| 90 → 150 %, 500/2000 ms | 102 ‰ | 82 ‰ | 102 / 117 ‰ |
| 75 → 150 %, 1000/4000 ms | 11 ‰ | 41 ‰ | 11 / 70 ‰ |

Alle Zahlen sind die Fenstersicht.

### Was der Simulator dazu sagt

Dieselben drei Ströme, Profile und Verträge wie `load-ramp`, dieselben
Aufnahmezeitpunkte wie `workload::run_stream` mit `Burst`
(`harness::run_captures`). Detektor, Fenster/Verbraucher in ‰:

| Profil | Governor | FIFO |
|---|---:|---:|
| 90 → 150 %, 200/2000 ms | 77–95 / 0 | 74–93 / 0 |
| 90 → 150 %, 500/2000 ms | 63–83 / 0 | 65–84 / 0 |
| 75 → 150 %, 1000/4000 ms | 59–73 / 0 | 59–73 / 0 |

Der Detektor ist unter Spitzen **auf beiden Seiten lückenlos versorgt**; die
Fenstersicht verfehlt trotzdem 6–10 %. Die Spanne je Zeile entsteht allein
aus der Transportzeit (0,2 oder 0,7 ms): eine halbe Millisekunde verschiebt
die Lieferungen gegen die Fenstergrenzen, und die Fenstersicht ändert sich
um 20 ‰, die Verbrauchersicht nicht. Nach einer Spitze liegen die Aufnahmen
gegen das Fenstergitter verschoben — 1000 ms Spitze mit 15 ms Periode sind
66,7 Aufnahmen —, und bei Laufzeiten nahe der halben Grundperiode fällt eine
Lieferung mal vor, mal hinter die Grenze.

Die Fenstersicht misst in diesem Szenario also vor allem Phase. Dass der
Messlauf für Triton 11 ‰ und für Vigilant 41 ‰ zeigte, ist in ihr kein
Unterschied in der Versorgung, sondern einer im Timing der beiden Wege. Ob
Vigilant unter Spitzen schlechter versorgt, ist mit diesen Zahlen weder
belegt noch widerlegt.

Die `high`-Ströme verlieren dagegen auch in der Verbrauchersicht, und zwar
auf beiden Seiten: Pose 87–222 ‰ beim Governor, 111–267 ‰ bei FIFO; Tiefe
81–244 ‰ gegen 99–247 ‰. Das ist die Spitze selbst — 150 % passen nicht auf
eine Karte —, und der Governor verteilt den Verlust etwas besser.

### Ist das Szenario sinnvoll?

Nur halb. Während einer Spitze liefert **dieselbe** Kamera schneller, der
Vertrag nennt aber die Grundperiode, und der Verbraucher tastet im
Grundtakt ab. Der Governor verdrängt die überzähligen Frames (`latest`), und
genau das braucht der Verbraucher auch nicht mehr. Für den geschützten Strom
ist die Spitze damit fast folgenlos, auf beiden Seiten — das Szenario prüft
nicht, was es prüfen soll.

Realistischer wäre eine Spitze als **zusätzliche Last**: eine weitere Kamera,
die für die Dauer der Spitze einschaltet, oder ein Schwall von
Sprachmodell-Aufträgen nach einem Alarm. Dann trifft die Spitze Arbeit, die
der Verbraucher tatsächlich braucht. Das ist ein neues Szenario und bleibt
hier Vorschlag; das Werkzeug zeigt ab jetzt beide Sichten, damit auch der
bestehende Lauf lesbar wird.

## Die Kante bei 100 %

Aus dem Margen-Fork (ADR-0038): im Simulator entscheidet die Marge an der
Kante nichts; feste 110 %, feste 100 % und gelernt liefern bis 105 % dieselbe
Abdeckung. Ein Kandidat, den der Simulator bisher nicht kannte, ist die
Lücke zwischen zwei Aufträgen: mit `pipelining_depth: 0` geht der nächste
Auftrag erst nach der Rückmeldung des vorigen an Triton; bei Triton direkt
warten bis zu acht offene Aufträge im Server.

Nachgestellt als Laufzeit plus Lücke nur auf der Governorseite (Rampe,
alle drei Ströme, Fenster/Verbraucher in ‰, Seed 1):

| Last | Lücke | Governor Detektor / Pose / Tiefe | FIFO Detektor / Pose / Tiefe |
|---|---|---|---|
| 95 % | 0 | 2/0 · 63/0 · 0/0 | 2/0 · 64/0 · 0/0 |
| 95 % | 0,3 ms | 5/1 · 253/0 · 20/0 | 2/0 · 64/0 · 0/0 |
| 95 % | 0,6 ms | 64/25 · 405/0 · 77/0 | 2/0 · 64/0 · 0/0 |
| 100 % | 0 | 30/9 · 315/0 · 56/0 | 27/9 · 297/95 · 37/0 |
| 100 % | 0,3 ms | 105/47 · 410/1 · 99/6 | 27/9 · 297/95 · 37/0 |
| 100 % | 0,6 ms | 146/84 · 410/27 · 185/24 | 27/9 · 297/95 · 37/0 |

Ohne Lücke sind beide gleichauf; mit einer Lücke von 0,3–0,6 ms je Auftrag
fällt der Governor bei 95–100 % deutlich zurück. Bei rund 2,5 Aufträgen je
23-ms-Periode kostet eine Lücke von 0,3 ms gut 3 % Kapazität. Die Rampe
meldete 165 ‰ für den schlechtesten `high`-Strom, ohne ihn zu nennen; wäre
es die Tiefe, die 17 % der Karte belegt, entspräche das knapp 3 % Kapazität —
dieselbe Größenordnung.
Das stützt die Vermutung, beweist sie aber nicht: der Simulator kennt die
Lücke nur als längere Laufzeit, und die Planung sieht sie mit. Entscheiden
muss die Rampe mit `VIG_RAMP_PIPELINING=1` auf der GPU.

## Welche GPU-Messung das bestätigt

1. **`frontier` auf dem Stand mit der Versorgungsfrist**, beide Tabellen.
   Erwartung: `auto` bei 90, 110 und 125 % wie `klein` (0 ‰ in beiden
   Sichten), bei 50 und 75 % zu 100 % groß. Die Verbrauchersicht von `gross`
   bei 100–125 % zeigt, wie viel die Fenstersicht bisher verschwieg.
2. **`load-ramp bursts`** mit den neuen Spalten. Erwartung: Detektor in der
   Verbrauchersicht auf beiden Seiten nahe null, die Fenstersicht streut wie
   bisher. Liegt Vigilant in der Verbrauchersicht schlechter, ist das der
   eigentliche Befund.
3. **`load-ramp` bei 95/100/105 % mit und ohne `VIG_RAMP_PIPELINING=1`.**
   Verschwindet der Verlust des Tiefenstroms bei 100 % mit einem zweiten
   Kredit je Slot, war es die Dispatchlücke.
