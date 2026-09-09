# 06 — Verifikation, Experimente und Pilotfreigabe

Status: Test- und Abnahmeplan. Die hier beschriebenen neuen Experimente sind
noch nicht ausgeführt. Vorhandene Nachweise stehen in Dokument 02.

## 1. Baselines: drei verschiedene Effekte getrennt prüfen

Compileroptimierung, Transportentfall und Scheduling werden nicht miteinander
vermischt. Für jeden Vergleich bleiben Eingangsdaten, fachlicher Vertrag,
Ausgabesemantik und Qualitätsgrenze gleich.

| Vergleich | Kontrollierter Unterschied | Aussage |
|---|---|---|
| ORT/Triton gegen TRT/Triton, beide ohne Vigilant | Runtime/Engine | Was verbessert die Modellausführung allein? |
| TRT/Triton ohne gegen mit Vigilant | Governor vor gleichem Server | Bleibt die Policy nützlich? |
| TRT Direct mit einfacher Latest-/Prioritätspolicy gegen Vigilant Direct | Policy bei gleichem Executor | Nutzen über eine kleine Eigenlösung hinaus |
| Vigilant/Triton/TRT gegen Vigilant Direct/TRT | Ausführungs-/Datenpfad | Rechtfertigt die direkte Integration ihren Aufwand? |
| Qualifizierter Kundenstack, ggf. Holoscan, gegen ergänztes Vigilant | realer Integrationsumfang | Kaufrelevanter Mehrwert statt künstlichem FIFO-Gegner |

TRT-Vergleiche verwenden dieselben Engines, soweit die Ausführungsbedingungen
identisch bleiben. Ein Layout-spezifischer Neubau ist ein separater Faktor.
Bei Runtimewechseln ist Qualitätsgleichheit zu prüfen, nicht bitweise
Identität zu behaupten. Alle Baselines erhalten ein vergleichbares Tuningbudget.

Triton-Baselines umfassen passende Instanzzahlen, begrenzte Queues,
Batching-/Timeout-Policies und die bereits erprobte gemeinsame Rate-Limiter-
Ressource. Eine konfigurierte Ressource und reine Prioritäten werden getrennt
benannt. Kein Rückgriff auf eine schwächere Einstellung, nur weil der Faktor
damit größer ist.

## 2. Messvertrag vor dem ersten Lauf

Je Verbraucher schriftlich festlegen:

- Capture- und Verbraucherzeitpunkt, Uhrendomäne und Unsicherheit;
- Deadline, Höchstalter, Periode/Phase/Jitter und Zustands-/Eventsemantik;
- Mindestqualität und erlaubte Varianten;
- erforderliche Consumer coverage, Lücken-/Burstgrenzen;
- Mindestfortschritt der Hintergrundarbeit, falls dieser Teil des Produktclaims ist;
- erlaubter Overhead, Speicherverbrauch und Energie-/Leistungsrahmen;
- Hardware-/Softwarezustände, in denen die Zusage gelten soll;
- Testdauer, Anlaufphase, Fehlversuche, Abbruchregeln und Auswertungsverfahren.

Diese Werte werden vom Betreiber vorgegeben, nicht aus dem besten Messlauf
abgeleitet. Ein Experiment zur erreichbaren Vertragsgrenze ist erlaubt,
wird aber als Boundary-Sweep geführt und nicht mit festen Verträgen vermischt.

## 3. Statistik und Reproduzierbarkeit

1. Pilotmessung zur Streuung; daraus Testumfang und benötigte Präzision bestimmen.
2. Für zentrale A/B-Aussagen als Startprotokoll zehn unabhängige Starts mit
   variierter Phase/Seed und alternierender Reihenfolge planen. Zehn ist kein
   universeller Nachweis für beliebig seltene Fehler.
3. Kalibrierung, Varianten-/Parameterwahl und abschließende Validierung strikt
   trennen. Ein nach einer Fehlmessung geänderter Parameter braucht frische Tests.
4. Per-Run-Rohdaten behalten. Aggregate, schlechtester Lauf und Verteilungen
   ausweisen; korrelierte Tokens/Frames nicht als unabhängige Trials behandeln.
5. Ein achtstündiger Soak prüft einen gewählten Releasezustand, ersetzt aber
   weder Start-/Phasenvariation noch Qualifikation anderer Plattformen.
6. Prognosegüte und tatsächliche Policywirkung getrennt berichten. Ein kleiner
   Modellfehler kann nahe einer Grenze eine große Vertragswirkung haben.

Keine nachträgliche Entfernung ungünstiger Runs ohne vorab definierte
Ausschlussbedingung. Verworfenes Material bleibt mit Grund referenziert.

## 4. Pflichtmetriken

