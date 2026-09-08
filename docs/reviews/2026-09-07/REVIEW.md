# Review vom 7. September 2026

Geprüft: `InferenceQoS`, Commit `1d703ac`, sowie der benachbarte Ordner `InferenceQoS-runtime`. Der Produktcode war zu Beginn unverändert. Der Runtime-Ordner ist kein Git-Repository; er enthält Modellkonfigurationen, Gewichte, Containerarchive und Messdaten. Dieser Review ergänzt nur seinen Bericht und einen isolierten Satz von Gegenbeispielen. Er implementiert keine Produktkorrekturen.

## Urteil

**Die Produktidee ist sinnvoll, ein allgemeiner Wettbewerbsvorsprung ist nicht belegt, und die aktuelle Implementierung ist nicht produktionsreif.**

Der interessante Ansatz ist, konkurrierende Inferenzarbeit anhand ihres fachlichen Alters und ihrer Dringlichkeit vor dem Backend zu steuern. Das passt zu mehreren stateless Wahrnehmungsströmen auf knapper Hardware. Die Trennung von Scheduler und Transport, explizite terminale Zustände und der Simulator sind eine brauchbare Grundlage zum Weiterbauen.

Die größte Schwäche liegt derzeit in der Verbindung der Bausteine: Einzelkomponenten erfüllen Tests, während die zusammengesetzte Implementierung wichtige Eigenschaften verletzt. Darüber hinaus sind einige Messgrößen und Schlussfolgerungen stärker bezeichnet, als das Experiment trägt. Besonders betroffen sind AoI, kooperative LLM-Quanten, die Wirkung des Triton Rate Limiters und die Übertragbarkeit virtueller Slots auf reale GPU-Kapazität.

Ich würde das Vorhaben weiterführen, aber zunächst als **experimentellen Governor für stateless Multi-Model-Perception**. Die nächste Investition sollte Fehlerkorrektur und ein belastbarer Vergleich mit einfachen Alternativen sein. Eine weitere Ausweitung der Produktversprechen wäre verfrüht.

## Stand der Umsetzung (nachgetragen)

Der Bericht unten beschreibt den Befundstand zu Commit `1d703ac`. Die
Reparaturen sind anschließend umgesetzt worden; dieser Abschnitt hält fest,
was erledigt ist und was offen bleibt. **Alle 21 Reproduktionen bestehen
jetzt**, die Workspace-Suite ist von 155 auf 202 Tests gewachsen, Clippy und
Formatprüfung sind ohne Befund.

Die Reproduktionen sind als Regressionstests ins Repo gehoben und laufen in
der normalen Suite mit — `crates/vig-core/tests/review_invariants.rs`,
`crates/vig-gateway/tests/review_invariants.rs`, sowie Ergänzungen in
`crates/vig-config/tests/schema.rs`, `crates/vig-sim/src/coverage.rs`
und `crates/vig-protocol-oip/src/params.rs`. Ein Befund, der nur in einem
Reviewordner nachgewiesen ist, ist nach drei Monaten kein Befund mehr, sondern
eine vergessene Behauptung.

| Befund | Stand |
|---|---|
| F01 verlorene Abschlussmeldungen | behoben — queueweise gesammelt, kein gemeinsamer Puffer mehr |
| F02 Backendfehler als Erfolg | behoben — der Actor sendet `Event::BackendFailure` |
| F03 kooperative Quanten | behoben — Auftrag entsteht bei der ersten Ankunft, Template bleibt erhalten, Quantum wird vor dem Guard bestimmt; end-to-end über drei Quanten belegt |
| F04 Panic bei akzeptierter Konfiguration | behoben — `ModelContract::validate` lehnt leere Tokenintervalle und Rate null ab; der Clamp im Dispatch ist zusätzlich entschärft |
| F05 Altersfallback | **teilweise** — unplausibles Alter wird geklemmt statt auf Ankunft zurückgesetzt, und die Zeitbasis hat jetzt eine Stunde Vorlauf, sodass kurz nach dem Start kein Alter mehr abgeschnitten wird; die Frage der gemeinsamen Epoche zwischen Client und Gateway bleibt offen |
| F06 Co-Run-Verbot | behoben — gilt bis zur gemeldeten Fertigstellung, die Prognose ist nur noch Weckzeitpunkt |
| F07 FIFO/Stateful | behoben — reihenfolgeerhaltende Policies stellen nur ihren Kopf als Kandidat |
| F08 gemeinsamer Bedarf im Guard | behoben — erwartete Ankünfte werden kumulativ und in Ankunftsreihenfolge reserviert |
| F09 gelernte Laufzeit im Guard | behoben — der Look-ahead nutzt denselben Schätzer wie der Dispatch |
| F10 Abbruch und hängendes Backend | **teilweise** — Clientabbruch räumt die Queue, zählt und stoppt auch einen bereits zerlegten Auftrag an seiner nächsten Quantengrenze; ein Inferenztimeout mit Zustand „Ausführungsende unbekannt" fehlt weiterhin und ist durch F11 **dringender geworden** |
| F11 decoupled Teilantworten | behoben — bis zur letzten Antwort gelesen; mehrere Nutzlasten werden gemeldet statt still abgeschnitten. Kehrseite: ein Backend, das weder Abschluss markiert noch den Stream schließt, hält den Aufruf jetzt offen — siehe F10 |
| F12 AoI-Metrik | behoben — `aoi_p*` heißt jetzt `response_age_p*`, zusätzlich `peak_aoi_ns` über die Zeit |
| F13 Triton-Baseline ohne begrenzte Ressource | **offen** — Messaufgabe, siehe unten |
| F14 blockierter Spitzenkandidat | behoben — Veto statt Abbruch der Dispatchschleife |
| F15 niedrigste Qualität = schnellste | behoben — die gemessen schnellste Variante gewinnt |
| F16 I/O-Kompatibilität der Varianten | **offen** — braucht eine Produktentscheidung über die kanonische Signatur |
| F17 OIP-Transparenz, mehrere Backends | **offen** |
| F18 still abgeschaltete Zeitregeln | behoben — unzulässiges `max_age_ms`/`period_ms` verhindert den Start |
| F19 Kalibrator misst nicht den behaupteten Effekt | **offen** — Messaufgabe |
| F20 nicht angeschlossene Reglerwirkungen | **teilweise** — die gelernte Marge wirkt jetzt im Look-ahead; `ProfileHealth`, `forces_degradation`, `aggressive_supersession` und der Fingerprintumfang bleiben offen |
| F21 Daueraktivität und Shutdown | **teilweise** — der 1-ms-Leerlauf ist beendet; SIGTERM mit Drain-Frist und ein Shutdown-Handle fehlen |
| F22 Aufnahme- und Vertrauensgrenze | **offen** |
| F23 Benchmarkabschluss | **offen** |
| F24 modellweites Latest | behoben — Scope- und Altersvergleich sind getrennt |

### Zweiter Durchgang: Mängel in den Reparaturen selbst

Eine Nachprüfung der eigenen Änderungen hat fünf Fehler gefunden, die alle
behoben und mit Tests belegt sind. Sie stehen hier, weil sie die gleiche
Fehlerklasse zeigen wie die Befunde oben — eine Korrektur, die nicht
nachgeprüft wird, ist eine Behauptung.

- **Speicherleck im Abbruchwächter.** Der Wächter wurde beim regulären
  Abschluss mit `core::mem::forget` entschärft. Damit blieb je erfolgreichem
  Request ein `Sender` am Eingangskanal hängen: der Kanal hätte nie als
  geschlossen gegolten, und der Speicher wäre über Stunden gewachsen —
  ausgerechnet das, was der Dauerlauf ausschließen soll. Jetzt über ein Feld
  entschärft, mit zwei Unit-Tests.
- **Zähler ohne Ausgang.** `cancelled` war eingeführt, aber nicht im
  Prometheus-Export. Ein Zähler, den niemand abfragen kann, ist keiner.
- **Abbruch wirkte nicht auf zerlegte Aufträge.** Erst durch das Zusammenspiel
  zweier Reparaturen entstanden: eine Fortsetzung trägt eine neue Kennung, der
  Client kennt nur seine erste. Der Auftrag lief nach dem Abbruch bis zum Ende
  seines Tokenbudgets weiter. Jetzt wird die Kennung übersetzt, und vor jeder
  Fortsetzung wird geprüft, ob der Antwortkanal überhaupt noch offen ist.
- **Regression im Auswertungswerkzeug.** Die Umbenennung der AoI-Spalte hat
  `tools/soak-report.py` gebrochen — das Werkzeug, mit dem der Achtstundenlauf
  ausgewertet wird. Es liest jetzt beide Spaltennamen; gegen die vorhandenen
  1.440 Messzeilen nachgeprüft.
- **Fehlgriff bei der Veto-Maske.** Der erste Optimierungsversuch trug alle
  Vetos über den ganzen Durchlauf mit. Das Veto des Look-ahead ist aber nicht
  monoton — es lautet „ohne dich ginge es, mit dir nicht" und kann entfallen,
  sobald die geschützte Ankunft ohnehin nicht mehr zu retten ist. Mitgeführt
  hätte es Slots leer stehen lassen, also genau F14 wieder eingeführt. Jetzt
  werden strukturelle Sperren mitgeführt und das Look-ahead-Veto nicht.

