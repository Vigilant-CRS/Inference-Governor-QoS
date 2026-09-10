# Aktueller Code- und Funktionsreview

Datum: 2026-09-10. Ausgangspunkt: `dc19470e985ba7afaa1fc899b4d3248f73af58e2`
**plus der uncommittete Arbeitsstand**, insbesondere NV-16. Kein Review nur
des letzten Commits und keine pauschale Freigabe aller rund 38.000 Rust-Zeilen.

## Urteil

Vigilant ist deutlich weiter als beim Architekturreview vom 09.09. Der
Triton-Pfad ist ein ernstzunehmender Prototyp mit Messwerkzeugen, Fehlertests,
Transportabsicherung und Betriebsdokumentation. Ein Rewrite ist nicht sinnvoll.

**Eine unbeaufsichtigte Produktionsfreigabe empfehle ich trotzdem nicht.**
Es bestehen reproduzierbare Fehler im Ausführungsnachweis und in den
Verbrauchermetriken. Mehrere als fertig bezeichnete Erweiterungen existieren
als Bibliothekscode, ohne dass sie im ausgelieferten Dienst wirksam werden.
Zuerst diese Lücken schließen, nicht weitere Forschungsmechanismen hinzufügen.

## Prüfmethode und Grenzen

- Architekturbezogener Quelltextreview von Core, Actor, Gateway, Profilierung,
  Hardwaremodulen, neuer generativer Zerlegung, Benchmarkauswertung und Releasepfad.
- Bestehende Tests, fmt und Clippy ausgeführt; separate Gegenproben gebaut.
- TensorRT-A/B-Rohprotokoll und Anfang/Ende des neuen Soak-Laufs eingesehen.
  Kein neuer GPU-Benchmark, kein Jetson-Test und keine neue Achtstundenmessung.
- Der Arbeitsbaum wurde während des Reviews extern weiter geändert. Ein
  zwischenzeitlicher Compilerfehler in `actor.rs` war kurz darauf wieder weg.
  Er wird deshalb **nicht** als dauerhafter Produktbefund geführt.
- Für Gegenproben und abschließende Prüfungen wurde eine Arbeitskopie unter
  `/tmp/vig-review-20260910.F8O51m` eingefroren. Die Gegenproben ändern keinen
  Produktcode. Quellen und Anleitung liegen unter [repros](repros/README.md).
- Produktcode und Lizenz wurden durch diesen Review nicht verändert.
  Die neuen Dateien in diesem Verzeichnis sind Review-Ergebnisse.

## 1. Was wirklich gebaut wurde

| Bereich | Stand anhand der Aufrufpfade |
|---|---|
| Deterministischer Core, Queues, Supersession, Deadline-/Frischeprüfung, Look-ahead | Implementiert und im Gateway wirksam; bestehende Architektur erhalten |
| Ausführungsleases, Timeout, Clientabbruch, Abgleich | Im Produktpfad; wesentliche Verbesserung gegenüber Timerfreigabe, aber R01 bleibt |
| OIP/gRPC, System-SHM-Passthrough, Bearer-Token, TLS/mTLS | Implementiert; Betrieb setzt passende Vertrauens- und Netzwerkgrenzen voraus |
| SIGTERM und begrenztes Herunterfahren | Implementiert; Recovery-/Readiness-Details weiterhin prüfen |
| Profilmanifest, Artefakt-Digest, I/O- und Semantikvergleich, Variantenfreigabe | Implementiert; Teile der Hardware-/Gültigkeitsprüfung noch nicht angeschlossen |
| Verbrauchersicht im Simulator/Benchmark, Weakly-hard-Monitor im Core | Implementiert; unterschiedliche Implementierungen mit Fehlern R02/R03 |
| Hardwarebeobachtung | Tatsächlicher `nvidia-smi`-Collector und Replay; noch kein vollständiger Jetson-Zustand |
| Zustandsabhängige Prognose | Tabelle und Schattenvergleich laufen; aktive Betriebsart nicht im Dienst konfigurierbar |
| Periodischer Messpfad | In `vig profile` vorhanden; `vig calibrate` verwendet weiterhin seinen alten Messloop |
| TensorRT über Triton | Gemessen; kein eigener neuer Executor erforderlich |
| TensorRT Direct | Nicht gebaut |
| Gerichtete Interferenz | Kernmodul und gerichtete Kalibriermessung vorhanden; Tabelle nicht im Scheduling-Pfad |
| Weakly-hard-Policy | Scheduler-Code und Tests vorhanden; Einschaltmethode nur aus Tests aufgerufen |
| Anwendungshinweise | Annahme-/TTL-/Berechtigungsmodul und Abfragefunktion vorhanden; kein Eingang im Gateway, keine Anwendung auf Dispatch |
| Energieregler | Bibliotheksmodul und Stellglied vorhanden; kein Produktaufrufer, kein kompletter geschlossener Regelkreis |
| Gültigkeitsbewusster DAG | Kernmodul, kein Gateway-/Clientpfad |
| Generative Kontextkosten | Im uncommitteten Code angeschlossen; reale Prefill-Kalibrierung und Token-/Fortsetzungssemantik offen |
| Release, SBOM, Signierung, ARM-Build, Deployment/Runbook | Dateien und Workflows vorhanden; das ist kein Nachweis eines erfolgreich veröffentlichten/qualifizierten Releases |

