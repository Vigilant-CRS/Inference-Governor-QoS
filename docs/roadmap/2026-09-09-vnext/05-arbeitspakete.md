# 05 — Arbeitspakete, Abhängigkeiten und Lieferstufen

Status: geplant, keines dieser Pakete wird durch dieses Dokument als
implementiert oder abgenommen markiert. Ausgangscode und Teststand: Dokument 02.
`NV-*` ergänzt die historischen `WP*`, ohne deren Nummern oder Nachweise umzudeuten.

## 1. Planungsregeln

- P0 bedeutet notwendige Grundlage für die entsprechende neue Zusage, nicht
  „jedes Feature muss vor jedem Kundengespräch fertig sein“.
- Jedes Paket liefert Code, Tests, eine kurze ADR/Kompatibilitätsnotiz und
  benannte Nachweise. Hardwarepakete zusätzlich Rohdaten und ein Manifest.
- Kein Abschluss nur aufgrund grüner Kompilierung. Kein experimenteller
  Mechanismus wird Standard, bevor das zugehörige Gate bestanden ist.
- Aufwand in **Personentagen (PT)**: grobe Engineering-Schätzung einschließlich
  paketbezogener Tests, keine Festpreis-/Terminverpflichtung. Zugriff auf
  Hardware, Modelle und kompetente Betreuung vorausgesetzt.
- Kundensupport, Beschaffung, juristische Arbeit und eine funktionale
  Sicherheitszertifizierung sind nicht in den PT enthalten.
- Forschungsbudgets enden auch mit einem dokumentierten No-Go. Ein positives
  Forschungsergebnis oder eine portable Präemption wird nicht versprochen.

## 2. Überblick

| Paket | Liefergegenstand | Priorität | Direkte Abhängigkeiten | PT |
|---|---|---|---|---:|
| NV-00 | Ausführungswissen, Quarantäne und Recovery | P0 | — | 3–6 |
| NV-01 | Verbrauchermetriken und Bursts | P0 | — | 4–7 |
| NV-02 | Versionierte Vertragszusätze | P0 | NV-01 | 3–5 |
| NV-03 | Profilidentität und Gültigkeitsdomäne | P0 | — | 3–5 |
| NV-04 | Read-only-Hardwarezustand | P0 | NV-03 | 5–9 |
| NV-05 | Periodischer, zustandsgeprüfter Messpfad | P0 | NV-01, NV-03, NV-04 | 6–10 |
| NV-06 | Zustandsabhängige Prognose mit Rückfall | P0 | NV-03, NV-04, NV-05 | 6–10 |
| NV-07 | Backendgrenze und Runtime extrahieren | P0 | NV-00 | 6–10 |
| NV-08 | TensorRT durch bestehendes Triton messen | P1 | NV-00, NV-01, NV-05 | 3–6 |
| NV-09 | TensorRT Direct, schmaler Referenzpfad | P1 | NV-07, NV-08 | 10–18 |
| NV-10 | Varianten mit kanonischer Semantik | P1 | NV-02, NV-03, NV-06 | 6–12 |
| NV-11 | Gerichtete Interferenzplanung | P1 | NV-05, NV-06, NV-07 | 8–15 |
| NV-12 | Warm-State und CUDA-Graph-Cache | P1 | NV-09, NV-06 | 4–8 |
| NV-13 | Optionale vorausschauende Aktuation | P2 | NV-02, NV-04, NV-06, NV-11 | 8–15 |
| NV-14 | Green-Context-Qualifikation | P2 | NV-09, NV-11 | 8–15 |
| NV-15 | XSched-Kompatibilitätsversuch | P2 | NV-07, NV-09 | 4–8 |
| NV-16 | Kooperatives Backend mit Fortschrittszustand | P2 | NV-02, NV-06, NV-07 | 10–20 |
| NV-17 | Begrenzter, gültigkeitsbewusster DAG | P2 | NV-02, NV-07, NV-10 | 8–15 |
| NV-18 | Autorisierte Anwendungssemantik | P2 | NV-02, NV-06, NV-17, NV-19 | 8–15 |
| NV-19 | Kundenstack und Pilotvertrag validieren | sofort | — | 3–6 |
| NV-20 | Release-, Betriebs- und Pilotqualifikation | P0 pro Release | NV-00, NV-01, NV-02, NV-07, NV-19; gewählte Features | 10–20 |
| NV-21 | Ein weiterer Backend-/Anwendungsadapter | optional | NV-07, NV-19 | 8–16 |
| NV-22 | Mehrere Ressourcendomänen | optional | NV-03, NV-06, NV-07, NV-11 | 6–12 |
| NV-23 | Begrenzte formale Analyse / Forschungsnachweis | Forschung | NV-02, NV-11; konkrete Ausführungsannahmen | 10–20 |
| NV-24 | Burstbewusste Dispatch-/Zulassungspolicy | P1/P2 | NV-02, NV-06, NV-11 | 6–12 |