Zusätzlich hat `ArrayVec::insert` jetzt eigene Tests (neue Logik mit
Indexverschiebung, vorher ungeprüft), und `docs/reviews/.../repro-results.txt`
hält fest, welche drei Erwartungen an die reparierte Semantik angepasst wurden
und warum.

### Dritter Durchgang: Messung auf der GPU (2026-09-08)

Die Reparaturen sind auf der Zielhardware nachgemessen —
`gpu/ERGEBNIS.md` mit Rohdaten.

- **Gate M3 hält.** Detector 22,6–22,9x, pose 11,9–12,0x, depth 5,4x bis
  „besser" gegen die getunte Triton-Baseline; zwei Läufe. Die Kernaussage des
  Produkts übersteht die Reparaturen unverändert und wird in zwei Punkten
  besser.
- **WP26 kippt — zugunsten des Produkts.** Die dokumentierte Antwort „Quanten
  lösen die Aushungerung nicht" war ein Fehlerbild, kein Messergebnis: es wurde
  gar nichts zerlegt (F03). Nach der Reparatur bringt die Zerlegung **40 statt
  1 Generierung** für 7 Punkte Detektor-Abdeckung, bei null
  Protected-Deadline-Misses.
- **Ein weiterer echter Fehler, erst durch die Messung sichtbar.**
  `size_quantum` rechnete die Quantendauer rein proportional zur Tokenzahl.
  Gemessen kostet jeder Auftrag einen festen Sockel von 14–18 ms — bei rund
  18 ms Slack ist das der ganze Unterschied, und ein proportionales Modell kann
  nicht ausdrücken, dass gar kein Quantum passt. `Cooperative` hat jetzt ein
  Pflichtfeld `base_cost_us`, `size_quantum` rechnet affin. Zwei Tests, ADR-0014
  nachgetragen.
- **Nebenbefund:** die Benchmarkkonfiguration trug `tokens_per_second: 55`,
  gemessen sind heute rund 242 — Faktor 4,4. Gemessene Größen veralten; sie
  gehören in `onetimer calibrate`, nicht in eine handgepflegte Datei. Für den
  Sockel gilt dasselbe, er ist derzeit ebenfalls von Hand eingetragen.

### Vierter Durchgang: die Produktionsblocker (2026-09-08)

Alle acht als offen benannten Punkte sind umgesetzt und mit Tests belegt; die
Suite ist von 155 auf 202 Tests gewachsen.

| Punkt | Umsetzung |
|---|---|
| Inferenztimeout mit Quarantäne (F10) | `backend.inference_timeout_ms`. Beim Ablauf wird **nur der Client** freigegeben, der Slotkredit nicht — die GPU rechnet womöglich noch. Der Slot bleibt in Quarantäne, bis das Backend antwortet. Eine verspätete Antwort zählt als Fehler, nicht als Fertigstellung, und trainiert den Margen-Regler nicht. |
| SIGTERM mit Drain (F21) | `serve` hört jetzt auf SIGTERM, nicht nur auf Ctrl-C. Neuer Verkehr stoppt, angenommene Arbeit wird beantwortet, laufende Inferenzen laufen aus — 20 s Frist, danach Exitcode ungleich null. |
| Readiness getrennt von Liveness (F22) | `/readyz` neben `/healthz`. „Nicht bereit" heißt kein Verkehr, „nicht lebendig" heißt Neustart — bei allen Slots in Quarantäne hilft ein Neustart nicht, denn das Backend startet dabei nicht mit. |
| Vertrauensgrenze (F22) | `backend.trust: strict` lehnt unkonfigurierte Modelle ab (sonst genügt der physische Name, um den Governor zu umgehen) und lässt eine Clientangabe die Klasse nur **senken**. Dazu ein Bytebudget (`max_inflight_mib`) statt nur einer Requestzahl, und Loopback als Bindevoreinstellung. |
| I/O-Kompatibilität der Varianten (F16) | Signaturvergleich aus den Backendmetadaten. Unterscheiden sich Ein- oder Ausgaben, wird die automatische Variantenwahl für dieses Modell **abgeschaltet**, nicht nur bemängelt. |
| Kalibrierung statt Handpflege | `vig calibrate` misst jetzt auch Sockel und Erzeugungsrate zerlegbarer Modelle. Der Anlass: in der Messkonfiguration stand eine Rate, die um Faktor 4,4 danebenlag. |
| Reglerwirkungen (F20) | `forces_degradation()` wählt unter Überlast die schnellste machbare statt der besten Variante; `aggressive_supersession()` verwirft unter Frischedruck auch das, was erst beim Fertigwerden zu alt wäre. Beide waren vorher tote Methoden. |
| Triton mit begrenzter Ressource (F13) | Nachgemessen, siehe `gpu/ERGEBNIS.md`. Die Ressource verteilt den Verlust um, statt ihn zu beseitigen. |

**Was danach noch offen ist:** Authentifizierung und TLS (der Governor bindet
deshalb auf Loopback), Hardware jenseits dieser einen Maschine, die Semantik
gleicher Signaturen bei verschiedener Bedeutung, sowie Paketierung, signierte
Releases und ein Updatepfad. Die Zeile `vlm: 0 %` gilt weiterhin für **nicht
zerlegbare** Blöcke; für zerlegbare ist sie durch die WP26-Neumessung erledigt.

Zur AoI-Kritik unten eine Einschränkung, die im ursprünglichen Bericht fehlt:
`CoverageTracker` maß auch vorher schon periodenbezogene Abdeckung über
`covered`/`uncovered_permille` und war damit nicht blind für Versorgungslücken.
Falsch war die Bezeichnung der Perzentile und das Fehlen einer zeitbezogenen
Spitzengröße. Beides ist korrigiert; das Messkonzept musste nicht ersetzt
werden.

## Prüfgrundlage und Grenzen

- Architektur-, Implementierungs- und Querverweisprüfung über die acht Rust-Crates: Core, Gateway, OIP-Protokoll, Triton-Adapter, Konfiguration, CLI, Simulation und Benchmarkcode. Besondere Tiefe bei Scheduling, Fehlerpfaden, Zeitbasis, Profilierung, Quanten und Messmethodik; keine Behauptung einer vollständigen formalen Verifikation jeder Zeile.
- `cargo test --workspace --all-features --locked --offline`: **155 Tests bestanden**. Die erste Ausführung scheiterte bei lokalen gRPC-Sockets an der Sandbox; mit freigegebenem Socketzugriff bestanden die Tests.
- `cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings`: bestanden.
- `cargo fmt --all --check`: bestanden.
- Zusätzliche Prüfungen gegen unveränderte Produktbibliotheken: **21 Tests, davon 17 fehlgeschlagene Soll-Eigenschaften und 4 erfolgreiche Kontrollen/Gegenbeispiele**. Ein Fehler wird zum Teil auf zwei Ebenen geprüft; dies ist keine Zählung von 17 voneinander unabhängigen Bugs und keine statistische Fehlerquote.
- Vorhandene achtstündige Messdaten mit `python3 tools/soak-report.py ../InferenceQoS-runtime/soak` erneut ausgewertet: 1.440 CSV-Zeilen; publizierte Werte für Coverage, Margen und Speicherverlauf reproduziert.
- Aktuelle Primärquellen zu Triton, CUDA, Holoscan, ROS/GStreamer und einschlägiger Forschung geprüft. Kein neuer GPU-Leistungsvergleich, kein erneuter Acht-Stunden-Lauf, keine Modellgüte-Evaluation und kein Penetrationstest.

Die Gegenbeispiele stehen in [repros/tests/invariants.rs](repros/tests/invariants.rs), das Ausführungsergebnis in [repro-results.txt](repro-results.txt). Auf dem geprüften Stand `1d703ac` war ein fehlgeschlagener Lauf beabsichtigt: Die Tests behaupten die gewünschte Eigenschaft, die der Produktcode verletzt. Sie sind als eigenes Cargo-Workspace vom normalen Testlauf getrennt. **Nach den Reparaturen bestehen sie; maßgeblich sind seither die ins Repo gehobenen Regressionstests**, siehe „Stand der Umsetzung".

```bash
cargo test --manifest-path docs/reviews/2026-09-07/repros/Cargo.toml \
  --locked --offline --target-dir /tmp/onetimer-review-target \
  -- --test-threads=1
```

Offline-Ausführung setzt den vorhandenen Dependency-Cache voraus. Die gRPC-Gegenbeispiele verwenden ausschließlich lokale Testserver. Für das Entwicklungsprofil wird die vorhandene Stack-Einstellung in `.cargo/config.toml` verwendet.

## Priorisierte Befunde