## 2. Priorisierte Befunde

### R01 — P1: Aggregierter Abschlusszähler ist kein auftragsspezifischer Endnachweis

Stellen: [actor.rs — start_reconciliation](../../../crates/vig-gateway/src/actor.rs),
[client.rs — completion_evidence](../../../crates/vig-backend-triton/src/client.rs).

Der Abgleich setzt sein Ziel auf `baseline + lease.dispatched_total`. Die
Ordnungsnummer dieses Requests ist aber keine Grenze für später abschließende
Requests. Gegenprobe mit zwei Slots und einem einzigen Governor:

1. A startet; RPC bricht ab, Ausführung von A bleibt unbekannt.
2. B startet und wird fertig; Triton-Zähler steigt von 0 auf 1.
3. A hat Ziel 1: sein Kredit wird freigegeben, obwohl nur B fertig ist.

Reproduziert: `quarantined` fällt von 1 auf 0, `reconciled` steigt auf 1.
Die Voraussetzung „einziger Client“ reicht also **nicht** aus.

Gegenrichtung ebenfalls reproduziert: Ein Verbindungsfehler vor dem Absenden
erhöht den lokalen Dispatchzähler, aber nie den Backendzähler. Der nächste
wirklich ausgeführte Request bleibt nach seinem Ende bei ansonsten ruhendem
Backend in Quarantäne, weil sein Ziel um eins zu hoch ist.

Weitere Schwächen desselben Nachweises: Schlüssel nur Modellname statt
Endpunkt/Modell/Version/Epoche; Statistik wählt die erste passende Version;
Neustart wird aus einem Zählerrückgang erschlossen. Eine aggregierte Statistik
enthält keine Requestidentitäten — genau so ist sie auch bei NVIDIA beschrieben:
[Statistics Extension](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/protocol/extension_statistics.html).

**Änderung:** Nachweise mit expliziter Backend-/Modellepoche und passender
Ausführungsidentität. Falls nur Zähler verfügbar sind: Zulassung der ganzen
betroffenen Domäne anhalten, vorhandene Arbeit samt Nichtstarts korrekt
bilanzieren und ausschließlich unter nachgewiesenen Exklusivitäts-/Drainannahmen
abgleichen. Keine einzelne Lease anhand ihres historischen Ordinals freigeben.

### R02 — P1: Der Live-Monitor misst Alter seit Fertigstellung statt seit Aufnahme

Stelle: [scheduler.rs — observe_supply / observe_cycles](../../../crates/vig-core/src/scheduler.rs).

`last_valid = Some(now)` speichert die Fertigstellung. Der Verbrauchertakt
berechnet danach `at - last_valid`. Gegenprobe: Aufnahme 0 ms, Fertigstellung
50 ms, maximales Alter 66 ms, Abtastung 70 ms. Tatsächliches Alter: 70 ms;
gerechnet: 20 ms. Der erforderliche Miss fehlt im Monitor.

