# Produktionsreife und Pilotfreigabe – 8. September 2026

Geprüfter Abschlussstand: **034c3a5b856fb50449f72313c315132e5733ba7e**. Während der Prüfung kamen weitere Änderungen und Commits hinzu; die Abschlussbewertung berücksichtigt TLS/Auth, deklarierte I/O-Signaturen und den Release-Workflow. Eine unveränderliche Quellkopie dieses Commits wurde für die Prüfung archiviert. Produktcode, Konfigurationen und Git-Historie wurden durch diesen Review nicht geändert.

## Entscheidung

**Kundengespräch und betreute Demonstration: ja. Eigenständig verwendbare Produktionssoftware: nein.**

Für eine Evaluation auf aufgezeichneten Daten oder einem getrennten Testsystem ist genügend technische Substanz vorhanden. Für eine Pilotinstallation in einer laufenden Roboterpipeline fehlen noch klar umrissene technische Freigaben. „Der Rest sind Vertriebs-, keine Technikaufgaben“ ist mit dem aktuellen Code nicht vereinbar.

| Stufe | Aktuelle Empfehlung |
|---|---|
| Technisches Erstgespräch mit NEURA oder einem Integrator | Jetzt beginnen; experimentellen Stand und Grenzen offen nennen. |
| Betreute Demo auf bekanntem Aufbau | Möglich, mit bekannten Modellen und eigener Aufsicht. |
| Gemeinsame Evaluation auf Kundenhardware | Als Entwicklungs-/Qualifikationsprojekt vereinbaren, zunächst getrennt vom produktiven Steuerungspfad. |
| Pilotinstallation, die reguläre Roboterarbeit versorgt | Noch nicht freigeben; zuerst die unten genannten Lebenszyklus- und Aufnahmefehler schließen. |
| Unbetreuter OEM-/Flottenbetrieb | Noch nicht: zusätzlich Release-, Zielhardware-, Störungs-, Update- und Supportnachweise nötig. |

Das sind abgestufte Freigaben, keine Forderung, vor dem ersten Kundengespräch ein vollständiges Flottenprodukt fertigzustellen.

## Was tatsächlich verifiziert wurde

- Aktueller Workspace: **209 Tests bestanden** mit `cargo test --workspace --all-features --locked --offline`.
- `cargo fmt --all --check`: bestanden.
- `cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings`: im Abschlussstand bestanden. Ein früherer Zwischenstand scheiterte noch an großen Futures; dieser Zwischenbefund ist keine offene Beanstandung des Abschlusscommits.
- Zusätzliche isolierte Prüfungen: **8 Tests, 6 fehlgeschlagene Soll-Eigenschaften, 2 erfolgreiche Kontrollen**. Diese sind nicht Teil der 209 vorhandenen Tests und keine statistische Fehlerrate.
- Docker-Bauschritt aus dem vorhandenen Dockerfile separat ausgeführt: `cargo build --offline --locked --release -p onetimer-cli` scheitert mit „package ID specification ... did not match any packages“.
- Neue GPU-Logs und Benchmarkcode geprüft. **Kein neuer GPU-Benchmark, kein aktueller Langzeitlauf, kein Jetson-Test und keine Sicherheitszertifizierung durchgeführt.**
- Ein implementierter CI-/Release-Workflow wurde nicht mit einem von mir unabhängig geprüften erfolgreichen Remote-Release gleichgesetzt.

[Zusatztests](repros/tests/production.rs) · [Testausgabe](repro-results.txt)

```bash
cargo test --manifest-path docs/reviews/2026-09-08/repros/Cargo.toml \
  --offline --locked --target-dir <tmp> \
  -- --test-threads=1
```

Offline benötigt den Dependency-Cache. Die Integrationstests verwenden lokale Mockserver, keine GPU und keine laufenden Kunden-/Produktbackends. Die Tests formulieren gewünschtes Verhalten; auf dem geprüften Stand ist ihr Fehlschlag beabsichtigt.

## Was aus den acht angeblich erledigten Blockern geworden ist

