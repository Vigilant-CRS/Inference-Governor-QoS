# Arbeitsstand

Stand: 2026-09-11 · **Gate M3 bestanden, auf Treiber 580.178.04 bestätigt** ·
Ausbaustufe R0 fertig, R1 fertig bis auf die Pakete, die einen Pilotfall
oder eine andere CUDA-Version brauchen · Releasequalifikation fertig bis auf
die Freigabe durch Pilotverantwortliche

Dieses Dokument beschreibt, **was jetzt gilt**. Was einmal galt, steht in den
ADRs (`docs/adr/`) und in der Git-Historie; hier wird es nicht
fortgeschrieben.

## Umfang

| | |
|---|---|
| Rust, ohne Kommentare und Leerzeilen gezählt | ~34 500 Zeilen in 9 Crates |
| Tests | 751 grün, zwei bewusst ignoriert (die Zeitprüfung der Datenpfadbudgets und das große NV-23-Gitter — beide gehören auf die Releasemaschine, nicht auf geteilte CI-Runner) |
| Architekturentscheidungen | 37 ADRs |
| Gate | fmt, clippy `-D warnings`, test, `cargo deny`, `reuse lint`, aarch64 unter Emulation |

Die Crates und ihre Zuständigkeit:

| Crate | Zeilen | Was darin liegt |
|---|---:|---|
| `vig-core` | 12 400 | Der deterministische Scheduling-Kern. Keine Uhr, kein I/O, keine Dependencies. |
| `vig-gateway` | 6 700 | Der OIP-Server, der Single-Owner-Actor, die Backendnaht, der Prometheus-Endpunkt, die Datenpfadbudgets. |
| `vig-bench` | 3 700 | Messläufe gegen echtes Triton: `gate-m3`, `load-ramp` (auch mit Lastspitzen), `frontier`, `soak`, `shm-latency`, `wp26`. |
| `vig-cli` | 2 900 | `vig doctor` / `profile` / `calibrate` / `serve` / `verify`. |
| `vig-config` | 2 800 | Schema, Parser, Validator. Lehnt ab, statt zu reparieren. |
| `vig-platform` | 2 600 | Lesende Hardwarebeobachtung und der Messpfad. Stellt nichts. |
| `vig-sim` | 2 200 | Discrete-Event-Simulator mit bitgleich reproduzierbaren Traces. |
| `vig-backend-triton` | 700 | Der Triton-Adapter samt Fehler- und Nachweislogik. |
| `vig-protocol-oip` | 500 | Die generierten OIP-Typen und die `vig_`-Parameter. |

## Was gemessen belegt ist

**Gate M3, RTX 3070 Laptop (8 GB), Triton 2.70, echte Modelle.** Gegen eine
getunte Baseline — gleiche Modelle, gleiche Instance Groups, gleicher
Shared-Memory-Datenpfad, Rate Limiter mit Prioritäten. Sechs Läufe auf zwei
Treibern ([R03](benchmark/gate-m3-r03.md), 580.173.02;
[R04](benchmark/gate-m3-r04.md), 580.178.04):

| Strom | Triton (getunt) | Vigilant | Unabgedeckte Lieferfenster |
|---|---:|---:|---:|
| detector (RF-DETR) | 84–85 % | 99 % | 20–22x weniger |
| pose | 91 % | 99 % | 10–12x weniger |
| depth | 97–98 % | 90–100 % | kein Gewinn — streut um null |

Der Treiberwechsel ändert an der Aussage nichts. Die Karte läuft auf beiden
Treibern am Software-Leistungslimit, unter 580.178.04 nur etwas höher
getaktet (1830 gegen ~1900 MHz); `vig doctor` sagt das vor jeder Messung
(ADR-0021).

Gegen Tritons **stärkste** Einstellung — global begrenzte gemeinsame
Ressource statt Prioritäten allein — sind es beim Detektor 12,9–15,1x; die
Pose fällt dabei auf 78 %. Diese Messung stammt von vor der Korrektur der
Lückenrechnung und ist seitdem nicht wiederholt.

