# ADR-0023: Die zustandsabhaengige Prognose laeuft erst im Schatten

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** `core/predictor`, `core/scheduler`, `core/variant`,
`gateway/actor`; Paket NV-06
**Ausloeser:** Der bisherige Schaetzer kann die Planung nur verschaerfen — und
verschenkt damit genau das Wissen, das NV-04 gerade sichtbar gemacht hat

## Kontext

`RuntimeEstimator` plant mit `max(offline_p99, online_p95)`. Das ist sicher
und einseitig: die Planung kann strenger werden, nie mutiger.

Seit NV-04 ist sichtbar, warum das Geld kostet. Die Zahlen dieses Projekts
sind auf einer Karte entstanden, die unter einem Leistungslimit lief — 1830
statt 2100 MHz. Laeuft dieselbe Karte spaeter ohne Limit, ist das Profil-p99
dauerhaft zu pessimistisch, und der Governor lehnt Arbeit ab, die gepasst
haette. Umgekehrt gilt ein unter vollem Takt gemessenes Profil nicht mehr,
sobald gedrosselt wird — und dort waere der alte Weg zu optimistisch, wenn er
nicht ohnehin nach oben korrigierte.

## Entscheidung

**Diskrete Zellen ueber Modell, Variante, Belegungsgrad und Zustandsklasse.**
Die Zustandsklasse ist grob: gedrosselt ja/nein, Takt voll/reduziert/unbekannt.
Zwei Stufen und nicht fuenf, weil jede zusaetzliche Stufe die Messwerte je
Zelle teilt. Ein Zustandsraum, der feiner ist als die Datenlage, erzeugt viele
Zellen, die alle nichts aussagen.

**Zellen werden nie ueber Zustandsgrenzen gemischt.** Jede Beobachtung traegt
die Epoche, unter der sie entstand: Profilidentitaet (NV-03) plus
Zustandsklasse (NV-04). Wechselt die Epoche, wird die Zelle **geleert**, nicht
fortgeschrieben — ein gleitendes Fenster wuerde die alten Werte sonst dutzende
Messungen lang mitschleppen, und ein Mittelwert ueber zwei Maschinen beschreibt
keine.

**Nichts wird interpoliert und nichts verteilt.** Zwischen Belegungsgrad 1 und
3 liegt kein Wert, sondern eine Wissensluecke. Keine Zelle heisst keine
Aussage, nicht eine geschaetzte.

**„Unbekannt" ist die Voreinstellung, nicht „am besten".** `ClockClass`
faellt auf `Unknown` zurueck, und unter `Unknown` gibt es gar keine Prognose.
Ein Governor ohne Hardwarebeobachtung bekommt damit den bisherigen Weg — nicht
den guenstigsten Fall.

**Lockern braucht mehr Beleg als Verschaerfen.** 48 Messwerte statt 16. Die
Asymmetrie ist dieselbe wie bei der Marge: der teurere Fehler wird strenger
behandelt.

**Erst Schatten, dann scharf, und nur auf ausdrueckliche Handlung.** Im
Schattenbetrieb entscheidet weiter der alte Weg; die Prognose wird gefuettert
und verglichen. Der Vergleich zaehlt getrennt, wie oft sie mutiger und wie oft
sie vorsichtiger war — denn eine Policy, die alles ablehnt, haelt jede Zusage
ein und ist trotzdem wertlos. Eine Policy, die sich selbst scharfschaltet,
sobald sie genug Daten hat, entzieht genau die Entscheidung, um die es geht.

**Ein Zustandswechsel zwischen Planung und Dispatch macht die Prognose
unzustaendig.** Sie traegt ihre Epoche mit; stimmt sie beim Dispatch nicht mehr,
gilt der bisherige Weg. Nicht falsch, sondern nicht mehr zustaendig.

**Die Tabelle hat die Form der Konfiguration, nicht die des Maximums.** Vier
Modelle mit je einer Variante auf einem Slot brauchen 24 Zellen. Die
Obergrenze steht als Konstante da, damit das Speicherbudget nachrechenbar ist
und nicht geschaetzt werden muss (Spec L-003).

## Konsequenzen

Der Vergleich laeuft auf der Schreibseite — bei jeder Fertigstellung — und
nicht im Entscheidungspfad. Gelesen wird bei jeder Planungsentscheidung; das
soll ein Tabellenzugriff bleiben (Spec 8.1).

Das Gateway liest den Hardwarezustand alle zwei Sekunden in einem eigenen Task
und meldet ihn dem Actor. Faellt der Collector aus, greift erst nach drei
Fehlversuchen der Rueckfall auf „unbekannt" — ein einzelner Timeout unter Last
darf die Betriebsart nicht umschalten (ADR-0021).

`vig_predictor_more_conservative_total` und
`vig_predictor_more_optimistic_total` sind die Zahlen, an denen sich die
Umstellung entscheidet. Ohne sie waere „v2 haelt alle Zusagen ein" eine Aussage
ueber nichts.

## Alternativen

**Den alten Schaetzer um eine Zustandsdimension erweitern.** Haette weniger
Code gebraucht und die Zellen im selben gleitenden Fenster vermischt — genau
der Fehler, um den es geht.

**Zwischen Zellen interpolieren.** Haette immer eine Zahl geliefert. Eine
erfundene Wahrscheinlichkeit ist schlimmer als eine eingestandene Luecke, weil
sie sich nicht ansieht wie eine.

**Automatisch scharfschalten, sobald genug Zellen belegt sind.** Bequem, und
es haette die Entscheidung, ob die neue Policy besser ist, dem System
ueberlassen, das sie selbst enthaelt.
