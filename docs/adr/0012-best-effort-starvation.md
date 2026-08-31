# ADR-0012: Aushungerung ist ein Befund, kein Nebeneffekt

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec 1.3, 10.6 (lexikographische Ordnung), 10.7, 10.11, 15
**Ausloeser:** `wire-bench`, Szenario B

## Kontext

Der Look-ahead aus Spec 10.7 verschiebt einen Best-Effort-Start, wenn er eine
erwartete geschuetzte Ankunft unmachbar machen wuerde. Jede einzelne dieser
Entscheidungen ist richtig. In der Summe ergeben sie etwas, das niemand
entschieden hat.

`wire-bench`, Szenario B: geschuetzte serialisierte Auslastung **61 %**, also
reichlich freie Kapazitaet. Ergebnis ueber 15 Sekunden:

| Strom | Abdeckung ohne Governor | mit Governor |
|---|---:|---:|
| detector (50 ms Periode) | 87 % | 99 % |
| pose | 93 % | 100 % |
| depth | 98 % | 100 % |
| **vlm (200 ms Laufzeit)** | **100 %** | **0 %** |

Der VLM lief **kein einziges Mal**. Nicht wegen Ueberlast — sondern weil ein
200-ms-Job auf einem nicht praeemptierbaren Slot **immer** die naechste
50-ms-Ankunft gefaehrdet. Die Auslastung ist dabei irrelevant: die Blockade ist
eine Eigenschaft der Laufzeitverhaeltnisse, nicht der Last.

Damit funktioniert genau das Szenario nicht, mit dem Spec 1.3 das Produkt
begruendet: Detektor und VLM auf einer GPU.

## Die eigentliche Ursache

Spec 10.10 sagt es bereits: eine laufende GPU-Inferenz ist nicht zuverlaessig
unterbrechbar. Daraus folgt eine Unmoeglichkeit, die keine Schedulingpolitik
aufloest:

> Ein nicht unterbrechbarer Job, der laenger dauert als die Periode einer
> geschuetzten Aufgabe, kann mit dieser nicht auf derselben Ausfuehrungseinheit
> koexistieren — ohne dass eine von beiden verliert.

Es gibt drei Auswege, und alle drei sind Entscheidungen des Betreibers:

1. **Mehr Slots.** In Szenario C laeuft derselbe Workload auf zwei Slots, und
   beide Seiten werden bedient.
2. **Den langen Job zerlegen.** Spec 15.3: kooperative Quanten fuer generative
   Modelle (WP26). Aus einem 200-ms-Block werden Abschnitte, zwischen denen
   geschuetzte Arbeit vorbei darf.
3. **Bewusst geschuetzte Arbeit opfern.** Eine begrenzte Zahl verpasster
   Frames gegen einen laufenden VLM eintauschen.

## Entscheidung

**1. OneTimer entscheidet Punkt 3 nicht selbst.**

Die lexikographische Ordnung aus Spec 10.6 ist die zentrale Produktzusage:
geschuetzte Arbeit wird nicht gegen Best-Effort-Arbeit verrechnet. Ein
Scheduler, der von sich aus anfaengt, Frames zu opfern, damit ein
Hintergrundjob laeuft, bricht genau diese Zusage — und zwar unsichtbar.

**2. Aushungerung wird gemessen und gemeldet, nicht verschwiegen.**

Neue Metrik `onetimer_best_effort_starved_total` und ein Zaehler fuer die
laengste Zeitspanne ohne Best-Effort-Ausfuehrung. Ein Betreiber muss aus den
Kennzahlen ablesen koennen, dass seine Hintergrundlast nie laeuft. Heute waere
das nur an ausbleibenden Antworten zu erkennen.

**3. `doctor` warnt vor dem Start.**

Uebersteigt die konservative Laufzeit eines Best-Effort-Modells die kuerzeste
geschuetzte Periode, wird das Modell unter Last nie starten. Das ist zur
Konfigurationszeit ausrechenbar, und der Nutzer soll es dort erfahren — nicht
nach zwei Wochen Betrieb aus einer leeren Metrik.

```text
WARN vlm: konservative Laufzeit 352 ms uebersteigt die kuerzeste geschuetzte
     Periode 50 ms (detector). Das Modell wird unter Last nie starten.
     Abhilfe: mehr Slots, kuerzere Quanten oder eine hoehere Klasse.
```

## Konsequenzen

- Die Wettbewerbsmatrix in Spec 3.4 verspricht „VLM neben Detektor". Bis WP26
  gilt das nur mit mehr als einem Slot. Das ist in der Aussendarstellung so zu
  sagen — Spec 3.5 verlangt genau diese Ehrlichkeit.
- WP26 (kooperative Quanten) steigt von „Vollausbau" zu einer Voraussetzung
  fuer den Ein-Slot-Fall auf.
- Der Protected Slack Server aus Spec 10.11 ist damit **nicht** erledigt. Er
  waere die Antwort fuer den Fall, dass der Best-Effort-Job kuerzer ist als die
  geschuetzte Periode und trotzdem verdraengt wird. Dieses ADR behandelt den
  anderen Fall: den, den auch ein Slack Server nicht loesen kann.

## Was hier bewusst offen bleibt

Ob ein Betreiber geschuetzte Frames opfern darf, um Best-Effort-Arbeit zu
ermoeglichen, ist eine Produktentscheidung mit Sicherheitsbezug. Sie gehoert in
die Konfiguration, nicht in die Voreinstellung, und sie braucht vorher eine
Antwort auf die Frage, wie viele Frames ein Roboter verlieren darf. Solange die
Antwort fehlt, ist „nie" die richtige Voreinstellung.