| Punkt | Vorhandener Fortschritt | Verbleibende Grenze |
|---|---|---|
| F10 Timeout/Quarantäne | Client wird nach Inferenztimeout beantwortet; Kredit bleibt zunächst belegt. | Drain ignoriert Quarantäne; neue wartende Requests können hängen; Transportabbruch ist kein Beweis für GPU-Abschluss. |
| F21 SIGTERM/Drain | Signalhandler und Actor-Drain vorhanden. | Drain-Frist beginnt erst nach dem potenziell unbegrenzten gRPC-Abschluss; erfolgreicher Drain kann noch laufende Arbeit übersehen. |
| Readiness | Eigener Endpunkt, Quarantäneprüfung. | Backendverfügbarkeit und Modellbereitschaft werden nicht geprüft; bestätigter Verbindungsfehler kann „ready“ ergeben. |
| Vertrauensgrenze | Strict-Modus, Bytebudget, Loopback-Default; zusätzlich TLS/mTLS und Bearer-Prüfung. | Bytebudget ist umgehbar; Shm-Lebensdauer/Verwaltungsrechte/Backendzugriff sind nicht vollständig abgesichert. |
| F16 Varianten-I/O | Vergleich und deklarierbare kanonische Signatur. | Nicht abrufbare Metadaten werden übersprungen; unbekannte Kompatibilität gilt praktisch weiterhin als zulässig. Fachsemantik bleibt Aufgabe der Integration. |
| Kalibrierung | Affines Textkostenmodell und Messfunktion vorhanden. | Der vorgeschaltete Standardpfad scheitert bei BYTES/anderen Endpunkten; Mess-/Fehlerbehandlung und Modellabdeckung bleiben lückenhaft. |
| F20 Regler | Degradation und aggressivere Stale-Entfernung angeschlossen. | ProfileHealth weiterhin ohne Produktaufruf; Fingerprint deckt reale Hardware-/Artefaktidentität nicht vollständig ab. |
| F13 Triton-Ressource | Zwei zusätzliche GPU-Läufe mit gemeinsamem Ressourcenpool dokumentiert. | Konkreter Baselineeinwand bearbeitet. Kein Beweis für alle Alternativen, gleichwertigen Nebendurchsatz oder sämtliche Betriebsbedingungen. |

## Offene und neu nachgewiesene technische Befunde

P1 ist ein Freigabeblocker für den betroffenen Produktionsumfang. P2 ist eine relevante Einschränkung, die behoben oder aus dem unterstützten Umfang genommen werden muss.

### P01 · P1 · Drain meldet Erfolg trotz Quarantäne

[actor.rs:358](crates/vig-gateway/src/actor.rs:358) beendet den Actor bei `waiting.is_empty() && responses.is_empty()`. Nach einem Timeout wurde der Client aus `waiting` entfernt. Ein noch laufender Backendaufruf steht nicht in `responses`; dort liegen bereits eingegangene Antworten. `quarantined` und die tatsächlichen In-Flight-Kredite werden nicht geprüft.

**Reproduziert:** Backend antwortet niemals; Timeout setzt Quarantäne auf 1; anschließend liefert `drain()` sofort `true`. Damit kann ein geordneter Shutdown Erfolg melden, obwohl sein eigener Kommentar das Gegenteil verspricht.

Zweiter Codebefund: [serve.rs:126](crates/vig-cli/src/serve.rs:126) wartet erst auf `serve_with_shutdown(...).await` und startet danach die 20-Sekunden-Drainfrist. Die lokal eingesetzte tonic-Implementierung wartet dabei selbst auf das Schließen der Verbindungen. Ein hängender RPC kann deshalb bereits vor Beginn der Frist feststecken.

**Freigabebedingung:** eine gesamte Shutdownfrist ab Signal; Aufnahme schließen, Readiness absenken, wartende und laufende Arbeit getrennt behandeln, echte In-Flight-Zustände berücksichtigen. SIGTERM mit hängendem Backend, wartenden Clients und offenem Verwaltungs-RPC testen. Nach Ablauf darf der Prozess begrenzt und mit Fehlerstatus enden; er darf die Ungewissheit nicht als erfolgreiche Leerung melden.

### P02 · P1 · Quarantäne ist noch kein vollständiger Fehler- und Wiederanlaufvertrag

[actor.rs:438](crates/vig-gateway/src/actor.rs:438) entfernt bei jedem `BackendDone` die Quarantäne. Auch `Err` führt zu `Event::BackendFailure`; [scheduler.rs:540](crates/vig-core/src/scheduler.rs:540) gibt den Kredit frei.

