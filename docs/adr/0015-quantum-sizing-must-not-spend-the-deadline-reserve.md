# ADR-0015: Ein Quantum darf die Deadline-Reserve nicht aufzehren

**Status:** Akzeptiert · 2026-09-01
**Betrifft:** ADR-0014 (Quantengroesse), Spec 10.7, 15.3
**Grundlage:** WP26-Messungen auf RTX 3070 mit RF-DETR und Qwen3-0.6B

## Kontext

ADR-0014 leitet die Quantengroesse aus dem Zeitbudget bis zur naechsten
geschuetzten Ankunft ab. Die erste Implementierung rechnete dieses Budget bis
zum **spaetesten zulaessigen Start** der Ankunft:

```text
budget = (deadline - runtime) - now
```

Das ist formal richtig — bis dahin kann die geschuetzte Arbeit noch starten und
ihren Vertrag halten — und in der Wirkung falsch.

Die Spanne zwischen Ankunft und spaetestem Start ist die **Deadline-Reserve**.
Sie existiert, um Jitter, Laufzeitausreisser und Prognosefehler aufzufangen.
Wer sie bei jedem Quantum planmaessig verbraucht, laesst die geschuetzte Arbeit
dauerhaft am Rand ihres Vertrags laufen; die erste Abweichung wird dann zum
Miss. Und weil der Look-ahead diese Gefaehrdung erkennt, vetoiert er das
Quantum — die Zerlegung erzeugt also grosse Quanten, die anschliessend fast
immer abgelehnt werden.

Gemessen (30 s, ein Slot, RF-DETR bei 30 Hz):

| Betriebsart | Detektor-Abdeckung | Generierungen |
|---|---:|---:|
| direkt zu Triton | 75 % | 70 |
| Governor ohne Zerlegung | 98 % | 2 |
| Governor mit Zerlegung, Budget bis zum spaetesten Start | 99 % | **1** |

Bei 1820 Veto-Ereignissen. Die Zerlegung hat das Ergebnis **verschlechtert**.

## Entscheidung

Das Budget reicht bis zur **Ankunft**, nicht bis zu ihrem spaetesten Start:

```text
budget = expected_arrival - now
```

Damit passt ein Quantum in die Leerlaufluecke zwischen zwei geschuetzten
Ausfuehrungen und verzoegert die geschuetzte Arbeit gar nicht, statt sie an
ihren Vertragsrand zu druecken.

## Die engere Regel loest das Problem nicht

Auch mit dem engeren Budget bleibt das Ergebnis unveraendert: zwei
Generierungen mit wie ohne Zerlegung, bei 1802 Veto-Ereignissen. Die
Aenderung ist trotzdem richtig — die Deadline-Reserve planmaessig zu
verbrauchen war in jedem Fall falsch —, aber sie macht die Zerlegung nicht
wirksam.

## Was die Messung ueber die Grenzen sagt

Die Kosten eines Generierungsauftrags, gemessen am selben Aufbau:

| Token je Auftrag | ohne CUDA-Graphen | mit CUDA-Graphen |
|---:|---:|---:|
| 1 | 23 ms | 12 ms |
| 2 | 38 ms | 14 ms |
| 4 | 77 ms | 21 ms |
| 8 | 147 ms | 36 ms |
| 64 | 1153 ms | 254 ms |

Zwei Groessen bestimmen, ob Zerlegung ueberhaupt moeglich ist:

* **Der feste Sockel je Auftrag** — rund 10 ms. Er faellt bei jedem Quantum an,
  weil OneTimer ausserhalb des Servers sitzt und nur an Requestgrenzen
  zerlegen kann. Ein Quantum kann nie billiger sein als dieser Sockel.
* **Die Kosten eines einzelnen Tokens** — rund 4 ms mit CUDA-Graphen, 18 ms
  ohne.

Daraus folgt eine harte Schranke:

> Zerlegung kann nur wirken, wenn der Sockel plus ein Token in die
> Leerlaufluecke zwischen zwei geschuetzten Ausfuehrungen passt.

Bei 33 ms Periode und 19 ms geschuetzter Laufzeit bleiben 14 ms Luecke. Mit
CUDA-Graphen (12 ms fuer ein Token) passt genau ein Token hinein; ohne sie
(23 ms) passt keines. Die Konfiguration des Backends entscheidet also
darueber, ob der Mechanismus ueberhaupt greifen kann.

## Ehrlichkeit zur Messgeschichte

Die erste Kostenmessung ergab 18 ms je Token und fuehrte fast zu dem Schluss,
ADR-0014 sei widerlegt. Ursache war `enforce_eager: true` in der eigenen
vLLM-Konfiguration — ein Flag, das CUDA-Graphen abschaltet und die
Dekodierung um den Faktor vier verlangsamt. Der Fehler lag in der Umgebung,
nicht im Verfahren.

Aus demselben Grund schlug die Zerlegung anschliessend erneut fehl: die
konfigurierte Erzeugungsrate `tokens_per_second: 55` stammte aus der Messung
**ohne** Graphen. Der Scheduler hielt jedes Quantum fuer viermal so teuer, wie
es war.

Daraus folgt eine Anforderung an das Produkt, nicht nur an diesen Versuch:

> **Eine veraltete Erzeugungsrate laesst die Zerlegung lautlos versagen.**
> Sie tut dann nichts, ohne einen Fehler zu melden.

`onetimer profile` muss die Rate messen koennen, und `onetimer doctor` muss
warnen, wenn Sockel plus ein Token nicht in die kuerzeste geschuetzte
Leerlaufluecke passen. Beides ist offen.

## Konsequenzen

- ADR-0014 bleibt als Mechanismus gueltig; seine Sizing-Regel wird durch
  dieses ADR ersetzt. **Wirksam ist er in der gemessenen Konfiguration
  nicht:** das kleinstmoegliche Quantum kostet 17 ms, die Leerlaufluecke
  zwischen zwei Detektorlaeufen betraegt 14 ms. ADR-0012 bleibt damit offen.
- Die Zerlegung ist kein Ersatz fuer Kapazitaet. Sie verwandelt „laeuft nie"
  bestenfalls in „laeuft langsam" — der Durchsatz des generativen Modells
  bleibt weit unter dem, was ohne geschuetzte Konkurrenz moeglich waere.
- Der feste Sockel ist der Preis dafuer, dass OneTimer ein Drop-in-Governor
  ist und nicht im Server sitzt. Eine echte kooperative Ausfuehrung innerhalb
  von vLLM haette ihn nicht — und waere kein Drop-in mehr.
