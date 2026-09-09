# ADR-0022: Messen ist eine Methode, keine Schleife

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** neues Modul `vig-platform/measure`, `cli/profile`; Paket NV-05
**Ausloeser:** Vier stille Fehler im bisherigen Messweg, die alle in dieselbe
Richtung wirken — das Ergebnis sieht besser aus, als es ist

## Kontext

Das bisherige Profilwerkzeug macht das Naheliegende: Anfrage schicken, Zeit
nehmen, wiederholen. Vier Dinge gehen dabei schief, ohne dass es jemandem
auffaellt:

1. **Der Takt wandert.** Wer nach jeder Antwort eine Periode wartet, misst bei
   einer langsamen Antwort automatisch seltener. Die Last sinkt genau dann,
   wenn es interessant wuerde.
2. **Fehler verschwinden.** Ein abgebrochener Aufruf, der aus der Reihe
   faellt, verbessert das Quantil.
3. **Die Uhr kann zu grob sein.** Eine Uhr mit Millisekundenaufloesung misst
   eine Inferenz von 4 ms nicht; sie rundet sie.
4. **Die Hardware wechselt mitten in der Reihe.** Faellt der Takt, beschreiben
   die Zahlen davor und danach zwei verschiedene Maschinen. Der Mittelwert
   beschreibt keine.

## Entscheidung

**Freigabe auf einem absoluten Raster.** Die Zeitpunkte sind
`start + k * period` und haengen nicht davon ab, wann die vorige Antwort kam.
Ein Ueberzug **verschiebt nichts**: er laesst Rasterpunkte aus, und die
ausgelassenen werden als solche gebucht. Das ist die Zahl, die zu einem
Vertrag gehoert.

**Saettigung ist eine andere Messung.** Ruecken an Ruecken zu messen ist
legitim — es ist eine Aussage ueber die Kapazitaet. Sie in denselben Zahlen zu
fuehren wie die Aussage ueber das Verhalten unter Takt macht beide unbrauchbar.
`vig profile` sagt jetzt im Kopf, welche der beiden gerade laeuft.

**Vier Zaehler statt einem.** Erfolge, Ueberzuege, Fehlschlaege und
ausgelassene Rasterpunkte. Ihre Summe ist die Zahl der Freigaben, und das wird
geprueft. Die Latenzverteilung enthaelt genau die Freigaben, die eine Zeit
geliefert haben — Erfolge und Ueberzuege; ein Ueberzug zaehlt in beides, weil
sein Wert richtig gemessen ist **und** die Reihe ihren Takt nicht gehalten hat.

**Die Uhr wird geprueft, bevor gemessen wird.** Als brauchbar gilt eine Uhr,
die mindestens hundertmal feiner aufloest als die kuerzeste zu messende Dauer.
Reicht sie nicht, gibt es einen Befund statt einer Zahl.

**Der Hardwarezustand wird vorher und nachher gelesen.** Aendert er sich, wird
die Zelle verworfen — mit Grund und mit beiden Zeitpunkten, nicht
stillschweigend. Ebenso bei zu wenigen verwertbaren Messwerten oder zu vielen
Fehlschlaegen: unter hundert Werten ist ein p99 kein Quantil, sondern das
Maximum.

**Eine verworfene Zelle liefert keine Zahl.** `quantile_ns` gibt `None`. Aus
einer als ungueltig erkannten Messung doch noch einen Wert zu ziehen waere
genau das, wogegen das Verwerfen da ist.

**Der Puffer steht vor der Schleife.** Eine Nachbelegung mitten in der Messung
waere ein Ausreisser, den niemand als solchen erkennt.

**Das Modul loggt nicht und ruft keine Uhr von sich aus ab.** Es nimmt
Zeitpunkte entgegen und gibt Daten zurueck. Ein `tracing`-Aufruf im gemessenen
Pfad kostet je nach Subscriber zwischen hundert Nanosekunden und mehreren
Mikrosekunden — bei einer Inferenz von 4 ms bis zu einem Promille Messfehler,
den niemand im Ergebnis sieht.

## Konsequenzen

Das bestehende Verhalten von `vig profile` bleibt die Voreinstellung:
Saettigung, wie bisher, damit alte Zahlen vergleichbar bleiben. Die
periodische Messung ist ein Schalter (`--periodic-us`) und nicht ein stiller
Wechsel der Bedeutung.

Ein Laufmanifest haelt fest, welcher Lauf von wie vielen das war, wie fein die
Uhr aufloeste und in welchem Zustand die Hardware vorher und nachher war. Zwei
Laeufe mit denselben Zellen sind damit vergleichbar — und zweihundert
Messwerte aus einem Prozessstart sind erkennbar etwas anderes als zweihundert
aus zweien.

## Alternativen

**Nach jeder Antwort eine Periode warten.** Einfacher, und es macht die
Messung genau in dem Bereich gnaedig, in dem sie hart sein muesste.

**Fehlschlaege einfach ueberspringen.** Haette kuerzeren Code ergeben und
Quantile, die besser aussehen als die Wirklichkeit.

**Bei Hardwarewechsel weitermessen und mitteln.** Haette eine Zahl geliefert,
die keine Maschine beschreibt.
