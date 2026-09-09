# 03 — Modulare Zielarchitektur bis zum Maximalausbau

Status: Entwurf. Alle neuen Modul- und API-Namen sind Vorschläge, keine bereits
existierenden Crates, CLI-Befehle oder Zusagen. Der erste produktive Ausbau
benötigt nicht alle hier beschriebenen Module.

## 1. Ein Produkt, klar getrennte Verantwortungen

```text
Anwendung / optional ROS- oder Holoscan-Integration
                  |
         OIP-Gateway oder lokales SDK
                  |
          Authentisierung + Vertragsgrenzen
                  |
       vig-runtime: ein Besitzer je Ressourcendomäne
       | Payload-Leases, Ausführungswissen, Lifecycle |
       |                                            |
       +------ vig-core: deterministische Policy ---+
       |          ^            ^                    |
       |          |            +-- gültige Prognosesicht
       |          +-- versionierte Zustandsereignisse
       |
       +-- Triton-Executor ------ Triton ------ TRT / ORT / ...
       +-- TensorRT-Executor ---- Contexts / Streams / CUDA Events
       +-- weitere Executor-Adapter (nur nach Bedarf)
                  |
       optionale Ausführungsmechanismen:
       kooperative Grenze / Green Contexts / XSched

Außerhalb des Hot Paths:
Telemetrie -> State Resolver -> Profilkatalog / Kalibrierung
                             -> experimenteller Ressourcenregler
Trace + Verbraucherfeedback -> Evaluation / Vertragsmonitor
```

Der Governor bleibt eine Policy- und Koordinationsschicht. Kein Modellcompiler,
kein allgemeines Robotik-Weltmodell und kein Ersatz für den Sicherheitsregler.

## 2. Modulzuschnitt und Abhängigkeiten

| Modul | Verantwortet | Darf ausdrücklich nicht |
|---|---|---|
| `vig-core` (bestehend) | Zeit-/Vertragstypen, Queue- und Dispatchentscheidungen, begrenzte Zustandssichten | SDKs laden, Sensoren abfragen, Hardware verstellen |
| `vig-runtime` (neu) | Actor, Ressourcendomänen, Ausführungs-/Payload-Leases, Wiederanlauf und Ereignisordnung | stillschweigend fachliche Verträge lockern |
| `vig-backend-api` (neu, klein) | Executor-Capabilities, Tickets, Wissenszustände, Payload-Handles | OIP- oder CUDA-Typen als universellen Vertrag erzwingen |
| Executor-Adapter | Backendaufruf, Datenpfad, eigene Objekte, Completion-/Fehlernachweis | Modelle oder Qualität außerhalb des Dispatchplans wählen |
| `vig-profiles` (neu) | Profilmanifest, Domänenabgleich, Samplingprovenienz, kompakte Prognosesichten | produktive Anforderungen aus Messungen ableiten |
| `vig-platform` (neu, optionale Adapter) | Beobachtung und getrennte privilegierte Aktuation | Rootrechte an den Netzwerk-Gateway vererben |
| Gateway / SDK (bestehend + Erweiterung) | Anwendungsmetadaten, Autorisierung, Protokolltreue | Ressourcenfreigabe aus Clientabbruch ableiten |
| Simulator / Bench / CLI (bestehend) | Replay, Kalibrierung, Qualifikation, Diagnose | synthetische Daten als Hardwarequalifikation ausgeben |

Logische Module müssen nicht sofort je ein eigenes Crate werden. Extrahiert
wird erst, wenn ein zweiter Nutzer oder eine klare Abhängigkeitsgrenze entsteht.
Kein Vorrat an leeren Adapter-Crates. Abhängigkeiten fließen von Runtime und
Adaptern zu Core/API, niemals zurück vom Core zu Plattformbibliotheken.

## 3. Was eine Ressourcendomäne bedeutet

Eine Domäne ist der Bereich gemeinsam verwalteter Ausführungskapazität:
zunächst eine physische GPU, später beispielsweise eine qualifizierte Partition.
Zwei Triton-Server auf derselben GPU sind nicht automatisch zwei Domänen.

