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
| Tests | 822 grün, sechs bewusst ignoriert (die Zeitprüfung der Datenpfadbudgets, das große NV-23-Gitter, zwei Messmatrizen der Margenkalibrierung, die Tabelle der Lastspitzen-Analyse und die des Versorgungsschutzes — sie gehören auf die Releasemaschine, nicht auf geteilte CI-Runner) |
| Architekturentscheidungen | 40 ADRs |
| Gate | fmt, clippy `-D warnings`, test, `cargo deny`, `reuse lint`, aarch64 unter Emulation |

Die Crates und ihre Zuständigkeit:

| Crate | Zeilen | Was darin liegt |
|---|---:|---|
| `vig-core` | 12 400 | Der deterministische Scheduling-Kern. Keine Uhr, kein I/O, keine Dependencies. |
| `vig-gateway` | 6 700 | Der OIP-Server, der Single-Owner-Actor, die Backendnaht, der Prometheus-Endpunkt, die Datenpfadbudgets. |
| `vig-bench` | 3 700 | Messläufe gegen echtes Triton: `gate-m3`, `load-ramp` (auch mit Lastspitzen), `frontier`, `soak`, `shm-latency`, `wp26`. |
| `vig-cli` | 3 500 | `vig autotune` / `init` / `doctor` / `profile` / `calibrate` / `serve` / `verify`. |
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

**Mit öffentlichen Modellen nachgefahren — und dort gewinnt der Governor
nicht.** Alle Zahlen oben stammen von Modellen, die niemand ausserhalb hat.
`tools/repro/run.sh` fährt dieselben Vergleiche mit RT-DETR R18/R50 und
Qwen3-0.6B (alle Apache-2.0, Digests gepinnt,
[Anleitung](benchmark/reproduce.md)). Das Ergebnis vom 14.09.2026, drei Läufe
je Lastfall:

| Lastfall | Ergebnis |
|---|---|
| Detektor + Sprachmodell | **direkt gewinnt doppelt**: 100 % gegen 33 % Detektorabdeckung *und* 26 gegen 20 Berichte |
| vier Kameras auf einen Detektor | **nicht messbar** auf dieser Karte — 29 verworfene Messreihen |
| zwei Detektorgrößen | **Governor verliert**: 89–94 % gegen 99–100 %, Faktor −52x bis −60x |

Das widerspricht Gate M3 nicht, sondern bestätigt dessen Geltungsbereich: die
öffentlichen Lastfälle landen bei 47 % und 91 % geschützter Auslastung, Gate
M3 misst bei 103 %, und der Knick liegt laut [load-ramp](benchmark/load-ramp.md)
zwischen 100 und 110 %. Unterhalb der Sättigung kostet der Governor nur
seinen Aufwand — genau das sagen die Startseite unter „When not to use it"
und `vig-fit` in zwei von drei Läufen wörtlich.

**Belegt ist damit:** die Werkzeuge laufen mit fremden Modellen, und sie
zeigen ihre eigene Grenze zuverlässig an. **Offen bleibt:** die Kernaussage im
Überlastbereich mit öffentlichen Modellen. Dafür fehlt ein Paar, dessen
Auslastung auf einer 8-GB-Laptopkarte über 100 % kommt, ohne dass die
Profilmessung am wandernden Takt scheitert. Die Lücke ist in der Anleitung
benannt.

**Messkette vom 11.09.** ([Bericht](benchmark/messkette-2026-09-11.md)) —
was hält und was nicht, auf dem neuen Treiber und mit `TCP_NODELAY` in den
Werkzeugen:

- **Stationäre Rampe:** Über 100 % Last verfehlt Triton 342–500 ‰ der
  Detektorperioden, der Governor 4–22 ‰ (21–125x); die nachrangigen Ströme
  zahlen dafür. **Genau bei 100 %** verfehlt Triton nichts, der Governor
  verwirft 165 ‰ eines nachrangigen Stroms. Die Marge ist es nicht: in der
  Simulation derselben Last entscheidet sie an der Kante nichts. Nächster
  Kandidat ist die Lücke zwischen zwei Aufträgen ohne Pipelining.
