# ADR-0040: Ein Neustart belegt nur, was mit ihm starb

**Status:** Akzeptiert · 2026-09-11
**Betrifft:** `gateway/actor`, `backend-triton/client`; ADR-0004, ADR-0018,
ADR-0032, ADR-0037
**Ausloeser:** Review vom 11.09., Befunde R01 und R02
(`docs/reviews/2026-09-11-runtime/REVIEW.md`)

## Kontext

Ein Slotkredit, dessen Aufruf abbrach, endet nur durch einen Nachweis aus der
Statistik des Backends (ADR-0004, ADR-0032). Zwei Wege gab es: der Zaehler
erreicht „Basislinie plus alle Auslieferungen", oder er faellt — ein Neustart
des Servers oder ein Neuladen des Modells, und dann sei „alles, was dort lief,
ohnehin verloren".

Der zweite Satz war zu weit. Der Poller merkte sich den hoechsten Stand selbst
und nannte jeden niedrigeren einen Neustart. Nach 100 → 0 → 0 galt auch die
dritte Meldung als Neustart und gab jeden Kredit frei, der nicht mehr auf eine
laufende Antwort wartete — auch den eines Aufrufs, der erst nach dem Reset
entstanden war, das Timeout ueberschritten hatte und im neuen Prozess
rechnete. Zugleich blieben Basislinie und Auslieferungssumme ueber den Reset
stehen; das Ziel war danach unerreichbar, und der einzige Nachweis war genau
diese falsche Neustartmeldung. Jeder Abbruch startete ausserdem einen eigenen
Poller, und keiner endete.

Der Schluessel war der Modellname. Zwei Server derselben GPU mit einem Modell
gleichen Namens teilten sich Basislinie und Summe, und der Abgleich fragte den
ersten Server — auch fuer einen Aufruf an den zweiten. Die Statistik nahm den
ersten Versionseintrag, obwohl Triton je Version zaehlt.

## Entscheidung

**Abgeglichen wird je Identitaet: Server und Backendmodell.** Die Domaene
steckt im Actor (ADR-0037); die Version wird aufsummiert, weil die Summe nur
mit abgeschlossener Arbeit waechst, egal welche Version die Policy waehlt.

**Eine Identitaet hat Epochen.** Eine Epoche ist die Lebensdauer eines
Zaehlers: Basislinie beim Start, Auslieferungen, die das Backend erreicht
haben koennen, der hoechste gesehene Stand. Der Ruhe-Nachweis gilt je Epoche.
Ob der Zaehler fiel, entscheidet der Actor gegen den hoechsten Stand der
laufenden Epoche, nicht der Poller.

**Ein Neustart beendet nur Anspruechen, deren Verbindung schon tot ist.**
`Reconciling` und `AwaitingBaseline` — der Aufruf ist mit unbekanntem Ende
zurueckgekehrt — lagen im alten Prozess und enden. `Running` und `TimedOut`
haben eine offene Verbindung: sie koennen im neuen Prozess rechnen, und ihre
Antwort ist der Nachweis. Sie wandern in die neue Epoche und zaehlen dort
mit. Die neue Epoche beginnt beim gemeldeten Stand.

**Ein Poller je Identitaet**, solange ein Anspruch auf den Abgleich wartet.
Danach wird er beendet.

## Konsequenzen

- Der Neustart wird genau einmal verarbeitet; dieselbe Meldung danach ist
  keiner.
- Ein offener Aufruf endet nie durch den Neustart eines anderen. Das ist die
  Aussage, die vorher falsch war, und sie ist jetzt eine Invariante.
- Mit einem aggregierten Zaehler nicht belegbar, und deshalb benannt:
  - Ein Aufruf, der nach dem Reset in den neuen Prozess ging und dort
    **vor** dem Erkennen des Resets einen Transportfehler bekam, endet mit dem
    Neustart zu frueh. Das Fenster ist hoechstens ein Abfrageintervall
    (250 ms) nach dem Wiederanlauf.
  - Ein Aufruf, der ueber den Reset offen blieb und im neuen Prozess
    abschloss, **bevor** der Reset erkannt war, steckt schon im Startstand der
    neuen Epoche und zaehlt trotzdem mit. Das Ziel liegt dann zu hoch; ein
    Kredit bleibt gehalten, bis der Governor neu startet. Sicher, nicht
    lebendig.
  - Ueberholt der neue Zaehler den alten Hoechststand, bevor der Abgleich ihn
    liest, wird kein Neustart erkannt. Das Ziel der alten Epoche ist dann
    unerreichbar, und Kredite bleiben gehalten — sichtbar in
    `vig_quarantined_slots`. Ein Governor-Neustart bei erreichbarem Backend
    loest es.
- Alle drei Faelle halten im Zweifel fest oder liegen in einem Fenster von
  einem Abfrageintervall. Was der Governor nicht wissen kann, sagt er im
  Runbook.
- Die Annahme aus ADR-0032 bleibt: der Governor ist der einzige Aufrufer des
  Modells. Zaehlt ein fremder Client mit, ist der Nachweis ein Indiz.
