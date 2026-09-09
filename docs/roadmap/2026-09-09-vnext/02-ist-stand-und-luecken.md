# 02 — Ist-Stand, Wiederverwendung und offene Lücken

Geprüfter Commit: `fa4c909339e5cc6851b309c0ea8e8d451c6fab34`, 2026-09-09.
Dies ist eine gezielte Architektur-/Quelltextprüfung, kein erneuter vollständiger
Security-Audit. Frühere Reviews werden nicht pauschal als weiterhin offen übernommen.

## 1. Bereits vorhanden — nicht nochmals erfinden

| Bestandteil | Evidenz im Repository | Behandlung im Ausbau |
|---|---|---|
| Deterministischer, payloadfreier Kern | [core/lib.rs](../../../crates/vig-core/src/lib.rs) | Erhalten; neue Eingaben ausschließlich als aufgezeichnete Ereignisse |
| Capture-Zeit, Deadline, Alter, Kritikalität, Latest/FIFO/Stateful | [request.rs](../../../crates/vig-core/src/request.rs) | Semantik erhalten, versioniert ergänzen |
| Modellvertrag und Qualitätsherkunft | [model.rs](../../../crates/vig-core/src/model.rs) | Qualitätsnachweise und Verbraucherverträge ergänzen |
| EDF/Kritikalität, Look-ahead, Supersession, Varianten | [scheduler.rs](../../../crates/vig-core/src/scheduler.rs) | Referenzpolicy behalten, neue Strategien dagegen testen |
| Endliche Slots und Co-Run-Veto | [slots.rs](../../../crates/vig-core/src/slots.rs) | Auf Ressourcenbereiche und bestätigte Ausführungszustände abbilden |
| Offline-Quantile, Online-Schätzung, Margen | [profile.rs](../../../crates/vig-core/src/profile.rs), [estimator.rs](../../../crates/vig-core/src/estimator.rs) | Legacy-Schätzer als funktionsfähigen Rückfall erhalten |
| Triton-Transport, OIP und Gateway | [actor.rs](../../../crates/vig-gateway/src/actor.rs), [backend-triton](../../../crates/vig-backend-triton/src/lib.rs) | Ausführung herauslösen, externes Protokoll behalten |
| Kalibrierung und Metadatenfingerprint | [calibrate.rs](../../../crates/vig-cli/src/calibrate.rs), [fingerprint.rs](../../../crates/vig-backend-triton/src/fingerprint.rs) | Genauere Messidentität und Messmethodik |
| Simulation, Replays, Coverage und Peak-AoI | [vig-sim](../../../crates/vig-sim/src/lib.rs), [coverage.rs](../../../crates/vig-sim/src/coverage.rs) | Zustandswechsel und korrelierte Traces ergänzen |
| Quanten für generative Aufträge | [cooperative.rs](../../../crates/vig-gateway/src/cooperative.rs) | Legacy-Request-Zerlegung von echter Backend-Fortsetzung unterscheiden |

`ProfileHealth` wird inzwischen vom Scheduler zur Metrikbildung abgefragt.
Der frühere Befund „überhaupt nicht angeschlossen“ ist insoweit überholt.
Die Beziehung zwischen dieser Metrik und einer gezielten Profil-Fallback-Policy
bleibt ausdrücklich zu testen.

## 2. Was noch kein vorhandenes Feature ist

- Nativer TensorRT-Executor, Engine-/Context-/Stream-/Graph-Verwaltung.
- Hardwaretelemetrie als versionierte Scheduling-Eingabe.
- Profile mit Engine-Hash, tatsächlichem GPU-/EMC-Zustand und validierter Domäne.
- Durchgesetzte weakly-hard-Verträge oder kalibrierte Verletzungswahrscheinlichkeiten.
- Ressourcenpartitionierung, XSched-Präemption, vLLM-/TensorRT-LLM-Tokenkontrolle.
- Anwendungsübergreifende DAG-Gültigkeit und autorisierte semantische Anpassung.

Das sind Ausbauoptionen, keine zwingenden Blocker für jeden bestehenden Triton-Piloten.

## 3. Vorrangige technische Befunde

### B01 — Unsicheres Ausführungsende darf nicht durch Warten bekannt werden

Der neuere Commit unterscheidet `ExecutionState::Unknown` von `NotStarted`.
Das ist die richtige Richtung. In `actor.rs`, `on_backend_done`, wird bei
`Unknown` ohne vorangehendes Timeout aber `release_at = now + inference_timeout`
gesetzt. `release_expired_quarantine` (um Zeile 875) gibt den Kredit danach
ohne Backendbeweis frei.

Zweiter Pfad: War der Request bereits im Timeout, ist `timed_out` wahr.
Die Bedingung `unknown_execution && !timed_out` greift dann nicht; eine spätere
unbekannte Fehlermeldung läuft in `BackendFailure`, das den Slot freigibt.

Ein Timeout ist eine Beobachtungsgrenze, keine obere Ausführungsgrenze.
Beide Pfade können die physische Kapazität größer erscheinen lassen, als sie
nachgewiesen frei ist. Reparaturentwurf und Repros: `NV-00`.

Der allgemeine Shutdown wartet inzwischen zeitbegrenzt auf den Server-Task;
der frühere unbegrenzte Server-Await ist nicht mehr derselbe offene Befund.
Drain-Erfolg bei bereits abgebrochenem RPC und noch gehaltenen Ausführungsleases
muss dennoch separat geprüft werden. RPC-Zähler sind kein GPU-Lease-Zähler.