Zusätzlich wird `observe_cycles` nach Verarbeitung einer Completion aufgerufen:
beim Nachholen früherer Takte kann der gerade überschriebene letzte Stand
die damals verfügbare Information nicht mehr rekonstruieren. `phase_ms` wird
im Monitor nicht als feste Startphase umgesetzt. Ohne `max_age` bleibt ein
konfiguriertes Missbudget faktisch unbeobachtet.

**Änderung:** Capture- und Verfügbarkeitszeit getrennt halten; vergangenen
Verbrauchertakt vor dem Zustandswechsel abschließen; Anlauf, Phase,
Out-of-order-Ergebnisse und fehlende Altersschranke explizit behandeln.

### R03 — P1: Veraltete Lieferungen verkürzen die gemessene Versorgungslücke

Stelle: [coverage.rs — longest_gap](../../../crates/vig-sim/src/coverage.rs).

Gegenprobe: Messfenster 150 ms, Höchstalter 10 ms, Lieferungen `(50,0)` und
`(100,50)` als `(Fertigstellung,Aufnahme)`. Beide sind bereits veraltet;
es war **nie** ein brauchbares Ergebnis vorhanden. Gemeldet werden 90 statt
150 ms zusammenhängender Lücke, obwohl `consumer_covered` korrekt null ist.

Ursache: `usable_until` wird auch mit einer schon vergangenen Ablaufzeit
fortgeschrieben. Damit wird eine Zeit ohne Versorgung rückwirkend verkürzt.

**Änderung:** Vereinigung tatsächlich verfügbarer, noch gültiger Intervalle
berechnen; deren Komplement ergibt die Lücken. Differentialtests zwischen
Benchmarktracker und Core-Monitor mit derselben Ereignisfolge einführen.
AoI vor dem ersten Ergebnis außerdem als explizite Initialisierungsannahme
ausweisen, nicht mit einer tatsächlich vorhandenen Aufnahme verwechseln.

### R04 — P1: Das Bytebudget gehört weiterhin zur Client-Future

Stelle: [service.rs — model_infer](../../../crates/vig-gateway/src/service.rs).

Der Permit ist eine lokale Variable um `scheduler.submit(...).await`.
Clientabbruch oder Timeout gibt ihn zurück, unabhängig von Ausführungs-/
Payloadlebensdauer. Gegenprobe: Budget 16 Bytes, erster 16-Byte-Auftrag läuft
nach Timeout weiter; ein zweiter 16-Byte-Auftrag wird trotzdem ausgeführt.
Sein Fehler ist erneut DeadlineExceeded statt ResourceExhausted.

Der Test belegt die entkoppelte Aufnahmegrenze, nicht einen gemessenen
RSS-Anstieg des Triton-Datenpfads. Je nach Executor können noch gehaltene
Puffer, Kopien und kooperative Zustände die nominelle Grenze überschreiten.
Auch Protobuf-Decodierung geschieht bereits vor der Reservierung.

SHM ist getrennt offen: Registrierung wird mitgezählt, aber es fehlen
Auftragsleases/Ownerquoten und eine Sperre gegen Abmeldung benutzter Regionen.
CUDA-SHM durchläuft nicht dieselbe Registrierungsbuchführung.

**Änderung:** Payload-Lease mit tatsächlichem Besitzer und Budget über den
Actor/Executor transportieren; Aufnahme vor großer Allokation begrenzen;
SHM-Lebensdauer und Administratorrechte ausdrücklich abgrenzen.

### R05 — P1: Nicht erfüllbare Vertragsforderungen werden kommentarlos akzeptiert

Stellen: [contract_ext.rs — validate](../../../crates/vig-core/src/contract_ext.rs),
[schema.rs — Vertragszusatz](../../../crates/vig-config/src/schema.rs).

