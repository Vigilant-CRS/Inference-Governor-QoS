# ADR-0042: Ein Ende wird belegt, nicht aus der Reihenfolge geschlossen

Datum: 2026-09-14 · Status: Akzeptiert · Bezug: Review 14.09. (R03, R04),
ADR-0040, ADR-0003

## Kontext

Zwei Stellen schlossen aus dem **Ablauf** auf ein Ende, statt es zu belegen.
Der externe Review vom 14.09. hat beide mit Gegenproben gezeigt.

**Der Messarm des Piloten.** `run_arm` wartete auf seine Kameraschleifen und
schlief danach 500 ms. Die Aufrufe, die diese Schleifen gestartet hatten,
liefen frei weiter. Ein Arm mit 100 ms Messdauer kehrte deshalb zurück,
während das Backend noch 2000 ms rechnete. Zusätzlich legte jeder Arm seinen
`RegionPool` neu an, über dieselben registrierten Shared-Memory-Regionen: Die
Quarantäne des vorigen Arms — „dieser Puffer wird nie wieder vergeben, sein
Leser könnte noch laufen" — war damit vergessen.

**Der Slotkredit im Actor.** Fällt der Abschlusszähler eines Backends, ist das
ein Neustart. Bisher endeten dabei alle Ansprüche, deren Aufruf schon
abgebrochen war: „abgebrochen heißt, er starb mit dem alten Prozess." Das
stimmt nur, wenn der Aufruf tatsächlich im alten Prozess lag. Startet das
Backend neu, **bevor** der Governor es merkt, geht der nächste Aufruf in den
neuen Prozess. Bricht dort seine Verbindung ab und meldet der Poller erst
danach den Abfall, wurde ein Kredit freigegeben, dessen Inferenz womöglich
weiterrechnet. ADR-0040 hatte diesen Restfall benannt; jetzt ist er belegt.

## Entscheidung

**Ein Ende zählt nur, wenn etwas es belegt.** Konkret:

1. **Die Sperre gehört der Region, nicht dem Pool.** Eine quarantänisierte
   Shared-Memory-Region bleibt für den ganzen Prozess gesperrt, über
   Armwechsel hinweg. Ein neuer Pool über dieselben Regionen bekommt sie nicht
   zurück.
2. **Ein Messarm endet erst, wenn keiner seiner Aufrufe mehr läuft.** Alle
   Aufrufe liegen in einem `JoinSet` und werden eingesammelt. Bleibt nach
   60 Sekunden einer offen, ist die Messzelle **ungültig** (`run_arm` gibt
   einen Fehler zurück) statt stillschweigend weiterzulaufen. Das pauschale
   Warten entfällt: Ein Timer war nie ein Nachweis.
3. **Ein Zählerabfall beendet nur Aufrufe, die das Backend in dieser Epoche
   nachweislich gesehen hat.** Ein Anspruch trägt dafür `seen_in_epoch`: Hat
   das Backend seit seinem Dispatch mindestens einmal in derselben Epoche
   gezählt, lief sie damals noch, und ein späterer Abfall belegt sein Ende.
   Fehlt dieser Beleg, wird der Anspruch **gesperrt** (`Quarantined`) statt
   beendet.
4. **Ein Nachweis gilt je Epoche.** Auch der reguläre Weg — „das Backend hat
   mindestens so viele Inferenzen abgeschlossen, wie es von uns bekam" —
   beendet nur Ansprüche derselben Epoche. Ein Ziel der neuen Epoche sagt
   nichts über einen Aufruf der alten.

## Konsequenzen

**Was besser wird.** Keine Messzelle beginnt mehr, während die vorige noch
rechnet. Kein Puffer wird wiederverwendet, dessen Leser unbekannt ist. Und
kein Slotkredit wird freigegeben, weil die Reihenfolge zweier Meldungen
zufällig günstig lag.

**Was es kostet.** Ein gesperrter Anspruch kommt nicht von selbst zurück: Sein
Kredit bleibt gehalten, sichtbar in `vig_quarantined_slots`, bis der Governor
neu startet. Das ist die ehrliche Seite der Regel — wo nichts ein Ende belegt,
bleibt ein Slot dauerhaft aus der Planung. Ein Backend, das während des
Betriebs wiederholt neu startet, verliert so nach und nach Kapazität. Der
Betrieb sieht das an der Kennzahl und im Log (`blocked`), und das Runbook
nennt den Weg zurück: Governor neu starten, wenn das Backend steht.

**Was offen bleibt.** Ein aggregierter Zähler kann grundsätzlich kein
Ausführungsende einer einzelnen Inferenz belegen. Erst eine Identität, die das
Backend selbst mitführt — Boot-Kennung, Modellversion, Ausführungsticket —
würde aus dem Beleg einen Beweis machen. Das verlangt eine Erweiterung auf
Backendseite und steht nicht in unserer Hand.
