# Arbeitsstand

Stand: 2026-09-09 · **Gate M3 bestanden** · Ausbaustufe R0 fertig, R1 zum
groessten Teil · 8-Stunden-Dauerlauf laeuft

Dieses Dokument beschreibt, **was jetzt gilt**. Was einmal galt, steht in den
ADRs (`docs/adr/`) und in der Git-Historie; hier wird es nicht
fortgeschrieben.

## Umfang

| | |
|---|---|
| Rust, ohne Kommentare gezaehlt | ~38 500 Zeilen in 9 Crates |
| Tests | 478, alle gruen |
| Architekturentscheidungen | 29 ADRs |
| Gate | fmt, clippy `-D warnings`, test, `cargo deny`, `reuse lint`, aarch64 unter Emulation |

Die Crates und ihre Zustaendigkeit:

| Crate | Zeilen | Was darin liegt |
|---|---:|---|
| `vig-core` | 14 500 | Der deterministische Scheduling-Kern. Keine Uhr, kein I/O, keine Dependencies. |
| `vig-gateway` | 6 600 | Der OIP-Server, der Single-Owner-Actor, die Backendnaht, der Prometheus-Endpunkt. |
| `vig-config` | 3 500 | Schema, Parser, Validator. Lehnt ab, statt zu reparieren. |
| `vig-bench` | 3 500 | Messlaeufe gegen echtes Triton: `gate-m3`, `soak`, `load-ramp`, `wp26`. |
| `vig-cli` | 3 400 | `vig doctor` / `profile` / `calibrate` / `serve` / `verify`. |
| `vig-sim` | 3 000 | Discrete-Event-Simulator mit bitgleich reproduzierbaren Traces. |
| `vig-platform` | 2 400 | Lesende Hardwarebeobachtung und der Messpfad. Stellt nichts. |
| `vig-backend-triton` | 1 100 | Der Triton-Adapter samt Fehler- und Nachweislogik. |
| `vig-protocol-oip` | 600 | Die generierten OIP-Typen. |

## Was gemessen belegt ist

**Gate M3, RTX 3070 Laptop (8 GB), Triton 2.70, echte Modelle.** Gegen eine
getunte Baseline — gleiche Modelle, gleiche Instance Groups, gleicher
Shared-Memory-Datenpfad, Rate Limiter mit Prioritaeten:

| Strom | Triton (getunt) | Vigilant | Unabgedeckte Zyklen |
|---|---:|---:|---:|
| detector (RF-DETR) | 84 % | 99 % | 22,4x weniger |
| pose | 91 % | 99 % | 12,0x weniger |
| depth | 97 % | 99–100 % | 5,4x weniger |

Gegen Tritons **staerkste** Einstellung — global begrenzte gemeinsame
Ressource statt Prioritaeten allein — sind es beim Detektor 12,9–15,1x, weil
diese Konfiguration Tritons eigene Detektorabdeckung auf 89–91 % hebt. Sie
verschiebt das Problem dabei: die Pose faellt auf 78 %.

Der VLM-Strom steht in derselben Tabelle bei 1 % Abdeckung. Das ist kein
Messfehler, sondern ADR-0012: ein nicht unterbrechbarer Block, der laenger
dauert als die kuerzeste geschuetzte Periode, startet unter Last nie. Genau
dafuer gibt es die kooperative Zerlegung (ADR-0014).

Was sie kostet, sagt seit NV-16 `vig doctor`: mit den Zahlen aus WP26 kosten
allein die Round-Trips 95 % mehr Arbeit als der ungeteilte Lauf, und das ist
eine Untergrenze. Die Zerlegung tauscht Gesamtarbeit gegen Blockadezeit — eine
Wahl, die jetzt als Wahl dasteht statt als Selbstverstaendlichkeit
(ADR-0031).

**Was die Messung ueber sich selbst sagt:** die Karte lief dabei unter einem
Leistungslimit, 1830 von 2100 MHz. Beide Vergleichsseiten liefen darunter, der
Vergleich gilt also — aber die absoluten Zahlen gelten fuer diesen Zustand und
nicht fuer die Karte. `vig doctor` sagt das jetzt vor jeder Messung (ADR-0021).

## Fertige Ausbaustufe R0