**Codebefund:** Ein abgebrochener Transport beweist nicht, dass die GPU den Auftrag beendet hat. Die Unterscheidung „bestätigter Backendabschluss“ versus „Ausführungsende unbekannt“ fehlt weiterhin. Das Problem gilt auch für einen Gatewayneustart vor einem unverändert laufenden Backend.

**Reproduziert:** Nach vollständiger Quarantäne wird ein weiterer Request angenommen. Trotz expliziter Deadline von 100 ms wartet sein Client nach 300 ms weiterhin. Der Inferenztimeout beginnt erst beim Dispatch und hilft einem Request ohne verfügbaren Kredit nicht. Der Test belegt die Fristüberschreitung, nicht durch 300 ms Beobachtung allein ein unendliches Warten; der fehlende Abschluss-/Queue-Timeoutpfad erklärt das weitere Risiko.

**Freigabebedingung:** definierte Aufnahme- und Queue-Timeoutpolicy bei nicht verfügbarer Ausführung; sichere Recovery über überprüfbaren Backendzustand beziehungsweise kontrollierten Backendneustart. Keine automatische Freigabe unbekannter Arbeit allein aufgrund eines RPC-Fehlers. Keine zweite Governorinstanz mit frischen Krediten gegen eine unversöhnte alte Ausführung starten.

### P03 · P1 · Readiness bleibt bei bestätigtem Backendausfall grün

[exporter.rs:315](crates/vig-gateway/src/exporter.rs:315) betrachtet nur, ob die Zahl quarantänierter Requests die Slotzahl erreicht. Backend-/Modellbereitschaft, jüngste Verbindungsfehler und Shutdownstatus fehlen.

**Reproduziert:** Ein Aufruf scheitert mit `Unavailable`, der korrekte Fehlerzähler steht auf 1, die Readinessfunktion liefert trotzdem Erfolg. Ein sofort ablehnendes Backend erzeugt normalerweise keine Quarantäne.

**Freigabebedingung:** begrenzt laufende, zwischengespeicherte Zustandsprüfungen pro notwendigem Backend und Modell; Shutdown/Quarantäne berücksichtigen. Liveness weiterhin unabhängig vom Backend halten. Ein HTTP-Endpunkt allein ist noch keine ausreichende Bereitschaftsaussage.

### P04 · P1 für fremd kontrollierte Eingaben · Bytebudget lässt gültige Payloadformen aus

[payload_bytes() in service.rs:123](crates/vig-gateway/src/service.rs:123) summiert ausschließlich `raw_input_contents`. OIP erlaubt Nutzlast auch in `inputs[].contents`.

**Reproduziert:** Ein einzelner Request mit 2 MiB `bytes_contents` wird bei 1 MiB Budget erfolgreich ausgeführt. Die Kontrolle mit 2 MiB `raw_input_contents` wird korrekt abgewiesen. Das ist eine echte Lücke in der Aufnahmegrenze, keine bloß fehlende Sicherheitsfunktion.

Zusätzlicher Codebefund: Der Byte-Permit hängt an der wartenden Service-Future, nicht an der Lebensdauer sämtlicher vom Actor/Backend gehaltener Daten. Clientabbruch oder Timeout kann den Permit freigeben, während noch Daten beziehungsweise weiterlaufende Arbeit existieren. Außerdem liegen Decodierung und Authentifizierung vor dieser Nutzlastreservierung; der gesamte Transport-/Antwortspeicher wird damit nicht begrenzt.

**Freigabebedingung:** alle unterstützten Payloaddarstellungen und Metadaten berücksichtigen; passende Byteverantwortung an den tatsächlichen Datenlebenszyklus binden; Verbindungs-/Transportlimits und Lasttests ergänzen. Unsupported-Darstellungen ausdrücklich ablehnen ist für einen engen Pilot ebenfalls möglich.

### P05 · P1 für den versprochenen Textkalibrierworkflow · Neuer Pfad wird zu spät erreicht

[calibrate.rs:169](crates/vig-cli/src/calibrate.rs:169) holt zuerst alle Modelle über `profiling_targets()` am Defaultendpunkt. Für jede Variante folgen `model_metadata`, `zero_request(...)?` und unäre Inferenzen. Erst nach dieser Schleife wird `measure_all_cooperative()` aufgerufen.

[request.rs:94](crates/vig-backend-triton/src/request.rs:94) unterstützt im Nullrequest absichtlich kein BYTES.

