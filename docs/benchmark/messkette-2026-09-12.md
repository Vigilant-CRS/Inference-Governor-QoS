# Messkette vom 12.09.2026: die Korrekturen auf echter Hardware

Maschine wie am Vortag: RTX 3070 Laptop (8 GB), Treiber 580.178.04, Triton
2.70.0 (26.06), ONNX-Modelle, System Shared Memory, Messung auf
`taskset -c 8-15`. Stand `49f3639`, eingefrorene Binaries mit Hash im
Manifest. Darin: ADR-0036 (Look-ahead ab Aufnahme), ADR-0038 (gelernte
Marge, opt-in), ADR-0040 (Abschlussabgleich je Epoche), die Pufferfixes aus
dem Review und die Versorgungsfrist in der Variantenwahl.

**Gültigkeit: sauber.** 03:38 bis 07:16, der Wächter markierte eine einzige
von rund 430 Proben mit Fremdlast. Rohdaten:
`InferenceQoS-runtime/measure-morgen-2026-09-12/`.

## Gate M3: der Kernbefund hält

Drei Läufe, Abdeckung Triton / Vigilant:

| Strom | Triton | Vigilant |
|---|---:|---:|
| Detektor | 85 % | 100 % |
| Pose | 91–92 % | 100 % |
| Tiefe | 97–98 % | 100 % |
| VLM | 100 % | 0 % |

In der Verbrauchersicht fehlt Triton beim Detektor in 8 % der Abtastungen
ein frisches Ergebnis, Vigilant in keiner; die längste Lücke liegt bei 117
gegen 20 ms. Keine der Korrekturen hat hier etwas verschlechtert.

## Die Kante bei 100 %: es war die Lücke zwischen zwei Aufträgen

Unabgedeckte Perioden des schlechtesten Stroms, Median über drei Läufe:

| Last | Triton | Vigilant, `pipelining_depth: 0` | Vigilant, `pipelining_depth: 1` |
|---|---:|---:|---:|
| 90 % | 0 ‰ | 0 ‰ | 0 ‰ |
| 95 % | 0 ‰ | 56 ‰ | **23 ‰** |
| 100 % | 0 ‰ | 188 ‰ | **17 ‰** |
| 105 % | 24 ‰ | 55 ‰ | 55 ‰ |
| 110 % | 316 ‰ | 92 ‰ | 108 ‰ |
| 125 % | 363 ‰ | 498 ‰ | 419 ‰ |

Beim Detektor bleibt der Vorsprung dabei erhalten: 12x bei 105 %, 79x bei
110 % ohne Pipelining, 16x und 78,5x mit.

**Damit ist die Ursache belegt.** Ohne Pipelining schickt der Governor den
nächsten Auftrag erst nach der Antwort auf den vorigen und lässt die GPU in
dieser Zeit leerlaufen; Triton hat bis zu acht offen. Bei exakt ausgelasteter
GPU kostet genau das die Abdeckung. Die Simulation konnte das nicht zeigen,
weil sie diese Lücke nicht kennt (ADR-0038).

Offen: ob Pipelining in Gate M3 etwas kostet. Erst danach gehört es in die
empfohlene Konfiguration.

## Die Variantenwahl: vollständig behoben

Unabgedeckte Perioden, ein Detektor mit großer und kleiner Variante:

| Last (effektiv) | nur groß | nur klein | auto | Anteil große Variante bei auto |
|---|---:|---:|---:|---:|
| 50 / 75 % | 0 ‰ | 0 ‰ | 0 ‰ | 100 % |
| 90 % | 184 ‰ | 0 ‰ | **0 ‰** | 0 % |
| 100 % (97 %) | 1 ‰ | 0 ‰ | **0 ‰** | 0 % |
| 110 % (105 %) | 64 ‰ | 0 ‰ | **0 ‰** | 0 % |
| 125 % (127 %) | 284 ‰ | 0 ‰ | **0 ‰** | 0 % |
| 150 % (158 %) | 501 ‰ | 0 ‰ | **0 ‰** | 0 % |

Am Vortag verfehlte `auto` bei 90 % noch 143 ‰ und bei 110–125 % 66–84 ‰.
Die Versorgungsfrist — gewählt wird die beste Variante, die fertig ist, bevor
das vorige Ergebnis abläuft — beseitigt das vollständig, ohne Pendeln und
ohne Qualitätsverlust unterhalb der Sättigung.

## Lastspitzen: kein Nachteil, es war die Messsicht

| Profil | Fenstersicht T / V | Verbrauchersicht T / V | längste Lücke T / V |
|---|---:|---:|---:|
| 90 → 150 %, 200/2000 ms | 94 / 92 ‰ | **0 / 0 ‰** | 21 / 13 ms |
| 90 → 150 %, 500/2000 ms | 112 / 75 ‰ | **0 / 0 ‰** | 21 / 12 ms |
| 75 → 150 %, 1000/4000 ms | 38 / 76 ‰ | **0 / 0 ‰** | 21 / 13 ms |

Der gemeldete Verlust vom Vortag (41 gegen 11 ‰) war die Fenstersicht, die
schon eine halbe Millisekunde Transport um 20 ‰ verschiebt. Aus Sicht des
Verbrauchers fehlt auf keiner Seite ein Ergebnis, und der Governor hält die
längste Lücke durchgehend kürzer.