| Paket | Was es aendert | ADR |
|---|---|---|
| NV-00 | Slotkredite enden durch Nachweis, nicht durch Frist | — |
| NV-01 | Verbraucherabdeckung, zeitgewichtetes AoI, laengste Luecke — getrennt von den Legacy-Lieferfenstern | — |
| NV-02 | Versionierte Vertragszusaetze, Weakly-hard-Monitor, Freigabeliste | [0020](adr/0020-contract-extensions-are-additive-and-versioned.md) |
| NV-03 | Profilmanifest: Artefakt-Digest, Runtime, Geraet, Aufteilung, Gueltigkeitsdomaene | [0019](adr/0019-profile-identity-beyond-a-metadata-hash.md) |

## Ausbaustufe R1

| Paket | Stand | ADR |
|---|---|---|
| NV-04 Hardwarebeobachtung | fertig, nur lesend, kein Root | [0021](adr/0021-hardware-is-read-never-set.md) |
| NV-05 Messpfad | fertig: absolutes Freigaberaster, vier Zaehler, Uhrpruefung | [0022](adr/0022-measurement-is-a-method-not-a-loop.md) |
| NV-06 Prognose v2 | fertig, laeuft im **Schatten**; Scharfschalten ist eine Betreiberhandlung | [0023](adr/0023-state-aware-prediction-runs-in-the-shadow-first.md) |
| NV-07 Backendnaht | Naht und Fake-Executor fertig; Crate-Verschiebung und OIP-freie Nutzlast bewusst aufgeschoben | [0024](adr/0024-the-backend-is-a-seam-not-a-type.md) |
| NV-08 TensorRT ueber Triton | fertig, gemessen — kein Codepfad noetig | [benchmark/tensorrt.md](benchmark/tensorrt.md) |
| NV-09 TensorRT Direct | **offen**, braucht CUDA SDK | — |
| NV-10 Semantik der Varianten | fertig | [0025](adr/0025-the-same-shape-is-not-the-same-meaning.md) |
| NV-11 Gerichtete Interferenz | angeschlossen: `backend.interference` geht in die Planung ein. **Auf dieser Maschine nicht messbar** — der Takt wandert waehrend jeder Reihe, alle vier Messreihen verworfen | [0026](adr/0026-interference-is-directed-and-not-additive.md), [Messung](benchmark/interference.md) |
| NV-13 Energieregler | fertig, opt-in, beobachtet statt angenommen; auf dieser Maschine fehlen die Rechte | [0030](adr/0030-actuation-is-an-exception-and-must-be-observed.md) |
| NV-16 Fortschrittskosten | Code fertig; der Prefill-Anteil ist auf dieser Maschine **nicht gemessen** — der `vlm` im Benchmark ist ein ResNet-Platzhalter ohne Texteingang | [0031](adr/0031-a-re-prefill-is-not-free-progress.md) |
| NV-17 Gueltigkeitsbewusster DAG | Kern fertig; **nicht** an das Gateway angeschlossen | [0028](adr/0028-a-fusion-needs-a-common-capture.md) |
| NV-18 Anwendungssemantik | fertig: ein Hinweis darf verschaerfen, nie lockern | [0029](adr/0029-a-hint-may-tighten-never-loosen.md) |
| NV-24 Missbudget in Entscheidungen | fertig, Voreinstellung **aus** | [0027](adr/0027-a-miss-budget-that-decides-not-only-observes.md) |

## Der Review vom 10.09. und was er kostete

Ein externes Codereview mit acht lauffaehigen Gegenproben. Alle acht liefen
rot; alle acht sind behoben und stehen jetzt in der Regression (ADR-0032).

Zwei davon aendern **gemessene Zahlen**, und zwar nach unten: die laengste
Versorgungsluecke laeuft ab dem Ablauf des letzten brauchbaren Ergebnisses
statt ab dessen Fertigstellung, und eine veraltete Lieferung schliesst keine
Luecke mehr. Aeltere Messberichte dieses Projekts sind damit **nicht** mit
neuen vergleichbar, wo veraltete Lieferungen vorkamen.

Ein dritter aendert das Verhalten generativer Auftraege: die Tokenobergrenze
ist jetzt garantiert statt geschaetzt und wird bis zu viermal so schnell
verbraucht. Wer dieselbe Ausgabelaenge will, hebt `max_total_tokens` an — und
weiss dann, was er zulaesst.

## Was ausdruecklich noch nicht angeschlossen ist

