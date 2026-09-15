# ADR-0045: `autotune` stellt ein — innerhalb der Vertraege

**Status:** Akzeptiert · 2026-09-15
**Betrifft:** `cli/autotune` (Schritt `tune`), `vig-bench/vig-fit`
(`VIG_FIT_ARMS=governed`); ADR-0002 (Pipelining), ADR-0004 (Slots), ADR-0034
(Sicherheitsmarge), ADR-0038 (Kalibrierung an der Karte), ADR-0041
(Versorgungsschutz), ADR-0044 (Qualifikation beim Anwender)
**Ausloeser:** Produktentscheidung vom 15.09.2026: `autotune` soll den
Governor auf der Last des Anwenders **einstellen** und nicht nur beurteilen.

## Kontext

`vig autotune` fuehrte bis hierher vier Schritte zusammen: `discover`,
`measure`, `fit`, `check` (ADR-0044). Gemessen wurde die Maschine, geurteilt
wurde ueber den Governor — aber ueber den Governor **in der Einstellung, die
gerade in der Datei stand**. Die Stellgroessen, die ueber seine Wirkung
entscheiden, waehlt bis heute der Mensch:

| Stellgroesse | Voreinstellung | Was sie tut |
|---|---|---|
| `backend.pipelining_depth` | 1 | zusaetzliche Kredite je Slot (ADR-0002) |
| `backend.protect_supply` | aus | Look-ahead haelt Hintergrund auch fuer die Versorgung zurueck (ADR-0041) |
| `backend.margin_learning` | aus | Planung kalibriert sich an der Karte (ADR-0038) |
| `backend.safety_margin_percent` | 110 | Marge auf die Laufzeitprognose (Spec 13.2, ADR-0034) |

Welche Einstellung auf einer Last traegt, haengt an Hardware, Backend und
Modellen — also genau an dem, was nach ADR-0044 nur beim Anwender gemessen
werden kann. Wer die Stellgroessen nicht kennt, bekommt die Voreinstellung,
und `vig-fit` sagt ihm dann, ob sich *die* lohnt. Das ist eine ehrliche
Antwort auf die falsche Frage.

Ein Werkzeug, das einstellt, bringt zwei neue Gefahren mit. Es kann **auf
Rauschen einstellen**: Zwei Laeufe derselben Einstellung liegen nicht auf
dasselbe Promille genau, und wer jede kleinere Differenz behaelt, schreibt
Zufall als Konfiguration fest. Und es kann **an Groessen drehen, die ihm
nicht gehoeren**: Ein Strom, dessen Periode verdoppelt wird, verliert weniger
Takte — und der Anwender bekommt ein besseres Ergebnis fuer eine Zusage, die
er nie gegeben hat.

## Entscheidung

**`autotune` bekommt den Schritt `tune` zwischen `measure` und `fit`.** Die
Reihenfolge ist der Kern: `fit` vergleicht danach den **eingestellten**
Governor mit dem direkten Weg, also den, der in Betrieb geht. Und `fit` misst
dafuer neu, mit beiden Armen und an seinen eigenen Lastpunkten. Die
Einstellung, die in `tune` auf einer verrauschten Messung gewonnen hat, wird
damit nicht durch dieselbe Messung bestaetigt, die sie ausgewaehlt hat.

### Wie bewertet wird

Jede Fassung laeuft durch `vig-fit` mit `VIG_FIT_ARMS=governed`: nur der
Governor-Arm, 10 s je Lastpunkt, bei 100, 110 und 125 % Last (`--quick`: 5 s,
110 und 125 %). 90 % fehlt absichtlich — unterhalb der Saettigung verliert
keine Einstellung etwas, und ein Punkt, an dem alle Fassungen null Promille
haben, kostet Zeit, ohne zu unterscheiden. Den direkten Arm braucht der
Vergleich zweier Einstellungen desselben Governors nicht; `vig-fit` sagt im
JSON (`arms: governed`, direkte Felder `null`) und im Satz, dass das eine
Bewertung ohne Vergleich ist und kein Urteil.

