# ADR-0027: Ein Missbudget, das entscheidet — auf ausdrueckliche Handlung

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** `core/contract_ext`, `core/scheduler`; Paket NV-24, NV-02
**Ausloeser:** NV-02 hat das Missfenster beobachtbar gemacht; beobachtet wird
seither, entschieden nicht

## Kontext

NV-02 hat die Weakly-hard-Bedingung eingefuehrt und ausdruecklich als
**Monitor** gebaut: eine begrenzte Ringstruktur genuegt zum Beobachten,
Durchsetzung braucht kuenftige Kapazitaet und beherrschte Stoerungen. Das war
richtig — und es laesst eine Groesse ungenutzt, die genau dort entsteht, wo es
knapp wird.

Der Monitor weiss, wie viele Misses ein Fenster noch vertraegt. Der Scheduler
weiss es nicht. Er ordnet nach Kritikalitaet und Deadline und behandelt damit
zwei gleichwertige Stroeme gleich, von denen der eine sein Budget aufgebraucht
hat und der andere nicht.

## Entscheidung

**Der Spielraum wird berechnet, nicht die Vergangenheit gezaehlt.**
`slack()` gibt `misses_left` und `consecutive_left` — nicht „wie viele waren
es", sondern „wie viele darf es noch geben". Waehrend der Aufwaermphase traegt
das Ergebnis ein `observed: false`: ein noch nicht vollstaendig beobachtetes
Fenster darf keinen Spielraum vortaeuschen, der nicht belegt ist.

**Der Pflichtzyklus ist ein eigener Begriff.** `next_is_mandatory()` ist wahr,
wenn ein Miss im naechsten Zyklus eine Zusage bricht — im Unterschied zu einem,
der noch im Budget liegt.

**Der Vorrang aendert sich nur innerhalb einer Kritikalitaetsklasse.** Der
Sortierschluessel ist `(Kritikalitaet, Pflichtzyklus, Deadline,
Generationszeit)`. Der Pflichtzyklus steht **nach** der Kritikalitaet: ein
ausgehungerter `best_effort`-Strom geht nie vor einen `protected`-Strom mit
Spielraum. Der Vorrang zwischen Klassen ist die Betreiberpolicy und bleibt es.

**Voreinstellung aus.** Dieses Paket liefert eine empirische Policy, keine
formale Zusage — eine Musterngarantie braeuchte NV-23 mit tragfaehigen
Ausfuehrungsgrenzen. Wer die Policy einschaltet, soll es entschieden haben.
Mit `set_miss_aware_policy(false)` ist der Sortierschluessel bitgleich der
alte, und die bestehenden Goldens gelten unveraendert.

**Das Fenster wird nie zurueckgestellt.** Auch nicht bei einer Verletzung, auch
nicht bei Ueberlast. Ein Zaehler, der sich unter Druck selbst zurueckstellt,
meldet nie eine Verletzung — und ein Nenner, der sich aendert, macht jede
Vorher-Nachher-Aussage wertlos. Die Erholung dauert genau so lange, wie das
Fenster gleitet, und das steht als Test da.

## Konsequenzen

`vig_weakly_hard_misses_left` je Modell zeigt dem Betreiber, wie eng es
zugeht — bevor eine Verletzung eintritt, nicht danach.

Ein Strom mit dauerhaft erschoepftem Budget wuerde innerhalb seiner Klasse
dauerhaft vorgehen. Das ist der beabsichtigte Effekt und gleichzeitig das
Risiko; das Gegengewicht ist `minimum_background_progress_pct` aus NV-02 und
die Tatsache, dass sich das Fenster nach jeder Versorgung weiterschiebt. Ob
das im Feld genuegt, ist eine Messfrage und nicht durch dieses ADR
beantwortet — deshalb ist die Policy abschaltbar und aus.

## Alternativen

**Den Pflichtzyklus vor die Kritikalitaet stellen.** Haette starker gewirkt und
die Betreiberpolicy ausgehebelt: ein Strom, den der Betreiber ausdruecklich als
`best_effort` eingeordnet hat, darf sich nicht durch schlechte eigene Zahlen
nach vorne bringen.

**Die Policy automatisch einschalten, sobald ein Missbudget konfiguriert ist.**
Bequem, und es haette eine empirische Policy als Nebenwirkung einer
Schemaaenderung scharfgeschaltet.

**Das Fenster nach einer Verletzung zuruecksetzen, um Erholung zu zeigen.**
Haette schoenere Dashboards ergeben und die Verletzung unsichtbar gemacht.
