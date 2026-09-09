# ADR-0021: Die Hardware wird gelesen, nie gestellt

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** neues Crate `vig-platform`, `cli/doctor`, `cli/profile`; Paket NV-04
**Ausloeser:** Ein Profil, das unter einem Leistungslimit entstand, beschreibt
nicht die Karte, sondern die Karte unter diesem Limit — und nichts im System
konnte das sehen

## Kontext

NV-03 hat das Profilmanifest eingefuehrt: Geraetename, Treiber, Compute
Capability, Speicher. Der Betreiber musste sie als Flags eintippen. Das ist
richtig, solange niemand sie beobachten kann — aber `nvidia-smi` liegt auf
jedem System mit Treiber, laeuft ohne Root und weiss all das.

Wichtiger noch ist, was es ausserdem weiss: **warum** die Karte gerade
langsamer laeuft. Auf dem Messrechner dieses Projekts sind das 1830 statt 2100
MHz, Grund `SwPowerCap`. Die Gate-M3-Zahlen sind unter genau diesem Zustand
entstanden. Das ist nicht falsch — beide Vergleichsseiten liefen darunter —
aber es stand nirgends.

## Entscheidung

**Ein eigenes, optionales Crate, das ausschliesslich liest.** Kein
Taktsetzen, kein Persistence-Mode, kein Power-Limit, kein `nvpmodel`. Ein
Governor, der die Hardware verstellt, braucht Rechte, die ein Governor nicht
haben sollte — und macht jede Messung des Betreibers zu einer Messung des
Governors. Kein Root fuer irgendetwas davon.

**`nvidia-smi` statt NVML.** NVML waere direkter und braeuchte eine Bindung an
eine Herstellerbibliothek, die zur Treiberversion passen muss. Ein
Prozessstart je Messung ist bei Sekundentakt kein Argument; die Kopplung
waere eines.

**Drei Zustaende, die nicht dasselbe sind.** `Observed`, `Unsupported`,
`Unavailable` — plus quer dazu die Frische. Der haeufigste Telemetriefehler
ist, „geht hier nicht", „weiss ich gerade nicht" und „ist alt" in einen
Nullwert zu falten. Ein Laptop-Ampere meldet kein `power.limit`; das ist kein
Ausfall und schon gar nicht null Watt.

**Vollstaendige Momentaufnahmen mit Zeitstempel, kein Dauerstrom.** Ein
Zustand, der zwischen zwei Messungen wechselt, ist nicht beobachtet, sondern
erschlossen. Erst der Vergleich zweier Aufnahmen ergibt eine Aenderung — und
die traegt dann beide Zeitpunkte, nicht einen. Das ist der Unterschied
zwischen „der Takt fiel irgendwann" und „der Takt fiel zwischen 12:03:11 und
12:03:12", und nur die zweite Aussage beantwortet die Frage, ob die Hardware
vor der ersten langsamen Fertigstellung schon anders war.

**Der Vergleich ist grob, wo die Groesse rauscht.** Temperatur und
Leistungsaufnahme werden gar nicht verglichen, der SM-Takt nur in Stufen von
100 MHz. Ein Aenderungsstrom, in dem jede Taktschwankung steht, versteckt die
eine Nachricht, um die es geht.

**Der Rueckfall ist Teil des Entwurfs.** Nach drei aufeinanderfolgenden
Fehlversuchen gilt `Fallback::QualifiedProfileOnly`: keine staerkere Zulassung
als das fest qualifizierte Betriebsprofil. Der Governor laeuft weiter, er
laeuft nur nicht mutiger, als er es belegen kann. Drei und nicht eins, weil
`nvidia-smi` unter Last einmal in einen Timeout laufen kann, ohne dass sich an
der Hardware etwas geaendert haette — die Betriebsart des Governors darf nicht
an der Laune eines Unterprozesses haengen.

**Beobachtung belegt vor, sie ueberschreibt nicht.** `vig profile` und `vig
calibrate` fuellen Geraetefelder aus der Beobachtung, die der Betreiber
**nicht** angegeben hat. Eine ausdrueckliche Angabe bleibt stehen: die
Beobachtung weiss nicht, welche Karte gemeint war, wenn mehrere im Rechner
stecken. Was ergaenzt wurde, wird genannt, statt es spaeter in der Datei zu
entdecken. `--no-hardware-probe` schaltet es ab.

## Konsequenzen

`vig doctor` meldet jetzt vor jeder Messung, ob die Karte gedrosselt ist und
warum. Das ist der eigentliche Gewinn: der Betreiber erfaehrt **vor** dem
Messen, was seine Zahlen bedeuten werden.

Ein aufgezeichneter Verlauf laesst sich abspielen (`Recorded`). Damit sind
Zustandswechsel testbar, die sich auf einem Messrechner nicht bestellen lassen
— ein thermisches Limit tritt ein, wenn es eintritt, und nicht, wenn ein Test
es braucht. Ein Replay markiert seine Werte als solche und gibt sich nicht als
Messung aus.

Jetson ist damit noch nicht bedient. `nvpmodel` und `tegrastats` sind eigene
Quellen; das Feld `power_mode` existiert und steht auf `Unsupported`, statt
einen Standardwert zu erfinden.

## Alternativen

**Die Werte im Governor-Prozess ueber NVML lesen.** Schneller, und die
Bibliotheksbindung haette den Governor an Treiberversionen gekoppelt — bei
einem Produkt, das auf fremder Hardware laufen soll, der teuerste denkbare
Tausch fuer ein paar Millisekunden.

**Bei Collector-Ausfall den letzten bekannten Zustand weiterbenutzen.** Waere
bequem und waere eine Planung auf einer Beobachtung, die es nicht mehr gibt.

**Auch stellen duerfen, etwa Persistence-Mode aktivieren.** Haette messbar
geholfen und den Governor zu einem Werkzeug gemacht, das die Bedingungen
veraendert, unter denen es gemessen wird.
