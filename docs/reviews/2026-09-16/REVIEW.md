# Review und Reparaturen · 16. September 2026

## Ergebnis

Die neuen Ergebnisziele und Mindestlaufzeitbudgets sind sinnvolle Erweiterungen.
Die wichtigsten Fehler lagen in der Bedeutung der Messwerte, im Nachholen
vergangener Zyklen, in der Kapazitätsrechnung und in der Behandlung unklarer
Backendabschlüsse. Diese Prüfung hat konkrete Reparaturen und Regressionstests
geliefert. Ein bestandener Testlauf ersetzt die erneute Hardwarequalifizierung
der geänderten Mess- und Steuerungsfassung nicht.

Prüfbasis: Änderungen seit `90f517c`, insbesondere `60a11ee`, `f7afadd` und
`c38006a`, sowie der während der Prüfung entstandene Stand `6c689dc` und die
anschließenden Arbeitsbaumänderungen. Parallel bearbeitete Änderungen wurden
mitgeprüft und weiterverwendet. Schwerpunkte: Ergebnisziele, Laufzeitbudget,
Blockierungsprüfung, `vig-fit`, Autotune-Korrekturen, SHM und TFLite. Die neuen
Demo-/Videofunktionen wurden mitgebaut, aber nicht vollständig visuell oder mit
echten Modellnutzlasten qualifiziert.

## Befunde und Reparaturen

### R01 · P1 · Ergebniszahl war keine Verbraucherabdeckung

**Stellen:** [objective.rs](../../../crates/vig-core/src/objective.rs),
[scheduler.rs](../../../crates/vig-core/src/scheduler.rs).

Der neue Zähler entstand erst bei einer brauchbaren Lieferung. Ein Stream,
dessen erste Aufträge nie ein brauchbares Ergebnis erzeugen, bekam dadurch
keinen Zielrückstand. Außerdem wurden Lieferungen gezählt: ein einzelnes länger
frisches Ergebnis wurde zu schlecht bewertet, mehrere vor dem nächsten
Verbrauchertakt veraltete Ergebnisse konnten die Abdeckung künstlich erhöhen.

**Stand:** Die parallel entstandene Reparatur startet die Beobachtung bei der
ersten angemeldeten Arbeit und zählt versorgte Verbraucherzyklen. Zähler und
Nenner stammen aus denselben Fensterabschnitten. Eigene zusätzliche
Regressionstests bestätigen Kaltstart ohne Erfolg, wiederverwendetes frisches
Ergebnis, Bursts und kontinuierliche Versorgung über Fenstergrenzen.

### R02 · P1 · Eine zweite Lieferung heilte vergangene Ausfälle rückwirkend

**Stelle:** [scheduler.rs](../../../crates/vig-core/src/scheduler.rs),
`on_event`, `observe_supply` und Zielbeobachtung.

Auch nach der ersten Messkorrektur blieb `valid_since` auf der ersten
Fertigstellung stehen. Die nächste Fertigstellung ersetzte das Ergebnis, bevor
die inzwischen verstrichenen Takte bewertet wurden. Damit erschien eine neue
Aufnahme schon vor ihrer tatsächlichen Fertigstellung verfügbar.

**Gegenprobe:** Erstes Ergebnis: Aufnahme 0 ms, Fertigstellung 10 ms. Zweites:
Aufnahme 190 ms, Fertigstellung 250 ms. Periode und Höchstalter je 100 ms.
Versorgt ist nur Takt 100; Takt 200 liegt vor der zweiten Lieferung und Takt
300 nach deren Ablauf. Beobachtet wurden **666 statt 333 ‰**.

**Repariert:** Vergangene Takte werden vor dem Ereignis mit dem vorherigen
Ergebnis ausgewertet; der Takt genau am Ereigniszeitpunkt danach. Aufnahme und
Verfügbarkeit werden zusammen aktualisiert. Die aktuelle Zielbeobachtung steht
vor der nächsten Kandidatenwahl fest. Die Gegenprobe besteht nun.

### R03 · P1 · Lieferlücke und frische Abdeckung wurden verwechselt

**Stellen:** [objective.rs](../../../crates/vig-core/src/objective.rs),
`delivered`, `observe`; [scheduler.rs](../../../crates/vig-core/src/scheduler.rs).

Ein weiter frisches Bestandsresultat setzte bei jedem versorgten Takt die
Lieferlücke zurück. Umgekehrt erkannte ein reines Lückenziel ohne Periode und
Höchstalter überhaupt keine neue Lieferung.

**Gegenproben vor der Reparatur:**

- Eine Lieferung bei 10 ms, Abfrage bei 300 ms: Lücke **0 statt 290.000 µs**.
- Reines Lückenziel, neue Lieferung bei 310 ms: Lücke **310.000 statt 0 µs**.