- **Lastspitzen (Spec 19.4):** gemeldet war kein Gewinn (−1,1x, 1,2x) und bei
  langen Spitzen über niedriger Grundlast ein Verlust (41 gegen 11 ‰). Das
  ist die Fenstersicht, und die misst unter Spitzen vor allem Phase: im
  Simulator verfehlen Governor und FIFO dort gleichermaßen 6–10 % der
  Fenster, in der Verbrauchersicht keine einzige Abtastung
  ([Analyse](analysis/bursts-and-frontier.md)). Ob der Governor unter
  Spitzen schlechter versorgt, ist damit offen; `load-ramp` zeigt jetzt beide
  Sichten. Das Szenario selbst wird überarbeitet
  ([Szenarien](benchmark/scenarios.md), S7).
- **Variantenwahl (Spec 19.7):** Bei 150 % hält die automatische Wahl den
  Strom (2 gegen 501 ‰ der großen Variante), bei 110–125 % verfehlte sie
  66–84 ‰, bei 90 % 143 ‰. Ursache belegt und behoben: die Wahl prüfte die
  Deadline (`1,5 P`), nicht die Versorgung (`max_age − P`); der Simulator
  trifft 71 und 84 ‰, nach der Korrektur 0 ‰ wie die kleine Variante
  ([Analyse](analysis/bursts-and-frontier.md)). GPU-Bestätigung steht aus.
  Die echten RF-DETR-Varianten geben keinen Betriebspunkt her
  ([Messung](benchmark/rfdetr-variants.md)); gemessen ist der Mechanismus,
  nicht die Qualität.
- **Datenpfad (NV-20): bestanden.** Shm-Zusatz +159 µs gegen Triton, +210 bis
  +236 µs gegen den Mock, größenunabhängig; alle budgetierten Zeilen PASS
  ([datapath-budgets.md](datapath-budgets.md)).

**Messkette vom 12.09.** ([Bericht](benchmark/messkette-2026-09-12.md)) —
dieselbe Maschine, Stand mit allen Korrekturen, in 430 Wächterproben eine
einzige mit Fremdlast:

- **Gate M3 hält:** Detektor 85 gegen 100 %, Pose 91–92 gegen 100 %.
- **Die Kante bei 100 % ist erklärt:** Es war die Lücke zwischen zwei
  Aufträgen. Mit `pipelining_depth: 1` sinkt der Verlust des nachrangigen
  Stroms von 188 auf 17 ‰, der Vorsprung des Detektors bleibt (78,5x bei
  110 %).
- **Die Variantenwahl ist behoben:** 0 ‰ über alle Lastpunkte, und bis 75 %
  läuft weiter die große Variante.
- **Lastspitzen sind kein Nachteil:** in der Verbrauchersicht 0 ‰ auf beiden
  Seiten; die alte Zahl war die Fenstersicht.
- **Die gelernte Marge** holt bei einem doppelt zu langsamen Profil bis 110 %
  fast alles zurück (Detektor 15 statt 150 ‰) und schadet bei 125 % (479
  statt 181 ‰). Voreinstellung bleibt aus.
- **Der Pilot** ist zum ersten Mal vollständig gelaufen: relativ gewinnt der
  Governor in jedem Lastpunkt (Alarm p95 1343 gegen 1675 ms, Trefferquote 133
  gegen 106 ‰), absolut verfehlt er seine Kriterien, und der Berichtspfad
  verhungert ohne Präemption.

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
der Kern, nicht die Inferenz.