P1 bezeichnet einen Freigabeblocker für die betroffene Funktion oder einen Fehler, der zentrale Nachweise entwertet. P2 bezeichnet eine erhebliche Einschränkung, die vor einer breiten Produktfreigabe korrigiert oder ausdrücklich aus dem unterstützten Umfang genommen werden sollte. „Reproduziert“ bedeutet einen ausgeführten Gegenbeispieltest; „Codebefund“ bedeutet einen anhand des konkreten Kontrollflusses nachvollzogenen Fehler ohne eigenen Ausfallversuch.

### F01 · P1 · Bei mehr als 32 gleichzeitig veralteten Requests verschwinden Abschlussmeldungen

In [scheduler.rs:556](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/scheduler.rs:556) besitzt der Sammelpuffer für Stale-Entfernungen die Kapazität `MAX_INFLIGHT = 8 * 4 = 32`. Warteschlangen können dagegen zusammen `32 * 64 = 2048` Einträge enthalten. `queue.collect_stale()` entfernt alle betroffenen Einträge; Fehler von `drops.push()` werden ignoriert.

**Reproduziert:** 64 wartende Requests altern gleichzeitig aus; nur 32 `Terminate`-Aktionen entstehen. Die übrigen Requests sind nicht mehr in der Queue, bleiben aber im Gateway in `waiting` und `inbox`. Clients warten unbegrenzt, Payloads bleiben erhalten. Wiederholungen ermöglichen unbegrenztes Speicherwachstum trotz begrenzter Einzelqueues. Die üblichen Beispiele mit drei `latest`-Queues entdecken das nicht.

**Korrektur:** jede Entfernung unmittelbar oder in vollständig abgearbeiteten Teilmengen melden. Invariante über Core und Actor prüfen: angenommen = terminal + wartend + tatsächlich in Ausführung. Keine verlorenen Ereignisse bei Kapazitätsgrenzen.

### F02 · P1 · Backendfehler werden im Scheduler als Erfolg verbucht

[actor.rs:270](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-gateway/src/actor.rs:270) verarbeitet jedes `BackendDone` als `Event::Completion`, auch wenn das Resultat `Err` ist. Der Core besitzt einen separaten `BackendFailure`-Pfad, den das Gateway hier nicht nutzt.

**Reproduziert:** eine fehlgeschlagene Verbindung ergibt `backend_failures = 0` und `completed_valid = 1`; der Client erhält gleichzeitig einen Fehler. Auch Laufzeitbeobachtung und Margenanpassung behandeln den fehlgeschlagenen Aufruf wie eine Inferenz.

**Folge:** Die Angabe „0 Backendfehler“ im Dauerlauf kann aus diesem Zähler nicht bewiesen werden. Das bedeutet nicht, dass im damaligen Lauf Fehler auftraten; der Zähler könnte sie aber nicht zuverlässig zeigen.

**Korrektur:** Erfolg und Fehler vor dem Scheduler-Ereignis unterscheiden. Backendfehler nicht als erfolgreiche Runtime-Samples aufnehmen; transportbezogene Fehler von ungültigen Requests unterscheiden und Statuscodes erhalten.

### F03 · P1 · Kooperative Quanten sind im realen Actor nicht durchgängig implementiert

In [actor.rs:258](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-gateway/src/actor.rs:258) legt eine erste Ankunft keinen `GenerativeJob` und keinen Eintrag in `descriptors` an. `GenerativeJob::from_request()` wird im Produktpfad nirgends aufgerufen. `jobs.insert()` und `descriptors.insert()` existieren nur in der Fortsetzung. Deshalb findet `forward()` beim ersten Quantum keinen Job und leitet den ursprünglichen Request weiter.

Zweiter Fehler in [scheduler.rs:628](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/scheduler.rs:628): Das Protected-Veto prüft die volle, aus dem Variantenprofil berechnete Joblaufzeit. `size_quantum()` kommt erst danach. **Reproduziert:** Ein 17-ms-Quantum würde vor die nächste geschützte Ankunft passen; der Scheduler blockiert es wegen des 100-ms-Gesamtjobs.

Dritter Codebefund: `forward()` entfernt das Requesttemplate aus `inbox`; `continue_job()` verlangt genau dort später wieder ein Template. Nur das Anlegen des Jobs zu ergänzen würde deshalb die Fortsetzung noch nicht reparieren. Bei manchen Abbruchpfaden fehlt außerdem die symmetrische Bereinigung von `jobs`.

**Folge:** „Mit und ohne Zerlegung identisches Ergebnis“ in WP26 kann durch die Implementierung selbst entstehen. Die gemessenen Tokenkosten bleiben interessant, beweisen aber nicht die behauptete Ursache des identischen Gesamtergebnisses. Der aktuelle Benchmark verwendet zudem `55 tokens/s, min_tokens=4` und alte Gesamtprofile, während der Bericht später andere Kostenmodelle diskutiert.

**Korrektur:** expliziter Joblebenszyklus mit ursprünglichem Template, Clientreply, Originaldeadline, verbleibendem Tokenbudget und Backendzustand. Zuerst tatsächlichen Quantumrequest und seine Kosten bestimmen, dann zulassen. Auf dem Draht Tokenlimits, mehrere Quanten, Detector-Einschübe und genau eine abschließende Antwort prüfen.

### F04 · P1 · Akzeptierte Konfiguration kann den Releaseprozess abbrechen

Die Konfigurationsauflösung und `ModelContract::validate()` prüfen die Grenzen von `cooperative` nicht. [scheduler.rs:733](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/scheduler.rs:733) ruft `clamp(min_tokens, max_total_tokens)` auf.

**Reproduziert:** `min_tokens=8, max_total_tokens=1` wird akzeptiert und erzeugt beim Dispatch eine Panic `min > max`. Im Releaseprofil gilt `panic = "abort"`; daraus folgt ein Prozessabbruch. Die Verbote von `unwrap` und explizitem `panic!` schützen nicht vor Panics in Standardmethoden.

**Korrektur:** `0 < min_tokens <= max_total_tokens`, sinnvolle Obergrenzen und `tokens_per_second > 0` vor dem Start erzwingen. Unterstützte Modellart und Kombination mit `stateful` ebenfalls validieren. Releaseprozess mit ungültiger Konfiguration gesondert prüfen.

### F05 · P1 · Zeitbasis und Altersfallback können alte Daten frisch erscheinen lassen

[clock.rs:32](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-gateway/src/clock.rs:32) liefert Nanosekunden seit `MonotonicClock::start()`. Die Protokolldokumentation verspricht dagegen absolute monotone Clientzeitstempel auf demselben Host. Ein Client mit `CLOCK_MONOTONIC` kennt den privaten Prozessnullpunkt nicht; normale absolute Werte werden als unplausibel verworfen. Dieselbe Maschine reicht als Bedingung nicht aus.

Zusätzlich setzt [params.rs:286](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-protocol-oip/src/params.rs:286) ein explizites Alter oberhalb der Plausibilitätsgrenze auf Ankunftszeit zurück. **Reproduziert:** 2 Sekunden gemeldetes Alter bei einer Grenze von 1 Sekunde werden zu 0 Sekunden. Auch kurz nach Prozessstart kann `arrival.saturating_sub(age)` Alter abschneiden.

**Korrektur:** eine dokumentierte gemeinsame Epoche oder explizite Umrechnung mit Uhrdomäne. Relative Altersangaben nicht wie fremde absolute Uhren behandeln. Ungültige Zeitangaben ablehnen oder als zeitlich ungewiss ausweisen; bekannte alte Daten dürfen keine neue Frist erhalten. Alter und Transportunsicherheit gegebenenfalls separat darstellen.

### F06 · P1 · Co-Run-Verbote enden vor der tatsächlichen Fertigstellung

[slots.rs:342](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/slots.rs:342) berücksichtigt verbotene laufende Modelle nur, solange `expected_finish > now`. `ready_slot()` verwendet diesen Prognosepfad auch für die reale Zulassung.

**Reproduziert:** A ist noch in Ausführung, sollte nach 5 ms fertig sein, hat aber noch keine Fertigstellung gemeldet. Ab 6 ms darf das verbotene B auf einem zweiten freien Slot starten. `corun_allowed(B)` sagt weiterhin „verboten“, `ready_slot(B)` erlaubt es.

**Korrektur:** harte Live-Ausschlüsse an tatsächliche In-Flight-Zustände binden; Zukunftsprognosen separat berechnen. Bei Überziehung nicht unterstellen, dass Hardwarearbeit verschwunden sei. Das ist besonders wichtig, wenn `no_corun` Ressourcenüberlast verhindern soll.

### F07 · P1 · FIFO ist im Scheduler keine FIFO; Stateful-Schutz ist unvollständig

[scheduler.rs:848](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/scheduler.rs:848) betrachtet alle Queueelemente nach Kritikalität und Deadline, unabhängig von ihrer Policy. `take_front()` existiert zwar, wird hier aber nicht verwendet.

**Reproduziert:** Nach Request 1 wird Request 3 vor Request 2 ausgeführt, wenn 3 eine frühere Clientdeadline hat; auch mit `stateful: true`. Der Golden-Test zur FIFO-Reihenfolge prüft die Queueoperation, nicht diesen Schedulerpfad.