Der VLM-Strom steht in derselben Tabelle bei 0 % Abdeckung. Das ist kein
Messfehler, sondern ADR-0012: ein nicht unterbrechbarer Block, der länger
dauert als die kürzeste geschützte Periode, startet unter Last nie. Dafür
gibt es die kooperative Zerlegung (ADR-0014); was sie kostet, sagt seit NV-16
`vig doctor`, und der Kontextterm ist gegen ein echtes vLLM-Backend gemessen
(0–5 µs je Kontexttoken mit Prefix-Cache, 35–39 ohne,
[Messung](benchmark/nv16-prefill.md)).

**Eine Lückenzahl aus einem einzelnen Lauf ist keine Aussage.** Sie ist ein
Maximum über ein 30-Sekunden-Fenster, und Maxima streuen — bei der Pose auf
Tritonseite 53 bis 91 ms zwischen zwei Läufen derselben Konfiguration. Die
Abdeckung, die über hunderte Perioden mittelt, tut es nicht.

**Mit TensorRT statt ONNX Runtime** wird die Baseline schneller: serialisierte
Auslastung 103 → 76 %, der Vorsprung des Governors halbiert sich (24,7x →
13,3x). Der Engpass bleibt: bei 76 % verfehlt ein getunter Triton weiter
jeden zehnten Detektorzyklus ([Messung](benchmark/tensorrt.md)).

**Messkette vom 11.09.** ([Bericht](benchmark/messkette-2026-09-11.md)) —
was hält und was nicht, auf dem neuen Treiber und mit `TCP_NODELAY` in den
Werkzeugen:

- **Stationäre Rampe:** Über 100 % Last verfehlt Triton 342–500 ‰ der
  Detektorperioden, der Governor 4–22 ‰ (21–125x); die nachrangigen Ströme
  zahlen dafür. **Genau bei 100 %** verfehlt Triton nichts, der Governor
  verwirft 165 ‰ eines nachrangigen Stroms. Die Marge ist es nicht: in der
  Simulation derselben Last entscheidet sie an der Kante nichts. Nächster
  Kandidat ist die Lücke zwischen zwei Aufträgen ohne Pipelining.
- **Lastspitzen (Spec 19.4):** kein Gewinn (−1,1x, 1,2x) und bei langen
  Spitzen über niedriger Grundlast ein Verlust (41 gegen 11 ‰). Die längste
  Lücke hält er kürzer (12–14 gegen 21–22 ms). Ursache in Analyse; das
  Szenario selbst wird überarbeitet ([Szenarien](benchmark/scenarios.md), S7).
- **Variantenwahl (Spec 19.7):** Bei 150 % hält die automatische Wahl den
  Strom (2 gegen 501 ‰ der großen Variante), bei 110–125 % schaltet sie zu
  spät (66–84 ‰), bei 90 % gar nicht (143 ‰). Ursache in Analyse. Die echten
  RF-DETR-Varianten geben keinen Betriebspunkt her
  ([Messung](benchmark/rfdetr-variants.md)); gemessen ist der Mechanismus,
  nicht die Qualität.
- **Datenpfad (NV-20): bestanden.** Shm-Zusatz +159 µs gegen Triton, +210 bis
  +236 µs gegen den Mock, größenunabhängig; alle budgetierten Zeilen PASS
  ([datapath-budgets.md](datapath-budgets.md)).