### Zielgroesse

Aus den Zellen, je Promille unabgedeckter Abtastungen aus Verbrauchersicht:

- **`protected_worst`** — ueber alle Lastpunkte das Maximum des schlechtesten
  geschuetzten Stroms.
- **`background_mean`** — je Lastpunkt der Mittelwert der nicht geschuetzten
  Stroeme, gemittelt ueber die Lastpunkte (abgerundet; null, wo es keinen gibt).

**Nachtrag 15.09., nach dem ersten Lauf auf Hardware:** Die erste Fassung nahm
je Lastpunkt den *schlechtesten* nicht geschuetzten Strom. Auf dem Laptop stand
darin in allen sechs Fassungen der unteilbare 95-ms-Block bei 1000 ‰ — er passt
neben einer 33-ms-Periode nie (ADR-0012) —, waehrend `pose` und `depth` zwischen
0 und 3 ‰ lagen. Als Maximum haette dieser eine unerfuellbare Strom jede
Verbesserung der anderen verdeckt; das Tuning konnte dort gar nichts gewinnen.
Der Mittelwert laesst ihn mitzaehlen, aber nicht alles andere zudecken.

Die geschuetzten Stroeme gehen vor. Das ist keine Wahl dieses Werkzeugs,
sondern die Policy, die der Betreiber mit `class: protected` getroffen hat.

### Rauschschwelle und Entscheidungsregel

Eine Fassung wird behalten, wenn sie die Konfigurationspruefung besteht
(`Config::diagnose` im Prozess, `vig-fit` nicht mit Exitcode 2) **und**

- `protected_worst ≤ bester.protected_worst − max(5, bester.protected_worst / 10)`,
  **oder**
- `protected_worst ≤ bester.protected_worst` und
  `background_mean ≤ bester.background_mean − max(10, bester.background_mean / 10)`.

**Nie** behalten wird eine Fassung mit `protected_worst` ueber dem der
unverstellten Fassung. Aus den beiden Regeln folgt das bereits; es steht
trotzdem als erste Pruefung im Code, weil es die Zusage ist, auf die es
ankommt. Gerechnet wird ganzzahlig mit `checked_sub`: Liegt der beste Wert
unter der Schwelle, gibt es dort nichts mehr zu gewinnen.

Fuenf und zehn Promille und „ein Zehntel" sind **gesetzte** Grenzen, keine
gemessene Streuung. Die Streuung einer Bewertung auf der Hardware des
Anwenders kennt dieses Werkzeug nicht, und sie zu messen hiesse, jede Fassung
mehrfach zu fahren. Die Grenzen sind deshalb bewusst grob: lieber eine echte,
kleine Verbesserung verpassen als Rauschen festschreiben. Der relative Teil
waechst mit, weil ein Strom, der 400 ‰ verliert, staerker streut als einer,
der 10 ‰ verliert.

### Warum Koordinatensuche und kein Gitter

Die Suche geht **einmal** ueber die Stellgroessen, in fester Reihenfolge
(Tiefe, Versorgungsschutz, Kalibrierung, Marge), und stellt jede ausgehend von
der bisher besten Fassung um: `pipelining_depth` 0 ↔ 1 (eine hoehere Tiefe
wird mit 0 verglichen), `protect_supply` umgekehrt, `margin_learning` aus ↔
Voreinstellungen (was `margin_learning: {}` ergibt; nicht versucht, wenn
`prediction: active` gesetzt ist, weil die Pruefung beides zusammen ablehnt),
`safety_margin_percent` ± 15 in den Grenzen 100 bis 300 — aus 110 werden 125
und 100. Das sind im ueblichen Fall sechs Bewertungen: die unverstellte und
fuenf Umstellungen.