Pro Domäne gibt es genau einen logischen Kapazitätsbesitzer. Er kann einen
eingebetteten Prozess oder mehrere kontrollierte Worker koordinieren. Mehrere
unkoordinierte SDK-Instanzen bekommen keine domänenweite QoS-Zusage.

Jede Domäne beschreibt:

- stabile Geräteidentität, Backend-/Worker-Generation und Layoutversion;
- begrenzte Slots, Contexts, erlaubte Modelle und In-Flight-Tiefe;
- statisch belegten Speicher für Engines, Contexts und Graphs;
- dynamische Puffer-, Workspace- und gegebenenfalls KV-Cache-Budgets;
- bekannte fremde Arbeit und den Status `unmanaged_load` / `unknown`;
- tatsächlich durchgesetzte, nur beobachtete oder bloß deklarierte Grenzen.

GPU-Prozentzahlen sind keine universelle Kapazitätseinheit. SM-Anzahl,
Speicherbytes, Copy-Engines und Ausführungscredits sind unterschiedliche Dinge.
Unbekannte Fremdlast reduziert die Nachweisstufe; sie wird nicht als null erfasst.

## 4. Executor-Schnittstelle: Wissen statt Hoffnung

Konzeptioneller Vertrag:

```text
describe() -> Capabilities + ExecutionIdentity
prepare(EngineManifest) -> PreparedEngineHandle
submit(DispatchPlan, PayloadLease) -> ExecutionTicket | ProvenNotStarted
observe(ticket) -> CompletionEvidence | ExecutionUnknown | StillRunning
request_cancel(ticket) -> CancelRequested | Unsupported
reconcile(domain_generation) -> ReconciliationEvidence
```

Das ist API-Pseudocode. `observe` muss nicht pollen; ein Adapter kann Events
liefern. Asynchrone Fehler gehören nicht in einen einzigen Zustand `Failed`.

| Ereignis | Client | Ausführungscredit / Speicher |
|---|---|---|
| Nachweislich vor Übergabe abgelehnt | terminaler Fehler | freigeben |
| Dispatch angenommen | wartet | bleibt gebunden |
| Client-Timeout / Clientabbruch | terminal bzw. Empfänger weg | keine automatische Freigabe |
| RPC nach Dispatch verloren | Fehler, falls noch offen | `Unknown`, gebunden halten |
| GPU-/Backendende bestätigt | Ergebnis oder Fehler | nach Nutzung der Outputs freigeben |
| Qualifiziertes Fencing beendet alte Generation | Fehler / Wiederanlauf | alte Leases nach Nachweis auflösen |
| Nur Zeit verstrichen oder Health-Ping erfolgreich | keine neue Erkenntnis | nicht freigeben |

`CompletionEvidence` enthält Ticket, Generation, Zeitpunkt, Quelle und
Beweisart. Ein generischer gRPC-Status reicht nur dann als Endnachweis, wenn
der konkrete Backendvertrag das tatsächlich zusagt.

Ressourcenreconciliation bedeutet nicht automatisch GPU-Reset: Ein Reset kann
fremde Arbeit treffen und benötigt eine vorher vereinbarte Betriebsbefugnis.
Ohne Nachweismöglichkeit bleibt die Domäne nicht bereit; der Betreiber erhält
einen konkreten Recovery-Grund. Ein Neustart nur des Gateways ist kein Fencing
des weiterhin laufenden Backends.

## 5. Zwei Lebenszyklen, nicht einer

```text
Client:       waiting --------------------------> terminal
Execution:    reserved -> submitted -> running -> confirmed_finished
                                  \-> unknown -> reconciliation
PayloadLease: ================================ bis letzte Nutzung =====
```

Clientantwort und Ausführungsende werden jeweils genau einmal verbucht.
Verspätete oder doppelte Events sind anhand Ticket und Generation idempotent.
Ein alter Completion-Event darf keine neue Slotbelegung freigeben.