### B02 — Bytebudget und Payload-Lebensdauer müssen zusammenfallen

[service.rs](../../../crates/vig-gateway/src/service.rs), um Zeile 375, hält
den Byte-Permit lokal im Clientaufruf. Er zählt inzwischen auch typisierte
Tensorinhalte; dieser frühere Zählfehler ist behoben. Ein Clientende kann
aber vor dem Ende der Backendarbeit liegen. Für den nativen Pfad muss die
Reservierung deshalb der tatsächlichen Payload gehören, nicht dem wartenden
Client-Future. Shared-Memory-Registrierungen brauchen zusätzlich Nutzungsleases.

### B03 — „Gleicher Belegungsgrad“ ist kein vollständiger Laufzeitzustand

Der Schätzer unterscheidet Modell, Variante und Slotbelegung, nicht die
Identität der gleichzeitig laufenden Arbeit. Der Fingerprint bildet Server-
und Modellmetadaten ab, keine Gewichts-/Enginebytes, GPU, Treiber oder
Power-Zustände. Das reicht nicht als Gültigkeitsnachweis für eine andere
Ausführungsumgebung. `NV-03` bis `NV-06`, später `NV-11`.

### B04 — Kalibrierung misst noch nicht automatisch die Produktionsverteilung

`calibrate::measure` führt Requests direkt hintereinander aus und überspringt
fehlgeschlagene Messungen. Warm ausgelasteter Betrieb und periodischer Betrieb
mit Leerlücken müssen getrennt charakterisiert werden. Fehlversuche gehören
in den Datensatz und die Profilzulassung, nicht nur erfolgreiche Laufzeiten.
Der Messpfad ist zudem Triton-spezifisch. `NV-05`.

### B05 — Die universelle 2x-Co-Run-Begründung ist mathematisch falsch

`NO_CORUN_SLOWDOWN_PERCENT = 200` in `calibrate.rs` und
[ADR-0018](../../adr/0018-calibrate-hardware-not-requirements.md) begründen ein
Veto mit einem angeblich allgemeinen Durchsatz-Break-even.

Gegenbeispiel: A braucht solo 1 ms, B 100 ms. Parallel braucht A 2 ms und B
weiter 100 ms. A wurde um 2x verlangsamt, beide sind gemeinsam dennoch nach
100 statt seriell 101 ms fertig. Bei enger A-Deadline kann Parallelität
trotzdem unerwünscht sein — aber das ist ein anderes Kriterium.

Eine Schwelle kann eine konservative Betreiberpolicy sein, kein allgemeiner
Satz über unterschiedliche Modellpaare. Directed-Messungen, absolute Zeit,
Mindestfortschritt und Vertragsziele müssen die Entscheidung begründen.

### B06 — Messgrenzen und Gültigkeit sind produktrelevant

`CoverageTracker` trennt inzwischen Antwortalter und Peak-AoI; das ist ein
Fortschritt. Coverage markiert weiterhin Lieferfenster, nicht beliebige
Verbraucher-Abtastzeitpunkte mit noch gültigem Bestandsresultat. Die
Initialisierung der Peak-AoI bei Messbeginn ist eine Messkonvention, keine
vorher tatsächlich empfangene Information. `NV-01` definiert separate Größen
und einen expliziten Zustand „noch kein Resultat“.

Für Varianten gilt weiter: gleiche Tensorform beweist keine gleiche Semantik.
Ein Wechsel benötigt zusätzlich Vor-/Nachverarbeitung, Koordinatensystem,
Labelordnung, Quantisierung und Qualitätszulassung. `NV-10`.

## 4. Architekturgrenzen, die erhalten bleiben sollen

1. Keine CUDA-, Dateisystem-, Netzwerk- oder Sensorabfrage in `vig-core`.
2. Keine automatische Änderung von Anforderungen durch den Kalibrator.
3. Keine unsichtbare zweite, unbegrenzte Queue hinter dem Scheduler.
4. Keine Supersession zustandsbehafteter Sequenzen ohne ausdrücklichen Vertrag.
5. Keine frei werdende GPU allein durch Clientende oder Schätzzeitablauf.
6. Keine erfundene Qualitätsgleichheit, Hardwarefähigkeit oder garantierte Latenz.

## 5. Verifikation dieses Ausgangspunkts

Ausgeführt mit `/home/dd/.cargo/bin/cargo`:

```text
test --workspace --locked                         PASS
test --workspace --locked -- --list                217 Tests
fmt --all -- --check                              PASS
clippy --workspace --all-targets --locked -- -D warnings   PASS
```

Keine neue Jetson-/TensorRT-/XSched-Messung, kein neuer Dauerlauf und keine
vollständige Wiederholung der historischen Repro-Suiten. Ein grüner Testlauf
widerlegt B01–B06 nicht; er zeigt die bestehende Regressionstestbasis.

## 6. Migrationsbilanz

Wiederverwendbar sind Policy, Verträge, Zeittypen, Queueverwaltung, große Teile
der Slot-/Variantenlogik, Tests und Simulation. Herauszulösen sind hauptsächlich
Ausführung, Payloadbesitz und Messidentität. Eine belastbare Prozentzahl für
„unveränderten Code“ gibt es nicht. Der Plan verlangt pro Extraktionsschritt
Verhaltensgleichheit und einen weiterhin funktionierenden Triton-Pfad.