Die Gegenprobe mit `evidence_required: proven` wird erfolgreich aufgelöst.
Es gibt keinen Produktionsnachweis dieser Stufe und keinen Betriebsabgleich,
der die Anforderung zurückweist. Ähnlich sind `delivery_boundary: consumer`,
Mindesthintergrundfortschritt und Gültigkeitsfelder speicherbar, ohne dass die
benötigte End-to-End-Beobachtung beziehungsweise Durchsetzung vorhanden ist.

**Änderung:** Syntaxannahme und Betriebsannahme trennen. Ein Konfigurationsformat
darf zukünftige Anforderungen darstellen; `serve` muss nicht unterstützte
Forderungen aber ablehnen oder ausdrücklich als nicht angenommen kennzeichnen.
Keine grüne Vertragsdarstellung allein wegen eines gültigen YAML-Dokuments.

### R06 — P1 vor LLM-Pilot: Zeichenheuristik ist keine Tokenobergrenze

Stelle: [cooperative.rs — from_request / absorb / build_quantum](../../../crates/vig-gateway/src/cooperative.rs).

Prompt- und Ausgabetoken werden als `ceil(UTF8-Bytes/4)` geschätzt. Das ist
keine obere Schranke. Synthetisches Gegenbeispiel: vier Ein-Byte-Token ergeben
vier Bytes; gezählt wird eins, drei weitere Token werden freigegeben. Der
Test ist **kein** Tokenizer-Benchmark für Qwen, sondern ein Gegenbeispiel gegen
die behauptete allgemeine Budgetgarantie ohne Tokenizer-/Backendvertrag.

Weitere Grenzen: EOS/Stop wird nicht zuverlässig übertragen; erneute
Text-Prompts reproduzieren nicht automatisch Token-IDs, Samplingzustand und
Penalty-/Stopzustand einer ununterbrochenen Generierung. Die Textmanipulation
von Sampling-JSON ist kein vollständiger Parser. Teilquanten werden im Core
als gültige Completions gezählt, bevor der wartende Client ein Gesamtergebnis
erhalten hat — Fortschritt und Verbraucherlieferung sind verschiedene Dinge.

**Änderung:** Expliziter generativer Backendvertrag mit Token-IDs/-Zählern,
Finish-Reason, definiertem Fortsetzungsmodus und strukturiertem JSON.
Ohne diese Fähigkeiten als experimentellen Text-Continuation-Modus anbieten,
nicht als transparente, semantisch identische Präemption.

### R07 — P1 vor Aktivierung: Hardwarezustand deckt den geplanten Predictor nicht ab

Stellen: [gpu.rs](../../../crates/vig-platform/src/gpu.rs),
[predictor.rs](../../../crates/vig-core/src/predictor.rs),
[scheduler.rs — observe_for_predictor](../../../crates/vig-core/src/scheduler.rs),
[verify.rs — observed_manifest](../../../crates/vig-cli/src/verify.rs).

- Kein Speichertakt/EMC im gemessenen `GpuState`, kein Jetson-Collector.
- Predictor unterscheidet Belegung, zwei Drossel- und drei SM-Taktklassen;
  600 und 1500 MHz können dieselbe reduzierte Klasse sein.
- Beobachtungen werden mit dem Zustand **bei Completion**, nicht der beim
  Dispatch geltenden Epoche gebucht. Zustandswechsel während der Ausführung
  können eine falsche Zelle füllen.
- Hardwareprobe betrachtet lokal GPU 0, nicht eine qualifizierte Zuordnung
  zum tatsächlichen Backendgerät. Auch die Momentaufnahme verliert beim
  Übergang zum Core ihr Alter.
- Collector-Unterprozess hat keine Laufzeitbegrenzung. Ein hängender Aufruf
  produziert keine drei Fehlversuche; der letzte Zustand bleibt stehen.
- Das Startmanifest vergleicht weiterhin keine beobachtete Geräte-/Treiber-
  identität, obwohl diese Informationen mittlerweile teilweise messbar sind.

**Änderung:** Zustand und Freshness je Ausführungsdomäne; beim Dispatch
Epoche speichern; Übergangsmessungen getrennt behandeln; EMC/Powerzustand
und Manifestprüfung verbinden. Bis dahin Schattenbetrieb beibehalten.

