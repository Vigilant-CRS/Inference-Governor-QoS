# ADR-0046: Eine Mindestlaufzeit wird von den nachrangigen Klassen bezahlt

**Status:** Akzeptiert, opt-in · 2026-09-15
**Betrifft:** `core/runtime_budget`, `core/scheduler` (Kandidatenwahl),
`config` (`contract.min_runtime`), `cli/doctor` (`check_utilization`),
Exporter; Spec 10.6, ADR-0012, ADR-0014, ADR-0027, ADR-0043
**Ausloeser:** Demo vom 15.09. ([Messung](../benchmark/demo-2026-09-15.md),
`examples/demo/krakow-cams4.yaml`)

## Kontext

Vier Kameras mit RF-DETR bei 30 Bildern/s und SmolVLM teilen sich eine GPU mit
zwei Slots. Die Frontkamera ist `protected`, die drei anderen sind `normal`,
das Sprachbildmodell ist `best_effort` (p99 170 ms). Unter dem Governor bleibt
die Frontkamera in jedem Takt frisch — und das Sprachmodell antwortet zwei-
bis dreimal in 40 Sekunden statt rund zweihundertmal.

Das ist die lexikographische Ordnung aus Spec 10.6, genau wie geschrieben: die
drei `normal`-Kameras wollen zusammen mehr als einen Slot, es wartet also immer
ein `normal`-Frame, und `best_effort` kommt erst dran, wenn keiner wartet. Kein
Fehler — aber ein Betreiber, der Wahrnehmung und Sprachmodell nebeneinander
betreibt, braucht beide mit Fortschritt: „das Sprachmodell bekommt mindestens
X ms GPU-Zeit je Sekunde, bezahlt von den nachrangigen Kameras, nie von der
geschuetzten".

Es gibt dafuer schon zwei Werkzeuge, und beide beantworten eine andere Frage.
`minimum_background_progress_pct` (NV-02) ist ein Weakly-hard-Kriterium, das
der Monitor **zaehlt**; ADR-0043 hat ausdruecklich entschieden, dass es den
Versorgungsschutz nicht ueberstimmt. Die missbudgetbewusste Policy (ADR-0027)
ordnet **innerhalb** einer Klasse um, nie ueber Klassengrenzen. Hier geht es
um eine Ausfuehrungszusage **ueber** eine Klassengrenze hinweg — aber nur
ueber die zwischen nachrangigen Klassen.

## Entscheidung

**Ein Budget je Modell, im Vertrag.**
`contract.min_runtime: { budget_ms, window_ms }` heisst: mindestens
`budget_ms` Ausfuehrungszeit in jedem gleitenden Fenster von `window_ms`.
Zulaessig nur fuer `normal` und `best_effort`. An `high` oder `protected` wird
es abgelehnt: diese Klassen stehen bereits darueber, und ein Budget dort waere
eine Zusage ohne Wirkung. `window_ms` liegt zwischen 1 und 60 000, `budget_ms`
ist groesser als null und hoechstens `window_ms` mal Zahl der regulaeren Slots.

**Der Rang wird um eine Stufe erweitert, nicht die Kritikalitaet.** Der
Sortierschluessel ist `(Rang, Pflichtzyklus, Deadline, Generationszeit)` mit

| Rang | Arbeit |
|---:|---|
| 4 | `protected` |
| 3 | `high` |
| 2 | `normal` oder `best_effort` mit verbleibendem Budget im Fenster |
| 1 | `normal` |
| 0 | `best_effort` |

Ist das Budget im Fenster aufgebraucht (gebucht ≥ Budget), faellt das Modell in
seinen eigenen Rang zurueck. Zwei Modelle mit verbleibendem Budget ordnen sich
untereinander wie bisher nach Pflichtzyklus, Deadline und Generationszeit.
Ohne Budget bildet der Rang die Kritikalitaet streng monoton ab; die Ordnung
ist bitgleich die alte, und alle bestehenden Goldens gelten unveraendert.

**Alles andere bleibt, wie es ist.** Der Look-ahead bekommt weiter die echte
Kritikalitaet des Kandidaten und prueft ihn gegen jede erwartete bewachte
Ankunft (ADR-0036, ADR-0041); ein Veto verschiebt ihn wie bisher. Co-Run-
Verbote, Slotkredite, die Zuschneidung zerlegbarer Auftraege (ADR-0014), das
Verwerfen wertloser Arbeit und die Admission unter Ueberlast gelten
unveraendert — auch fuer den Auftrag mit Budget selbst.

**Gebucht wird Ausfuehrungszeit, geplant und dann gemessen.** Beim Dispatch
die geplante Laufzeit (`p99 × Marge`, bei einem Quantum dessen Dauer), bei
Fertigstellung oder Fehler ersetzt durch die gemessene. Die Planung muss sofort
zaehlen: sonst naehme ein zweiter freier Slot denselben Vorrang, bevor der erste
Auftrag fertig ist. Jeder Dispatch des Modells bucht, auch einer ohne Vorrang —
vereinbart ist Ausfuehrungszeit, nicht Vorrang.