**Codebefund:** Ein gewöhnliches Textmodell mit BYTES-Eingaben beendet den Ablauf vor der neuen Textmessung. Liegt das Modell nur am modellspezifischen zweiten Backend, kann bereits die Metadatenabfrage scheitern. Die hinzugefügte Messfunktion behebt den End-to-End-Workflow daher noch nicht.

Weitere Einschränkungen: zwei Tokenbudgets mit je fünf Wiederholungen und Mittelwerten; tatsächliche Ausgabetoken/EOS werden nicht verifiziert; kein Modell für Kontextlänge/Cache-Miss; Fehler können vorhandene Handwerte stehen lassen; Gesamtoutput wird nicht abschließend wie eine Betriebskonfiguration validiert. Die alte einseitige Paarmessung und das mathematisch nicht allgemeingültige Faktor-zwei-Kriterium bleiben bestehen.

**Freigabebedingung:** bereits vor der ersten Messung nach Endpoint, Eingabeart und Antwortmodus routen. Den gesamten CLI-Aufruf mit der echten Vision-plus-Text-Konfiguration testen, Ausfälle sichtbar machen und nur vollständig valide Messkonfigurationen als erfolgreich ausgeben.

### P06 · P1 für transparente LLM-/VLM-Nutzung · Zerlegung verändert den Requestvertrag

[cooperative.rs:79](crates/vig-gateway/src/cooperative.rs:79) ersetzt Inputs durch Text und fest erzeugte Samplingparameter. Das Clientbudget, weitere Samplingparameter und zusätzliche Eingaben werden nicht erhalten. `GenerativeJob::from_request()` erhält lediglich die konfigurierte Gesamtobergrenze.

**Reproduziert:** Client verlangt maximal 4 Tokens, Modellkonfiguration erlaubt 64, Schedulerquantum beträgt 32. Bereits der erste tatsächliche Quantumrequest verlangt 32 Tokens.

Zusätzlich werden Tokens weiterhin über Bytes/4 geschätzt; das ist keine harte Tokenzählung. Ein Request, dessen Texteingabe der Parser nicht lesen kann, erzeugt keinen Jobzustand, während der Scheduler die Modellkonfiguration weiterhin für kurze Quanten verwenden kann. Für einen solchen Request muss die Zulassung fehlschließen oder mit der vollen Arbeit planen.

**Produktgrenze:** WP26 testet Textrequests. Daraus folgt kein validierter multimodaler VLM-Pfad: der derzeitige Quantumaufbau würde zusätzliche Bildeingaben entfernen. Die Qualitäts-/Semantikgleichheit geteilter und ungeteilter Antworten ist nicht gemessen.

**Freigabebedingung:** genau unterstützte Modelle, Eingabeformen, Samplingoptionen und Endkriterien nennen. Clientlimits einhalten; unsupported Requests ablehnen. Multimodale Arbeit und transparente beliebige Textfortsetzung nicht als bereits unterstützten Produktionsumfang verkaufen.

### P07 · P1 bei automatischer Variantenwahl · Unbekannte Signatur wird weiter zugelassen

[verify.rs:288](crates/vig-cli/src/verify.rs:288) überspringt nicht abrufbare Metadaten auch beim Vergleich mit einer ausdrücklich deklarierten `io_signature`. Der paarweise Vergleich verhält sich ebenso. `unverified_models()` berücksichtigt weiterhin nur Mismatch, nicht Missing/Unavailable.

**Codebefund:** „keine nachgewiesene Abweichung“ wird mit ausreichender Freigabe verwechselt. Eine später ladende Variante kann ohne erfolgreich geprüfte Signatur verfügbar werden. Startupprüfungen besitzen außerdem nicht überall begrenzte RPC-Wartezeiten.

**Freigabebedingung:** für automatische Auswahl nur erfolgreich verifizierte Varianten zulassen; unbekannte Varianten sperren oder Start verweigern. Geänderte Modellversionen/Hotswaps berücksichtigen. Für den ersten Pilot kann eine fest gebundene, fachlich geprüfte Variante diesen Umfang bewusst reduzieren.

### P08 · P1 für Docker-Auslieferung · Umbenennung bricht den vorhandenen Build

[Dockerfile:16](deploy/docker/Dockerfile:16) baut weiterhin `onetimer-cli`, kopiert `target/release/onetimer` und setzt dieses alte Binary als Entrypoint.