Summe dieser Einzelbudgets: **156–295 PT**. Das ist ausdrücklich nicht die
Kostenobergrenze für ein fertiges Maximalprodukt: insbesondere ein positiver
XSched-Spike, weitere Portierungen oder zusätzliche Hardwarefamilien brauchen
eine neue Aufwandsschätzung. Mit Integrations-/Ungewissheitsreserve ist für
den beschriebenen Ausbau eher ein Rahmen von etwa **200–400 PT** zu diskutieren.
Parallelisierung verkürzt nicht alle Abhängigkeiten. Ein kleines Team muss
priorisieren, statt einen kurzfristigen Kompletttermin zu versprechen.

Die Grundlagen NV-00–07 plus NV-19 ergeben 39–68 PT. Ein einzelner Fehlerfix
oder ein erstes Kundengespräch kann deutlich früher fertig sein. Für einen
bestehenden Triton-Piloten werden nur dessen tatsächlich benötigte Pakete und
das Release-Gate verbindlich.

## 3. Paketdetails

### NV-00 — Ressourcen nur mit Endnachweis freigeben

- **Ziel/Ort:** `backend-triton/error`, `gateway/actor`, `cli/serve` und Tests;
  Wissenszustände auch nach Timeout dauerhaft korrekt erhalten.
- **Lieferung:** kein blindes `release_at` für Unknown; Reconciliation mit
  Generation/Fencing-Nachweis; Drain zählt physische Leases; Statuscode-Mapping
  anhand tatsächlicher Endbeweise prüfen. Recovery benötigt keine heimliche
  Befugnis zum Reset fremder Prozesse.
- **Abnahme:** Backend rechnet länger als mehrere Timeouts nach RPC-Abbruch;
  kein neuer Slotcredit. Timeout gefolgt von `Unavailable` bleibt unbekannt.
  Später bestätigtes Ende gibt genau einmal frei; alter Generationsevent nie.
  Drain meldet in diesen Fällen nicht irrtümlich Erfolg.
- **Rückfall:** Domäne nicht bereit, Betreiber erhält konkrete Recovery-Aktion;
  keine Rückkehr zur zeitbasierten Freigabe. Risiko: weniger Verfügbarkeit bei
  Backends ohne Reconciliation ist ehrlich auszuweisen.

### NV-01 — Verbraucherorientierte Metriken

- **Ziel/Ort:** `vig-sim/coverage`, `vig-bench`, `vig-gateway/exporter`.
- **Lieferung:** expliziter `no_result`-Zustand; Consumer coverage,
  zeitgewichtete AoI, längste Lücke, Miss-Folgen und M/K-Fenster getrennt von
  Legacy-Lieferfenstern. Begrenzte Metrikzustände, keine unbeschränkten Labels.
- **Abnahme:** gleiche Miss-Rate mit unterschiedlicher Burststruktur wird
  unterschieden; Schweigen, out-of-order, Fensterende und mehrfach genutzte
  Bestandsresultate korrekt; alter Report bleibt reproduzierbar.
- **Rückfall:** neue Metriken zusätzlich exportieren; bestehende Vergleichszahlen
  nicht still neu interpretieren. Risiko: fehlende Verbraucherzeitpunkte.

### NV-02 — Verträge und semantische Grenzen