Darüber hinaus erlaubt `stateful + fifo` weiterhin Altersdrops und mehrere offene Requests. Es fehlen sequenzbezogene Zulassung, Reihenfolge und Behandlung von Start/Ende/Fehlern. Ob Triton einen konkreten Fall auffängt, ersetzt diese Regeln nicht.

**Korrektur:** innerhalb FIFO nur den zulässigen Kopf anbieten. Stateful vorerst ausdrücklich aus dem Produktumfang nehmen oder einen vollständigen Sequenzvertrag implementieren. Bloßes Abschalten von Supersession und Variantenwechsel reicht nicht.

### F08 · P1 · Der Look-ahead reserviert geschützte Arbeit nicht gemeinsam

[feasibility.rs:163](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/feasibility.rs:163) prüft jede erwartete Ankunft separat gegen denselben hypothetischen Belegungszustand. Die erwarteten Jobs werden darin nicht nacheinander eingeplant.

**Reproduziert:** Zwei geschützte Jobs erscheinen bei 10 ms, brauchen je 5 ms und haben Deadline 20 ms. Ohne Kandidaten passen sie in `[10,15]` und `[15,20]`. Ein Best-Effort-Job bis 11 ms verschiebt sie auf `[11,16]` und `[16,21]`. Der Guard meldet trotzdem `Clear`, weil jeder geschützte Job einzeln passt.

**Korrektur:** den gemeinsamen Bedarf wartender und erwarteter Jobs simulieren oder eine begründete Bedarfsobergrenze reservieren. Gleiche Kritikalität, mehrere Perioden innerhalb des Horizonts, Ankunftsjitter und Jobs über den Horizont hinaus berücksichtigen. Die jetzige Prüfung ist eine Heuristik, keine Feasibility-Garantie für die Jobmenge.

### F09 · P1 · Der Look-ahead ignoriert bereits gelernte Laufzeitverschlechterung

[scheduler.rs:907](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/scheduler.rs:907) verwendet für erwartete Arbeit Offlineprofil und ursprüngliche globale Marge. Die eigentliche Variantenplanung verwendet dagegen Online-Estimator und modellspezifische Marge.

**Reproduziert:** Der Estimator kennt nach 16 Beobachtungen eine geschützte Laufzeit von 10 ms statt des Offlinewertes von 1 ms. Ein Best-Effort-Job wird dennoch zugelassen, obwohl er die nächste geschützte Fertigstellung von 1610 auf mindestens 1612 ms verschiebt.

**Korrektur:** dieselbe Prognosefunktion für Dispatch und Reservierung verwenden, einschließlich Onlinezustand, Modellmarge und hypothetischer Nebenlast. Ein selbstlernender Scheduler schützt nicht zuverlässig, wenn ausgerechnet seine Schutzrechnung alte Zahlen liest.

### F10 · P1 · Clientabbruch und hängendes Backend haben keinen vollständigen Lebenszyklus

`Handle::submit()` wartet auf eine Antwort, aber ein fallengelassener Empfänger erzeugt kein Cancel-Ereignis. [actor.rs:420](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-gateway/src/actor.rs:420) startet Backendtasks ohne überwachten Laufzeitabschluss. [client.rs:291](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-backend-triton/src/client.rs:291) hat keinen Inferenztimeout; der 5-Sekunden-Wert gilt nur für den Verbindungsaufbau, nicht für alle Health-/RPC-Antworten.

**Reproduziert:** Ein Client bricht einen noch wartenden Request ab; das Backend führt ihn anschließend trotzdem aus. Bei Shared Memory kommt ein Datenintegritätsrisiko hinzu, falls der Client die zugehörige Region nach Abbruch wiederverwendet.

**Codebefund:** Ein erreichbares, aber nie antwortendes Backend kann Kredite dauerhaft belegen. Metrikabfragen können währenddessen erfolgreich sein; `/healthz` prüft lediglich, ob der Actor antwortet.

**Korrektur:** Cancel-Zustände, Queuebereinigung, überwachte Tasks, begrenzte RPC-Wartezeiten und unabhängige Readiness. Ein RPC-Timeout darf GPU-Kredite nicht blind freigeben: die physische Ausführung könnte noch laufen. Dafür ist ein Zustand „Ausführungsende unbekannt“ mit Quarantäne/Backendabgleich nötig. Triton unterscheidet selbst zwischen Clientabbruch und backendabhängiger Abbruchunterstützung. [Triton Request Cancellation](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/request_cancellation.html)

### F11 · P1 · Ein beliebiges decoupled Modell wird nach seiner ersten Teilantwort als fertig behandelt

[client.rs:269](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-backend-triton/src/client.rs:269) kehrt beim ersten `infer_response` zurück. Die letzte Antwort beziehungsweise `triton_final_response` wird nicht abgewartet. Decoupled-Modelle dürfen mehrere Antworten liefern; die erste ist keine allgemeine Fertigstellungsbestätigung. [Triton Decoupled Models](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/decoupled_models.html)

**Folge:** Bei einem tatsächlich streamenden Modell können Teiltext als Endergebnis und ein freigegebener Slot bei noch laufender Backendarbeit entstehen. Das ist ein Codebefund für den unterstützten generischen `decoupled`-Modus, kein Nachweis, dass der konkrete nichtstreamende Qwen-Aufruf im vorhandenen Benchmark mehrere Teilantworten erzeugte.

**Korrektur:** explizit unterstützte Antwortmodi definieren, Request-ID zuordnen, finales Ende erkennen und Ausgabe korrekt aggregieren. Alternativ die enge Annahme „genau eine vollständige Antwort“ bei der Modellintegration erzwingen.

### F12 · P1 · Zentrale AoI-Metrik misst Antwortalter statt Informationsalter über die Zeit

[coverage.rs:91](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-sim/src/coverage.rs:91) speichert nur `completion - generation` pro Lieferung und bildet darüber Quantile. Nach einer Antwort ohne weitere Lieferungen wächst keine Beobachtung mehr nach. Ein leerer Strom erhält sogar AoI-Quantile von 0.