**Reproduziert:** Der Cargo-Bauschritt scheitert, weil es das Paket nicht mehr gibt. Kein Dockerimage wurde dafür heruntergeladen oder gebaut.

Der Compose-Aufbau verwendet zudem weiterhin alte Namen und veröffentlicht die direkten Backendports sowie Governor/Monitoring auf Hostinterfaces. Der Loopback-Default des CLI schützt diesen Aufbau nicht, weil Compose ihn ausdrücklich überschreibt.

**Freigabebedingung:** komplette Installation aus sauberem Checkout und dokumentierten Artefakten auf leerem Zielsystem testen – inklusive Modellmanifest, Rechten, Ports, Shm, Healthchecks, Start/Stop und Rollback. Nicht nur den Rust-Workspace kompilieren.

### P09 · P2 · Messdefinitionen und Messfenster sind noch inkonsistent

Die Umbenennung zu `response_age` und das zusätzliche Peak-AoI sind Fortschritte. [coverage.rs:126](crates/vig-sim/src/coverage.rs:126) nimmt aber weiterhin Lieferungen außerhalb des Messfensters in Alter und Peak-AoI auf.

**Reproduziert:** Ein 100-ms-Messfenster enthält nur eine bei 1000 ms registrierte Lieferung und meldet 1000 ms Peak-AoI. Der Test setzt keine vorhandene Information vor Fensterbeginn voraus; der künstliche Peak stammt eindeutig aus der Lieferung nach Messende.

Die Coverage markiert nur das Fenster einer neuen ausreichend frischen Lieferung. Ein im nächsten Fenster noch brauchbares Ergebnis zählt dort nicht weiter. Die README-Definition „Anteil der Kontrollzyklen mit verfügbarer ausreichend frischer Information“ ist daher weiterhin zu stark. Präzise wäre derzeit „Anteil der Zeitfenster mit einer neu gelieferten frischen Antwort“.

**Freigabebedingung:** Consumer-Abtastzeitpunkte und Datenalter definieren; Fenstergrenzen, Anlaufzustand und Drain sauber abbilden. Liveexport um maximale Versorgungslücke und Frischemetriken der wirklich unterstützten Ströme ergänzen. Zero Protected Deadline Misses nicht als Zero verpasste Regelzyklen ausgeben.

### P10 · P2 · Regler- und Profilvertrauen bleiben nur teilweise wirksam

Die beiden Überlastmaßnahmen sind jetzt im Scheduler angeschlossen. `ProfileHealth::health()` bleibt außerhalb der Tests ungenutzt. Metadatenfingerprints enthalten noch keinen vollständigen Nachweis von GPU, Power-Modus, Treiber, Gewichtsinhalt und Backendkonfiguration.

**Freigabebedingung:** nicht verwendete Circuit-Breaker-Versprechen streichen oder tatsächlich integrieren. Deploymentmanifest und Profilgültigkeit an reale Hardware-/Artefaktbedingungen binden; Fehlreaktion testen. Eine erfolgreiche Prüfung identischer OIP-Metadaten qualifiziert keine andere GPU.

### P11 · P1 abhängig vom Vertrauensbereich · TLS/Auth schließen Shm- und Verwaltungsrisiken nicht allein

Die neuen Transport- und Tokenmechanismen sind vorhanden. Ein gültiger Token berechtigt aber auch zu Verwaltungsaufrufen wie Modell-Unload und Shm-Unregister. Es existiert keine getrennte Rolle für reine Inferenzclients. Die Shm-Registry verwaltet keine Lease pro noch aktiver Ausführung; Registrierung/Unregister gehen weiterhin hauptsächlich zum Defaultbackend.

**Freigabebedingung:** entweder genau einen vertrauenswürdigen lokalen Betreiber als unterstützten Umfang technisch durchsetzen oder Berechtigungen und Shm-Eigentümerschaft trennen. Clienttimeout ist ausdrücklich keine Freigabe, denselben Shm-Puffer für noch unbekannt laufende GPU-Arbeit zu überschreiben. Ein sicherer Pilot braucht dafür Beispielclient und verbindlichen Lebenszyklus.

### P12 · Vor Weitergabe klären · Lizenzmetadaten sind widersprüchlich