### R08 — P1 für belastbare Kalibrierung: Der verbesserte Messpfad ist nicht überall verwendet

Stellen: [calibrate.rs — measure](../../../crates/vig-cli/src/calibrate.rs),
[profile.rs — Messloop](../../../crates/vig-cli/src/profile.rs).

`vig profile` nutzt den neuen periodischen Messpfad. `vig calibrate` misst
weiter back-to-back und überspringt fehlgeschlagene Aufrufe. Gerade der
Befehl, der Profile automatisch in Konfigurationen schreibt, kann damit ein
anderes und systematisch günstigeres Messbild erzeugen. Auch Paarinterferenz
wird für die Regelableitung über Medianverlangsamung beurteilt.

**Änderung:** Einen gemeinsamen Messkern für beide Befehle; Erfolge, Fehler,
Timeouts, ausgelassene Releases und Hardwarezustände im selben Runmanifest.
Profilaktivierung erst nach Mindeststichprobe und Qualifikation. Die alte
pauschale 2x-Durchsatzbehauptung wurde inzwischen korrekt als Heuristik begrenzt.

### R09 — P2: Featurestatus verwechselt Modul, Integration und Freigabe

Stellen: [STATUS.md](../../STATUS.md), [support-matrix.md](../../support-matrix.md),
[scheduler.rs](../../../crates/vig-core/src/scheduler.rs),
[actuation.rs](../../../crates/vig-platform/src/actuation.rs).

`set_miss_aware_policy`, `set_hint_policy` und `offer_hint` werden außerhalb
der Tests nicht aufgerufen; `effective_max_age` beeinflusst nicht die
Schedulingentscheidung. Für `Actuation` existiert kein Produktaufrufer.
Ein Betreiber kann diese Funktionen also nicht durch einen dokumentierten
Konfigurationsschritt aktivieren. Das ist mehr als „Voreinstellung aus“.

**Änderung:** Vier getrennte Zustände führen: Modul gebaut, integriert,
verfügbar/aktivierbar, für benannten Stack qualifiziert. Pflicht-Gatewaytest:
Konfiguration bzw. autorisierte Nachricht muss eine echte Entscheidung ändern.

### R10 — P2 heute, P1 vor Aktuation: Die beobachtete Vorgabe kann Zusagen unterschreiten

Stelle: [actuation.rs — request / acquire_at](../../../crates/vig-platform/src/actuation.rs).

Reproduziert: zugesagter Mindesttakt 1500 MHz, angefordert 1500, beobachtet
1470, Toleranz 50. Rückgabe ist Erfolg, und `planning_clock()` darf den
unzulässigen Betriebspunkt melden. Geprüft wird die Anforderung, nicht der
tatsächliche Wert gegen den zugesagten Boden.

Weitere Quelltextbefunde: keine Freshness-/Nach-Anforderung-Prüfung des
Snapshots; Besitzdatei über read/check/truncate statt atomarem Lock;
Bestätigung wird ohne fortlaufende Revalidierung gehalten. Noch kein
Produktionsregler, der diese Zustände bei Treiber-/Thermikänderungen nachführt.

**Änderung:** Erst real beobachteten Zustand gegen alle Grenzen prüfen,
atomarer Besitz pro Gerät, explizite Requested/Observed/Expired-Zustände,
begrenzte Befehle und qualifizierter Restorepfad. Nicht vorzeitig anschließen.

### R11 — P1 für Orchestratorbetrieb: Readiness ist ein globaler letzter Fehler

Stellen: [exporter.rs — readiness](../../../crates/vig-gateway/src/exporter.rs),
[actor.rs — on_backend_done](../../../crates/vig-gateway/src/actor.rs).

Ein Transportfehler setzt Readiness rot; erst eine weitere erfolgreiche
Inferenz setzt den globalen Zähler zurück. Nimmt ein Loadbalancer daraufhin
allen Verkehr weg, fehlt der Auslöser zur Erholung. Umgekehrt setzt ein
Erfolg an Backend B den Ausfall von Backend A zurück. Vor der ersten Inferenz
ist der Zähler ebenfalls null, ohne geprüfte Lieferfähigkeit.