**Planbarkeit im begrenzten Modell (NV-23).** Unter benannten Annahmen —
ein Slot, ein geschützter Strom, Laufzeiten innerhalb des Plans, Jitter J —
ist das Alter jedes geschützten Frames höchstens `2J + D`, und die längste
Versorgungslücke ist nach oben begrenzt. Bewiesen, gegen den echten
Scheduler per Gittersuche ohne Gegenbeispiel geprüft; die Schranke wird
exakt erreicht. Die Suche hat die erste Fassung des Satzes widerlegt und drei
Schwächen des Look-ahead gefunden, die seit
[ADR-0036](adr/0036-the-look-ahead-counts-from-the-capture.md) behoben sind:
er gab verspätete Frames auf (jetzt nicht mehr, sofern der Vertrag eine
Jitterhülle nennt), seine Deadline zählte ab Ankunft statt Aufnahme (die
Schranke verliert damit die Transportzeit `δ`), und er sah fest 100 ms voraus
(jetzt jede nächste Ankunft eines bewachten Stroms)
([Analyse](analysis/nv23-bounded-claim.md)).

**Der Kern auf ARM.** Auf echten ARM-Kernen (Pixel 2 und Pixel 5, A53- bis
A76-Klasse) kostet ein Scheduling-Ereignis im Gate-M3-Satz p99 3–20 µs, mit
32 Modellen bis 150 µs. Eine Entscheidung braucht auf dem ältesten Kern
0,08 % einer 33-ms-Periode ([Messung](benchmark/arm-phones.md)). Das ist
der Kern, nicht die Inferenz — auf ARM ist keine gemessen.

## Fertige Ausbaustufe R0

| Paket | Was es ändert | ADR |
|---|---|---|
| NV-00 | Slotkredite enden durch Nachweis, nicht durch Frist | — |
| NV-01 | Verbraucherabdeckung, zeitgewichtetes AoI, längste Lücke — getrennt von den Legacy-Lieferfenstern | — |
| NV-02 | Versionierte Vertragszusätze, Weakly-hard-Monitor, Freigabeliste | [0020](adr/0020-contract-extensions-are-additive-and-versioned.md) |
| NV-03 | Profilmanifest: Artefakt-Digest, Runtime, Gerät, Aufteilung, Gültigkeitsdomäne | [0019](adr/0019-profile-identity-beyond-a-metadata-hash.md) |

## Ausbaustufe R1

Vier Zustände, nicht zwei: **gebaut**, **erreichbar**, **angeschlossen**,
**qualifiziert** ([Support-Matrix](support-matrix.md)).