[LICENSE](LICENSE) und Cargo nennen BUSL-1.1. [.reuse/dep5](.reuse/dep5) deklariert für `Files: *` weiterhin Apache-2.0 und alte Projekt-/Repositorynamen. Als Lizenz-/Pilotkontakt ist `info@vigilant.example` eingetragen.

Das ist zunächst ein konkreter Dokumentations- und Freigabebefund, keine abschließende juristische Auslegung. Für einen Pilot brauchen Anbieteridentität, echter Kontakt, zulässiger Evaluations-/Produktionsumfang, Artefaktversion und Nutzungsrechte eine eindeutige Fassung. Gerade der eigene weit formulierte Begriff „Production Purpose“ sollte mit einem Lizenzjuristen gegen den gewünschten Kundenpilot geprüft werden.

BSL ist vor dem Change Date keine Open-Source-Lizenz. Apache-2.0 hätte kommerziellen Geräte-/OEM-Vertrieb und bezahlten Support nicht ausgeschlossen; die Umstellung ist eine Geschäftsmodellentscheidung, kein technisches Muss. Bereits erteilte Apache-Rechte an älteren Fassungen verschwinden nicht einfach durch eine neue LICENSE-Datei. [MariaDB BSL](https://mariadb.com/bsl11/), [Apache-2.0, insbesondere §§2, 4 und 9](https://www.apache.org/licenses/LICENSE-2.0)

## Was die neuen GPU-Zahlen jetzt tragen

**Der konkrete Ressourcenpool-Einwand ist bearbeitet.** Die beiden Ressourcenläufe zeigen weiterhin einen Vorteil beim Detektor: Triton 91/89 %, Governor 99 %. Die Rohlogs nennen dafür **12,9x und 15,1x weniger unabgedeckte Fenster**, nicht 22x. Die rund 22x stammen aus dem anderen Arm mit Prioritäten ohne gemeinsamen Pool.

Das ist weiterhin ein deutlicher positiver Befund. Die stärkere Verkaufsaussage ist präzise: „Im geprüften überlasteten Mehrmodellaufbau verbesserte der Governor die Frische wichtiger Ströme auch gegenüber einer Triton-Konfiguration mit gemeinsamem Ressourcenpool.“ Den Verlust der Hintergrundlast dabei mitnennen. [Lauf 1](../2026-09-07/gpu/gate-m3-triton-resources.txt), [Lauf 2](../2026-09-07/gpu/gate-m3-triton-resources-lauf2.txt)

**WP26 zeigt nun wirksame Zerlegung und einen realen Kompromiss.** Im zweiten Log stehen 2 gegenüber 41 erfolgreich beantworteten Generierungen, 606 gegenüber 9532 Zeichen, Detektor-Coverage 98 gegenüber 91 % und Antwortalter p95 33 gegenüber 65 ms. Mehr Textfortschritt ist belegt; 20x gleichwertig gelöste Kundenaufgaben sind es noch nicht. Unterschiedliche Ausgabelängen und P06 verlangen eine eigene Qualitätsprüfung. [WP26 Lauf 2](../2026-09-07/gpu/wp26-lauf2.txt)

Weiter offen bleiben:

- Ein zentraler Minimaldispatcher mit Latest plus Priorität als Gegenprobe zur zusätzlichen Algorithmuskomplexität.
- Gleiche Mindestversorgung der Nebenlast und gleiche fachliche Ergebnisqualität in allen Vergleichsarmen.
- Zielhardware, reale Kamera-/Text-/VLM-Inputs, längere Prompts, variierende Outputbudgets, thermischer Betrieb und wiederholte Messungen mit kontrollierter Reihenfolge.
- Ein neuer Langzeitlauf **nach** den Reparaturen und nach dem Einbau von Timeout, Auth und den übrigen Änderungen. Der ältere Acht-Stunden-Lauf qualifiziert diese neue Version nicht.
- Reproduzierbare Zuordnung jeder Messung zu genauem Code, Modellhash, Runtimekonfiguration, Image-Digest und Betriebszustand.

„Innerhalb eines Laufs verglichen“ beseitigt zeitliche/thermische Effekte nicht automatisch, wenn die Arme nacheinander laufen. Balancierte oder randomisierte Reihenfolge und Wiederholungen bleiben nötig. „Stärkste Triton-Konfiguration“ sollte auf „stärkste der von uns geprüften Konfigurationen für das betreffende Ziel“ begrenzt werden.

## Konkreter Weg zur ersten Kundeninstallation

### Vor dem technischen Erstgespräch

Ein kurzes Paket vorbereiten: Problem, geprüfter Aufbau, aktuelle Messwerte samt Kosten für Nebenlast, unterstützter Umfang, bekannte Grenzen und Vorschlag für eine gemeinsame Evaluation. Kein „hard real time“, keine Safety-Zusage und kein pauschales „22x besser als Triton“.

NEURA beschreibt öffentlich lokale Inferenz sowie NVIDIA Isaac-/Jetson-Nutzung. Das macht den Anwendungsbereich plausibel, belegt aber **nicht**, dass der relevante NEURA-Stack Triton/OIP verwendet oder den Eingriffspunkt des Governors besitzt. Diese Architekturfrage gehört ins erste Gespräch. [NEURA zur NVIDIA-/Jetson-Zusammenarbeit](https://neura-robotics.com/combining-neuras-unique-real-world-data-and-ai-enabled-neuraverse-platform-with-nvidias-advanced-simulation-and-computing-platform-is-accelerating-the-development-of-cognitive-robots/)

Geeignete Ansprechpartner sind die Verantwortlichen für Perception, Edge-Inferenz oder die Laufzeitplattform. Ziel ist zuerst ein technischer Sponsor mit einem konkreten Engpass, nicht eine allgemeine OEM-Lizenzzusage.

### Vor einer Installation in der laufenden Kundenpipeline

1. P01–P04 schließen; Fehlerpfade für Timeout, Transportabbruch, Backendneustart, Clientabbruch und SIGTERM automatisiert prüfen.
2. Einen engen unterstützten Umfang festlegen: zunächst zustandslose Vision, fester Backend-/GPU-Aufbau, kein unkontrollierter direkter GPU-Zugriff, geprüfte Shm-Lebensdauer und eindeutige Zeitbasis.
3. Für Textzerlegung P05/P06 reparieren oder sie im Pilot deaktiviert lassen. Für Varianten P07 reparieren oder exakt eine Variante fest binden.
4. Docker-/Installationspfad, Lizenzmetadaten und Kontakt korrigieren. Ein konkretes Releaseartefakt bauen, signieren, unabhängig verifizieren und auf einem sauberen Zielsystem installieren.
5. Mit dem Kunden quantitative Abnahmebedingungen festlegen: maximale Informationslücke, Mindestqualität, notwendiger Hintergrundfortschritt und definierte Störungsreaktion.

### Während der gemeinsamen Qualifikation

Zuerst aufgezeichnete Daten oder eine getrennte Testpipeline verwenden. Auch ein „Shadow“-Test auf derselben produktiv genutzten GPU kann Last verursachen und ist nicht automatisch rückwirkungsfrei. Produktive Steuerung und vorhandene Sicherheitsfunktionen zunächst unverändert lassen.

Dann Zielhardware und echte Modelle vergleichen. Ein Ablauf sollte mindestens normale Last, Überlast/Bursts, Backendausfall/-neustart, hängende RPCs, verlorene Verbindung, Shm-Wiederverwendung, SIGTERM, Ressourcenknappheit und Update/Rollback enthalten. Die geforderte Dauer folgt dem Risiko und Einsatzprofil; ein mehrtägiger Lauf nach den Reparaturen ist ein sinnvoller nächster Nachweis, aber keine universelle Garantie.

Vor unbetreutem Betrieb müssen Betriebshandbuch, Alarme, Verantwortlichkeit, Support-/Updateumfang und eine überprüfte Rückfallprozedur stehen. Das folgt zusätzlich zu guten Benchmarks und grünen Unit-Tests.

## Zusammenfassung für die Entscheidung

Die Reparaturen und GPU-Ergebnisse rechtfertigen, **jetzt** an NEURA und andere geeignete Entwicklungspartner heranzutreten. Sie rechtfertigen ein gemeinsames Evaluationsangebot, nicht die Aussage „fertiges Produktionsprodukt, einfach einsetzen“.

Die verbleibende Arbeit ist überschaubarer als ein grundlegender Architekturwechsel, aber sie enthält konkrete Korrektheits- und Betriebsfehler. Weiterbauen, Umfang begrenzen, an Fehlerpfaden abnehmen – und die Kundenqualifikation parallel vorbereiten.