- **Ziel/Ort:** `core/model`, `config/schema`, Protokoll-/SDK-Metadaten.
- **Lieferung:** optionale, versionierte Vertragszusätze aus Dokument 04;
  kein zweites inkompatibles Kernmodell. Beobachtetes SLO und geforderte
  Nachweisstufe sind separate Felder.
- **Abnahme:** alte YAML-Dateien unverändert nutzbar; ungültiges M/K/L abgelehnt;
  Vertragstakt bleibt bei Drops erhalten; unautorisierte Lockerung scheitert.
- **Rückfall:** Erweiterungen deaktiviert, alter Vertrag gilt. Risiko:
  Anforderungen werden versehentlich aus Laufzeitmessungen abgeleitet.

### NV-03 — Profilmanifest v2

- **Ziel/Ort:** Profil-/Config-Module und bisheriger Metadatenfingerprint.
- **Lieferung:** Artefakt-/Runtime-/Geräteidentität, Messgrenze, Datensatzherkunft,
  unabhängige Läufe, Gültigkeitsdomäne und Profilrevision. Legacy-Import bleibt.
- **Abnahme:** gleiche Tensorform bei verändertem Enginehash invalidiert Profil;
  andere Runtime oder Ressourcenaufteilung ebenso. Fehlende Felder sind
  `unknown`, nicht `verified`. Roundtrip und Migrationsgoldens.
- **Rückfall:** klar markiertes Legacy-Profil mit bestehender konservativer
  Planung, sofern Vertrag dies erlaubt. Risiko: unzugängliche Remote-Identität.

### NV-04 — Hardwarebeobachtung ohne Stellrechte

- **Ziel/Ort:** optionales Plattformmodul; zunächst RTX und ein gewähltes Jetson.
- **Lieferung:** gelesene CPU-/GPU-/EMC-Werte soweit verfügbar, Power-Mode,
  Throttling, Messalter und Quellenstatus; Snapshot-Events und Replay.
- **Abnahme:** unsupported/stale/unknown unterscheidbar; Wechsel vor erster
  langsamer Completion sichtbar; Collector-Ausfall löst definierten Rückfall
  aus; gemessener Overhead bleibt innerhalb des NV-19-Budgets.
- **Rückfall:** fest qualifiziertes Betriebsprofil oder keine stärkere Zulassung.
  Keine Root-Pflicht für den gesamten Governor. Risiko: Messwert ist nicht der
  tatsächlich wirksame Zustand.

### NV-05 — Messpfad statt pauschaler Profil-Sweep

- **Ziel/Ort:** `vig profile/calibrate`, Bench und optional importierte
  Messbausteine nach Lizenz-/Sicherheitsprüfung.
- **Lieferung:** absolute periodische Releases, getrennte Saturation-Tests,
  Clock-/Provider-Verifikation, Errors und Overruns als Daten, Vorallokation,
  Laufmanifeste, wiederholte Starts, Restore/Abbruchkonzept für spätere Aktuation.
- **Abnahme:** keine verschobenen Releases nach Overrun; kein stiller CPU-Fallback;
  Logging nicht im gemessenen Hot Path; falscher Hardwarezustand verwirft die
  betroffene Zelle mit Grund. Erfolgs-/Fehlernenner stimmen.
- **Rückfall:** bestehendes Profilwerkzeug bleibt verfügbar und als Legacy
  markiert. Risiko: Messung verändert selbst den Zustand.

### NV-06 — Predictor v2 als begrenzte Lookup-Policy

- **Ziel/Ort:** Profilverwaltung plus reine, deterministische Core-Sicht.
- **Lieferung:** diskrete gültige Zellen, konservativer Rückfall, Profile-/State-
  Versionen, keine erfundene Wahrscheinlichkeit. Erst Shadow, dann opt-in.
- **Abnahme:** keine Cross-Runtime-Zellenmischung; kalter Profilwechsel,
  fehlende Telemetrie und State-Wechsel zwischen Planung/Dispatch behandelt;
  gehaltene Versuche schlagen Legacy nicht nur durch mehr Ablehnung.