Ein vollstaendiges Gitter ueber dieselben Werte haette 2 × 2 × 2 × 3 = 24
Fassungen. Bei rund 40 s je Bewertung sind das 16 Minuten statt vier — mehr
als die Haelfte der zugesagten halben Stunde (ADR-0044) fuer einen einzigen
Schritt. Und jede zusaetzliche Fassung ist ein zusaetzlicher Versuch, auf
Rauschen hereinzufallen: Wer 24 verrauschte Messungen vergleicht und die beste
nimmt, nimmt mit hoher Wahrscheinlichkeit eine, die Glueck hatte.

Der Preis ist ausgesprochen: **Wechselwirkungen zwischen Stellgroessen werden
nicht gefunden**, und das Ergebnis haengt von der Reihenfolge ab. Eine Marge,
die erst mit eingeschalteter Kalibrierung hilft, sieht ein Durchgang, der die
Kalibrierung vorher verworfen hat, nicht. Eine Stellgroesse wird nicht noch
einmal versucht, nachdem sich eine spaetere geaendert hat. Der Bericht sagt
deshalb „die beste von N versuchten Einstellungen" und nicht „das Optimum",
und er listet **jede** versuchte Fassung mit ihren Zahlen und dem Grund.

### Was nie eingestellt wird

- **Vertraege** — `period_ms`, `deadline_ms`, `max_age_ms`, `class`. Sie sind
  Zusagen des Betreibers an seine Anwendung (ADR-0044). Ein Werkzeug, das
  sie verstellen duerfte, koennte jedes Ergebnis verbessern, indem es weniger
  verspricht — und der Bericht wuerde eine Versorgung loben, die niemand
  bestellt hat.
- **`backend.slots`** — beschreibt das Backend, nicht den Governor. Er kommt
  aus der gemessenen Nebenlaeufigkeit (ADR-0004); zu hoch gesetzt entsteht
  hinter dem Governor eine unsichtbare Queue, und auf dem Telefon beschreibt
  schon ein Slot das Backend falsch
  ([android-gpu.md](../benchmark/android-gpu.md)). Slots nach der besseren
  Zahl zu waehlen hiesse, ein anderes Backend zu beschreiben als das, das
  gemessen wurde.
- **Modelle, Varianten, Profile, Domaenen.** Jede Fassung wird vor der
  Bewertung darauf geprueft, dass Modelle, Slots und Domaenen gleich der
  gemessenen Fassung sind; eine Fassung, die daran etwas aendern wuerde, wird
  verweigert. Nach Bauart kann das nicht passieren — die Pruefung steht da,
  weil „nie" eine Zusage ist und keine Beobachtung ueber den heutigen Code.
- **`prediction`, `miss_aware_policy`, `actuation`, `hints`, `trust`** und die
  `pipelining_depth` weiterer Domaenen. Sie sind entweder ausdrueckliche
  Handlungen des Betreibers mit eigener Begruendung (ADR-0023, ADR-0027,
  ADR-0029, ADR-0030) oder betreffen nicht die Planung einer Last.

### Was aus ADR-0038 und ADR-0041 wird

Beide ADRs nennen das Einschalten ihrer Stellgroesse **eine Handlung des
Betreibers und keine, die die Policy selbst trifft**. Das gilt weiter: Der
Governor schaltet zur Laufzeit nichts davon selbst ein. `tune` ist ein
Werkzeug, das der Betreiber startet und das eine **Datei** schreibt; in Betrieb
geht sie erst, wenn er sie in Betrieb nimmt. Der Bericht nennt jede
Umstellung gegen die gemessene Fassung einzeln. Den Preis, den ADR-0041 fuer
den Versorgungsschutz nennt — Hintergrundfortschritt —, fuehrt die
Zielgroesse als `background_mean` mit und der Bericht daneben: Eine Fassung,
die die geschuetzten Stroeme um mehr als die Schwelle besser versorgt, wird
auch dann behalten, wenn die nachrangigen dafuer mehr verlieren. Das ist die
Rangfolge aus `class: protected`, und sie steht mit beiden Zahlen im Bericht.

### Ehrlichkeitsregeln

Die Regeln aus ADR-0044 gelten fuer den neuen Schritt ohne Abstrich:

1. **Nichts steht als gemessen da, was nicht gemessen wurde.** Eine nicht
   bewertete Fassung hat im Bericht keine Zahl, sondern einen Grund.
2. **Eine verweigerte Fassung wird genannt**, mit den Befunden der Pruefung
   („refused by the configuration check: …"), und die Suche geht weiter.
3. **Fremdlast entwertet den Vergleich.** Vor und nach dem Schritt wird die
   fremde Rechenzeit beobachtet wie bei `measure` und `fit`. Ist sie zu hoch
   oder nicht beobachtbar, steht der Schritt als `contaminated` da, **und die
   unverstellte Fassung wird nach `measured.yaml` zurueckgeschrieben** — ob der
   Vorsprung von der Einstellung kam oder von der anderen Arbeit, weiss dann
   niemand.
4. **Die unverstellte Fassung bleibt liegen** (`tune/untuned.yaml`), jede
   versuchte ebenso (`tune/candidate-<n>.yaml`, Bewertung `eval-<n>.json`).
   `measure` raeumt `untuned.yaml` vor jeder neuen Messung weg; ein
   wiederholtes `tune` auf derselben Messung beginnt deshalb von der
   gemessenen und nicht von einer schon verstellten Fassung.
5. **Fehlt `vig-fit`, bleibt der Schritt offen** (`skipped`), die Datei
   unveraendert und der Lauf unvollstaendig — wie bei `fit`. Scheitert die
   Bewertung der unverstellten Fassung, scheitert der Schritt.

## Konsequenzen

- **Die Dauer waechst um rund vier Minuten.** Die Schaetzung setzt sechs
  Bewertungen zu je drei Punkten × 10 s plus 10 s Aufwand an, 240 s
  (`--quick`: 120 s). Der Aufwand stammt aus den Laeufen vom 15.09.: `vig-fit`
  brauchte fuer 80 s Messfenster auf dem Laptop 82 s, auf den Telefonen 87 bis
  93 s. Die Messfenster sind feste Wanduhrzeit; anders als `measure` wird
  `tune` auf einem langsamen Geraet kaum laenger. Die Schaetzung fuer die
  Referenzgroesse steigt von 289 s auf 529 s, die zugesagte halbe Stunde
  haelt. Auf dem Pixel 2 lagen `measure` und `fit` zusammen bei rund
  11 Minuten; mit `tune` sind es geschaetzt rund 15.
- **`fit` urteilt ueber den eingestellten Governor.** Ein Urteil gegen uns
  kann dadurch seltener werden. Es wird nicht dadurch seltener, dass so lange
  gesucht wird, bis es passt: `tune` hat fuenf Versuche, eine feste Schwelle
  und eine feste Reihenfolge, und `fit` misst unabhaengig davon neu.
- **`measured.yaml` kann Schalter tragen, die vorher von Hand gesetzt
  wurden.** Der Bericht listet sie. Kommentare gingen in dieser Datei schon
  vorher verloren (`vig calibrate` schreibt sie mit `to_yaml`).
- **Ein Zustand aus der Fassung mit vier Schritten** laedt weiter. `tune` fehlt
  darin unter den erledigten Schritten; nach der Regel aus ADR-0044 („ab dem
  ersten offenen Schritt laeuft alles Folgende mit") laufen `tune`, `fit` und
  `check`.
- **Was offen bleibt:**
  - Jede Fassung wird **einmal** gemessen. Die Schwelle ist gesetzt, nicht aus
    einer gemessenen Streuung abgeleitet; eine Wiederholung der besten und der
    unverstellten Fassung wuerde das belegen und kostet eine weitere Minute.
  - Wechselwirkungen und ein zweiter Durchgang fehlen (siehe oben).
  - `margin_learning` wird nur mit seinen Voreinstellungen versucht, die
    `pipelining_depth` weiterer Domaenen gar nicht.
  - Ob eine auf 100 bis 125 % Last eingestellte Fassung ueber Stunden traegt,
    sagt dieser Schritt so wenig wie der Rest von `autotune` — dafuer gibt es
    den Dauerlauf.