**Eine zweite GPU, ein zweites Backend (NV-25).** Derselbe Governor vor
TFLite auf der Adreno 540 eines Pixel 2, alles auf dem Telefon
([Messung](benchmark/android-gpu.md), [ADR-0039](adr/0039-a-second-backend-proves-the-seam.md)).
Die Logik läuft unverändert; der Einbruch des ungesteuerten Backends aus
Gate M3 tritt dort bei geplanten 138 % nicht ein, weil sich Transport und
CPU-Anteile überlappen und die GPU nicht voll ist. Der Governor hält die
längste Lücke des Detektors um ein Viertel kürzer und bezahlt mit älteren
Ergebnissen der anderen Ströme. Bei geplanten 277 % verliert er auf jedem
Strom: ein serieller Slot reicht dann schon für den Detektor allein nicht,
während das Backend direkt überlappt; ein Zusatzkredit
(`pipelining_depth: 1`) ändert daran nichts. Mit der Rechenzeit statt der
gemessenen Laufzeit als Profil ist er dreimal schlechter als kein Governor.
Ein Sprachmodell auf der CPU daneben kostet den Detektor nichts und sich
selbst ein Achtel seines Durchsatzes.

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
| NV-25 Zweites Backend | **gebaut, auf einem Gerät gemessen**: `vig-tflite-server` (TFLite, GPU-Delegate, Android) als eigener Prozess hinter dem unveränderten Governor; Gate-M3-Analogon auf der Adreno 540 eines Pixel 2. Bis 138 % kein Einbruch des Backends wie in Gate M3, der Governor hält den Detektor bei kürzeren Lücken; bei 277 % ist ein Slot schon für den Detektor zu wenig, und er verliert überall; mit Rechenzeit statt gemessener Laufzeit geplant schadet er. Es fehlt: Slots, die der gemessenen Nebenläufigkeit entsprechen (zweiter Betriebspunkt) | [0039](adr/0039-a-second-backend-proves-the-seam.md), [Messung](benchmark/android-gpu.md) |

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
| Externer Review vom 11.09. | **alle Befunde behoben** (R01–R08) | R01/R02 ([ADR-0040](adr/0040-a-restart-proves-only-what-died-with-it.md)): ein Backend-Neustart beendet nur Aufrufe, deren Verbindung schon abgebrochen ist; abgeglichen wird je Server und Backendmodell über alle Versionen. R03/R04: Puffer in Pilot und ROS-Brücke werden erst nach belegtem Ende wieder benutzt, sonst Quarantäne. R05: Samplingparameter über einen JSON-Parser. R06–R08: XSched-Level, Bereitschaft, Pilotmatrix mit Wiederaufnahme und Exitcodes. R09 (Gültigkeit je Messzelle) ist Regel in [scenarios.md](benchmark/scenarios.md), aber noch nicht automatisiert ([Review](reviews/2026-09-11-runtime/REVIEW.md)). |
| Externer Review vom 14.09. | **R01–R04 behoben**, R06 offen | R01/R02: der Look-ahead prognostiziert die freigegebene Variante samt Vorverarbeitung, und die Verweildauer blockiert keine Rettung der Versorgung. R03/R04 ([ADR-0042](adr/0042-an-end-is-proven-not-assumed.md)): ein Messarm endet erst, wenn keine seiner Inferenzen mehr laeuft, die Quarantaene einer Region gilt prozessweit, und ein Zaehlerabfall beendet nur Aufrufe, die das Backend in dieser Epoche gezaehlt hat. Offen: R06, das Basisalter eines Lageberichts nennt die frischeste Kamera statt der aeltesten. R05/R07/R08 sind Einordnung, kein Codefehler ([Review](reviews/2026-09-14/REVIEW.md)). |
| Lastspitzen und Variantenwahl | **erledigt** | Die Wahl achtet auf die Versorgung, nicht nur auf die Frist (`variant.rs`); auf der GPU am 12.09. 0 ‰ über alle Lastpunkte. Die Lastspitzen waren ein Artefakt der Fenstersicht; beide Werkzeuge zeigen jetzt auch die Verbrauchersicht ([Analyse](analysis/bursts-and-frontier.md), [Messung](benchmark/messkette-2026-09-12.md)). |
| Zweiter Betriebspunkt | **auf dem Telefon gemessen**, auf der Laptop-GPU offen | Auf der Adreno 540 mit `slots: 2` und gemessener Interferenz (`vig calibrate` gegen das TFLite-Backend, [Messung](benchmark/android-gpu.md#der-zweite-betriebspunkt-zwei-slots)): über Last liefert der Governor `pose` mehr Lieferfenster als das Backend direkt (80–83 gegen 75 %), in schwerer Überlast steigt `pose` von 8–13 auf 37–45 % und `depth` von 43–76 auf 85–95 % — bezahlt mit dem geschützten Detektor, dessen längste Lücke von 252–297 auf 413–452 ms wächst. Offen: `slots: 2` auf der Laptop-GPU mit zwei Triton-Instanzen samt Parallelprofilen; die Lastrampe sagt selbst, dass sich ihre Kante damit verschiebt. |
| NV-22 mehrere Ressourcendomänen | **erreichbar, nicht qualifiziert** ([ADR-0037](adr/0037-a-domain-is-a-gpu-with-one-owner.md)) | Gebaut: ein Scheduler je GPU (`backend.domains`, `domain:` am Modell), feste Zuordnung, kein Failover, Kennzahlen je Domäne, `vig doctor` je GPU; mit zwei Fake-Executoren belegt, dass eine belegte oder ausgefallene GPU der anderen weder Slot noch Kredit nimmt. Es fehlt: eine zweite GPU für die Qualifikation (Interferenz über PCIe, Hauptspeicher, Leistungsbudget), Shared-Memory-Registrierung an allen Endpunkten, Domänen in `vig calibrate`. Erledigt aus diesem Block: NV-21 ([ROS-2-Brücke](integrations/ros2.md)) und NV-23 ([begrenzter Nachweis](analysis/nv23-bounded-claim.md)). |
| Kante bei 100 % | **behoben** | Es war die Dispatch-Lücke. Mit `pipelining_depth: 1` fällt der Verlust von 188 auf 17 ‰, zusammen mit dem Versorgungsschutz auf 4 ‰; Gate M3 kostet Pipelining nichts (drei Läufe). Die Beispielkonfiguration steht seitdem auf 1 ([Abnahme](benchmark/abnahme-2026-09-12.md)). |
| Gelernte Marge auf Hardware | gemessen, bleibt opt-in | Nützt bei falschem Profil bis 110 % (Detektor 15 statt 150 ‰) und schadet bei 125 % (182 gegen 21 ‰ bei fester Marge) — Pipelining ändert daran nichts. Zusammen mit dem Versorgungsschutz kehrt sich das Bild bei 110 % um ([Abnahme](benchmark/abnahme-2026-09-12.md)). |
| Vig-Edge-Pilot | **Kriterien getrennt**; Planung bestanden, Präemption nicht verfügbar | Seit dem 12.09. urteilt der Pilot getrennt: Planung (P1–P6, gegen den Arm „Backend direkt") trägt den Exitcode, Anwendung (A1–A3) gilt zuerst für die Referenz ohne Konkurrenz und fällt sonst als „nicht anwendbar: Erkennungsqualität" heraus. Ohne XSched besteht die Planungsgruppe vollständig — das Backend direkt bricht bei C und D auf 0 ‰ Abdeckung und 45 s Lücke ein. **Präemption für den Berichtspfad geht auf diesem Stapel nicht:** vLLM lädt unter dem XSched-Shim seine Modelle nicht (Gegenprobe allein auf der Karte). Eine nur angegebene Lane bringt 30 statt 2 Berichte/min und kostet +28 % Alarmzeit bei D; der Shim selbst kostet am geschützten Pfad 17–20 % Laufzeit ([Messung](benchmark/pilot-praemption-2026-09-12.md)). Seit dem 14.09. (Review R05): **P1 gilt nur, solange der Vergleichsarm mindestens 500 ‰ der Perioden versorgt** — ein Arm mit 0 ‰ ist kein Maßstab für Latenz; und neben dem Status steht ein **Freigabeurteil aus fünf Feldern** (Ablauf, Planung, Anwendung, Qualifikation, Freigabe). Eine Teilmatrix liefert Exitcode 3 statt 0, und `qualifikation` lautet nie „bestätigt": ein Messlauf spricht keine Freigabe über seine eigene Hardware aus. |
| Versorgungsschutz im Look-ahead | **auf der GPU gemessen**, opt-in ([ADR-0041](adr/0041-the-look-ahead-protects-the-supply-not-only-the-deadline.md)) | Er hält den geschützten Strom bei 0–3 ‰, wo er ohne Schutz 16–21 ‰ verfehlt. Allein kostet er bei 110 % den ganzen Hintergrund (998 ‰); zusammen mit `margin_learning` ist er in beiden Spalten am besten (12 ‰ / 99 ‰) ([Abnahme](benchmark/abnahme-2026-09-12.md)). Die Frage der Deckelung ist seit dem 14.09. entschieden ([ADR-0043](adr/0043-an-impossible-contract-is-reported-not-alternately-broken.md)): Der Mindestfortschritt überstimmt den Schutz **nicht**. Passt ein unteilbarer Hintergrundauftrag nicht in die Lücke (`B > A − 2C`), melden beide Zusagen zusammen einen unerfüllbaren Vertrag — `vig doctor` sagt das beim Start, statt im Betrieb abwechselnd beide zu brechen. |

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
  | Dauerlauf auf dem freizugebenden Stand | bestanden, zweimal 8 Stunden: 02.09. und 11./12.09. auf dem Stand mit den Review-Fixes — kein Speicherwachstum, keine Drift, kein Fehler ([soak.md](benchmark/soak.md)) |
  | Datenpfadbudgets | [datapath-budgets.md](datapath-budgets.md) — **bestanden** am 11.09. auf ruhiger Maschine: +159 µs gegen Triton, +210–236 µs gegen den Mock, alle budgetierten Zeilen PASS ([Messung](benchmark/messkette-2026-09-11.md)) |
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