**Repariert:** Verbraucherabdeckung und Lieferlücke haben getrennte Zähler.
Nur eine brauchbare Lieferung setzt die Lieferlücke zurück. Beide Fälle und
die bestehenden Ziel-/Variantenprüfungen bestehen.

### R04 · P1 · Verlorene SHM-Antwort gab einen möglicherweise belegten Namen frei

**Stellen:** [service.rs](../../../crates/vig-gateway/src/service.rs),
`settle_shm`; [shm.rs](../../../crates/vig-gateway/src/shm.rs), `Reservation`.

Das separate Backend-Task schützt bereits gegen den Abbruch des wartenden
Clients. Bei einem Transportfehler ließ es die Reservierung aber fallen. Der
Server kann die Registrierung schon ausgeführt haben, bevor seine Antwort
verloren geht. Ein fremder Aufrufer konnte anschließend dasselbe physische
Segment erneut reservieren.

**Gegenprobe:** `Unavailable` als verlorene Antwort, anschließend fremder
Registrierungsversuch auf denselben Schlüssel. Die erwartete Verweigerung
schlug vor der Reparatur fehl.

**Repariert:** Vor Versand markierte Reservierungen bleiben bei unklarem
Ausgang gehalten, auch beim Abbruch des ausführenden Tasks. Bestätigung bucht,
eine definitive Anfrageablehnung gibt frei. Tests prüfen sowohl verlorene
Antworten als auch die Freigabe bei `InvalidArgument`.

**Betriebliche Folge:** Unklare Registrierungen können Plätze belegen, bis der
Bestand kontrolliert abgeglichen wird. Eine automatische Wiederfreigabe ist
nicht implementiert; diese Grenze steht in [security.md](../../security.md).
Das gilt auch für eine unklare Abmeldung aller Regionen.

### R05 · P2 · Mindestlaufzeit und Ergebnisziel wurden doppelt berechnet

**Stellen:** [schema.rs](../../../crates/vig-config/src/schema.rs),
`additional_objective_utilization_permille`;
[doctor.rs](../../../crates/vig-cli/src/doctor.rs), `check_utilization`.

Ein Modell kann beide Anforderungen tragen. Dieselbe Ausführung trägt zur
Ergebnisversorgung bei und verbraucht Laufzeitbudget. Die bisherige Summe
zählte diese Arbeit zweimal und konnte deshalb eine Konfiguration fälschlich
als `NOT_READY` zurückweisen.

**Gegenprobe:** Rund 60 % geschützte Last, 35 % Laufzeitbudget eines weiteren
Modells und dessen Ergebnisziel mit 33 % geschätztem Bedarf. Erwartet wird
rund 95 % Gesamtbedarf; bisher wurden rund 128 % berechnet und abgelehnt.

**Repariert:** Pro Modell zählt zusätzlich nur der Zielbedarf oberhalb seines
eigenen Laufzeitbudgets. Fremde Budgets dürfen den Bedarf nicht ausgleichen.
Die bestehende Kapazitätsprüfung enthält nun die zuvor fehlgeschlagene
Kombinationsgegenprobe. Die Rechnung bleibt eine Kapazitätsabschätzung.

### R06 · P2 · Zyklusnachholung verursachte lange Schleifen und verschob die Phase

**Stelle:** [objective.rs](../../../crates/vig-core/src/objective.rs), `observe`.

Die erste Zykluskorrektur holte bis zu zwei Fenster Takt für Takt nach. Bei
60-s-Fenstern und 1-ms-Perioden sind das bis zu 120.000 Schleifenschritte je
Modell. Ein Sprung auf den verkürzten Horizont änderte außerdem die ursprüngliche
Taktphase. Sättigende Zeiterhöhung konnte an `u64::MAX` endlos weiterlaufen.

**Repariert:** Takte werden arithmetisch in höchstens 16 Fensterabschnitten
gezählt. Die Phase bleibt am ursprünglichen Beginn verankert. Tests vergleichen
dichte Beobachtung mit einem langen Sprung und prüfen Terminierung sowie
Idempotenz an der Zeitgrenze. Das ist ein Nachweis des begrenzten Arbeitsumfangs,
keine neue Messung der Scheduler-p99-Latenz.

### R07 · P1 bei mehreren Endpunkten · Direkter Vergleich lief auf dem falschen Gerät

**Stellen:** [vig-fit.rs](../../../crates/vig-bench/src/bin/vig-fit.rs),
[workload.rs](../../../crates/vig-bench/src/workload.rs), `drive_routed`.

Metadaten wurden am jeweiligen Modellendpunkt gelesen, direkte Inferenzen
anschließend jedoch sämtlich an `base.backend_endpoint` gesendet. Ein Vergleich
mehrerer Domänen oder modellbezogener Endpunkte konnte damit eine andere Last
ausführen oder mit Modellfehlern scheitern.