## Die gelernte Marge: repariert falsche Profile bis 110 %, schadet bei 125 %

Detektor unabgedeckt, Median über drei Läufe:

| Profil | Marge | 90 % | 100 % | 110 % | 125 % |
|---|---|---:|---:|---:|---:|
| richtig | fest 110 % | 0 ‰ | 0 ‰ | 4 ‰ | 0 ‰ |
| richtig | gelernt | 0 ‰ | 0 ‰ | 12 ‰ | **205 ‰** |
| x2 (pessimistisch) | fest | 0 ‰ | 68 ‰ | 150 ‰ | 181 ‰ |
| x2 | gelernt | 0 ‰ | **0 ‰** | **15 ‰** | 479 ‰ |
| x0,7 (optimistisch) | gelernt | 0 ‰ | 0 ‰ | 15 ‰ | 221 ‰ |

Und die anderen Ströme, Profil x2: mit fester Marge verlieren sie schon bei
90 % 572 ‰ und bei 125 % 883 ‰; gelernt sind es 0 ‰ bzw. 489 ‰.

**Wofür sie taugt:** Ein Profil von fremder Hardware kostet ohne sie fast den
ganzen Vorteil. Bis 110 % holt die Kalibrierung ihn zurück.

**Was sie kostet:** Bei 125 % verfehlt der geschützte Strom mit Lernen mehr
als ohne — auch mit korrektem Profil (205 gegen 0 ‰). Der Grund ist der
Mechanismus selbst: Ein knapperer Plan vetoiert seltener, mehr
Hintergrundarbeit läuft, und der Wächter schützt die **Frist**, nicht die
**Versorgung**. Dieselbe Lücke, die die Variantenwahl hatte. Die Korrektur
dafür ist ADR-0041.

Die Voreinstellung bleibt deshalb: Kalibrierung aus.

## Präemption (NV-15): unverändert Gleichstand

Drei Läufe, XSched Level 2, Vigilant mit präemptierbarer Lane und R = 4 ms
(geschätzt, nicht gemessen):

| Strom | Triton | Vigilant |
|---|---:|---:|
| Detektor | 100 % | 100 % |
| Pose | 100 % | 99–100 % |
| Tiefe | 100 % | 100 % |
| VLM | 100 % | 100 % |

Das Antwortalter p95 des Detektors liegt bei 20–21 ms (Triton 25 ms), das
des VLM bei 291–321 ms (Triton 198–200 ms). Der Befund vom Vortag hält auf
dem neuen Stand.

## Der Pilot: relativ besser, absolut verfehlt

Erster vollständiger Lauf des Vig-Edge-Piloten: vier Lastpunkte, drei
Wiederholungen, 60 s je Zelle, Aufgabenmetriken gegen die Annotation.
Exitcode 1 — sauber gelaufen, fachlich verfehlt.

| Punkt | Arm | Alarm p95 | Trefferquote (ideal 223 ‰) | Abdeckung | Berichte/min |
|---|---|---:|---:|---:|---:|
| B | Triton direkt | 1675 ms | 106 ‰ | 854 ‰ | 30 |
| B | Vigilant | **1343 ms** | **133 ‰** | **969 ‰** | 2 |
| C | Triton direkt | 1891 ms | 103 ‰ | 588 ‰ | 30 |
| C | Vigilant | **1445 ms** | **124 ‰** | **641 ‰** | 1 |
| D | Triton direkt | 1886 ms | 94 ‰ | 339 ‰ | 30 |
| D | Vigilant | **1652 ms** | **117 ‰** | **429 ‰** | 1 |

- **Relativ gewinnt der Governor durchgehend:** kürzere Alarmzeit, höhere
  Trefferquote, bessere Abdeckung.
- **Absolut verfehlen beide Arme die Kriterien.** K1 verlangt 300 ms
  Alarmzeit; gemessen sind 1,3 bis 1,9 Sekunden. K4 verlangt 90 % der
  Referenztrefferquote; erreicht werden gut 55 %. Der ungestörte Referenzlauf
  trifft selbst nur 223 von 1000 — diese beiden Kriterien messen die
  Erkennungsqualität mit, nicht die Planung, und gehören getrennt.
- **Der Berichtspfad verhungert** (1–2 statt 30 Berichte/min). Das ist
  ADR-0012, und es ist der Grund, den Pilot mit Präemption zu wiederholen.
- **Bestanden:** K5 (der Bericht verzögert den Alarm nicht, außer im härtesten
  Punkt), K6 bei moderater Last, K7 (Fehlalarme) überall.

## Was daraus folgt

| Befund | Stand |
|---|---|
| Gate M3, stationäre Überlast | hält, unverändert |
| Kante bei 100 % | Ursache belegt: Dispatch-Lücke; `pipelining_depth: 1` senkt den Verlust von 188 auf 17 ‰ |
| Variantenwahl | behoben, 0 ‰ über alle Lastpunkte |
| Lastspitzen | kein Nachteil; die alte Zahl war die Fenstersicht |
| Gelernte Marge | nützt bei falschem Profil bis 110 %, schadet bei 125 %; bleibt opt-in |
| Präemption | Gleichstand mit Triton + XSched, alle Ströme 100 % |
| Pilot | relativ besser, absolut verfehlt; Kriterien und Präemption nachziehen |