Die Payload-Lease umfasst Host-/Devicebytes und Shared-Memory-Referenzen.
Ein Registrierungsname darf nicht auf anderen Speicher umgebogen werden,
während laufende Requests ihn referenzieren. Unregister wird verzögert oder
explizit abgelehnt. CUDA-Pointer werden nicht als unkontrollierte Netzwerkwerte
akzeptiert. Für das lokale SDK gelten dokumentierte Speicher- und Streambesitzer.

OIP-Felder bleiben im Triton-Pfad unverändert erhalten. Ein backendprivater,
typisierter Payload-Handle vermeidet, dass das Runtime-Interface beliebige
OIP-Erweiterungen verlustbehaftet in eine neue Universal-Tensornachricht presst.
Der Core sieht weiterhin nur Referenzen und deklarierte Größen.

## 6. Fähigkeiten werden qualifiziert, nicht geraten

Ein Capability-Datensatz enthält Version und Nachweisstatus, unter anderem:

```text
cancel_scope: none | queued_only | cooperative | executing
preemption_boundary: none | request | token | kernel | graph | device_specific
completion_evidence: backend_final | cuda_event | fenced_generation
memory_accounting: declared | measured | enforced
resource_partition: none | sm_subset | other_qualified_partition
supported_shapes, tensor_schema_id, streaming_mode
resume_semantics, checkpoint_cost, measured_blocking_envelope
```

Eine behauptete Fähigkeit ohne Hardware-/Treiberqualifikation erhält keinen
verlässlichen Status. Eine Präemption ist erst wirksam, wenn das Backend sie
bestätigt; bis dahin bleiben Credits belegt. Graph- und Tokenunterbrechung
sind unterschiedliche Fähigkeiten. Unterschiedliche Capabilities dürfen in
derselben Produktinstallation nebeneinander existieren.

## 7. Zustand: statische Identität und dynamische Beobachtung trennen

`ExecutionIdentity` identifiziert Modell-/Enginehash, Präzision, Shape/Batch,
Vor-/Nachverarbeitung, Runtime, Provider, Serverkonfiguration, Gerät, Treiber,
CUDA-/Bibliotheksversionen und gewählten Datenpfad. Ein kryptographischer
Artefakthash dient Integrität; der alte FNV-Metadatenfingerprint bleibt als
Legacy-Indikator erhalten, nicht als Sicherheitsnachweis.

`StateSnapshot` enthält tatsächliche statt nur angeforderte Clock-Werte,
Power-Mode, Temperatur-/Throttlingzustand, aktuelle Ressourcenzuordnung,
Co-Tenant-Identitäten, warm/cold und optional Fortschrittsklassen.

Jedes Feld trägt Quelle, Messzeit, Einheit, Gültigkeitsdauer und Qualität:
`known`, `unsupported`, `stale` oder `unknown`. Nicht verfügbare EMC-Telemetrie
auf einer Plattform ist kein Wert von 0 MHz. Remote-Triton braucht für
Hardwarewissen einen vertrauenswürdigen Agenten oder ein begrenztes,
deklariertes Betriebsprofil; der Governor kann es nicht aus Metadaten erraten.

Nur **entscheidungsrelevante** Änderungen erhöhen die Zustandsversion.
Quantisierte Temperaturklassen und Hysterese vermeiden ein anderes Profil für
jede Sensorabweichung. Hardwarezustand wird als Event aufgezeichnet; ein Replay
liest keine aktuelle GPU und reproduziert trotzdem die getroffene Entscheidung.

## 8. Profilkatalog und Predictor

Kein riesiges neuronales Modell als erste Implementierung. Ein sparsamer
Katalog enthält qualifizierte Betriebspunkte; der bestehende Quantilschätzer
bleibt der Basispfad. Neue Zellen werden gezielt für beobachtete Kundenlasten
gemessen, nicht als vollständiges kartesisches Produkt aller Zustände.