Vier Zustaende, nicht zwei: **gebaut**, **erreichbar**, **angeschlossen**,
**qualifiziert**. Der Review vom 10.09. hat den Unterschied als Befund
notiert — mehrere Funktionen galten als „Voreinstellung aus", waren aber durch
keinen dokumentierten Konfigurationsschritt zu erreichen. Das ist eine andere
Aussage, und sie steht jetzt getrennt in der
[Support-Matrix](support-matrix.md).

Seit dem Review **erreichbar** (Konfigurationsschritt vorhanden, ein Test
belegt, dass er eine Entscheidung aendert): die Missbudget-Policy
(`backend.miss_aware_policy`), die Anwendungshinweise (`backend.hints`), der
Mindestfortschritt fuer Hintergrundlast, der Taktregler (`backend.actuation`)
und die gerichtete Interferenztabelle (`backend.interference`, geschrieben von
`vig calibrate`). Erreichbar heisst **nicht** qualifiziert: ob das
Einschalten auf einer bestimmten Last besser ist, sagt keine Messung.

Diese Bausteine sind weiterhin nur **gebaut** und tun im Betrieb nichts:

- **Die zustandsabhaengige Prognose (NV-06)** laeuft im Schattenbetrieb. Sie
  wird gefuettert und verglichen; entschieden wird weiter mit
  `max(offline_p99, online_p95)`. Die Umstellung entscheidet der Betreiber
  anhand von `vig_predictor_more_conservative_total` und
  `vig_predictor_more_optimistic_total` — eine Policy, die nur mehr ablehnt,
  haelt jede Zusage ein und ist trotzdem wertlos.
- **Der Abhaengigkeitsgraph (NV-17)** braucht eine Zusage vom Client, welche
  Anfrage zu welcher Aufnahme gehoert. Das ist eine Protokollerweiterung, die
  ohne benannten Pilotfall nicht sinnvoll zu entwerfen ist.
- **Der Kontextanteil der Zerlegung (NV-16)** ist gerechnet und getestet, aber
  nicht gemessen: der `vlm`-Strom in den Benchmarks ist ein ResNet-50 mit
  Batch 48 und hat keinen Texteingang. `vig calibrate` sagt das auch so und
  laesst die vorhandenen Werte stehen. `prefill_per_token_us` steht in jeder
  Beispielkonfiguration auf null — was hier **nicht gemessen** heisst und
  nicht „kostenlos". Die erste Installation mit einem echten generativen
  Backend schliesst diese Luecke in einem Kalibrierlauf.

## Die offenen Arbeitspakete

| Paket | Stand | Was fehlt |
|---|---|---|
| NV-09 TensorRT Direct | **offen** | Ein schmaler Referenzpfad an Triton vorbei. Zwei Dinge fehlen: die TensorRT-Header (im Triton-Image nicht enthalten, auf PyPI nur als Stub) und eine Entscheidung ueber `unsafe_code = "forbid"` — jede CUDA-FFI braucht `unsafe`. Das ist eine Entscheidung ueber die Kernzusage dieses Projekts und keine technische Huerde. |
| NV-12 CUDA-Graphs | **gemessen, negativ** | Ueber Tritons Modellkonfiguration erreichbar, ohne eigenen Codepfad — wie NV-08 bei TensorRT. 3,7 % weniger p50, und **mehrere** Modelle mit Graphs laden nicht mehr: die Aufnahme des einen vergiftet den Stream des anderen. Fuer einen Governor, der mehrere Modelle ordnet, unbrauchbar. [Messung](benchmark/cuda-graphs.md) |
| NV-14 Green Contexts | **offen** | Braucht die CUDA-Treiber-API (`cuGreenCtxCreate`, in `/usr/include/cuda.h` vorhanden) und damit dieselbe `unsafe`-Entscheidung wie NV-09. |
| NV-15 XSched-Spike | **gefahren, positiv** | Level 2 (abgeschickte Queue stilllegen) funktioniert auf sm86 — entgegen der Upstream-Tabelle — und senkt das Restblocking von ~50 auf ~14 ms. Die API meldet ihre Luecken **nicht**: alle drei Ebenen antworten „Erfolg", auch die unfertige. [Spike](spikes/nv15-xsched.md) |
| NV-17 DAG | Kern fertig, nicht angeschlossen | Eine Zusage vom Client, welche Anfrage zu welcher Aufnahme gehoert. Protokollerweiterung, ohne benannten Pilotfall nicht sinnvoll zu entwerfen. |
| NV-21/22/23 | optional / Forschung | Ein weiterer Backendadapter, mehrere Ressourcendomaenen, formale Analyse. |

