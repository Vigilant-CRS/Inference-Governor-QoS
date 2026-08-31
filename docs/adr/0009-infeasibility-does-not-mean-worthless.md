# ADR-0009: Verworfen wird, was wertlos ist — nicht, was zu spaet kommt

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec 10.3 (Stufe B), 10.6 (Zielordnung), 12.3 (Auswahlregel)
**Ausloeser:** Gate-S-Lauf, Szenario A

## Kontext

Die erste Implementierung folgte Spec 10.3 Stufe B woertlich und verwarf einen
supersedierbaren Request, sobald

```text
predicted_finish > absolute_deadline
```

galt. Der erste Gate-S-Lauf zeigte, wohin das fuehrt.

Szenario A (ein 30-FPS-Strom, Deadline 66 ms, `max_age` 66 ms) bei 125 %
Angebotslast:

| | unabgedeckte Perioden | stale compute | gelieferte gueltige Ergebnisse |
|---|---:|---:|---:|
| OneTimer | 1000 ‰ | 0 ‰ | **0** |
| FIFO-Baseline | 692 ‰ | 639 ‰ | **282** |

Bei dieser Last liegt `p99 * 1.10` knapp ueber der Deadline. Damit war **jeder**
Request unmachbar, jeder wurde abgelehnt, und das System fuehrte nichts mehr
aus. Die Metrik „stale compute 0 ‰" war dabei formal richtig und inhaltlich
wertlos: wer nichts rechnet, verschwendet nichts.

Das ist die vakuose Loesung der lexikographischen Zielordnung aus Spec 10.6.
Wer Protected-Deadline-Misses minimiert, indem er keine Protected-Arbeit mehr
ausfuehrt, hat null Misses — und einen blinden Roboter. Dass ADR-0005 diesen
Fall sichtbar gemacht hat, ist genau der Zweck der periodenbezogenen Metrik;
eine requestbezogene Miss-Rate haette hier ein perfektes Ergebnis gemeldet.

## Entscheidung

**Die Deadline steuert Reihenfolge und Variantenwahl. Ueber das Verwerfen
entscheidet die Frische.**

Ein Request wird vor dem Dispatch nur dann verworfen, wenn sein Ergebnis bei
prognostizierter Fertigstellung fachlich wertlos waere:

```text
(predicted_finish - generation_time) > max_age
```

Verfehlt er lediglich seine Deadline, bleibt aber innerhalb von `max_age`, wird
er mit der schnellsten verfuegbaren Variante **ausgefuehrt** und bei
Fertigstellung nach Stufe C bewertet.

Unveraendert bleiben:

- Der Variant Resolver waehlt weiterhin die hoechstwertige Variante, die die
  Deadline haelt (Spec 12.3), und die schnellste, wenn keine sie haelt.
- EDF und Kritikalitaet bestimmen weiterhin die Reihenfolge (Spec 10.5, 10.6).
- Stufe A (Ingress Supersession) und die Alterspruefung wartender Requests
  bleiben unveraendert.
- Ohne konfiguriertes `max_age` wird vor dem Dispatch nicht verworfen. Wer
  keine Frischegrenze angibt, bekommt keine Freshness-Ablehnung.

## Begruendung

Spec 10.2 trennt ausdruecklich Request Latency von Information Age und erklaert
Letztere zur eigentlichen Produktgroesse. Dann muss auch die
Verwerfensentscheidung an ihr haengen. Eine um 5 ms verspaetete, aber frische
Objekterkennung ist fuer den Roboter brauchbar; eine puenktliche Erkennung auf
einem 300 ms alten Bild ist es nicht.

Spec 10.3 formuliert Stufe B mit „**darf** ein supersedierbarer Request
verworfen werden" — als Erlaubnis, nicht als Pflicht. Dieses ADR schoepft die
Erlaubnis enger aus, als die erste Implementierung es tat.

## Konsequenzen

- `protected_deadline_misses` steigt. Das ist beabsichtigt und ehrlich: die
  Deadline wird tatsaechlich verfehlt, und die Metrik soll das zeigen, statt
  den Miss durch eine Ablehnung zu ersetzen.
- Die Erfolgsbewertung braucht eine **Lebendigkeitsbedingung**. Ein Governor,
  der weniger liefert als die Baseline, hat den Vergleich nicht gewonnen,
  gleichgueltig wie gut seine uebrigen Kennzahlen aussehen. Der Gate-S-Report
  fuehrt diese Bedingung ab sofort explizit.
- `max_age` wird zum wichtigsten Konfigurationswert eines Streams. Fehlt er,
  verliert OneTimer sein staerkstes Werkzeug. `onetimer doctor` muss das
  Fehlen als Warnung melden.