**Änderung:** Periodische, begrenzte Readiness-/Modellprobes je erforderlichem
Backend; kontrollierter Half-open-Zustand und Modell-/Domänenbereitschaft.
Liveness bleibt davon getrennt. Statistikabfragen und Metadaten-RPCs benötigen
eigene Antwortfristen; der Connect-Timeout begrenzt keine hängende RPC-Antwort.

## 3. Funktions- und Messbewertung

Positiv: Das TensorRT-Experiment ist tatsächlich vorhanden. Im eingesehenen
[Rohprotokoll](../../../../InferenceQoS-runtime/gate-m3-trt-run/gate-m3-trt.txt)
stehen für den Detektor 90 % gegen 99 % Lieferfensterabdeckung und 13,3x
weniger unabgedeckte Lieferfenster. Das ist **kein** 13,3x schnelleres Modell.

Dasselbe Protokoll zeigt außerdem:

| Kennzahl | Triton | Vigilant |
|---|---:|---:|
| Detektor, Verbraucherabdeckung | 97 % | 100 % |
| Detektor, mittlere AoI | 34 ms | 49 ms |
| Pose, mittlere AoI | 25 ms | 49 ms |
| Hintergrundstrom, Abdeckung | 100 % | 0 % |

Vigilant verbessert hier eine Versorgungskennzahl, nicht alle Frischegrößen
und nicht die gemeinsame Versorgung beider Klassen. Diese Rohzahlen wurden
eingesehen, **nicht neu gemessen**; R03 verlangt eine erneute Lückenauswertung.
Der Live-Monitorfehler R02 macht nicht automatisch die separat berechnete
Benchmark-Coverage falsch.

Gate-M3-Namen sind Lastrollen: `pose`/`depth` nutzen in diesem Aufbau ResNet-
Surrogate und `vlm` einen großen ResNet-Batch. Das belegt Scheduling unter
bestimmten Lasten, keine fachliche Pose-/Depth-/VLM-Qualität. WP26 ist davon
zu trennen: dort existieren historische Läufe mit echtem Qwen/vLLM; der
aktuelle Kontextkostenpfad wurde damit noch nicht neu qualifiziert.

Die Vergleichstabelle wählt je Strom das beste Ergebnis aus Puffertiefen 1
und 8 — bei **beiden** Verfahren. Das ist symmetrisch, aber kann ein
zusammengesetztes Ergebnis sein, das keine einzelne Gesamtkonfiguration
gleichzeitig erreicht. Alle Läufe und gemeinsame Betriebspunkte ausweisen;
Auswahl und Bewertung nicht auf derselben Stichprobe durchführen.

Der neue Achtstundenlauf hat tatsächlich 480 Fenster und einen Abschluss
im Rohprotokoll. Er stützt begrenzte Stabilität auf einer Maschine. Er
qualifiziert nicht nachträglich die danach hinzugefügten Funktionen. Die
verlinkte `soak.md` enthält noch den älteren September-02-Bericht; die
Supportmatrix nennt TensorRT über Triton noch ungemessen. Dokumente müssen
auf konkrete Commits, Konfigurationen, Hardwarezustände und Rohdaten zeigen.

## 4. Was gut ist und bleiben sollte

- Abhängigkeitsfreier, deterministischer Core und explizite Zeit statt
  impliziter Uhrzugriffe. Sehr gute Grundlage für Replays und Gegenbeispiele.
- Getrennte Payloads, begrenzte Queues, klare Terminalzustände.
- Fehlermodelle und Fake-Executor machen gefährliche Backendzustände prüfbar.
- I/O-Signatur, fachliche Semantik und Variantenfreigabe werden unterschieden.
- Anforderungen werden nicht aus Messwerten passend gemacht.
- Echte Vergleichsläufe und dokumentierte negative Ergebnisse statt nur
  Simulation oder maximale FPS. Release-/Betriebsarbeit hat sichtbar begonnen.

