# ADR-0047: Ein Ziel hat zwei Haelften — Anteil und Luecke

**Status:** Entwurf · 2026-09-16
**Betrifft:** `core/objective` (neu), `core/scheduler` (Kandidatenwahl),
`config` (`contract.objective`), `cli/doctor` (Zulassung), Exporter;
Spec 10.6, NV-24, ADR-0027, ADR-0035, ADR-0043, ADR-0046
**Ausloeser:** Mischlastmessung vom 16.09.2026
(`InferenceQoS-runtime/uebergabe/mischlast-voll-2026-09-16.md`)

## Kontext

Die Wichtigkeit eines Stroms ist heute eine von vier Stellungen: `protected`,
`high`, `normal`, `best_effort`. Die Ordnung ist lexikographisch, und das
Ergebnis ist grobkoernig. Die Messung vom 16.09. zeigt es in einer Zeile: unter
Last blieb die geschuetzte Kamera bei 13–24 ‰ unabgedeckt, die Klasse darunter
landete bei 406–465 ‰ und die naechste bei 511–581 ‰. Zwischen „fast alles" und
„fast nichts" gibt es nichts.

Ein Betreiber will aber Zwischenstufen aussprechen koennen:

> Frontkamera 98 %. Heckkamera 20 % — aber mindestens einmal je Sekunde muss
> eines durch.

Der zweite Satz ist der entscheidende. Ein Anteil allein sagt nichts ueber die
Verteilung: 20 % in zehn Sekunden koennten acht Sekunden Stille und dann zwei
Sekunden Vollgas sein. Fuer einen Verbraucher ist das wertlos. Ein Ziel muss
deshalb **zwei Haelften** haben — wie viel insgesamt, und wie lange nie nichts.

## Entscheidung

Ein Vertrag kann ein Ziel tragen:

```yaml
contract:
  period_ms: 33
  max_age_ms: 100
  objective:
    coverage_permille: 200    # Anteil frischer Zyklen im Fenster
    window_ms: 10000
    max_gap_ms: 1000          # und nie laenger als 1 s ohne Ergebnis
```

Beide Haelften sind einzeln optional, mindestens eine muss dastehen.

**Gebaut wurde ohne die urspruenglich geplante Haerte.** Der Entwurf sah ein
Feld `hardness: firm | soft` vor. Beim Einbau zeigte sich, dass es
ueberfluessig ist: Die **Klasse ist die Haerte**. Das Ziel ordnet innerhalb
der Klasse um, genau wie der Pflichtzyklus aus NV-24, und eine zweite
Haertesystematik daneben haette dieselbe Aussage doppelt gefuehrt — mit dem
Risiko, dass beide sich widersprechen.

**Beide werden in dieselbe Waehrung umgerechnet: Restzeit bis zum Bruch.**

* *Anteil:* Im gleitenden Fenster erwartet der Vertrag `N = window / period`
  Zyklen; erfuellt sind davon `ok`. Der Puffer ist `ok − ⌈z·N⌉` Zyklen, in Zeit
  also `Puffer × period`. Ist er negativ, liegt der Strom hinter seiner Zusage.
* *Luecke:* `letzter Erfolg + max_gap − jetzt`.

Der Slack eines Stroms ist der **kleinere** der beiden. Er tritt in der
Kandidatenwahl an die Stelle, an der heute die Deadline steht — also
**innerhalb** der Klasse, nie darueber (NV-24, wie die missbudgetbewusste
Policy aus ADR-0027). Die Klasse bleibt die Haerte des Ziels: ein Ziel auf
einem `normal`-Strom wirkt gegen andere `normal`-Stroeme, nicht gegen `high`.

Damit braucht es keine neue Rangstufe und keine zweite Haertesystematik. Wer
kein Ziel vereinbart, wird geplant wie bisher — bitgleich.

## Warum nicht anders

**Warum keine reinen Gewichte (WFQ)?** Gewichte teilen Kapazitaet, sie halten
keine Zusage. „98 %" ist eine Zusage, „Gewicht 5" ist keine.

**Warum nicht Ziele statt Klassen?** Weil bei transienter Ueberlast jemand
zuerst nachgeben muss. Reine Defizitsteuerung verteilt den Schmerz gleichmaessig
— alle verfehlen ihr Ziel ein wenig. Fuer eine Bremskamera ist das schlechter
als: die Wichtigste haelt, die Unwichtigste bricht. Die Klasse bleibt deshalb
die grobe Ordnung, das Ziel ist die Feinverteilung darin.