- **Rückfall:** bestehender Quantilschätzer. Risiko: Zustandsraumexplosion;
  Speicher-/Kandidatenbudget ist begrenzt und getestet.

### NV-07 — Runtime und Backend-API extrahieren

- **Ziel/Ort:** Actor aus `vig-gateway` in Runtime; Triton als erster Executor.
- **Lieferung:** neutrale Tickets, Capabilities, Ereignisse und Payload-Leases;
  OIP-spezifische Daten verbleiben im Adapter. Kein CUDA im Core.
- **Abnahme:** alte Triton-Traces und Protokollgoldens gleich; genau ein
  Ressourcenbesitzer; Permits überleben Clientende solange nötig; keine
  neue unbegrenzte Queue; Fake-Executor testet Fehler und Events ohne GPU.
- **Rückfall:** alte äußere API über Kompatibilitätsadapter. Risiko: verlorene
  OIP-Felder, doppelte Antwort, falsche Payload-Lebensdauer.

### NV-08 — TensorRT im bestehenden Serverpfad

- **Ziel/Ort:** isolierte Benchmarkkonfiguration mit vertrauenswürdigen Engines.
- **Lieferung:** A/B gleicher TensorRT-Engines über Triton mit/ohne Governor,
  unter starker Baseline und unveränderten fachlichen Anforderungen.
- **Abnahme:** Qualitäts-/Shapegleichheit, getrennte Runtimeprofile und
  reproduzierbare Rohdaten. Overhead und Hintergrundfortschritt ausgewiesen.
- **Rückfall:** bestehende ONNX-Runtime-Demo bleibt gültig für ihren alten Scope.
  Risiko: Compileroptimierung beseitigt den Engpass; das ist ein No-Go-Signal
  für einen unnötigen Ausbau, kein Grund die Baseline zu verschlechtern.

### NV-09 — TensorRT Direct, erster vertikaler Durchstich

- **Ziel/Ort:** neuer optionaler Executor und kleine auditable Native-Brücke.
- **Lieferung:** feste Shapes, vorgebaute Engines, begrenzte Context-/Buffer-
  Anzahl, ein tatsächlich laufender Auftrag je Credit, CUDA-Endevents.
- **Abnahme:** Outputprüfung gegen Referenz; Buffer-Reuse erst nach letzter
  Nutzung; Clientabbruch, Eventfehler und Prozessende getestet; keine
  Backendqueue außerhalb der Core-Sicht. Triton-only Build ohne CUDA SDK.
- **Rückfall:** Triton-Executor. Risiko: FFI und asynchrone Devicefehler;
  native Sanitizer-/GPU-Werkzeugtests gehören zum Liefergegenstand.

### NV-10 — Semantisch sichere Qualitätswahl

- **Ziel/Ort:** Variantenmanifest, Resolver, Vor-/Nachverarbeitungsvertrag.
- **Lieferung:** kanonische Labels, Koordinaten, Einheiten, Shapes und
  Qualitätsnachweise; Kosten von Resize/Postprocessing eingeschlossen.
- **Abnahme:** gleiche Shape mit vertauschter Labelordnung wird abgelehnt;
  Minimumqualität bleibt auch unter Last erhalten; Task-Metrik und zeitliche
  Qualität gemeinsam besser als feste Variante oder bewusster Trade-off.
- **Rückfall:** freigegebene feste Variante. Risiko: Offline-Accuracy passt
  nicht zur realen Aufgabe; keine automatische Freigabe aus einem Score.

### NV-11 — Interferenz und gemeinsame Ressourcen

- **Ziel/Ort:** Profiler, Slots/Ressourcendomäne, Kandidatenwahl.
- **Lieferung:** gerichtete Paardaten mit absoluten Kosten; gezielte N-Wege-
  Validierung; 2x-Regel als Heuristik deklarieren/ersetzen, keine universelle
  Durchsatzbehauptung. Compute-, Speicher- und Phasenkonflikte berücksichtigen.
- **Abnahme:** asymmetrisches Gegenbeispiel aus Dokument 02, drei konkurrierende
  Modelle, wechselnde Co-Tenant-Rate; kein additiv erfundener Laufzeitbound.