Schwach ist momentan vor allem die **Konsistenz zwischen Schichten**:
Lease vs. Backendstatistik, Completion vs. Lieferung, Capture vs. Empfang,
Manifest vs. Laufzeitbeobachtung, Konfigurationsforderung vs. Fähigkeit.
Noch mehr isolierte Unit-Tests ersetzen diese Integrationsinvarianten nicht.

## 5. Empfohlene nächste Schritte

1. Einen Review-/Releasekandidaten einfrieren. Keine wechselnde Kombination
   aus Arbeitsbaum, altem Binary, neuen Profilen und historischen Messungen.
2. R01 beheben und beide Gegenrichtungen als Regression aufnehmen.
3. R02/R03 zusammen lösen: eine gemeinsame Verbrauchersemantik und
   Differentialtests; betroffene Metriken neu auswerten.
4. R04/R05/R11 für einen klar abgegrenzten Vertrauens-/Betriebsbereich schließen.
   Inferenzidentitäten dürfen nicht automatisch Modellverwaltung oder fremde
   Shared-Memory-Regionen verwalten; entsprechender Proxy oder Rollenmodell.
5. `profile` und `calibrate` vereinheitlichen. Hardwareidentität, EMC und
   Telemetriefristen anschließen, bevor Predictor v2 Entscheidungen übernimmt.
6. Für den ersten Pilotfall genau einen neuen Funktionspfad fertigstellen:
   entweder zuverlässige klassische Wahrnehmung auf dem Kundenstack oder
   echte generative Kooperation inklusive Qualitäts-/Tokenvertrag.
7. Zweite reale Hardware, feste Vertragsmetriken, einfache Latest/Priority-
   Baseline, stärkste passende Triton-Konfiguration und Kundenstack vergleichen.
   Sowohl Wahrnehmung als auch vereinbarter Hintergrundfortschritt müssen bestehen.

**Kundengespräche jetzt, allgemeine Produktionszusage noch nicht.** Ein
betreuter Entwicklungspilot nach den Korrekturen ist sinnvoll. TensorRT Direct,
Green Contexts, XSched und DAG-Integration sind keine Voraussetzung dafür.

## 6. Reproduktionsstand

Die eingefrorene Kopie besteht **581 bestehende Tests**, `cargo fmt --all --
--check` und `cargo clippy --workspace --all-targets --locked -- -D warnings`.
Der zunächst eingeschränkte Testlauf konnte lokale Testports nicht öffnen;
die vollständige Wiederholung mit zugelassenen Loopback-Ports bestand.
Die im älteren STATUS-Dokument genannten 478 Tests sind nicht mehr aktuell.

Acht zusätzliche Gegenproben wurden in der eingefrorenen Arbeitskopie
ausgeführt. Alle acht verletzen ihre fachlich erwartete Assertion: zwei für
R01, je eine für R02, R03, R04, R05, R06 und R10. Das sind gezielte
Gegenbeispiele, **nicht acht zufällig rot gewordene Bestandstests**.

R06 verwendet ein synthetisches Tokenmodell; R04 prüft Aufnahme und
Ausführung nach dem Client-Timeout, keine RSS-Messung. R07–R09/R11 sind
Quelltext-/Aufrufpfadbefunde, keine neuen Hardwareexperimente.

Snapshot-Prüfsummen:

```text
actor.rs       7b3d8c16cb0f86a3442142aaf33532ee6eff7444d5bca0514d4a178fdaa0cbd7
scheduler.rs   705ade1123db815b82914c3af1d7c852c4c61d565c7bc2a813503682c10ea6f9
predictor.rs   7c445d2819bb2992c1b075908b742a9d579c45178b8f2fa35952453df4fc4634
service.rs     2cdf25183417ac87b768791a441a5997d9a0a8a26e3e72d5d9c961f70de11bf6
coverage.rs    a8b02a2336197f65f461c9d4b6ce18f2d48670f70d0631d75783b4ec010ed9aa
cooperative.rs c47dfaf60f32fbd110681699ffd4df0ab3eb9c96e1eddf6952d0f0b3b3872dab
```

Die Codeverweise oben zeigen in den weiter veränderlichen Arbeitsbaum;
Funktionsnamen und Prüfsummen identifizieren den tatsächlich geprüften Stand.