**Repariert:** Der direkte Arm verwendet die jeweilige Modelladresse. Alle
Verbindungen entstehen vor einem gemeinsamen Messbeginn; Verbindungsaufbau
verbraucht damit nicht mehr je nach Stream einen Teil des Messfensters.
Fehlende Verbindungen bleiben ungültige Messungen. Ein Test mit zwei
unabhängigen Mock-Backends prüft die tatsächlich an beiden ausgeführten
Inferenzen gegen die jeweiligen Lieferungsberichte.

## Grenzen, die für die Freigabe relevant bleiben

1. **SHM-Schlüssel sind noch nicht vorab einer Identität zugeteilt.** Die
   Aliasprüfung verhindert die spätere Übernahme bereits registrierter
   Segmente. Ein Angreifer kann weiterhin versuchen, einen fremden Schlüssel
   zuerst zu registrieren. Vor einer Nutzung mit gegenseitig nicht vertrauten
   Clients braucht es vom Betreiber zugewiesene Namensräume oder vergleichbare
   Zugriffskontrolle. Dieser bereits dokumentierte Rest ist durch R04 nicht
   gelöst.
2. **Die Blockierungsprüfung ist kein allgemeiner Machbarkeitsbeweis.** Sie
   zählt lange Modelle; ein Modellname begrenzt jedoch nicht automatisch seine
   gleichzeitig laufenden Requests. `SlotSet` erlaubt grundsätzlich mehrere
   Slots für dasselbe Modell. Eine belastbare Aussage braucht durchgesetzte
   Parallelitätsgrenzen, erlaubte Slotzuordnungen und reale Backendfähigkeiten.
3. **Backendneustart und unklare Ausführungen brauchen bessere Identität.**
   Die in ADR-0042 beschriebene fehlende Boot-/Ausführungsepochenkennung kann
   weiterhin Kapazität in Quarantäne halten. Ein Timeout ist kein Abschluss.
4. **Mindestlaufzeit ist noch kein garantierter Nutzen.** Gemessene
   Dispatch-bis-Antwort-Zeit kann Backendwarten enthalten. Eine Zuteilung in
   dieser Größe beweist weder reine GPU-Rechenzeit noch gültige VLM-Ergebnisse.
5. **Die Messwerkzeuge unterstützen noch nicht jede Modellnutzlast.**
   `vig-fit` baut weiterhin seine Standardeingabe aus dem ersten Input der
   Modellmetadaten. Das ersetzt keine qualifizierte Mehrinput-/BYTES-/VLM-
   Nutzlastdefinition. Erfolgreiche Syntax-/Hosttests sind keine Modellabnahme.

Diese Grenzen verlangen gezielte Erweiterungen und Qualifizierung. Die
gefundenen und oben als repariert bezeichneten Fehler sind davon getrennt
nachprüfbar.

## Nachprüfung des Reviews vom 15.09.

- Der Runtime-Start `triton-umzug.sh` bindet die drei Ports jetzt ausdrücklich
  an `127.0.0.1`. Laufende Container wurden in dieser Runde nicht neu angelegt
  oder extern auf Erreichbarkeit geprüft.
- Vorhandene neue Tests prüfen SHM-Aliase und parallele Platzreservierungen,
  ungültige `vig-fit`-Zellen, den Schutz je Stream, Endpoint-Konflikte und
  fehlende/veränderte Resume-Artefakte. Sie gehören zur ausgeführten Suite.
- Die TFLite-Korrekturen liegen in einem separaten Workspace; dessen Hosttests
  werden zusätzlich ausgeführt. Die Rust-Hauptsuite deckt diesen Workspace
  nicht automatisch mit ab.

## Verifikation

Die endgültigen Ergebnisse und Befehle stehen in [VALIDATION.md](VALIDATION.md).
Die Prüfungen verwenden die vorhandene Build-/Messsperre und den NVMe-Cache.
Es wurden keine neuen GPU-Lastläufe, keine Geräte-Neustarts und keine
Produktionsänderungen an laufenden Diensten ausgeführt.

## Was jetzt für das Produkt zählt

1. Den reparierten Stand einfrieren und dieselben Hardwarelasten erneut
   auswerten: je Stream, mit Lieferlücke und Verbraucherabdeckung getrennt,
   über Startphasen und thermischen Dauerzustand hinweg.
2. Ein vollständiges Kundenszenario qualifizieren: echte Kamera-/Ereignisdaten,
   echte Generierung, Mindestqualität und messbarer Fortschritt beider Seiten.
3. Backendfähigkeiten und Wiederanlauf verlässlich machen; danach die
   Parametersuche erweitern. Zusätzliche Regler auf falschen Messwerten wären
   derzeit die falsche Reihenfolge.

Der bestehende [Autotune-Entwurf](../2026-09-15/AUTOTUNE-DESIGN.md) beschreibt
die weitere Optimierung. Die hier geprüften neuen Funktionen ergänzen diesen
älteren Entwurfsstand; ihre Reparatur und Qualifizierung haben Vorrang.
