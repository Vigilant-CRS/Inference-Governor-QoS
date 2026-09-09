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
| NV-08 TensorRT ueber Triton | **offen**, braucht die GPU und gebaute Engines | — |
| NV-09 TensorRT Direct | **offen**, braucht CUDA SDK | — |
| NV-10 Semantik der Varianten | fertig | [0025](adr/0025-the-same-shape-is-not-the-same-meaning.md) |
| NV-11 Gerichtete Interferenz | Tabelle und gerichtete Messung fertig; **nicht** an die Zulassung angeschlossen | [0026](adr/0026-interference-is-directed-and-not-additive.md) |
| NV-17 Gueltigkeitsbewusster DAG | Kern fertig; **nicht** an das Gateway angeschlossen | [0028](adr/0028-a-fusion-needs-a-common-capture.md) |
| NV-24 Missbudget in Entscheidungen | fertig, Voreinstellung **aus** | [0027](adr/0027-a-miss-budget-that-decides-not-only-observes.md) |

## Was ausdruecklich noch nicht angeschlossen ist

Drei Bausteine sind gebaut, getestet und tun im Betrieb noch nichts. Das steht
hier, weil „umgesetzt" und „wirksam" verschiedene Aussagen sind:

- **Die zustandsabhaengige Prognose (NV-06)** laeuft im Schattenbetrieb. Sie
  wird gefuettert und verglichen; entschieden wird weiter mit
  `max(offline_p99, online_p95)`. Die Umstellung entscheidet der Betreiber
  anhand von `vig_predictor_more_conservative_total` und
  `vig_predictor_more_optimistic_total` — eine Policy, die nur mehr ablehnt,
  haelt jede Zusage ein und ist trotzdem wertlos.
- **Die Interferenztabelle (NV-11)** ist leer, bis eine Messkampagne sie
  fuellt. Bis dahin bleibt der Slot-Belegungsgrad die Naeherung, die er laut
  ADR-0006 immer war.
- **Der Abhaengigkeitsgraph (NV-17)** braucht eine Zusage vom Client, welche
  Anfrage zu welcher Aufnahme gehoert. Das ist eine Protokollerweiterung, die
  ohne benannten Pilotfall nicht sinnvoll zu entwerfen ist.

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

## Laeuft gerade

Ein 8-Stunden-Dauerlauf mit wechselnder Last gegen echtes Triton. Auszuwerten
mit `python3 tools/soak-report.py <Ausgabeverzeichnis>`. Geprueft wird
Speicherdrift, Kennzahlendrift und ob eine Vertragsverletzung als solche
gemeldet wird.