| Paket | Stand | ADR |
|---|---|---|
| NV-04 Hardwarebeobachtung | fertig, nur lesend, kein Root; gestartet von `vig serve`, nicht vom Scheduler | [0021](adr/0021-hardware-is-read-never-set.md) |
| NV-05 Messpfad | fertig: absolutes Freigaberaster, vier Zähler, Uhrprüfung | [0022](adr/0022-measurement-is-a-method-not-a-loop.md) |
| NV-06 Prognose v2 | **erreichbar** über `backend.prediction: active`, Voreinstellung Schatten. Scharf ohne Marge brach die Zusage (Detektor bis 90 %) — korrigiert; mit Marge in sechs Läufen gleichauf mit dem Schatten (99 %), aber ohne Gewinn auf Gate M3. Voreinstellung bleibt Schatten | [0023](adr/0023-state-aware-prediction-runs-in-the-shadow-first.md), [Messung](benchmark/nv06-ab.md) |
| NV-07 Backendnaht | Naht und Fake-Executor fertig; Crate-Verschiebung und OIP-freie Nutzlast bewusst aufgeschoben | [0024](adr/0024-the-backend-is-a-seam-not-a-type.md) |
| NV-08 TensorRT über Triton | fertig, gemessen — kein Codepfad nötig | [benchmark/tensorrt.md](benchmark/tensorrt.md) |
| NV-09 TensorRT Direct | **nicht gebaut**: Durchstich gemessen, 500–730 µs je Inferenz; das trägt keinen nativen Executor | [0033](adr/0033-native-code-lives-in-the-backend-process.md), [Durchstich](spikes/nv09-tensorrt-direct.md) |
| NV-10 Semantik der Varianten | fertig | [0025](adr/0025-the-same-shape-is-not-the-same-meaning.md) |
| NV-11 Gerichtete Interferenz | erreichbar über `backend.interference`; auf dieser Maschine nicht messbar — der Takt wandert während jeder Reihe | [0026](adr/0026-interference-is-directed-and-not-additive.md), [Messung](benchmark/interference.md) |
| NV-12 CUDA-Graphs | **abgeschlossen, negativ**: 3,7 % weniger p50, mehrere Modelle mit Graphs laden nicht mehr | [0033](adr/0033-native-code-lives-in-the-backend-process.md), [Messung](benchmark/cuda-graphs.md) |
| NV-13 Energieregler | erreichbar, opt-in, beobachtet statt angenommen; auf dieser Maschine fehlen die Rechte | [0030](adr/0030-actuation-is-an-exception-and-must-be-observed.md) |
| NV-14 Green Contexts | **abgeschlossen, negativ für den Engpass**: begrenzt SMs, schützt nicht gegen Bandbreite, löst kein Zeitproblem | [0033](adr/0033-native-code-lives-in-the-backend-process.md), [Qualifikation](spikes/nv14-green-contexts.md) |
| NV-15 XSched | **gemessen**: die scheinbare CUDA-13-Blockade war eine zweite `libcuda` im Prozess; mit `CUXTRA_CUDA_LIB` und einem Patch für die Level-2-Queue auf sm86 läuft Präemption ([Einrichtung](../deploy/xsched/README.md)). Triton + XSched hält die geschützten Ströme bei 100 % und lässt das VLM laufen; der Governor mit präemptierbarer Lane und R = 4 ms zieht gleich (alle Ströme 100 %, Detektor frischer, VLM etwas älter). R ist **nicht gemessen** — `vig calibrate` verwarf jede Reihe wegen wandernden Takts; 4 ms sind aus den Läufen geschätzt ([Messung](benchmark/messkette-2026-09-11.md#xsched-und-präemption-nv-15), [ADR-0035](adr/0035-preemption-is-a-measured-backend-property.md)) | [0033](adr/0033-native-code-lives-in-the-backend-process.md), [Spike und Nachtrag](spikes/nv15-xsched.md) |
| NV-16 Fortschrittskosten | fertig und gemessen | [0031](adr/0031-a-re-prefill-is-not-free-progress.md), [Messung](benchmark/nv16-prefill.md) |
| NV-17 Gültigkeitsbewusster DAG | **erreichbar**: der Client nennt `vig_capture_id` und `vig_depends_on`; eine Zusammenführung über Aufnahmegrenzen wird abgelehnt, bevor sie rechnet. Der Fehler, dass nach 256 Aufnahmen je Prozess jede weitere abgelehnt wurde (gefunden live von der ROS-2-Brücke), ist behoben: jedes Ende schließt seinen Knoten, ein Test liefert 2000 Aufnahmen aus. Kennungen gelten je Aufrufer, mit Kontingent je Identität | [0028](adr/0028-a-fusion-needs-a-common-capture.md), [Clientparameter](getting-started.md#results-from-the-same-capture) |
| NV-18 Anwendungssemantik | erreichbar: ein Hinweis darf verschärfen, nie lockern | [0029](adr/0029-a-hint-may-tighten-never-loosen.md) |
| NV-24 Missbudget in Entscheidungen | erreichbar, Voreinstellung **aus** | [0027](adr/0027-a-miss-budget-that-decides-not-only-observes.md) |

**Erreichbar ist nicht qualifiziert.** Ein erreichbares Paket hat einen
dokumentierten Schalter und einen Test, der zeigt, dass der Schalter eine
Entscheidung ändert. Ob das Einschalten auf einer bestimmten Last besser ist,
sagt erst eine Messung — und NV-06 zeigt, warum diese Unterscheidung keine
Förmelei ist: der Schalter war erreichbar, getestet, und hat auf der ersten
echten Last die Kernzusage gebrochen.

## Die `unsafe`-Frage ist entschieden

[ADR-0033](adr/0033-native-code-lives-in-the-backend-process.md): der
Governorprozess bleibt frei von `unsafe`, ohne Ausnahme und ohne Feature-Flag.
Nativer Code, der die GPU berühren muss, gehört in den Backendprozess — dem
die GPU ohnehin gehört. Für XSched ist das keine Theorie: sein Shim sitzt in
Triton, nicht im Governor, und eine FFI im Governor säße im falschen Prozess.

Die Folge für die vier Pakete: NV-09 wird nicht gebaut, NV-12 und NV-14 sind
auf dieser Plattform mit negativem Ergebnis abgeschlossen, NV-15 ist die
einzige Präemption, die den Engpass angreift — und sie läuft seit dem
11.09. unter dem qualifizierten Triton.

**Wie.** Am Vormittag sah es nach einer Blockade durch CUDA 13.3 aus; das
war falsch zugeordnet. Die vorkompilierte `cuxtra` lud eine zweite `libcuda`
in den Prozess — die des Hosts, die CDI in den Container einblendet — neben
der Compat-Bibliothek, die Triton benutzt. Mit `CUXTRA_CUDA_LIB` auf dieselbe
Bibliothek und einem Patch von neun Zeilen für die Level-2-Queue auf sm86
läuft XSched unter Triton 26.06. Keine Zeile davon liegt im Governor —
genau der Weg, den ADR-0033 beschreibt.

## Was ausdrücklich noch nicht angeschlossen ist

**Die zustandsabhängige Prognose (NV-06)** ist erreichbar, entscheidet aber
per Voreinstellung nichts. Mit Marge ist `active` sicher, auf Gate M3 aber ohne Gewinn ([Messung](benchmark/nv06-ab.md)); ob es auf einer Last mit Varianten nützt, zeigt die Frontier-Messung.

## Die offenen Arbeitspakete

| Paket | Stand | Was fehlt |
|---|---|---|
| NV-15 XSched | gemessen, Gleichstand mit Triton + XSched | R messen statt schätzen: mit festem Takt (braucht Rechte) oder online aus dem Betrieb (ADR-0038). Die Antwort auf die Frage vom 11.09.: Die Lane schützt den Detektor (100 %), und das VLM bekommt unter dem Governor erstmals vollen Fortschritt (100 %). |
| Kante bei 100 % | Ursache offen; die Marge ist es nicht (Simulation) | Bei 100 % verliert Triton nichts, der Governor 165 ‰ eines nachrangigen Stroms. In der Simulation derselben Last liefern feste 110 %, feste 100 % und eine gelernte Marge bis 105 % dieselbe Abdeckung. Nächster Kandidat: die Lücke zwischen zwei Aufträgen bei `pipelining_depth: 0`; Messung nach dem Dauerlauf. Die gelernte Marge (ADR-0038) wird nach dem Look-ahead-Fix nachgeprüft. |
| Lastspitzen und Variantenwahl | Schwäche gemessen, Analyse läuft | Lange Spitzen über niedriger Grundlast: 41 gegen 11 ‰. Variantenwahl bei 90 % ohne Herunterschalten, bei 110–125 % zu spät ([Messung](benchmark/messkette-2026-09-11.md)). |
| Externer Review vom 11.09. | Skriptbefunde behoben, Produktbefunde in Arbeit | R01–R05: Abschlussabgleich nach Backend-Neustart, Identität von Endpunkt und Version, Pufferlebensdauer in Pilot und ROS-Brücke, Sampling-JSON ([Review](reviews/2026-09-11-runtime/REVIEW.md)). R06–R08 behoben. |
| Zweiter Betriebspunkt | offen | Zwei Ausführungseinheiten (`slots: 2`, Instance Groups mit zwei Instanzen) samt gemessener Parallelprofile. Die Lastrampe sagt selbst, dass sich ihre Kante damit verschiebt. |
| NV-22 mehrere Ressourcendomänen | **erreichbar, nicht qualifiziert** ([ADR-0037](adr/0037-a-domain-is-a-gpu-with-one-owner.md)) | Gebaut: ein Scheduler je GPU (`backend.domains`, `domain:` am Modell), feste Zuordnung, kein Failover, Kennzahlen je Domäne, `vig doctor` je GPU; mit zwei Fake-Executoren belegt, dass eine belegte oder ausgefallene GPU der anderen weder Slot noch Kredit nimmt. Es fehlt: eine zweite GPU für die Qualifikation (Interferenz über PCIe, Hauptspeicher, Leistungsbudget), Shared-Memory-Registrierung an allen Endpunkten, Domänen in `vig calibrate`. Erledigt aus diesem Block: NV-21 ([ROS-2-Brücke](integrations/ros2.md)) und NV-23 ([begrenzter Nachweis](analysis/nv23-bounded-claim.md)). |

## Offen für eine Produktionsfreigabe

- **NV-19 — ein Entwicklungspartner.** Das einzige Paket, das nicht durch Code
  zu erledigen ist, und die Voraussetzung für einen qualifizierten Einsatz
  von NV-17 und NV-18. Ohne einen benannten Lastfall und eine benannte
  Hardware ist jeder weitere Ausbau eine Vermutung.
- **NV-20 — Releasequalifikation.** Fertig bis auf das, was eine Person
  braucht:

  | | |
  |---|---|
  | Feature- und Hardwarematrix | [support-matrix.md](support-matrix.md) |
  | Runbook, Recovery- und Supportgrenzen | [runbook.md](runbook.md) |
  | Fehlerinjektion, 11 Fehlerbilder ohne GPU | `crates/vig-gateway/tests/fault_injection.rs` |
  | Update und Rollback | geprüft; brachte einen echten Fehler zutage (`b6f0777`) |
  | Rechteliste, Modellverwaltung, Offlinebetrieb | [support-matrix.md](support-matrix.md) |
  | Installationspfad mit Bereitschaftsprüfung | `deploy/docker-compose/` |
  | SBOM, signierbare Artefakte, `cargo auditable` | `.github/workflows/release.yml` |
  | Dauerlauf auf dem freizugebenden Stand | bestanden, 8 Stunden ([soak.md](benchmark/soak.md)) |
  | Datenpfadbudgets | [datapath-budgets.md](datapath-budgets.md) — definiert; Urteil auf ruhiger Maschine ausstehend |
  | Freigabe durch Pilotverantwortliche | offen, braucht NV-19 |
- **Zweite Hardware.** Die Logik ist portabel, die Zahlen sind es nicht. Auf
  `aarch64` ist der Kern unter Emulation gebaut und getestet; über Laufzeit,
  Durchsatz und Interferenz auf Jetson sagt das nichts
  ([Hardwarequalifikation](hardware-qualification.md)).

## Zuletzt gemessen

**11.09.2026, Treiber 580.178.04.** Gate M3 dreimal ([R04](benchmark/gate-m3-r04.md)),
NV-06 scharf gegen Schatten ([nv06-ab.md](benchmark/nv06-ab.md)),
weitere Messungen des Tages laufen auf ruhiger Maschine.

**8-Stunden-Dauerlauf (2026-09-09/10), bestanden.** Keine Kennzahlendrift:
erste gegen letzte Stunde alle Werte innerhalb von 1 %. Kein Fehler, keine
Panic. Speicher 12 640 → 15 912 kB, davon 2,5 MB im Anlauf der ersten Stunde;
danach +748 kB über sieben Stunden ohne erkennbaren Trend. Kein unbegrenztes
Wachstum im beobachteten Fenster; „kein Leck" leiten wir daraus nicht ab
([soak.md](benchmark/soak.md)).
