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
| Tests | 628 grün, einer bewusst ignoriert (die Zeitprüfung der Datenpfadbudgets — sie gehört auf die Releasemaschine, nicht auf geteilte CI-Runner) |
| Architekturentscheidungen | 33 ADRs |
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

**Lastspitzen (Spec 19.4).** _Neumessung läuft (11.09.), Ergebnis folgt._

**Lastrampe auf dem neuen Treiber.** _Neumessung läuft (11.09.), Ergebnis folgt._

**Variantenwahl (Spec 19.7).** _Neumessung läuft (11.09.), Ergebnis folgt._ Die echten RF-DETR-Varianten
geben dafür keinen Betriebspunkt her: die Auflösung bestimmt die Laufzeit,
das Modell fast nicht ([Messung](benchmark/rfdetr-variants.md)).

**Datenpfad (NV-20).** _Neumessung läuft (11.09.), Ergebnis folgt._

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
| NV-06 Prognose v2 | **erreichbar** über `backend.prediction: active`, Voreinstellung Schatten. Scharf ohne Marge brach die Zusage — korrigiert. _Neumessung läuft (11.09.), Ergebnis folgt._ | [0023](adr/0023-state-aware-prediction-runs-in-the-shadow-first.md), [Messung](benchmark/nv06-ab.md) |
| NV-07 Backendnaht | Naht und Fake-Executor fertig; Crate-Verschiebung und OIP-freie Nutzlast bewusst aufgeschoben | [0024](adr/0024-the-backend-is-a-seam-not-a-type.md) |
| NV-08 TensorRT über Triton | fertig, gemessen — kein Codepfad nötig | [benchmark/tensorrt.md](benchmark/tensorrt.md) |
| NV-09 TensorRT Direct | **nicht gebaut**: Durchstich gemessen, 500–730 µs je Inferenz; das trägt keinen nativen Executor | [0033](adr/0033-native-code-lives-in-the-backend-process.md), [Durchstich](spikes/nv09-tensorrt-direct.md) |
| NV-10 Semantik der Varianten | fertig | [0025](adr/0025-the-same-shape-is-not-the-same-meaning.md) |
| NV-11 Gerichtete Interferenz | erreichbar über `backend.interference`; auf dieser Maschine nicht messbar — der Takt wandert während jeder Reihe | [0026](adr/0026-interference-is-directed-and-not-additive.md), [Messung](benchmark/interference.md) |
| NV-12 CUDA-Graphs | **abgeschlossen, negativ**: 3,7 % weniger p50, mehrere Modelle mit Graphs laden nicht mehr | [0033](adr/0033-native-code-lives-in-the-backend-process.md), [Messung](benchmark/cuda-graphs.md) |
| NV-13 Energieregler | erreichbar, opt-in, beobachtet statt angenommen; auf dieser Maschine fehlen die Rechte | [0030](adr/0030-actuation-is-an-exception-and-must-be-observed.md) |
| NV-14 Green Contexts | **abgeschlossen, negativ für den Engpass**: begrenzt SMs, schützt nicht gegen Bandbreite, löst kein Zeitproblem | [0033](adr/0033-native-code-lives-in-the-backend-process.md), [Qualifikation](spikes/nv14-green-contexts.md) |
| NV-15 XSched | **blockiert**: Level 2 wirkt auf dieser Karte mit CUDA 12.4; unter Triton 26.06 (CUDA 13.3) stürzt jeder Prozess beim Anlegen der ersten Queue ab | [0033](adr/0033-native-code-lives-in-the-backend-process.md), [Spike und Nachtrag](spikes/nv15-xsched.md) |
| NV-16 Fortschrittskosten | fertig und gemessen | [0031](adr/0031-a-re-prefill-is-not-free-progress.md), [Messung](benchmark/nv16-prefill.md) |
| NV-17 Gültigkeitsbewusster DAG | **erreichbar**: der Client nennt `vig_capture_id` und `vig_depends_on`; eine Zusammenführung über Aufnahmegrenzen wird abgelehnt, bevor sie rechnet | [0028](adr/0028-a-fusion-needs-a-common-capture.md), [Clientparameter](getting-started.md#results-from-the-same-capture) |
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
einzige Präemption, die den Engpass angreift — und sie ist heute blockiert.

**Warum blockiert.** XSched wählt für sm86 immer eine Queue, deren
Konstruktor Befehlsspeicher über nicht dokumentierte Treiberinterna
(`cuxtra`) anlegt. Mit der CUDA-12.4-Laufzeit geht das; mit der 13.3-Laufzeit
des qualifizierten Triton stürzt es ab, mit jeder `libcuda` und mit beiden
Präemptionsimplementierungen. Der Upstream hat seit dem gepinnten Stand
keinen Commit. Die zwei Wege — ein Triton-Release mit CUDA 12 oder
CUDA-13-Unterstützung im Upstream — sind beide keine Codezeile im Governor.

## Was ausdrücklich noch nicht angeschlossen ist

**Die zustandsabhängige Prognose (NV-06)** ist erreichbar, entscheidet aber
per Voreinstellung nichts. _Neumessung läuft (11.09.), Ergebnis folgt._

## Die offenen Arbeitspakete

| Paket | Stand | Was fehlt |
|---|---|---|
| NV-15 XSched | blockiert | CUDA-13-Unterstützung im Upstream oder ein qualifizierter Stack mit CUDA 12. Das Startskript für zwei Tritonprozesse unter XSched liegt bereit (`InferenceQoS-runtime/xsched-triton.sh`), `gate-m3` fährt mehrere Backendprozesse gleichzeitig. |
| Zweiter Betriebspunkt | offen | Zwei Ausführungseinheiten (`slots: 2`, Instance Groups mit zwei Instanzen) samt gemessener Parallelprofile. Die Lastrampe sagt selbst, dass sich ihre Kante damit verschiebt. |
| NV-21/22/23 | optional / Forschung | Ein weiterer Backendadapter, mehrere Ressourcendomänen, formale Analyse. |

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
  | Datenpfadbudgets | [datapath-budgets.md](datapath-budgets.md) — _Neumessung läuft (11.09.), Ergebnis folgt._ |
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
