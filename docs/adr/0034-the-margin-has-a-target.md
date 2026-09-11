# ADR-0034: Die Sicherheitsmarge hat ein Ziel

**Status:** Akzeptiert · 2026-09-11
**Betrifft:** `core/estimator` (`MarginController`), `core/scheduler`,
`core/variant`, `core/predictor`; Spec 13.3, ADR-0016, ADR-0023, ADR-0027
**Ausloeser:** Die erste scharfe Messung von NV-06 und die Frage, ob die
Marge starr ist

## Kontext

Die Planung rechnet mit `max(offline_p99, online_p95) × Marge`. Die Marge
ist je Modell ein Regler: sie steigt nach jeder Ausfuehrung, die ihre
geplante Laufzeit ueberzogen hat, und sinkt nach jeder anderen. Der Boden
ist die konfigurierte Marge.

Die Schrittweiten waren +10 und −1 Prozentpunkt. Die Begruendung war
richtig — eine zu knappe Marge kostet eine Deadline, eine zu grosse nur
Durchsatz —, aber sie hatte eine Folge, die nirgends stand: der Regler haelt
still, wenn sich beide Richtungen aufheben, also bei

```text
10 · p = 1 · (1 − p)   →   p = 1/11 ≈ 9 %
```

**Jede elfte Ausfuehrung durfte ihren Plan ueberziehen**, ohne dass der
Regler reagierte. Diese Zahl hat niemand gewaehlt; sie ergab sich aus zwei
Schrittweiten. Sie hing weder am Vertrag noch am Missbudget eines Stroms.

Solange die konfigurierte Marge darueber lag, fiel das nicht auf: der Boden
hielt die Marge oben. Sichtbar wurde es mit NV-06, das mit einem kuerzeren
Wert plant und damit die Marge in den Bereich bringt, in dem der Regler
arbeitet.

## Entscheidung

**Der Margenregler hat ein ausdrueckliches Ziel: den Anteil der
Ausfuehrungen, die ihren Plan ueberziehen duerfen.**

```text
Ueberziehung:  Marge += G · (1 − Ziel)
sonst:         Marge −= G · Ziel
```

Beide Richtungen heben sich genau dann auf, wenn der Anteil der
Ueberziehungen gleich dem Ziel ist. Das ist die bekannte Online-Schaetzung
eines Quantils, angewandt auf die Marge: der Regler findet die Marge, unter
der der Plan das gewuenschte Quantil der tatsaechlichen Laufzeit ist.

1. **Das Ziel ist ein Prozent**, wenn der Vertrag nichts anderes sagt. Die
   Planung beginnt beim Profil-p99; ein Prozent Ueberziehung heisst, dass der
   Plan auch im Betrieb ein p99 bleibt.
2. **Vereinbart der Vertrag ein Missbudget** (`miss_budget: M/K`), gibt es das
   Ziel vor, begrenzt auf fuenf Prozent. Nur ein ausdruecklich vereinbartes:
   das aus `minimum_background_progress_pct` abgeleitete Budget erlaubt bis
   zu 80 % Misses fuer Hintergrundarbeit und ist als Planungsziel sinnlos.
3. **Die Verstaerkung `G` bleibt 10 Prozentpunkte.** Eine einzelne
   Ueberziehung macht spuerbar vorsichtiger; die Asymmetrie zum Absenken
   ergibt sich aus dem Ziel.
4. **Die konfigurierte Marge bleibt der Boden.** Der Regler darf
   vorsichtiger werden als der Betreiber, nie leichtsinniger.
5. **Die Marge wird intern in Hundertstelprozent gefuehrt** und nach aussen
   auf ganze Prozent aufgerundet — zugunsten der Vorsicht.
6. **Die Zelle der zustandsabhaengigen Prognose (NV-06) bekommt dieselbe
   gelernte Marge wie das Profil.** Die Zelle ersetzt das Profil, nicht die
   Marge.

## Warum je Modell, und nicht je Kamera

Die Laufzeit eines Modells haengt nicht davon ab, welcher Sensor das Bild
geliefert hat: gleiche Form, gleiche Gewichte, gleiche Kernel. Welche Kamera
wichtiger ist, sagt der Vertrag — Klasse, Periode, Hoechstalter,
Missbudget —, nicht die Marge. Eine Marge je Kamera wuerde dieselbe
Laufzeitverteilung mehrfach lernen, jede mit einem Bruchteil der Daten.

Feiner als je Modell waere je Variante und Hardwarezustand. Der Kern hat
diese Zellen bereits (NV-06); ob eine Marge je Zelle den Aufwand traegt,
entscheidet eine Messung und nicht dieser ADR.

## Konsequenzen

- **Die Planung wird im Mittel vorsichtiger.** Ein Regler, der ein Prozent
  statt neun Prozent Ueberziehungen hinnimmt, haelt die Marge hoeher, wo die
  Laufzeit streut. Das kostet Durchsatz genau dort, wo die alte Fassung ihre
  Zusagen stillschweigend weicher gemacht hat.
- **Nach einer Ueberziehung braucht die Marge laenger zurueck:** bei einem
  Prozent Ziel 99 eingehaltene Plaene je Ueberziehung, bei 30 Hz rund drei
  Sekunden. Das ist gewollt; es ist die Definition des Ziels.
- **Jede Messung vor diesem ADR lief mit der alten Dynamik.** Die Messungen
  vom 11.09. (zweiter Anlauf) laufen mit der neuen; die Berichte nennen den
  Stand.

## Verworfen

**Eine feste Marge.** Sie waere entweder zu gross fuer die ruhige Karte oder
zu klein fuer die gedrosselte — die Frage aus NV-06.

**Die Schrittweiten nur umzustellen** (etwa +10/−0,1). Das haette ein
anderes, wieder implizites Gleichgewicht ergeben. Das Ziel gehoert in den
Code als Zahl mit Namen, nicht als Verhaeltnis zweier Konstanten.