**Warum nicht `min_runtime` erweitern (ADR-0046)?** Das Budget ist eine Zusage
in *Rechenzeit* („300 ms je Sekunde"), ein Ziel ist eine Zusage in *Ergebnissen*
(„98 % der Zyklen frisch"). Beide sind sinnvoll und beide bleiben; ein Modell
kann beides tragen, und dann gilt der kleinere Slack.

## Die Randfaelle, die das Modell tragen muss

1. **Stiller Verbraucher.** Die Lueckenuhr darf nur zaehlen, wenn Arbeit anliegt.
   Die Kandidatenwahl betrachtet ohnehin nur nichtleere Queues — der Strom wird
   also nie dringend, weil niemand etwas will.
2. **Kaltstart.** Ein halb gefuelltes Fenster rechnet gegen die bisher
   verstrichene Zeit, nicht gegen das volle Fenster. Sonst waeren in der ersten
   Sekunde alle Stroeme maximal dringend.
3. **Fenster schon verloren.** Der Slack saettigt bei einem Fenster. Ein Strom,
   dessen Ziel nicht mehr erreichbar ist, darf nicht unendlich dringend werden
   und alles niederwalzen; die Klassengrenze schuetzt darueber hinaus.
4. **Widerspruch in den Haelften.** `max_gap` unter der Periode ist unerfuellbar,
   `coverage` ohne `period` ist nicht ausrechenbar — beides wird abgelehnt, an
   seiner Fundstelle (Spec L-020).
5. **Die strengere Haelfte zaehlt fuer die Zulassung.** Ein `max_gap` von 1 s
   erzwingt bei 33 ms Takt mindestens 33 ‰, auch wenn `coverage` darunter
   steht. Die Zulassung rechnet mit `z_eff = max(z, period / max_gap)`.
6. **Gleichstand.** Der Schluessel bleibt vollstaendig geordnet: Klasse, dann
   Pflichtzyklus, dann Slack, dann Deadline, dann Generationszeit, dann
   Modellindex. Ohne Determinismus ist nichts reproduzierbar testbar.
7. **Erst Qualitaet, dann Verdraengung.** Wer sein Ziel verfehlt und eine
   schnellere Variante hat, nimmt sie, bevor er einen Nachbarn verdraengt.
8. **Zulassung.** `Σ z_eff·C/T` plus der Blockierterm aus ADR-0035 muss in die
   regulaeren Slots passen. Ohne diese Rechnung ist ein Ziel ein Wunschzettel —
   deshalb kam die Blockierpruefung zuerst.

## Folgen

Die Erklaerbarkeit wird teurer. Heute genuegt „protected geht vor"; kuenftig
muss `vig explain` sagen „die Heckkamera lag 3 % unter ihrer Zusage und ihre
Luecke war bei 940 von 1000 ms — deshalb ging sie vor". Das ist Arbeit an den
Metriken und an der Erklaerung, nicht nur am Scheduler.

Der Kern bekommt je Modell einen gleitenden Zaehler (fester Speicher, 16
Abschnitte, wie `RuntimeLedger`). Der Aufwand je Entscheidung bleibt konstant
(Spec L-003).

## Stand

1. **Ziel in Vertrag und Konfiguration, mit Validierung.** Fertig:
   `core/objective` (13 Tests), `contract.objective` in der Konfiguration
   (11 Tests). Der Zaehler entsteht **beim ersten Ereignis eines Stroms**,
   nicht beim Bau des Schedulers — das loest Randfall 2 an der Wurzel, statt
   ihn nachtraeglich zu glaetten.
2. **Messung im Governor.** Fertig: gebucht wird in `observe_supply`, und nur
   dort — ein gueltiges Ergebnis, das bei der Auslieferung noch trug. Vier
   Reihen je Strom (`vig_objective_coverage_permille`, `_gap_us`, `_slack_us`,
   `_deficit_us`); Luft und Rueckstand getrennt, damit keine Auswertung ein
   Vorzeichen deuten muss. Sechs Tests.
3. **Zulassungsrechnung.** Fertig: `objective_utilization_permille` und der
   Befund `OBJECTIVE_UNSCHEDULABLE`. Bewachte Stroeme zaehlen **nicht
   doppelt** — sie stecken ueber ihre Klasse schon in der geschuetzten
   Auslastung. Ohne Takt traegt die Luecke die Rechnung.
4. **Steuerung.** Fertig: Der Slack steht im Kandidatenschluessel zwischen
   Pflichtzyklus und Deadline. Ein Strom ohne Zusage gilt als unendlich
   geduldig; sind es alle oder keiner, faellt die Stufe heraus. Belegt durch
   die 27 Golden-Tests und 392 Kern-Tests, die unveraendert gruen sind, und
   durch einen Test, der **seine eigene Referenz erzeugt**: derselbe Aufbau
   ohne und mit Zusage. Der erste Versuch dieses Tests haette eine Zahl
   gefeiert, die ohne jede Zusage genauso herausgekommen waere.
5. **Variantenkopplung (Randfall 7).** Fertig: `PlanningContext.degrade` trug
   die Semantik bereits — „unter Ueberlast gewinnt die schnellste Variante,
   die die Mindestqualitaet noch erfuellt". Ein negativer Slack setzt ihn
   ebenfalls. Wer hinter seiner Zusage liegt, senkt zuerst die eigene
   Qualitaet; die Mindestqualitaet bleibt unantastbar.
6. **Messung auf Hardware.** Durchgefuehrt, **Ergebnis negativ.** Sechs Laeufe
   zu 30 s auf einer RTX 4070 Laptop
   (`InferenceQoS-runtime/uebergabe/ziele-hardware-2026-09-16.md`): Bei einer
   Zusage von 800 ‰ liefert der versprochene Strom 522 bis 557 ‰ — genauso
   viel wie ohne jede Zusage. Die Steuerung **wirkt** nachweisbar (der
   Referenzstrom ohne Zusage faellt in allen vier Zusage-Laeufen unter beide
   Vergleichslaeufe, ohne Ueberlapp), und die bewachte Kamera bleibt
   unberuehrt (5–13 ‰ unabgedeckt gegen 6–16 ‰). Aber das Genommene kommt
   beim versprochenen Strom nicht an.

## Warum Schritt 6 scheiterte

Die Zulassungsrechnung hielt die Zusage fuer tragbar — 676 ‰ von 1000 — und
die GPU war gemessen voll. Zwei Gruende, beide strukturell:

* **Die Rechnung veranschlagt den zugesagten Anteil, die Stroeme senden mit
  vollem Takt.** Ein Strom, der 800 ‰ von 600 Bildern verspricht, bietet
  trotzdem 600 an; die Rechnung sieht 128 ‰ der Slots, das Angebot ist ein
  Vielfaches davon.
* **Eine Zusage auf `normal` holt beim Versorgungsschutz nichts, was bewachter
  Arbeit gehoert.** Der Slack ordnet innerhalb der Klasse (NV-24). Er
  verschiebt zwischen gleichrangigen Stroemen — genau das zeigt die Messung —,
  aber er schafft keine Kapazitaet.

Und ein methodischer Fund, der beide Male dieselbe Form hatte: Der erste
Versuch lief mit 500 ‰, und die reine Klassenordnung lieferte bereits 522 bis
540 ‰ — die Zusage war erfuellt, bevor die Steuerung etwas tun musste. Im
Kern-Test war es dieselbe Falle: 12 zu 8 Starts sahen nach Wirkung aus und
waren ohne jede Zusage identisch. Beide Versuche erzeugen jetzt ihre eigene
Referenz; ohne die waere hier eine Wirkung berichtet worden, die es nicht
gibt.

## Was bewusst nicht gebaut ist

**Die Daempfung (Randfall 4).** Der Slack aendert sich nur in
Fensterabschnitten, und die Saettigung begrenzt jeden Ausschlag — aber dass
zwei Stroeme mit gleichem Ziel nicht in ein Zickzack geraten, ist damit
**nicht bewiesen**. Hysterese und eine Anpassung nur alle k Zyklen stehen aus;
bis dahin gilt die Steuerung als erprobt, nicht als abgesichert.

**Akzeptiert wird diese ADR erst, wenn Schritt 6 auf gemessener Hardware
zeigt, dass die Ziele gehalten werden — vorher ist sie ein Entwurf.**