**Das Fenster gleitet in 16 Abschnitten.** Gebucht wird im Abschnitt des
Dispatchzeitpunkts; ein Auftrag zaehlt ganz in das Fenster, in dem er begann.
Eine Buchung faellt fruehestens nach 15/16 und spaetestens nach 16/16 des
Fensters heraus. Speicher und Aufwand sind fest, es wird nichts allokiert, und
die Rechnung haengt nur an der uebergebenen Zeit.

**`vig doctor` rechnet die Budgets zur geschuetzten Auslastung.**
`Summe(budget/window)` ueber die Slots kommt auf `U` aus Spec 10.9. Liegt die
Summe ueber 100 %, meldet er `RUNTIME_BUDGET_UNSCHEDULABLE` und `NOT_READY`:
die Budgets koennten nur aus Zeit bedient werden, die der bewachten Arbeit
gehoert, und die gibt sie nicht her. Ohne Budget sind Rechnung und Meldungen
die bisherigen.

## Was das Budget zusagt — und was nicht

Es sagt zu: **Solange im Fenster Budget bleibt, geht wartende Arbeit dieses
Modells vor jeder `normal`- und `best_effort`-Arbeit, wo immer ein Slot frei ist
und der Look-ahead den Start zulaesst.** Bezahlt wird das von den nachrangigen
Klassen. Bewachte Arbeit gibt nichts ab: sie steht im Rang darueber, und der
Look-ahead haelt den Auftrag zurueck, wenn er eine erwartete bewachte Ankunft
unmachbar machen wuerde.

Es sagt nicht zu:

- **Es schafft keine Luecke.** Ein nicht unterbrechbarer Block, der neben der
  bewachten Arbeit nicht passt, passt mit Budget genauso wenig (ADR-0012). Auf
  einem Slot mit 33-ms-Detektor startet ein 200-ms-Block nie; das Budget bleibt
  dann unerfuellt und ist in `vig_runtime_budget_used_us` gegen
  `vig_runtime_budget_granted_us` sichtbar. Abhilfe bleiben mehr Slots,
  Zerlegung, Praemption oder eine eigene Ressource.
- **Es reserviert nichts.** Wird der Auftrag vetoiert, darf nachrangige Arbeit
  den Slot trotzdem nehmen, wenn sie die bewachte Ankunft nicht gefaehrdet; der
  Governor laesst keinen Slot fuer ein Budget leer stehen.
- **Es ist eine Untergrenze fuer den Vorrang, keine Obergrenze fuer die
  Laufzeit.** Der letzte Auftrag im Fenster darf das Budget um bis zu seine
  eigene Laufzeit ueberziehen; ein unteilbarer Block laesst sich nicht anteilig
  starten. Wartet sonst nichts, rechnet das Modell ohnehin mehr.
- **Es hebt die Ueberlaststufen nicht auf.** Weist der Ueberlastregler
  `best_effort` ab, weil bewachte Arbeit unter Druck steht, gilt das auch fuer
  ein Modell mit Budget.
- **Die `normal`-Stroeme werden aelter.** Das ist der Preis und steht als
  Befund in ihren Abdeckungszahlen, nicht versteckt.

## Konsequenzen

- Opt-in je Modell. Ohne `min_runtime` aendert sich keine Entscheidung und
  keine Meldung.
- Neue Metriken je Modell: `vig_runtime_budget_granted_us`,
  `vig_runtime_budget_window_us`, `vig_runtime_budget_used_us` und der Zaehler
  `vig_runtime_budget_dispatches_total`.
- Im Simulator mit der Form der Demo (Front `protected`, drei `normal`, 150-ms-
  Bloecke, zwei Slots, Marge 110 %) belegt; auf der GPU **nicht gemessen**. Ob
  die Frontkamera dort unveraendert frisch bleibt, entscheidet erst eine
  Messung — insbesondere, weil die tatsaechliche Laufzeit neben einem laufenden
  Sprachmodell laenger ist als das Alleinprofil.

## Alternativen

**Das Budget ueber `high` oder `protected` stellen.** Haette das Sprachmodell
auch bei ueberzeichneter bewachter Last durchgesetzt — und genau die Zusage
gebrochen, die dieses Produkt ausmacht (ADR-0012). Wer bewachte Frames opfern
will, entscheidet das nicht nebenbei ueber einen Budgetwert.

**Den Look-ahead fuer ein Modell mit Budget lockern.** Der Regler zwischen zwei
Zusagen, den ADR-0043 verworfen hat: er wuerde abwechselnd beide brechen.

**Die Kritikalitaet des Modells zeitweise auf `normal` setzen.** Haette gegen
die drei `normal`-Kameras nur Gleichstand gebracht; die Deadline des
Sprachmodells (5 s) haette ihn in der EDF-Ordnung fast immer verloren. Und die
Kritikalitaet steuert mehr als die Reihenfolge — Look-ahead, Admission und
Ueberlast lesen sie.

**Einen Slot fuer das Sprachmodell reservieren.** Einfach, und der Slot stuende
leer, sobald das Modell nichts zu tun hat. Das Budget nimmt nur, was es
braucht, und gibt den Rest an die nachrangigen Klassen zurueck.

**Gewichtete Anteile statt eines Rangs.** Eine Score-Funktion, in der viele
nachrangige Auftraege einen wichtigeren aufwiegen koennen, schliesst Spec 10.6
aus. Ein Rang ist lexikographisch pruefbar; ein Gewicht ist es nicht.