- **Rückfall:** qualifizierte Serialisierung / bestehendes `no_corun`.
  Risiko: mangelnde Profilabdeckung kostet Zulassung; explizit messen.

### NV-12 — Graphs und warme Ausführung

- **Ziel/Ort:** TensorRT-Executor und Profilidentität.
- **Lieferung:** begrenzter Graph-Cache mit Shape-/Context-/Adressbindung,
  dokumentiertem Warm-up und Speicherkonto; Auxiliary Streams kontrolliert.
- **Abnahme:** Shape-/Profilwechsel führt nicht zu falschem Graph-Reuse;
  Eviction nicht während Nutzung; kalter Pfad korrekt budgetiert;
  gemessener Nutzen gegenüber NV-09.
- **Rückfall:** regulärer Enqueue. Risiko: mehr Speicher für kaum CPU-Gewinn.

### NV-13 — Langsamer, optionaler Ressourcen-/Energieregler

- **Ziel/Ort:** privilegierter, eng begrenzter Plattformadapter.
- **Lieferung:** Vorlaufplanung, beobachtete statt angenommene Aktuation,
  exklusive Stellrechte, Plattformlimits, Dwell/Hysterese und Restore-Policy.
- **Abnahme:** konkurrierender Governor, abgelehnter Clock-Write, verspätete
  Wirkung, Thermal-Throttling und Serviceabbruch; kein Clock-Wechsel verletzt
  wissentlich einen bereits zugesagten Betriebsbereich.
- **Rückfall:** Betreiber setzt qualifizierten festen Betriebsmodus.
  Risiko: globale Stellgröße beeinträchtigt andere Arbeit; explizites Opt-in.

### NV-14 — Räumliche Partitionierung qualifizieren

- **Ziel/Ort:** TensorRT-Executor, Ressourcendomäne und Profile.
- **Lieferung:** Capability-Probe, festes Green-Context-Layout, eigene Profile,
  Vergleich mit normaler Nebenläufigkeit; keine Produktklasse „deterministisch“.
- **Abnahme:** tatsächliche SM-/Streamzuordnung, Auxiliary Streams,
  Speicherbandbreitengegner, nicht verwalteter Prozess und Fehlerpfad geprüft.
  Reservekosten und beide Verbraucherseiten dokumentiert.
- **Rückfall:** qualifizierter Standardpfad. Risiko: fehlende Wirkung auf den
  eigentlichen Engpass; negatives Ergebnis beendet diesen Ausbau auf der Plattform.

### NV-15 — XSched-Spike mit klarer Abbruchgrenze

- **Ziel/Ort:** isolierter Integrationsversuch, keine Pflichtabhängigkeit.
- **Lieferung:** gepinnter Upstreamstand, geprüfte Lizenz-/Treiber-/Buildkette,
  tatsächlich erreichbare Präemptionsebene und Controller-Zuständigkeit.
- **Abnahme:** reproduzierbar suspend/resume unter gewählter GPU/Graph-Version;
  Restblocking und Fortschritt gemessen; keine stillschweigende CUDA-API-Lücke.
- **Rückfall:** keine XSched-Integration. Nach positivem Spike wird ein eigener
  Portierungs-/Wartungsauftrag geschätzt; diese 4–8 PT versprechen keine
  produktionsreife Portierung auf beliebige Jetsons.

### NV-16 — Echte kooperative Fortsetzung und Fortschrittskosten

- **Ziel/Ort:** genau ein ausgewähltes LLM-Backend mit explizitem Vertrag.
- **Lieferung:** Prefill/Decode, Token-/Kontextstand, KV-Leases, TTFT/TBT,
  bestätigte Yield-Grenzen, Context-abhängige Kosten statt pauschaler Tokenrate.
- **Abnahme:** Nutzerparameter und Tokenlimit erhalten; Ausgabequalität gegen
  ungeteilte Referenz; lange Kontexte und Abbruch im Quantum; kein verstecktes
  Re-Prefill als kostenloser Fortschritt. Gemessene sichere Yield-Latenz.