**Zur `unsafe`-Frage bei NV-09/12/14.** Die Header sind da: CUDA 12.4 ist
installiert, `cuda.h` und `cuda_runtime.h` liegen in `/usr/include`,
`libcuda.so.580` ist geladen, und `cuGreenCtxCreate` steht im Header. Was
fehlt, ist keine Datei, sondern eine Entscheidung: `Cargo.toml` setzt
`unsafe_code = "forbid"` fuer den ganzen Workspace, und das ist die staerkste
Zusage, die dieses Projekt macht. Ein eigenes Crate mit enger, gepruefter
FFI-Oberflaeche waere der uebliche Weg — er kostet die Zusage in ihrer heutigen
Form.

## Offen fuer eine Produktionsfreigabe

- **NV-19 — ein Entwicklungspartner.** Das einzige Paket, das nicht durch Code
  zu erledigen ist, und die Voraussetzung fuer NV-08, NV-18 und NV-20. Ohne
  einen benannten Lastfall und eine benannte Hardware ist jeder weitere Ausbau
  eine Vermutung.
- **NV-20 — Releasequalifikation.** Fertig bis auf das, was eine Person oder
  eine Messung braucht:

  | | |
  |---|---|
  | Feature- und Hardwarematrix | [support-matrix.md](support-matrix.md) |
  | Runbook, Recovery- und Supportgrenzen | [runbook.md](runbook.md) |
  | Fehlerinjektion, 11 Fehlerbilder ohne GPU | `crates/vig-gateway/tests/fault_injection.rs` |
  | Update und Rollback | geprueft; brachte einen echten Fehler zutage (`b6f0777`) |
  | Rechteliste, Modellverwaltung, Offlinebetrieb | [support-matrix.md](support-matrix.md) |
  | Installationspfad mit Bereitschaftspruefung | `deploy/docker-compose/` |
  | SBOM, signierbare Artefakte, `cargo auditable` | `.github/workflows/release.yml` |
  | Dauerlauf auf dem freizugebenden Stand | laeuft |
  | Datenpfadbudgets | offen, braucht eine Messung |
  | Freigabe durch Pilotverantwortliche | offen, braucht NV-19 |
- **Zweite Hardware.** Die Logik ist portabel, die Zahlen sind es nicht. Auf
  `aarch64` ist der Kern unter Emulation gebaut und getestet; ueber Laufzeit,
  Durchsatz und Interferenz auf Jetson sagt das nichts
  ([Hardwarequalifikation](hardware-qualification.md)).
- **Die Luecken im Messbild.** Bursts und Lastrampe aus Spec 19.4, ein zweiter
  Betriebspunkt, die Qualitaets-Deadline-Frontier aus Spec 19.7.

## Zuletzt gemessen

**8-Stunden-Dauerlauf (2026-09-09/10), bestanden.** Keine Kennzahlendrift:
erste gegen letzte Stunde alle Werte innerhalb von 1 %, die einzige groessere
Abweichung ist `depth` mittlere AoI mit −11 % — also besser. Kein Fehler, keine
Panic. Speicher 12 640 → 15 912 kB, davon 2,5 MB im Anlauf der ersten Stunde;
danach +748 kB ueber sieben Stunden ohne erkennbaren Trend. Kein unbegrenztes
Wachstum im beobachteten Fenster; „kein Leck" leiten wir daraus nicht ab.
Details in [benchmark/soak.md](benchmark/soak.md).

**TensorRT (2026-09-10), NV-08 beantwortet.** Dieselben Gewichte als
TensorRT-Engine: serialisierte Auslastung 103 % → 76 %, Detektorlaufzeit
15 639 → 11 488 us. Der Vorsprung des Governors halbiert sich (24,7x → 13,3x),
weil die Baseline besser wird — der Engpass bleibt: bei 76 % verfehlt ein
getunter Triton weiter jeden zehnten Detektorzyklus.
[benchmark/tensorrt.md](benchmark/tensorrt.md)

**RF-DETR-Varianten (2026-09-10), negatives Ergebnis.** Fuenf echte Modelle
gemessen: die Auflaesung bestimmt die Laufzeit, das Modell fast nicht — bei
gleicher Auflaesung liegen 7 bis 28 Klassen innerhalb von 1,1 %. Variantenwahl
hat auf dieser Modellfamilie keinen Betriebspunkt.
[benchmark/rfdetr-variants.md](benchmark/rfdetr-variants.md)