Eine Prognose ist ein strukturiertes Ergebnis:

```text
Prediction {
  planning_cost, optimistic_cost,
  measurement_boundary,
  profile_id, state_version, resource_layout_version,
  applicability: exact_cell | validated_envelope | fallback | unknown,
  evidence: synthetic | empirical | qualified | analytical,
  sample_count, independent_run_count, validated_domain,
  risk_bound: optional, method_and_assumptions: optional
}
```

Ein hoher Quantilwert ist keine automatisch kalibrierte `risk_bound`.
Schätzungen außerhalb der gültigen Domäne geben `unknown` oder einen ausdrücklich
qualifizierten Rückfall zurück. Keine stillschweigende Interpolation über
Runtime-, Clock-, Präzisions- oder Enginegrenzen.

Schwere Auswertung und Profilbau laufen außerhalb des Actors. Der Core bekommt
eine begrenzte, versionierte Lookup-Sicht; Updates werden atomar als Event
sichtbar. Zwischen Prognose und Dispatch wird der Snapshot nochmals geprüft.
Veränderte Ressourcen oder abgelaufene Telemetrie erfordern Neuplanung.

## 9. Mehrere Regelkreise mit klarer Rangordnung

| Regelkreis | Eingabe / Aufgabe | Beschränkung |
|---|---|---|
| Schneller Dispatch | aktuelle Aufträge, gültige Prognosen | keine blockierenden Sensor-/SDK-Abfragen |
| Vertragsmonitor | Verbraucherzyklen, Lücken, Miss-Bursts | misst Anforderungen; setzt sie nicht neu |
| Profilanpassung | Beobachtungen und Drift | begrenzte Updates, keine verdeckte Optimismussteigerung |
| Ressourcen-/Energieregler | längerfristige Last, Kosten und Stellverzögerung | optional, Mindestverweildauer, exklusiver Stellgrößenbesitzer |
| Anwendungssemantik | autorisierte Hinweise und Gültigkeit | bleibt innerhalb freigegebener Vertragsgrenzen |

Priorität: Betriebssicherheit/Plattformlimits vor Vertragsgrenzen, danach
Ausführungswahl, danach Energieoptimierung. Der Energieregler darf nicht
gleichzeitig mit einem unabhängigen Regler dieselben Clocks gegeneinander
verstellen. Überlast, Variantenwahl und Taktänderungen benötigen gemeinsame
Hysterese-/Stabilitätstests und ein definiertes Zustandswechselbudget.

Aktuationszustände: `requested -> pending -> observed_stable`, alternativ
`failed/overridden`. Planung verwendet bis zur bestätigten Wirkung den alten
oder einen konservativen Übergangszustand. Thermal-Schutz wird niemals
ausgeschaltet. Read-only-Telemetrie ist Standard; Aktuation ist opt-in.

## 10. Native TensorRT-Stufen

Erste Stufe: feste Shapes, vorgebaute vertrauenswürdige Engines, ein Context
je tatsächlicher gleichzeitiger Ausführung, begrenzte Buffers, kein zusätzlicher
Enqueue-Vorrat (`pipelining_depth = 0`), Completion per geeignetem CUDA-Event.
Enginebau und Warm-up finden vor Admission statt. Ausgabe- und Kopierkosten
gehören je nach Messgrenze zum Vertrag.

Weitere Stufen: mehrere qualifizierte Varianten, feste Context-/Stream-Pools,
Graph-Cache mit begrenztem Speicher und expliziten Shape-/Adressschlüsseln,
danach kontrollierte Nebenläufigkeit und optionale Partitionierung.

Native Bindings werden in einem kleinen, separat geprüften FFI-Bereich
gekapselt. Das `unsafe_code = forbid` des Scheduling-Cores bleibt erhalten.
Keine globale Lockerung der Workspace-Regeln nur für bequemere CUDA-Aufrufe.

