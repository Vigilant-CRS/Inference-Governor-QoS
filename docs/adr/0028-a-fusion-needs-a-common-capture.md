# ADR-0028: Eine Zusammenfuehrung braucht eine gemeinsame Aufnahme

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** `core/dag`; Paket NV-17, ADR-0005, NV-00
**Ausloeser:** Frische allein verhindert nicht, dass zwei Ergebnisse aus
verschiedenen Aufnahmen zusammengerechnet werden

## Kontext

ADR-0005 hat die Erfolgsmetrik auf Frische gestellt: ein Ergebnis zaehlt,
wenn sein Alter unter dem Hoechstalter liegt. Das ist fuer einen einzelnen
Strom richtig und fuer eine Zusammenfuehrung nicht genug.

Ein Roboter kombiniert Detektionen aus einem Kamerabild mit einer Tiefenkarte
aus demselben Bild. Kommt die Tiefe langsamer als die Detektion, liegt
irgendwann eine frische Detektion neben einer aelteren Tiefenkarte. **Beide**
koennen unter ihrem Hoechstalter liegen. Zusammengerechnet ergeben sie eine
Szene, die es nie gegeben hat: Objekte an Stellen, an denen sie vor zwei
Bildern standen.

Was fehlt, ist die Aufnahme selbst als Begriff.

## Entscheidung

**Eine `CaptureId` je Sensoraufnahme, und eine Zusammenfuehrung verlangt, dass
alle Eltern dieselbe tragen.** Geprueft beim **Einfuegen** und nicht erst beim
Kombinieren: ein Graph, der einen unmoeglichen Knoten enthaelt, ist bereits
falsch, auch wenn ihn niemand auswertet.

**Wer ueber Aufnahmen hinweg rechnen will, sagt es.**
`insert_across_captures` — eine Bewegungsschaetzung braucht zwei Aufnahmen.
Ausdruecklich und benannt, damit es nicht die bequeme Umgehung der Regel wird.
Die Epochenpruefung bleibt auch dort: eine geaenderte Ausgabesemantik ist auch
ueber Aufnahmen hinweg ein Problem (NV-10).

**Ein Ergebnis bleibt gueltig, bis der letzte Verbraucher es freigibt.**
Referenzgezaehlt, nicht nach Frist. Zwei Verbraucher an einem Elternteil
heissen zwei Freigaben; die erste beendet nichts. Eine Freigabe ohne Halter
wird abgelehnt — ein Zaehler unter null gaebe ein Ergebnis frei, das noch
jemand haelt.

**Laufende Arbeit wird nicht als abgebrochen erfunden.** Faellt ein Elternteil
aus, werden nur die Kinder abgebrochen, die noch nicht gestartet sind. Ein
Kind, das schon rechnet, laeuft zu Ende — es als storniert zu buchen waere
eine Behauptung ueber die GPU, die niemand belegen kann. Das ist wortgleich
das Argument aus NV-00, und es gilt hier aus demselben Grund.

**„Abgebrochen" und „unzustaendig" sind verschiedene Zustaende.**
`Cancelled` ist nur aus `Pending` erreichbar. Ein fertig gerechnetes Ergebnis,
dessen Elternteil inzwischen ausfiel, wird `Superseded`: es existiert, es ist
nur nicht zu gebrauchen. Der Unterschied ist der zwischen „hat nie gerechnet"
und „hat gerechnet, umsonst" — und nur der zweite kostet Backendzeit, die in
`vig_stale_compute_seconds_total` gehoert.

**Knotenkennungen werden nicht wiederverwendet.** Sonst koennte ein spaeter
Abschluss einen frischen Knoten treffen — dasselbe Fencing-Argument wie bei
den Slotkrediten.

**Der Graph ist begrenzt.** 256 Knoten, acht Eltern je Knoten. Bei vier
Modellen und einer Aufnahme je 33 ms sind das rund zwei Sekunden Historie —
mehr, als eine Zusammenfuehrung sinnvoll ueberbrueckt (Spec L-003).

## Konsequenzen

Der Graph fuehrt **Metadaten, keine Tensoren**. Er weiss, welches Ergebnis zu
welcher Aufnahme gehoert und wer es noch braucht; er beruehrt keine Nutzlast
(ADR-0003) und fuehrt keine Berechnung aus.

Er ist noch nicht an das Gateway angeschlossen. Das braucht eine Zusage vom
Client — welche Anfrage zu welcher Aufnahme gehoert — und damit eine
Protokollerweiterung, die ohne einen benannten Pilotfall nicht sinnvoll zu
entwerfen ist (NV-19). Die Struktur steht; der Weg hinein ist eine
Produktentscheidung.

## Alternativen

**Aufnahmen ueber die Generationszeit erkennen.** Haette ohne
Protokollaenderung funktioniert und waere raten: zwei Kameras mit leicht
verschiedenen Zeitstempeln gehoeren zur selben Aufnahme, zwei Bilder derselben
Kamera im Abstand von 1 ms nicht.

**Nur beim Kombinieren pruefen.** Haette den Graphen zulassen, der die
unmoegliche Zusammenfuehrung schon enthaelt — und die Pruefung an jede
Auswertungsstelle verschoben.

**Laufende Kinder mit abbrechen.** Haette aufgeraeumter ausgesehen und einen
Zustand behauptet, den nur die GPU kennt.