| Ebene | Größen |
|---|---|
| Verbraucher | Consumer coverage, zeitgewichtete/Peak-AoI, `no_result`, längste Lücke, Burstlänge, M/K-Verstöße |
| Requests | angeboten/angenommen/gestartet/ersetzt/abgelehnt/abgebrochen, Deadlines und Antwortalter |
| Ausführung | bestätigte Enden, Unknown-Leases, Recovery-Dauer, In-Flight-Tiefe, tatsächliche Blockierzeit |
| Prognose | Fehler je gültiger Zustandszelle, Unterprognosen, unbekannte/abgelaufene Domänen, Rückfallhäufigkeit |
| Qualität | konkrete Task-Metrik, zulässige Varianten, Ergebnisverwendbarkeit, bei LLM semantischer Fortschritt |
| Kosten | CPU-/RAM-/GPU-Speicher, aktive Energie, Idle-/Reservierungskosten, Integrationsaufwand |

„Wasted GPU Compute“ braucht GPU-nahe Messung. Bei parallelen Aufträgen darf
die Summe ihrer Wall-Clock-Latenzen nicht als physische GPU-Zeit ausgegeben
werden. Copy-, Queue- und Rechenzeiten werden getrennt oder unter ausdrücklich
benannter Messgrenze berichtet. Zeichenmenge allein beweist keine gleichwertige
LLM-Aufgabenerfüllung.

Prozentpunkte, absolute Fehlerzahlen und relative Faktoren gemeinsam zeigen.
Ein großer Faktor aus sehr wenigen Restfehlern trägt keine breite Aussage
über beliebige Roboter oder Lasten.

## 5. Experimentmatrix

| ID | Versuch | Erwartete Unterscheidung |
|---|---|---|
| E01 | identisches Modell bei verschiedenen tatsächlich beobachteten Betriebszuständen | zustandsblindes gegen zustandsabhängiges Profil |
| E02 | periodische Releases gegen Back-to-back | Idle-/Warm-/Queuing-Effekte sichtbar |
| E03 | Profil unter falscher Runtime, Engine oder Partition | Profil ungültig, keine stille Wiederverwendung |
| E04 | Nebenlast gleicher Slotzahl, aber anderer Modellidentität | Occupancy-only gegen gerichtete Kontextdaten |
| E05 | Pairwise-Training, ungemessene Dreiwege-Last | keine unbewiesene additive Zulassung |
| E06 | kurze gegen lange Dekodierhorizonte | Fortschrittsmodell und tatsächliche Fortsetzungssemantik |
| E07 | gleiche Miss-Rate, isolierte gegen gebündelte Misses | Bursts/Versorgungslücken unterschiedlich bewertet |
| E08 | Consumer schweigt, erhält alte oder out-of-order Ergebnisse | Nenner, `no_result` und Gültigkeit bleiben ehrlich |
| E09 | Clock-Anforderung ohne Wirkung / verzögerte Wirkung | bestätigter Zustand statt Wunschzustand |
| E10 | kalter Shape-/Graphwechsel unter knapper Deadline | Übergangskosten statt Warmprofil |
| E11 | GPU-/Speicherbandbreitenlast außerhalb des Governors | Domäne verliert Qualifikation oder nutzt validierten Rückfall |
| E12 | gleiche I/O-Shape, andere Label-/Koordinatensemantik | unzulässige Variante abgewiesen |
| E13 | zwei aktive Regler mit gegensätzlichen Stellwünschen | Arbitration, Hysterese und Dwell verhindern Schwingen |
| E14 | stateful Verarbeitung mit Supersession/DAG-Cancel | keine fachlich unzulässige Löschung oder Reihenfolgeänderung |

Auf jeder Zielplattform erst Capabilities lesen und Messplan freigeben.
Keine Übernahme von Jetson-Debugfs-Pfaden oder Frequenzen auf andere Boards.
Aktuation nur auf freigegebenem Testgerät mit gültiger Wiederherstellungs-
strategie; Thermal- und Power-Schutz bleiben aktiv.

## 6. Fehler- und Lebenszyklustests

Mindestens folgende Fälle als Fake-Executor-Tests, wo möglich zusätzlich als
GPU-/Prozesstest des gewählten Backends:

- Request nachweislich nie gestartet: Credit sofort zurück.
- Request läuft; Client geht: keine vorzeitige Payload-/Slotfreigabe.
- RPC bricht ab, GPU läuft weiter über mehrere Timeouts.
- Erst Timeout, danach unbekannter Transportfehler.
- Verspätete doppelte Completion sowie Completion aus alter Worker-Generation.
- Shared Memory wird während Nutzung deregistriert oder neu zugeordnet.
- Readiness bei unbekannter Ausführung, fehlendem Modell und veralteter Telemetrie.
- SIGTERM während Queue, Ausführung, Quarantäne und noch offener gRPC-Verbindung.
- Absturz des Beobachters/Aktuators; Clock wird extern verändert.
- Nativer asynchroner Devicefehler; keine fälschlich gesunden Nachbarcontexts.
- Update/Restart zwischen Lease-Erteilung und Completion.
- Missmonitor und Trace unter hoher Last: begrenzter Speicher, keine
  blockierende Ausgabe im Entscheidungspfad.