- **Rückfall:** ungeteilter Request oder separat deklarierte Legacy-Zerlegung.
  Risiko: Semantik und internes Scheduling des Fremdbackends.

### NV-17 — DAG-Gültigkeit und Dependency Cancellation

- **Ziel/Ort:** begrenzter Metadatengraph außerhalb der Tensorberechnung.
- **Lieferung:** Capture-/Frame-/Epoch-IDs, Parent-Leases, Referenzzählung,
  Gültigkeitsprädikate und kontrollierte Weitergabe terminaler Zustände.
- **Abnahme:** ein Parent mit zwei Verbrauchern bleibt bis zum letzten gültig;
  alter Depth-Output wird nicht mit neuem Detector-Frame als gleiche Aufnahme
  kombiniert; laufende Arbeit wird nicht als abgebrochen erfunden.
- **Rückfall:** explizite Einzeljobs, DAG-Funktion ausgeschaltet.
  Risiko: unbeschränkter Graph oder fachlich falsche kausale Stornierung.

### NV-18 — Anwendungszustand innerhalb freigegebener Grenzen

- **Ziel/Ort:** optionales Policy-/SDK-Modul für einen benannten Pilotfall.
- **Lieferung:** authentisierte Gültigkeits-/Aktionshorizont-Hinweise mit TTL,
  freigegebene Moduswechsel und definierter konservativer Grundvertrag.
- **Abnahme:** veraltete, widersprüchliche oder unberechtigte Hinweise dürfen
  keinen Vertrag lockern; Task-Erfolg und Consumer-Metriken gegen feste Policy.
- **Rückfall:** statischer Betreibervertrag. Risiko: fehlerhafte Confidence;
  keine eigene Weltinterpretation oder automatische Safety-Entscheidung.

### NV-19 — Entwicklungspartner und messbaren Nutzen finden

- **Ziel:** Gespräche jetzt, unabhängig vom nativen Entwicklungsfortschritt.
- **Lieferung:** bestätigter Kundenstack, reale Engpassbeschreibung, zwei bis
  drei repräsentative Lastfälle, gewählte Hardware und schriftliche Messgrenzen.
- **Abnahme:** Kunde benennt einen technisch verantwortlichen Ansprechpartner,
  zulässigen Integrationsaufwand und Kriterien für einen begrenzten Pilot.
- **Rückfall:** kleinerer Integrations-/Analyseauftrag oder kein Ausbau ohne
  Bedarf. 3–6 PT bezeichnen unsere Arbeit, nicht die Wartezeit auf Antworten.

### NV-20 — Release und Pilotbetrieb qualifizieren

- **Ziel/Ort:** CI, Paketierung, Installationspfad und benannte Pilotkonfiguration.
- **Lieferung:** Feature-/Hardwarematrix, SBOM, signierbare Releases/Artefakte,
  Update/Rollback, Rechte-/Modelladministration, Offlinebetrieb, Runbook,
  Drain/Recovery und Supportgrenzen; aktuelle statt historische Statusdoku.
- **Abnahme:** gesamtes Gate G4 aus Dokument 06 einschließlich Dauerlauf,
  Fault Injection, Datenpfadbudgets und Freigabe durch Pilotverantwortliche.
- **Rückfall:** vorherige Release-/Profilversion ohne verlorene Lease-Generation.
  Risiko: Packaging oder Update verletzt den zuvor qualifizierten Zustand.

### NV-21 — Portabilität an einem echten zweiten Nutzer beweisen

- **Ziel:** nach Kundenbedarf ein ORT-/CPU-Executor **oder** eine ROS-/Holoscan-
  Anwendungsintegration. Weitere Varianten werden separat geschätzt.
- **Lieferung:** Wiederverwendung derselben Core-/Lease-/Profilverträge ohne
  CUDA-Abhängigkeit in allgemeinen Modulen.
- **Abnahme:** identische Metadatentraces im Simulator, korrekte Capabilities,
  keine erzwungene TensorRT-Semantik bei fehlenden Fähigkeiten.
- **Rückfall:** bestehende Adapter. Risiko: vorzeitige Universalabstraktion.

### NV-22 — Mehrere Ressourcendomänen koordinieren