**Reproduziert:** Eine einzige 1-ms-Antwort und danach fast eine Sekunde Funkstille ergeben weiterhin `aoi_p95 = 1 ms`. Am Verbraucher ist die Information am Ende fast eine Sekunde alt. Die Kennzahl ist als bedingtes Antwortalter nützlich, trägt aber nicht die übliche AoI-Bedeutung. Die Fachdefinition verwendet zu jedem Zeitpunkt den Erzeugungszeitpunkt der frischesten bereits empfangenen Information. [Yates et al., AoI Survey](https://www.mit.edu/~modiano/papers/CV_J_122preprint.pdf)

Coverage zählt ausschließlich Fenster mit einer neuen frischen Lieferung. Ein älteres, noch ausreichend frisches Ergebnis zählt im nächsten Fenster nicht weiter. Damit ist sie „Fresh-delivery window coverage“, nicht automatisch die Frage „hatte der Regler zum Abtastzeitpunkt brauchbare Information?“. Fensterphase und Messstart beeinflussen das Ergebnis.

**Korrektur:** Antwortalter separat benennen, zeitgewichtete oder regelzyklisch abgetastete AoI messen, Ausfälle und lange Lücken einschließen. Bisherige Coverage behalten, aber semantisch korrekt benennen und um Consumer-Coverage ergänzen. Alle veröffentlichten AoI-Vergleiche neu berechnen.

### F13 · P1 · Die mitgelieferte Triton-Baseline konfiguriert keine gemeinsame begrenzte Ressource

Die Runtime-Konfigurationen, etwa [rfdetr/config.pbtxt:12](/run/media/dd/USB_40281/Projekte/InferenceQoS-runtime/onetimer-vision/rfdetr/config.pbtxt:12), enthalten `rate_limiter { priority: ... }`, aber keine `resources`. Der dokumentierte Start setzt `--rate-limit=execution_count`.

Triton reserviert standardmäßig keine Rate-Limiter-Ressourcen für eine Instanz. Die Ressourcen müssen explizit konfiguriert werden; Prioritäten entscheiden bei Ressourcenknappheit. Auch der Quellcode von `r26.06` allokiert bei leerem Ressourcenbedarf ohne solche Konkurrenz. [Triton Rate Limiter](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/rate_limiter.html), [Implementierung r26.06](https://raw.githubusercontent.com/triton-inference-server/core/r26.06/src/rate_limiter.cc)

**Schlussfolgerung:** Mit diesen Dateien ist eine wirksame gemeinsame Kapazitätsbegrenzung nicht nachgewiesen. Ein global auf einen Slot begrenzender Governor wird mit weitgehend frei konkurrierenden Modellinstanzen verglichen. Ein erheblicher Teil des Vorteils könnte aus der Begrenzung selbst stammen. Wie groß er nach besserer Baseline bleibt, muss gemessen werden. Die historischen Messungen lassen sich mangels versioniertem Runtime-Manifest nicht vollständig auf ihre damalige Konfiguration festlegen.

**Korrektur:** gemeinsame benannte Ressource mit geeignetem Budget für alle relevanten Modelle, Prioritäten, clientseitiges Latest, Queuegrenzen/Timeouts und Batchingoptionen separat optimieren. Den marginalen Nutzen von Supersession, globaler Begrenzung, Priorität, Look-ahead und Variantenwahl durch Ablationen zeigen.

### F14 · P2 · Blockierter Spitzenkandidat hält unabhängige Arbeit auf

In [scheduler.rs:657](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/scheduler.rs:657) beendet ein nicht sofort startbarer Gewinner den gesamten Dispatchversuch. Nur ein Protected-Veto führt dazu, einen anderen Kandidaten zu versuchen.

**Reproduziert:** A läuft; B hat höhere Priorität als C, darf aber nicht mit A koexistieren. Ein zweiter Slot ist frei und C darf neben A laufen. Trotzdem startet C nicht. Das ist vermeidbare Blockierung durch den Queuekopf.

**Korrektur:** aktuell nicht startbare Modelle für diesen Durchlauf überspringen und andere Kandidaten prüfen, sofern deren Ausführung die höher priorisierte Arbeit nicht gefährdet.

### F15 · P2 · Niedrigste Qualität wird fälschlich mit kürzester Laufzeit gleichgesetzt

[variant.rs:171](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/variant.rs:171) setzt `fastest` auf die zuletzt betrachtete Variante. Validiert wird nur absteigende Qualität, nicht die Laufzeitordnung. Der Doctor trifft dieselbe Annahme.

**Reproduziert:** Varianten mit Qualität 1,0/0,9 und Laufzeit 10/20 ms; keine hält die Deadline. Gewählt wird die 20-ms-Variante. Backendwechsel, Quantisierung oder Inputformen können solche Ordnungen erzeugen.

**Korrektur:** kürzeste prognostizierte Fertigstellungszeit explizit bestimmen. Eine niedrigere Qualitätsstufe muss keinen Geschwindigkeitsvorteil haben; gegebenenfalls dominierte Varianten aus der Auswahl entfernen.

### F16 · P1 · Ausgabekompatibilität von Varianten wird nicht validiert

Der Actor verändert nur `model_name`. Die Auswahl prüft Qualität und Laufzeit, aber nicht Eingabenamen, Formen, Ausgabenamen, Postprocessing und Bedeutung. In den Runtime-Beispielen unterscheiden sich bereits die ResNet-Ausgabenamen zwischen `detector_large` und `detector_small`.

**Folge:** Ein Client, der die Ausgabe der besten Variante explizit anfordert, kann nach einem Wechsel einen Backendfehler erhalten. Bei anderer Eingabeauflösung kann der Tensor nicht passen. Bei gleicher Form kann die semantische Interpretation trotzdem falsch sein. Ein `measured`-Qualitätslabel ohne validierte Messreferenz löst das nicht.

**Korrektur:** eine verpflichtende kanonische I/O-Signatur pro logischem Modell. Varianten müssen sie direkt erfüllen oder über ausdrücklich implementierte Adapter abgebildet werden. Reale Varianten auf denselben Daten und mit fachlichen Untergrenzen prüfen.

### F17 · P2 · OIP-Transparenz und mehrere Backends sind nur teilweise umgesetzt

[service.rs:181](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-gateway/src/service.rs:181) fragt Readiness und Metadaten über den Default-Client ab, selbst wenn die Inferenz zum modellspezifischen Backend geht. `model_config` und weitere Verwaltungsaufrufe bilden logische Modellnamen nicht entsprechend ab. Shared-Memory-Registrierungen gehen ebenfalls nur zum Defaultbackend; CUDA-Registrierungen werden zudem nicht wie System-Shm in der Registry gebucht.

**Reproduziert:** Die Inferenzantwort auf `detector` enthält `model_name = detector_large`; die Metadatenantwort verwendet dagegen den logischen Namen. Eine dokumentierte Variantenmetainformation fehlt. Darüber hinaus gehen gRPC-Metadaten durch `request.into_inner()` verloren, etwa Authentifizierung, Tracing und Transportdeadline. Bei Fehlern werden unterschiedliche Backendstatus pauschal zu `Unavailable`.

**Korrektur:** API-Kompatibilitätsmatrix erstellen und alle unterstützten Methoden gegen echte Referenzclients prüfen. Zentrales Routing für Inferenz und Modellmetadaten; klare Semantik für backendübergreifende Shm-Registrierung. Alle Methoden im Trait zu implementieren ist kein vollständiger Kompatibilitätsnachweis. Ein erfolgreicher OVMS-Smoke-Test bleibt ein positiver, enger Integrationsnachweis.

### F18 · P1 · Einige ungültige Zeitwerte deaktivieren still Regeln

[schema.rs:846](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-config/src/schema.rs:846) verwandelt Umwandlungsfehler bei Periode und Maximalalter mit `.ok()` in `None`. Ungültige Verweildauer wird zu null. Andere Werte werden durch sättigende Multiplikation normalisiert. Das widerspricht dem Anspruch, ungültige Konfigurationen abzulehnen.

**Reproduziert:** `max_age_ms = u64::MAX` wird akzeptiert und deaktiviert den Altersschutz. Auch eine Nullperiode und zu viele Lastprofilstufen werden nicht ausreichend zurückgewiesen; überzählige Stufen werden abgeschnitten.

**Korrektur:** jede Umwandlung als validierbaren Fehler behandeln. Zeitgrenzen, Profilsamples, Endpunkte und Quantenbedingungen vollständig vor der Actor-Erzeugung prüfen. Fuzzing muss „erfolgreich validiert => keine Panic und keine still deaktivierte Regel“ abdecken.

### F19 · P2 · Kalibrator misst nicht den allgemein behaupteten Interferenzeffekt

[calibrate.rs:217](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-cli/src/calibrate.rs:217) erzeugt Nebenlast mit derselben Modellvariante. Bei einer einzigen Instanz, wie in den Runtime-Konfigurationen, kann das überwiegend die Queue vor dieser Instanz messen. Das ist nicht gleichbedeutend mit der Laufzeit unter einem anderen gleichzeitig rechnenden Modell.

Paarmessungen laufen nur in einer Richtung: A unter Last von B, nicht zusätzlich B unter Last von A. Die gemessenen Paarwerte unterhalb der Schwelle gehen nicht als spezifische Prognosen ein. Hintergrundlastfehler können unbemerkt den Lastgeber beenden; fehlgeschlagene Messungen werden übersprungen. Der Kalibrator schreibt trotzdem eine Konfiguration und bestätigt Erfolg, ohne das resultierende Profil vollständig zu validieren.

Zusätzlich verwenden `profile` und `calibrate` nur `profiling_targets()` mit einem Defaultendpunkt. Modellspezifische Endpunkte und decoupled/BYTES-Eingaben werden nicht passend bedient. Der beworbene Workflow passt damit gerade zum Vision-plus-Qwen-Aufbau nicht vollständig.

**Korrektur:** explizite Messmatrix über reale Kombinationen, beide Richtungen, kontrollierte Nebenlast, Erfolgsquote, tatsächliche Serverqueues und Transportmodus. Varianten pro Endpoint profilieren. Output vor dem Schreiben validieren; `--out` darf nicht unbemerkt die Eingabe überschreiben. Eine Slotempfehlung wird aktuell trotz entsprechender Beschreibung nicht berechnet.

### F20 · P2 · Nicht alle behaupteten Reglerwirkungen sind angeschlossen

`ProfileHealth::health()` wird außerhalb seiner Tests nicht aufgerufen. `forces_degradation()` und `aggressive_supersession()` werden vom Scheduler nicht genutzt. Die Überlaststufen ändern ab späteren Stufen die Aufnahme nach Klasse, bewirken aber nicht die beschriebenen früheren Maßnahmen.

`verify::unverified_models()` berücksichtigt ausschließlich echte Fingerprint-Mismatches; fehlende oder nicht abfragbare Fingerprints erhöhen die Marge nicht. Der Fingerprint enthält außerdem weder GPU/Power-Modus noch Treiber, Gewichtsinhalt oder tatsächliche Instancekonfiguration. Ein Hardwarewechsel mit gleichen OIP-Metadaten kann daher als „Verified“ erscheinen; Teile dieser Einschränkung sind dokumentiert.

**Korrektur:** jede angekündigte Wirkung mit einem Integrationstest nachweisen oder aus Status/CLI entfernen. Profilstatus präzise als „Metadaten stimmen überein“ ausdrücken, Hardware-/Artefaktidentität im Deploymentmanifest führen und definierte Reaktion auf nicht belastbare Profile einbauen.

### F21 · P2 · Actorlebensdauer und Timer verursachen unnötige Daueraktivität

[actor.rs:240](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-gateway/src/actor.rs:240) schläft bei abgelaufenem `next_wake` nur 1 ms. Der Wert wird beim Tick oder bei leerem System nicht zurückgesetzt. Nach einem einmaligen geplanten Wake läuft der Actor auch ohne Arbeit ungefähr im Millisekundentakt weiter.

Der Actor hält außerdem selbst einen Sender seines Eingangskanals; das Fallenlassen aller externen Handles schließt diesen deshalb nicht. `spawn()` liefert keinen Join-/Shutdown-Handle. `serve()` reagiert nur auf `ctrl_c()`, nicht ausdrücklich auf das übliche Container-SIGTERM mit definierter Drain-Frist.

**Korrektur:** einmalige Timer konsumieren, zukünftige Timer neu berechnen, explizite Shutdownnachricht, überwachte Actor-/Backendtasks und beschränkte Drain-Frist. Das spart auf Edgehardware Leerlaufenergie und macht wiederholtes Starten im Harness kontrollierbar.

### F22 · P1 für Netzbetrieb · Es fehlt eine geschlossene Aufnahme- und Vertrauensgrenze

[service.rs:171](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-gateway/src/service.rs:171) reicht unkonfigurierte Modelle ohne Kreditkontrolle durch. Ein Aufruf des physischen Namens kann damit den Governor umgehen. Clientparameter dürfen die Klasse frei auf `protected` setzen und Verträge lockern. Das kann für vollständig vertrauenswürdige Clients bewusst erlaubt sein, ist aber keine durchgesetzte Ressourcenisolation.

Der Standardstart bindet an `0.0.0.0`; Authentifizierung/Autorisierung und TLS sind nicht implementiert. Verwaltungs- und Shm-Endpunkte werden weitergereicht, soweit das Backend sie zulässt. Compose veröffentlicht zusätzlich die direkten Tritonports. Ein begrenzter Ereigniskanal begrenzt außerdem nur Requestanzahl, nicht den gesamten Payloadspeicher: bereits 1.024 mal 64 MiB sind 64 GiB, vor Queues und Transportpuffern.

**Korrektur:** für die erste Produktionsversion einen klaren lokalen Vertrauensbereich festlegen und technisch erzwingen; alternativ Identitäten und serverseitige Obergrenzen pro Client. Unkonfigurierte Arbeit über ein gemeinsames Best-Effort-Budget führen oder im strikten Modus ablehnen. Bytebudgets, Verbindungs-/Requestlimits, geschützte Verwaltungszugänge und direkten Backendzugriff kontrollieren. Dies folgt aus konkreten offenen Pfaden, nicht aus einer behaupteten Multi-Tenant-Funktion.

### F23 · P2 · Benchmarkläufe sind zeitlich nicht sauber abgeschlossen

[workload.rs:233](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-bench/src/workload.rs:233) startet Inferenz-Futures ohne sie am Ende des Arms vollständig einzusammeln. `drive()` wartet auf Erzeugertasks, nicht auf jede letzte Inferenz. Alte Arbeit kann in den nächsten Arm beziehungsweise in ein neues Soak-Fenster hineinlaufen. Der Pumpenarm wartet dagegen innerhalb seines Sendeloops auf die Antwort.

Coverage speichert Antwortalter auch für Lieferungen außerhalb des betrachteten Zeitfensters, da die Altersliste vor der Fensterprüfung gefüllt wird. Verbindungsaufbau liegt nach Start der Messuhr. Die Lastkurven verändern zugleich Ankunftsrate, Deadline und Maximalalter; sie isolieren daher nicht die Wirkung höherer Last bei festem Produktvertrag.

**Korrektur:** getrennte Warmup-/Mess-/Drain-Phasen, gemeinsame vorab erzeugte Traces, neue Modelle/Queues nur nach belegtem Abschluss, zufällige oder balancierte Reihenfolge der Arme. Rate und Frischevertrag in getrennten Experimenten variieren. Kleine Effekte unter Sättigung sind besonders empfindlich für diese Details.

### F24 · P1 für modellweites Latest · Unterschiedliche Clientkeys umgehen die Supersession

[queue.rs:247](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/queue.rs:247) dokumentiert ausdrücklich: `LATEST` ersetzt modellweit, `LATEST_PER_KEY` nur innerhalb eines Keys. Die Queue normalisiert den Vergleichsscope, ruft aber anschließend `RequestDescriptor::is_superseded_by()` auf. Diese Methode verlangt in [request.rs:237](/run/media/dd/USB_40281/Projekte/InferenceQoS/crates/vig-core/src/request.rs:237) erneut die Gleichheit der ursprünglichen, nicht normalisierten Keys.

**Reproduziert:** Zwei zeitlich aufeinanderfolgende Requests desselben Modells mit unterschiedlichen Keys bleiben beide in einer `LATEST`-Queue. Die Kontrollprüfung mit gleichem Key besteht. Auch das Zurückweisen eines verspätet eintreffenden älteren Frames kann so umgangen werden. Bei Queuekapazität 1 mit `RejectNew` droht stattdessen, dass der neuere Frame abgewiesen wird und der alte verbleibt.

**Korrektur:** Scopevergleich und Altersvergleich konsistent trennen oder den wirksamen Scope einmal verbindlich normalisieren. Policy, beliebige Keys und vertauschte Ankunfts-/Generationsreihenfolge gemeinsam testen; modellweites Latest darf nicht von einer Clientkonvention für Keys abhängen.

## Theoretische Bewertung

### Die Grundidee ist richtig, die gegenwärtige Optimierungsbehauptung zu stark

Auf staleness-toleranten Sensorströmen kann es sinnvoll sein, veraltete Arbeit vor der Inferenz zu verwerfen. Bewusstes Warten kann bei nicht unterbrechbarer Arbeit eine erwartete wichtige Ankunft schützen. Auch eine hochwertigere Variante nur bei ausreichendem Zeitbudget auszuführen ist plausibel.

Die Implementierung kombiniert aber lokale Regeln. Strikte Klassenpriorität plus EDF plus lokal beste Variante minimiert nicht automatisch lexikographisch die Gesamtzahl der Deadlineverletzungen. Beispiel: A hat Deadline 10 ms und Varianten 9/1 ms; B hat Deadline 11 ms und Laufzeit 3 ms. Die lange A-Variante passt für A allein, lässt B aber zu spät enden. A klein und danach B erfüllt beide Deadlines und wäre nach der behaupteten Zielreihenfolge vorzuziehen. Gemeinsame Feasibility ist deshalb auch für die Variantenwahl wichtig.

Ein praktikabler nächster Schritt ist ein kleiner exakter Referenzplaner für begrenzte Testfälle. Er liefert Gegenbeispiele, wann der schnelle Greedy-Scheduler machbare Lösungen übersieht. Für das Produkt kann weiterhin eine schnelle Heuristik genügen, solange ihre Annahmen und gemessenen Grenzen klar sind.

### AoI und fachlicher Nutzen brauchen eine eindeutige Definition

Für Strom i sei `u_i(t)` der größte Generation-Zeitstempel eines bis t empfangenen verwertbaren Ergebnisses. Dann ist `AoI_i(t) = t - u_i(t)`. Bei mehreren Slots muss ein später eintreffendes älteres Ergebnis die Information nicht zurücksetzen. Lange Zeit ohne Ergebnisse gehört in die Verteilung; „kein Ergebnis“ darf nicht „0 ms alt“ bedeuten. [AoI Survey](https://arxiv.org/abs/2007.08564)

Am Regeltakt `t_k` bietet sich `ConsumerCoverage_i = Mittelwert[ AoI_i(t_k) <= A_i ]` an. Ergänzend sollten maximale Ausfalllänge, Peak-AoI, Resultatqualität und Lieferquote berichtet werden. Ein kurzes Antwortalter bei sehr wenigen Antworten erfüllt keine dauerhaft frische Wahrnehmung.

„Neuester Frame“ ist außerdem nur für austauschbare Zustandsbeobachtungen automatisch passend. Ein verworfener älterer Frame kann ein kurzes relevantes Ereignis enthalten. Das ist keine Widerlegung von Latest, sondern eine Einschränkung des fachlichen Vertrags: Event-Erkennung, Tracking, Fusion und Sequenzmodelle benötigen andere Regeln und eigene Qualitätsmessung.

### Quantile plus Marge sind kein Worst-Case-Bound

`max(offline_p99, online_p95) * margin` ist eine Prognoseheuristik. Ein p99 lässt per Definition noch einen Verteilungsschwanz zu; Multiplikation mit 1,10 macht daraus ohne Verteilungsannahmen keine garantierte Schranke. Nach einem Verteilungswechsel kann das Online-p95 dominieren, während seltene Ausreißer ungeschützt bleiben. Laufzeiten sind zudem abhängig von Inputform, Batch, Nachbarmodell und dessen zeitlichem Verlauf; die Zahl belegter Slots zu Beginn ist kein hinreichender Zustandsparameter.

Die Stichprobenzahlen sind klein für Tail-Aussagen. Unter einer idealisierten stationären unabhängigen Verteilung liegt die Wahrscheinlichkeit, in 120 Messungen keinen einzigen Wert aus dem obersten Prozent zu sehen, bei `0,99^120 ≈ 30 %`. Rund 299 Proben würden nur die Chance erhöhen, überhaupt mindestens einen solchen Wert zu sehen; sie liefern noch kein präzises p99. Korrelierte thermische oder GPU-Effekte machen eine einfache Interpretation noch schwächer.

Der MarginController erhöht bei Unterprognose um 10 Punkte und senkt sonst um 1. Abseits von Boden/Decke wäre sein erwarteter Drift `10q - (1-q)`, mit Gleichgewicht bei `q=1/11`. Das beweist keine reale Missrate von 9 %, weil Profilboden und Quantile mitwirken; es zeigt aber, dass die Schrittweiten selbst nicht auf ein 99-%-SLO kalibriert sind.

Empfehlung: zunächst empirische SLOs mit definiertem Lastbereich und Konfidenzintervallen verkaufen. Falls harte Zeitgarantien nötig werden, sind explizite Laufzeit-/Jittergrenzen, Ressourcenisolation und ein dazu passender Machbarkeitsnachweis erforderlich.

### Virtuelle Slots sind keine unabhängigen GPU-Prozessoren

`SlotSet::homogeneous(2, ...)` erlaubt zwei logische Ausführungen; es erzeugt weder zusätzliche GPU-Kapazität noch eine Partition. Das Gateway bindet Slotnummern nicht an konkrete GPU-/Backendinstanzen. Mehrere Anfragen desselben Modells können trotz freier logischer Slots vor dessen einziger Backendinstanz warten. Umgekehrt können unterschiedliche Modelle auf einer GPU teilweise parallel laufen, sich aber sehr verschieden behindern.

CUDA-Streams erlauben mögliche Nebenläufigkeit; deren tatsächliche Ausführung hängt von Ressourcen und Hardware ab. Streamprioritäten garantieren keine bestimmte Ausführungsreihenfolge. Deshalb folgt aus „zwei Slots“ keine zugesicherte Detector-plus-LLM-Isolation. [CUDA Asynchronous Execution](https://docs.nvidia.com/cuda/cuda-programming-guide/02-basics/asynchronous-execution.html)

Für die erste Freigabe ist ein globaler Ausführungskredit mit vollständig kontrolliertem Backendzugang einfacher belastbar. Mehrfachausführung braucht zusätzlich modell-/instanzbezogene Kredite und Messungen der jeweils erlaubten Kombinationen. Hardwarepartitionierung ist eine andere, gesondert zu validierende Option.

### Zwei Schlussfolgerungen im Doctor/Kalibrator sind mathematisch nicht allgemein richtig

**„Best-Effort-Laufzeit > kürzeste geschützte Periode bedeutet niemals ausführbar“:** Das hängt auch von Deadline, Phase, Jitter und gewünschter Abdeckung ab. Das ausführbare Gegenbeispiel hat geschützte Periode 20 ms, Laufzeit 1 ms, Deadline 100 ms und einen Best-Effort-Job von 30 ms. Der aktuelle Core startet ihn selbst. Für enge Deadlines kann die Warnung richtig sein; als allgemeines Unmöglichkeitstheorem ist sie falsch.

**„Einseitige Verlangsamung >= 2 bedeutet keinen Durchsatzgewinn mehr“:** A brauche allein 10 ms, B 100 ms. Parallel verdoppele sich A auf 20 ms, B bleibe bei 100 ms. Beide sind nach 100 statt seriell 110 ms fertig. Der Grenzwert 2 ist unter symmetrischen Annahmen nachvollziehbar, aber kein allgemeines Kriterium. Bei Prioritätsschutz kann bereits eine kleinere Verlangsamung unzulässig sein. Beide Richtungen, absoluten Zeiten und das eigentliche Ziel müssen eingehen.

Auch `Summe(C_i/T_i)/slots <= 1` reicht nicht für Feasibility. Mit kurzen Deadlines kann dieselbe Last lokal unmöglich sein; mit alternativen schnelleren Varianten kann eine Rechnung nur über die besten Varianten unnötig pessimistisch sein. Eine Warnmetrik und ein bewiesenes Unmöglichkeitsurteil sollten unterschiedliche Bezeichnungen erhalten.

### Textfortsetzung an Requestgrenzen ist keine allgemeine LLM-Präemption

Selbst nach der Reparatur von F03 ist Prompt-plus-Text nicht dasselbe wie Fortsetzung eines laufenden Tokenzustands. Tokenisierung an der Verkettungsgrenze, Samplingzustand, Stopkriterien, EOS, Logitsprozessoren und wiederholtes Prefill können sich ändern. `build_quantum()` ersetzt außerdem die Samplingeingaben durch feste Temperatur 0 und verliert zusätzliche Inputs; `absorb()` schätzt Tokens mit Bytes/4. Diese Schätzung erzwingt keine exakte Tokenobergrenze.

Prefix-Caching kann Rechenarbeit sparen, garantiert aber keine Cachetreffer oder konstante Quantumkosten. Ein sinnvolles Kostenmodell benötigt mindestens festen Request-/Prefillanteil plus kontextabhängige Tokenkosten, nicht nur `tokens / tokens_per_second`. Ohne Erhalt der Semantik sollte diese Funktion als eigener resumierbarer Textjobmodus gelten, nicht als transparente Fortsetzung beliebiger generativer Requests.

## Was der bisherige Nachweis trägt

Der Dauerlauf belegt für seine Konfiguration, dass die ausgewertete Fenster-Coverage über acht Stunden ähnlich blieb und der gemessene RSS nur langsam stieg. Das ist nützlich. Er beweist wegen F01 und der engen Queuekonfiguration kein allgemeines Fehlen von Speicherlecks. Wegen F02 ist der Fehlerzähler nicht belastbar. Minutenweise gelesene Margen zeigen nur beobachtete Ausschläge; daraus folgt keine vollständige Zählung aller Zwischenereignisse.

Der Datenpfadvergleich zeigt einen wichtigen Integrationseffekt: große kopierte Tensoren können den Proxy teuer machen, Shared-Memory-Referenzen vermeiden diese Zusatzkopie. Die konkrete 160-µs-Zahl ist an Messaufbau, Transport, Hardware und Last gebunden. Sie gehört als Beispiel in die Dokumentation, nicht als allgemeine Laufzeitprognose für jeden OVMS-/Edge-Einsatz.

Die Vision-Runtime enthält ein tatsächliches RF-DETR-Modell. Pose, Tiefe und der `vlm_main`-Block sind laut Konfiguration ResNet-Laststellvertreter; ihre Ausgaben belegen keine Pose-, Tiefen- oder VLM-Qualität. Der separate Qwen-Versuch ist realer generativer Workload, sein Quantenschluss aber durch F03 eingeschränkt. Es fehlen vergleichbare echte Qualitätsvarianten auf identischen Szenen.

Die publizierten Faktoren sind Verhältnisse **unabgedeckter Fenster**, keine Faktoren schnellerer Inferenz oder eingesparter Hardware. Das muss im Vertrieb ausdrücklich so bezeichnet werden. Ein Vorteil, der wesentlich durch Aushungern anderer notwendiger Ströme entsteht, ist nur dann ein Produktnutzen, wenn der Kunde genau diesen Verlust akzeptiert.

## Wettbewerb und tragfähige Differenzierung

| Alternative | Bereits vorhandene Fähigkeit | Mögliche Nische für OneTimer |
|---|---|---|
| Triton | Modellübergreifende Ressourcensteuerung und Prioritäten; Queuegrenzen, Timeouts und Batching. [Rate Limiter](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/rate_limiter.html), [Batcher](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/batcher.html) | Automatische Capture-Freshness, gemeinsame vorausschauende Auswahl und verständlicher Betrieb vor bestehenden Modellen. Aktuell nicht gegen die stärkste Kombination nachgewiesen. |
| Holoscan | Mehrere Scheduler und aktuelle Mechanismen zur priorisierten Pipelineausführung; Async-Buffer mit Latest-Semantik. [Schedulers](https://docs.nvidia.com/holoscan/sdk-user-guide/components/schedulers) | Ein kleiner Dienst am bestehenden OIP-Endpunkt kann weniger Integrationsaufwand verursachen als eine Pipelineumstellung. Das muss mit Integrationszeit und Kundenstack belegt werden. |
| ROS 2 / GStreamer / Eigenbau | Begrenzte Historie und Lifespan beziehungsweise Verwerfen alter Puffer. [ROS QoS](https://docs.ros.org/en/humble/Concepts/Intermediate/About-Quality-of-Service-Settings.html), [GStreamer Queue](https://gstreamer.freedesktop.org/documentation/coreelements/queue.html) | Gemeinsame Ressourcenentscheidung über mehrere Quellen und Modelle; der stärkere Eigenbauvergleich ist ein zentraler Latest-plus-Prioritätsdispatcher, nicht nur drei unabhängige Pumpen. |
| Clockwork / InferLine | Vorhersagbare Inferenzplanung beziehungsweise Profilierung, Simulation und ressourcen-/latenzbewusste Pipelineplanung. [Clockwork](https://www.usenix.org/conference/osdi20/presentation/gujarati), [InferLine](https://arxiv.org/abs/1812.01776) | Einfache lokale Einführung und Sensorfrische. Planung anhand von Profilen ist als Forschungsansatz nicht neu. |
| REEF | Kernelnahe Unterbrechung und kontrollierte Nebenläufigkeit; das veröffentlichte Artefakt nennt AMD MI50 als unterstützte GPU. [REEF](https://github.com/SJTU-IPADS/reef) | Weniger invasive Standardbackend-Integration. Eine andere Eingriffsebene bedeutet keine nachgewiesene Leistungsüberlegenheit. |
| EdgeServing / RED, 2026 | EdgeServing verbindet Modell-, Early-Exit- und Batchauswahl; RED behandelt dynamische robotische DAGs und Timing unter Modellannahmen. [EdgeServing](https://arxiv.org/abs/2605.05527), [RED](https://arxiv.org/abs/2605.24044) | Zuverlässige Produktintegration und Betrieb bestehender Modelle. Wissenschaftliche Überlegenheit wäre durch eigene Vergleiche zu zeigen. |

Die sinnvolle Positionierung lautet sinngemäß: **„Definierte Wahrnehmungsströme behalten unter Last eine messbare Aktualität auf vorhandener Hardware; Integration und Betrieb übernehmen wir.“** Das ist eine überprüfbare Nutzenhypothese. „Schneller als Triton“, „47-mal besser“, „neue Scheduling-Theorie“ oder „LLM und Wahrnehmung garantiert auf einer GPU“ sind derzeit nicht belastbar.

Der mögliche dauerhafte Vorteil liegt in getesteten Adaptern, Hardware-/Workloadprofilen, nachvollziehbaren Ursachenanalysen, Kundenintegration und validierten Qualitätsgrenzen. Die einzelnen Schedulingregeln allein sind leicht nachzubauen. Ob Kunden dafür zahlen, ist durch die vorliegenden Repositories nicht beantwortet.

## Weg zu einer Produktionsversion

### Zuerst: Korrektheit und ein enger unterstützter Umfang

(Erledigt, siehe „Stand der Umsetzung": F01–F04, F06–F09, F11, F12, F14, F15, F18, F21 teilweise, F24; F05 und F10 teilweise.) F01–F10, F18 und F24 beheben; Quanten und Stateful bis zu vollständigen Integrationstests als experimentell kennzeichnen. Für die erste Freigabe einen klaren Fall wählen: stateless Vision, feste geprüfte I/O-Signaturen, eine kontrollierte Ressourcen-Domäne, dokumentierte Zeitbasis und verbindliche Shared-Memory-Lebensdauer.

Releasebedingungen: keine verlorenen terminalen Ereignisse bei maximalen Queues; keine akzeptierte Konfiguration verursacht eine Panic; kein verbotener Co-Run vor bestätigtem Abschluss; abgesagte wartende Arbeit verschwindet; Backendstillstand führt zu definiertem begrenztem Verhalten. Fuzz-/zustandsbasierte Tests über Ankunft, Completion, Fehler, Duplikate, Cancellation, Überziehung und Shutdown ergänzen. Einen kleinen Referenzplaner gegen die Schedulingheuristik verwenden.

### Danach: Den Nutzen mit korrigierter Messung neu bestimmen

Vergleichsarme: ordentlich begrenzter Triton, Triton plus Latest-Clients, zentraler Latest-plus-Prioritätsdispatcher, vollständiger OneTimer. Ablationen jeweils ohne Look-ahead, ohne Adaptivität und ohne Variantenwahl. Bei allen Armen denselben zulässigen Verlust an Best-Effort- und High-Service festlegen.

Echte Consumer-AoI, maximale Versorgungslücke, Fachqualität, erforderlicher Nebendurchsatz, Energie und E2E-Zeit messen. Neben dem bisherigen Laptop mindestens die vorgesehene Jetson-/Edgeplattform einsetzen. Feste Sensortraces und Vertragswerte, Burst-/Jittertests, thermischer Dauerbetrieb, Queue-Drain, balancierte Armreihenfolge und Unsicherheitsintervalle verwenden. Rohdaten, Commit, Konfiguration, Modellhashes, Image-Digests, GPU-/Power-Modus und Treiber gemeinsam archivieren.

**Go/No-Go:** Der vollständige Governor muss bei gleichem fachlichen Ergebnis und Nebenlastservice einen für Kunden relevanten Vorteil gegenüber dem besten einfachen Arm bringen. Wenn der zentrale Minimaldispatcher fast den ganzen Nutzen erzielt, sollte daraus ein kleineres Produkt oder ein Integrationsangebot werden; mehr Algorithmuskomplexität wäre dann nicht gerechtfertigt.

### Anschließend: Betrieb und Installation freigeben

Überwachte Prozesse und SIGTERM-Drain, getrennte Liveness/Readiness, Deadline-/Altersmetriken pro Modell und Stream, Bytebudgets und definierte Störungsreaktionen ergänzen. Der zentrale Produktnutzen muss im Kundenbetrieb messbar sein; derzeit fehlen gerade die dazu nötigen Coverage-/AoI-Metriken im Liveexport.

Runtime-Konfigurationen versionieren; Gewichte und Containerarchive außerhalb des Quellrepositories über reproduzierbare Manifeste referenzieren. Modellherkunft, Hashes, kompatible Formen und Versionen sowie Nutzungs-/Weitergabebedingungen erfassen. Die Softwareversion allein reproduziert das aktuelle Experiment nicht.

Signierte, fest versionierte x86_64-/aarch64-Artefakte, passende Jetson-/Backend-Matrix, funktionierende Beispielclients für Zeit- und Shm-Semantik, SBOM und reproduzierbare Installation bereitstellen. Ein 72-Stunden-Fehlereinspielungstest und anschließender Wochenlauf auf Zielhardware wären sinnvolle Freigabestufen, ersetzen aber keine Abdeckung der konkreten Fehlerpfade. Ein Rewrite des Rust-Kerns ist dafür nicht notwendig.

## Vertrieb: Was ich konkret als Nächstes tun würde

**Mit einem bezahlten Integrationspilot beginnen.** Zielkunde: ein Team mit mehreren bereits laufenden Wahrnehmungsmodellen auf begrenzter lokaler Hardware, gemessenen Frischeproblemen und einem klaren Verantwortlichen für diesen Stack. Mobile Industrie-/Lagerrobotik und lokale Maschineninspektion sind passende Hypothesen; ein Bedarf an einem OIP-Governor muss im Gespräch bestätigt werden. Reine Einmodellpipelines und Systeme, die nie die Kapazitätsgrenze erreichen, haben nach den eigenen Messungen wenig Anlass zum Kauf.

Als erstes Angebot: Workload vermessen, Minimalalternativen testen, Governor einbauen und gemeinsam akzeptierte Kriterien nachweisen. Beispielsweise eine vom Kunden festgelegte maximale Versorgungslücke und Mindestqualität bei gleichzeitig festgelegtem Nebendurchsatz. Der Pilot liefert dem Kunden Diagnose und einen reproduzierbaren Vergleich, auch wenn die zusätzliche Schedulinglogik sich nicht lohnt.

Für die erste Serie würde ich drei Designpartner suchen und einen vier- bis sechswöchigen Pilotumfang anbieten. Als zu testende **eigene Preisannahme**, nicht als recherchierten Marktpreis, wäre etwa 5.000–15.000 Euro für eng begrenzte Analyse und Integration diskutierbar. Entscheidend ist der nachgewiesene Wert: eingesparte Hardware, vermiedene Ausfälle oder eingesparte Entwicklungszeit. Ohne diesen Nachweis keine belastbare Gerätepreisliste.

Bei erfolgreicher Einführung folgt ein Vertrag für unterstützte Releases, validierte Hardwarekombinationen und Updates; je nach Flottengröße pro Gerät oder als OEM-Jahresvertrag. Den vorhandenen offenen Kern kann man als Einstieg nutzen. Bezahlt werden sollten verlässlich gelieferte Integration, Qualifikation, Support und gegebenenfalls Flottenfunktionen. Die lokale Frischeentscheidung sollte ohne Cloudverbindung funktionieren.

Vertriebsmaterial zuerst technisch: ein reproduzierbares Kundenszenario, korrekt beschriftete Messkurven, vollständiger Preis des Prioritätsschutzes für andere Ströme, Installationsdauer und eine klare Supportmatrix. Geeignete Zugänge sind direkte Gespräche mit Perception-/Edge-Verantwortlichen, Integratoren und Hardwarepartnern. Vor breiter Bewerbung sollten zwei unabhängige Zielsysteme den korrigierten Vorteil zeigen und mindestens ein Pilotkunde bereit sein, dafür zu bezahlen.

**Nächste Entscheidung:** zunächst die nachgewiesenen Freigabeblocker schließen und den Vergleich gegen gemeinsamen Triton-Ressourcenpool plus zentrale Latest-Prioritätssteuerung durchführen. Dessen Ergebnis entscheidet, ob OneTimer ein eigenständiges Produkt, eine kleine Bibliothek oder ein wertvolles Integrationsangebot wird.