## 7. Gates

### G0 — korrekte Basis

Bestehende Tests, Format und Lints erfolgreich; neue Lebenszyklus- und
Metrikrepros erfolgreich; kein offener kritischer Befund im gewählten Scope.
Für die im Ausbau neu eingeführten Entscheidungen existieren deterministische
Replaytests. B01 ist ausdrücklich kein durch den bisherigen Testlauf erledigtes Gate.

### G1 — Profil-/Zustandsintegrität

Falsche Identität, fehlender Readback und ungemessener Zustand werden erkannt.
Zulassung nutzt gültige Profile oder einen vorher qualifizierten Rückfall.
Messläufe sind wiederholbar und nicht durch stillen Providerwechsel entwertet.

### G2 — Policy lohnt sich

Auf dem vorab definierten Kundenlastsatz Verbesserung einer vereinbarten
Verbraucherzielgröße, ohne andere Mindestziele zu unterlaufen. Qualität und
Hintergrundfortschritt bleiben innerhalb des Vertrags. Alle Läufe und
Konfidenz-/Unsicherheitsangaben liegen offen vor.

Kein universeller „2x“-Schwellwert als Ersatz für den Kundennutzen. Als
Produktentscheidung muss die Verbesserung den Integrations- und Betriebsaufwand
rechtfertigen; diese Schwelle wird in NV-19 konkret vereinbart.

### G3 — Mechanismus rechtfertigt zusätzliche Komplexität

Für TensorRT Direct, Graphs, Aktuation, Green Contexts und XSched jeweils
separat: Funktionskorrektheit, relevante Verbesserung gegen den nächsteinfacheren
Pfad, hinreichend geringer Betriebsaufwand und erfolgreich getesteter Fallback.
Ein bestandener Standard-TensorRT-Test gibt keine XSched-/Green-Context-Freigabe.

### G4 — begrenzte Produktions-/Pilotfreigabe

G0–G2 und alle verwendeten G3-Prüfungen, qualifizierte Hardware-/Softwarematrix,
Dauerlauf auf dem freizugebenden Stand, Fault Injection, nachvollziehbare
Installation und Updates, Recovery-/Supportregeln und Securityprüfung im Scope.
Der Betreiber akzeptiert Grenzen, Fehlerreaktion und Integration.

Das Resultat ist eine Freigabe **für diesen Scope**, nicht pauschal „prod ready
auf NVIDIA“ und nicht funktional sicher. Eine erste Evaluierung kann vorher
auf Prüfstand oder aufgezeichneten Daten erfolgen, ohne sicherheitskritische
Aktuation vom Governor abhängig zu machen.

## 8. Pilot und Vertrieb beginnen vor dem Maximalausbau

Jetzt: kurze technische Gespräche mit mehreren potenziellen Entwicklungspartnern.
Keine Behauptung, ein öffentlich genannter NVIDIA-Stack beweise bereits einen
konkreten Bedarf oder die Bereitschaft zur Integration.

Angebot: einen realen Engpass auf der bestehenden Pipeline untersuchen,
Baseline reproduzieren, Mehrwert gemeinsam messen und nur die notwendige
Integration bauen. Ein bezahlter Engineering-Pilot braucht abgegrenzten
Aufwand und Abnahmekriterien; kein unbegrenztes Gratis-Customizing.

Erster Pilot bevorzugt:

- ein Gerät, feste Softwareversionen, wenige reale Modelle;
- Zugang zu Capture-/Consumerzeitpunkten und fachlichen Qualitätskriterien;
- Shadow-/Replay-/Prüfstandbetrieb vor produktiver Einwirkung;
- klarer technischer Eigentümer und vereinbarter Rückfall;
- standardmäßig keine Cloudpflicht, keine externe Telemetrie und keine
  Clock-Manipulation ohne gesonderte Freigabe.

## 9. Artefakte je Freigabe

```text
release/feature matrix
hardware + runtime + engine manifest
contract + data + profile revisions
raw traces + failure log + run order
analysis version + all run summaries
independent validation report
installation/update/rollback/recovery runbook
known limitations + operator acceptance
```

Die Struktur ist ein Artefaktentwurf, kein schon vorhandenes Releaseverzeichnis.
Jede erneute Bewertung muss auf diese Nachweise zeigen, nicht auf die Zahl
implementierter Features oder geschriebener Rust-Zeilen.