- **Ziel:** mehrere Geräte/Partitionen mit einem expliziten Domänenregister.
- **Lieferung:** Routing, Transferkosten, Datenlokalität, Gerätegenerationen,
  unabhängige Budgets und festgelegte Failover-Policy.
- **Abnahme:** Kopier-/Migrationszeit nicht unterschlagen; kein Doppelbesitz;
  Geräteausfall verändert nicht die Credits anderer Domänen. Stateful-Jobs
  wechseln nicht ohne gültigen Zustandstransfer.
- **Rückfall:** feste Gerätezuordnung. Kein globaler Fleet-/Cloud-Scheduler in
  diesem Paket. Risiko: bessere Rechenzeit, aber schlechtere Gesamtlieferzeit.

### NV-23 — Begrenzten Forschungsclaim prüfen

- **Ziel:** präzise Teilfrage statt universeller Echtzeitbeweis.
- **Lieferung:** Annahmenkatalog, Referenzmodell und Gegenbeispielsuche, etwa
  für nichtpräemptive Slots mit begrenztem Jitter und vorgegebenen Laufzeiten.
- **Abnahme:** nachvollziehbarer Beweis im begrenzten Modell **oder** ein
  dokumentiertes Gegenbeispiel/No-Go; Abgleich mit ausführbarer Simulation.
- **Rückfall:** E2-SLO ohne analytische Garantie. 10–20 PT sind ein begrenzter
  Forschungsversuch, keine Zusage einer Zertifizierung oder Patentfähigkeit.

### NV-24 — Miss-Fenster tatsächlich in Entscheidungen einbeziehen

- **Ziel/Ort:** Vertragsmonitor und Core-Policy; getrennt von der bloßen
  Beobachtung in NV-01 und den Schemafeldern in NV-02.
- **Lieferung:** begrenzter Zustand für verbleibende M/K-/Burst-Spielräume,
  priorisierte nächste Versorgung, gemeinsame Zulassungsprüfung und explizite
  Verletzungsreaktion. Keine Vorrangänderung außerhalb der Betreiberpolicy.
- **Abnahme:** zwei Verbraucher mit kollidierenden nächsten Pflichtzyklen,
  enge Fenster, Recovery nach Miss, Sensorstillstand und untragbare Last;
  unveränderte Nenner und keine künstliche Fensterzurücksetzung. Vergleich
  gegen vorhandene Kritikalitäts-/EDF-Policy auf gehaltenen Traces.
- **Rückfall:** Monitoring plus bisherige Policy, als solche ausgewiesen.
  Eine formale Pattern-Garantie benötigt zusätzlich NV-23 mit tragfähigen
  Ausführungsgrenzen; dieses Paket allein liefert eine empirische Policy.

## 4. Lieferstufen und Entscheidungen

1. **R0 — belastbare Basis:** NV-00, NV-01, NV-02, NV-03; gleichzeitig NV-19.
   Behebt fundamentale Wissens-/Messfehler, ohne Backendwechsel.
2. **R1 — zustandsbewusster Triton-Pfad:** NV-04–07, NV-08 sowie gewählter
   Umfang von NV-20. Gate G1/G2; auch allein ein verkaufbares Pilotziel.
3. **R2 — nativer Edge-Pfad:** NV-09, bei Bedarf NV-10/11/12, erneut NV-20.
   Nur bei bestätigtem Kundenbedarf und bestandenem nativen Vergleich.
4. **R3 — kontrollierte Mechanismen:** geeignete Teilmenge NV-13–16 und NV-24, nicht
   notwendigerweise alle. Eigenes Gate je Hardware-/Backendkombination.
5. **R4 — semantischer/maximaler Ausbau:** NV-17/18/21/22, NV-23 als separater
   Forschungsstrang. Kein Anspruch, alle Optionen gleichzeitig aktivieren zu müssen.

Nach jedem Gate darf der Ausbau enden, wenn der zusätzliche Nutzen die
Komplexität nicht trägt. Ein negatives Experiment ist ein fertiges Ergebnis,
kein Anlass, Baseline oder Kundenanforderung nachträglich passend zu machen.
