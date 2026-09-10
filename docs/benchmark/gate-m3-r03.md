# Gate-M3 nach der Korrektur der Lückenrechnung (R03)

Datum: 10.09.2026. Maschine: RTX 3070 Laptop, Treiber 580.173.02, Triton
2.70.0, ONNX-Modelle, System Shared Memory. Rohprotokolle:
`InferenceQoS-runtime/gate-m3-r03/`.

## Warum neu gemessen wurde

Das Codereview vom 10.09. hat gezeigt, dass der Benchmarktracker die
Versorgungslücke zu kurz gerechnet hat: eine Lieferung, die schon bei ihrer
Ankunft zu alt war, schob die Brauchbarkeitsgrenze trotzdem nach vorn
(ADR-0032, R03). 150 ms ohne ein einziges brauchbares Ergebnis erschienen als
90 ms.

Die veröffentlichten Gate-M3-Zahlen sind unter der alten Rechnung entstanden.
Sie mussten deshalb neu gemessen werden — nicht, weil der Vergleich falsch
wäre, sondern weil eine der Zahlen darin etwas anderes bedeutete, als
draufstand.

**Drei Läufe statt einem.** Der zweite Grund für diese Messung: ein einzelner
Lauf trägt die Lückenzahl nicht. Das zeigt sich unten.

## Die Kernzahlen halten

Lieferfensterabdeckung, Triton gegen Vigilant, drei Läufe:

| Strom | Triton | Vigilant | Faktor weniger unabgedeckt |
|---|---|---|---|
| detector | 85 / 84 / 84 % | 99 / 99 / 99 % | **20,1 / 22,4 / 22,4x** |
| pose | 91 / 91 / 91 % | 99 / 99 / 99 % | **10,5 / 12,0 / 10,6x** |
| depth | 98 / 97 / 97 % | 96 / 90 / 97 % | −1,7 / −3,7 / +1,1x |
| vlm | 100 / 100 / 100 % | 0 / 0 / 0 % | — |

Die Korrektur ändert an der Aussage nichts. Der Detektor bleibt bei rund
20x weniger unabgedeckten Lieferfenstern, die Pose bei rund 11x.

**Der Tiefenstrom ist kein Gewinn**, und das war er nie: er schwankt zwischen
−3,7x und +1,1x, also um null. Er hat mit 66 ms die längste Periode der drei
geschützten Ströme und leidet unter Last am wenigsten — es gibt dort nichts zu
gewinnen.

**Der VLM-Strom bleibt bei 0 %.** Das ist ADR-0012 und kein Messfehler: ein
nicht unterbrechbarer 90-ms-Block startet unter 103 % Auslastung nie.

## Die Lückenzahl trägt einen einzelnen Lauf nicht

Längste zusammenhängende Zeit ohne brauchbares Ergebnis, drei Läufe:

| Strom | Triton | Vigilant |
|---|---|---|
| detector | 97 / 119 / 121 ms | 15 / 25 / 19 ms |
| pose | 86 / 54 / 60 ms | 19 / 28 / 5 ms |
| depth | 50 / 73 / 60 ms | 26 / 10 / 26 ms |

Der Median steht klar: Triton 60–119 ms, Vigilant 19–26 ms. Die **Streuung
innerhalb** der drei Läufe ist aber groß — bei der Pose auf Tritonseite 54 bis
86 ms, also 60 % Unterschied zwischen zwei Läufen derselben Konfiguration.

Daraus folgt eine Regel für diesen Bericht und alle künftigen: **eine
Lückenzahl aus einem einzelnen Lauf ist keine Aussage.** Sie ist ein
Maximum über ein 30-Sekunden-Fenster, und Maxima streuen. Die Abdeckung, die
über hunderte Perioden mittelt, tut es nicht.

## Was sich gegenüber den alten Zahlen geändert hat

Die alte Einzelmessung nannte für Triton: depth 106 ms, detector 90 ms, pose
50 ms. Neu, als Median über drei Läufe: depth 60 ms, detector 119 ms, pose
60 ms.

Beim Detektor ist die Lücke gewachsen — das ist die Korrektur: seine Abdeckung
liegt bei 84 %, also kommen dort veraltete Lieferungen vor, und die schließen
jetzt keine Lücke mehr. Bei depth und pose bewegt sich die Zahl in beide
Richtungen, und das ist die Streuung.

Auf Vigilants Seite ändert die Korrektur fast nichts: bei 99–100 % Abdeckung
gibt es kaum veraltete Lieferungen, die vorher fälschlich eine Lücke
geschlossen hätten.

## Was weiterhin gilt

Der Vergleich misst **Scheduling**, nicht Durchsatz. Beide Seiten fahren
dieselben Modelle, dieselbe Hardware, dieselben Frames, denselben Transport.
Die Karte lief dabei unter einem Leistungslimit — beide Seiten darunter, der
Vergleich gilt also, die absoluten Zahlen gelten für diesen Zustand.

Und die unbequeme Zeile bleibt: Vigilant gewinnt beim Detektor und bei der
Pose, verliert beim mittleren Informationsalter (49 gegen 42–43 ms beim
Detektor) und hungert den Hintergrundstrom aus. Das ist ein Kompromiss und
keine allgemeine Verbesserung. Welche Seite davon zählt, entscheidet der
Vertrag.