Engines sind vertrauenswürdige ausführbare Artefakte, keine beliebigen
Nutzerdaten. Native Fehler können Prozessgrenzen relevant machen; siehe
[NVIDIA zu TensorRT-Lebensdauer, Engine-Vertrauen und CUDA-Fehlern](https://docs.nvidia.com/deeplearning/tensorrt/latest/architecture/how-trt-works.html).
Ein Worker-Prozess kann den Fehlerumfang begrenzen, garantiert aber allein
keine vollständige GPU-Isolation. Datenpfad und Fencing müssen dazu passen.

## 11. Maximalausbau ohne Pflichtabhängigkeiten

- **Qualität:** kanonische Semantik je logischem Modell, validierte
  Qualitätsfronten je Aufgabe; Mindestqualität bleibt harte Auswahlgrenze.
- **Interferenz:** gerichtete, kontextbezogene Kombinationen; bekannte
  N-Wege-Lasten validieren, unbekannte Kombinationen nicht additiv erfinden.
- **LLM:** Prefill, Decode, Kontextlänge und KV-Budget getrennt; Fortsetzung
  benötigt echtes Backendwissen. Text erneut einsenden ist kein kostenloses
  Pause/Resume und keine nachgewiesene semantische Gleichheit.
- **Green Contexts:** optionale Layouts mit eigenem Profil und Fallback;
  ungenutzte Reserven sind ein sichtbarer Kostenfaktor.
- **XSched:** optionaler Mechanismus unter genau einer Policy-Zuständigkeit;
  nicht zwei konkurrierende Scheduler mit widersprüchlicher Priorisierung.
- **DAG:** Frame-/Epochenidentitäten, begrenzte Abhängigkeiten und
  Referenzzählung; Stornierung nur bei wirklich fehlendem Verbraucher.
- **Anwendungszustand:** Zeit bis Aktionspuffer leer, Qualitätsbedarf oder
  erlaubte Gültigkeitsdauer als authentisierte Hinweise mit TTL.
- **Weitere Geräte/Backends:** neue Executor-/Platform-Adapter; kein implizites
  Cross-GPU-Scheduling oder globales Clockschema in der ersten Version.

## 12. Migration und Rückfall

1. Lebenszyklusinvarianten auf dem alten Pfad absichern.
2. Legacy-Prognose und alte Konfiguration über Kompatibilitätsadapter erhalten.
3. Actor herauslösen, ohne außen OIP oder CLI aufzubrechen.
4. Neue Zustände zunächst nur protokollieren, neue Policy im Shadow-Modus
   berechnen. Shadow führt keine zusätzliche GPU-Arbeit aus.
5. Gültige neue Profile opt-in aktivieren; pro Profil und Domäne zurückschaltbar.
6. TensorRT Direct separat bauen/aktivieren. Triton-Installation benötigt
   keine CUDA-Entwicklungsbibliotheken zum Bauen des Standardprodukts.
7. Jede Forschungsoption hat Capability-Test, Abschalter und dokumentierten
   Rückfall. Fehlende Option verhindert nicht den Basisbetrieb.

Alte YAML-Dateien bleiben lesbar. Neue Funktionen bekommen explizite
Versionierung; unbekannte Felder bleiben Fehler. Ein alter Binary muss eine
neue, nicht unterstützte Konfiguration ablehnen statt sie teilweise auszuführen.
Neue Metriken stehen neben alten Namen mit dokumentierter Messsemantik.

## 13. Unverhandelbare Invarianten

- Kein Ressourcenlease wird allein durch verstrichene Prognosezeit frei.
- Kein Backendevent aus alter Generation ändert neue Credits.
- Keine Snapshot-/Profiländerung bleibt bei einer Entscheidung unsichtbar.
- Kein Client kann Klassen, Budgets oder Frischegrenzen über seine Befugnis heben.
- Kein fehlendes Telemetriefeld wird zu einem günstigen Messwert.
- Kein Schalter gibt einem nicht qualifizierten Mechanismus eine Garantie.
- Jede experimentelle Erweiterung lässt sich ohne Verlust des Basispfads weglassen.
