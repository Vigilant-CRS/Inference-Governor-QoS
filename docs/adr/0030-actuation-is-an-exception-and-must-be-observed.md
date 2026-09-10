# ADR-0030: Aktuation ist eine Ausnahme, und sie muss beobachtet werden

**Status:** Akzeptiert · 2026-09-10
**Betrifft:** `platform/actuation`; Paket NV-13, ADR-0021
**Ausloeser:** ADR-0021 verbietet das Stellen. NV-13 verlangt es. Beides hat
recht, und der Unterschied liegt in den Bedingungen

## Kontext

ADR-0021 sagt: die Hardware wird gelesen, nie gestellt. Ein Governor, der Takte
verstellt, braucht Rechte, die ein Governor nicht haben sollte, und macht jede
Messung des Betreibers zu einer Messung des Governors.

Das bleibt richtig. Es laesst aber einen Fall aus, in dem Stellen den
Unterschied macht: eine Karte, die im Leerlauf heruntertaktet, braucht nach
einer Ruhephase Zeit, bis sie wieder liefert. Wer den Takt vorher anhebt,
gewinnt diese Zeit — auf Kosten von Energie und auf Kosten **anderer Arbeit
auf derselben Karte**.

## Entscheidung

**Die Aktuation ist ein eigenes, ausdruecklich einzuschaltendes Modul.** Ohne
`enable` ist der Regler ein teurer `NoOp`; die Voreinstellung lehnt jede
Anforderung mit `Disabled` ab. Der lesende Pfad aus NV-04 braucht weiterhin
kein Root und bleibt unberuehrt. Wer die Aktuation nicht einschaltet, hat einen
Governor ohne jede Stellbefugnis — und das ist der Normalfall.

**Beobachtet statt angenommen.** Eine Anforderung gilt erst als wirksam, wenn
ein Lesen sie bestaetigt. Ein `nvidia-smi -lgc`, das mit Code 0 zurueckkehrt,
hat nichts bewiesen: der Treiber kann die Vorgabe beschneiden, ein anderer
Prozess kann sie ueberschreiben, ein thermisches Limit sticht ohnehin. Ein
unbestaetigter Wunsch ist **kein Betriebspunkt** — `planning_clock` gibt dann
`None`, und geplant wird wie ohne Aktuation.

**Zugesagte Betriebsbereiche sind tabu.** Ein Profil wurde bei einem bestimmten
Takt gemessen; eine Zusage, die darauf beruht, darf nicht durch eine
Taktsenkung gebrochen werden. Wer senken will, gibt vorher die Zusage auf —
nicht umgekehrt. Der Boden kommt aus den Profilmanifesten (NV-03).

**Verweildauer, und ein Fehlversuch startet keine.** Ohne Verweildauer pendelt
der Regler, und Pendeln kostet mehr als der schlechtere Betriebspunkt. Aber ein
Versuch, der nicht bestaetigt wurde, hat nichts gestellt und darf den Regler
nicht fuer die volle Dauer sperren.

**Exklusivitaet ist beratend, und das steht dabei.** Eine Sperrdatei haelt
einen zweiten Vigilant-Prozess ab, nicht einen Betreiber mit `nvidia-smi` in
der Hand. Mehr ist von aussen nicht zu erreichen — der Treiber kennt keinen
Besitzer einer Taktvorgabe. Deshalb prueft der Regler **zusaetzlich** durch
Lesen: die Sperre verhindert das Versehen, die Beobachtung faengt den Rest.
Eine Sperre ohne lebenden Halter wird uebernommen; ein abgestuerzter Governor
soll die Karte nicht dauerhaft blockieren.

**Zuruecknehmen gehoert in den geordneten Ablauf.** `Drop` gibt nur die Sperre
zurueck, nicht die Taktvorgabe: ein `Drop`, der scheitert, schweigt. Und ein
fehlgeschlagenes `restore` laesst den Zustand als **unbestimmt** stehen, nicht
als zurueckgesetzt.

**Der Regler hat keine Uhr und liest nicht selbst.** Er bekommt die Zeit und
die Beobachtung gesagt — dasselbe Prinzip wie im Kern. `settle_time` sagt dem
Aufrufer, wie lange er warten soll; wer zu frueh liest, bekommt `NotObserved`,
und das ist richtig.

## Konsequenzen

Auf dieser Maschine meldet `nvidia-smi -lgc` *„The current user does not have
permission to change clocks"* — der Normalfall, und er ist als solcher
getestet, nicht nachgestellt.

Der Regler ist noch nicht an den Scheduler angeschlossen. Wann eine Anhebung
sich lohnt, ist eine Messfrage, und die braucht eine Installation, auf der das
Stellen ueberhaupt erlaubt ist. Die Struktur steht; die Politik nicht.

## Alternativen

**Auf Aktuation verzichten (ADR-0021 unveraendert lassen).** Waere die
einfachste Antwort und schliesst einen realen Gewinn aus. Die Eingrenzung —
opt-in, beobachtet, mit Boden — kostet weniger als der Verzicht.

**Dem Erfolgscode des Kommandos glauben.** Haette die Haelfte des Codes
gespart und dem Regler erlaubt, auf einem Betriebspunkt zu planen, den es nicht
gibt.

**Eine harte Sperre statt einer beratenden.** Gibt es nicht. Der Treiber bietet
keinen Besitzbegriff fuer Taktvorgaben; eine „harte" Sperre waere eine
Behauptung.
