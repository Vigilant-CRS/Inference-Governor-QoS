# Code-, Betriebs- und Produktreview · 15. September 2026

## Urteil

**Ein bezahlter, eng begrenzter Pilot ist plausibel. Eine breite Produktionsfreigabe und ein belegter Product-Market-Fit fehlen.** Der technische Kern ist substanziell; die dringendsten Probleme liegen in der Durchsetzung von Grenzen, in der Messkette und im Verhalten nach Abbrüchen. Mehr Scheduler-Funktionen sind deshalb derzeit weniger wertvoll als belastbare Qualifizierung und ein wiederholbarer Kundennutzen.

Der aussichtsreichste Einstieg ist ein Robotik-/Industrie-OEM mit mehreren konkurrierenden Modellen auf einem Edge-Rechner, bereits beobachteten Aktualitätsproblemen und wirtschaftlichem Interesse an weniger Hardware, weniger Ausfällen oder kürzerer Integrationszeit. Das ist eine **Positionierungshypothese**, keine nachgewiesene Nachfrage nach Vigilant.

## Umfang und Nachweise

- Quellstand: `90f517c`, Rust-Workspace, Gateway, Autotuning, Messwerkzeuge, Android-Backend, ROS-Integration, CI/Release, Produktdokumentation.
- `InferenceQoS-runtime` ist **kein zweites Git-Repository**, sondern die lokale Betriebs-, Modell- und Messablage. Gelesen wurden insbesondere aktuelle Übergaben, Start-/Messskripte und ausgewählte Rohdaten. Die eingebettete fremde XSched-Codebasis wurde nicht vollständig auditiert.
- Vorhandene Suite erneut ausgeführt: **884 bestanden, 0 fehlgeschlagen, 6 ignoriert, 57 Suiten**, `cargo test --workspace --locked --offline`. [Log](workspace-tests.log).
- **Acht zusätzliche Gegenproben schlagen am geprüften Code fehl**, weil die jeweils erwartete Eigenschaft verletzt wird: sechs im Hauptworkspace, zwei im Android-Backend. Tests liefen in einer Kopie unter `/tmp`, mit Mock-Backend bzw. auf dem Host, ohne GPU-Inferenz. [Hauptlog](counterexamples.log), [Android-Log](android-counterexamples.log), [Reproduktionsskript](reproduce.py).
- Aktuelle Container-Ports lesend geprüft. [Beobachtung](runtime-ports.txt).
- Keine neue Hardwarequalifikation, kein Last-/Dauerlauf, kein vollständiger Security-Audit. ROS-Python-Tests konnten mit dem vorhandenen System-Python nicht starten: `No module named pytest`. Die 884 Workspace-Tests decken sie und den ausgeschlossenen Android-Workspace nicht ab. fmt/clippy/deny wurden in diesem Review nicht erneut ausgeführt.
- Produktcode und laufende Container wurden nicht verändert. Bereits vorhandene und währenddessen hinzugekommene Dokumentationsänderungen wurden nicht überschrieben. Neu hinzugefügt wurde nur dieser Reviewordner.

## Priorisierte Fehler

P1 = vor externer Nutzung des betroffenen Pfads beheben. P2 = relevantes Robustheits-/Korrektheitsproblem, im folgenden Stabilisierungsschritt beheben.

### R01 · P1 · Der laufende Triton umgeht den vorgesehenen Netzwerkschutz

**Stelle:** `InferenceQoS-runtime/skripte/triton-umzug.sh:15`.

Das Skript veröffentlicht `8000`, `8001` und `8002` ohne Hostadresse. Der tatsächlich laufende Container zeigt:

```text
vig-triton  0.0.0.0:8000-8002->8000-8002/tcp, [::]:8000-8002->8000-8002/tcp
```

Wer diese Ports erreichen kann, spricht Triton direkt an und umgeht Authentifizierung, Aufnahmeprüfung und Scheduling des Governors. Zusammen mit `--ipc=host` und `--allow-client-shm=true` ist das deutlich mehr als ein offener Metrikport. Eine zusätzliche Firewall kann Zugriffe begrenzen; deren externe Erreichbarkeit wurde nicht getestet.

**Beheben:** Für den lokalen Messaufbau Ports ausdrücklich an `127.0.0.1` binden; im Produktdeployment Triton nur intern erreichbar machen, wie die vorhandene Compose-Datei es bereits tut. Den laufenden Container dafür in einem geplanten Wartungsschritt neu anlegen. Außerdem den Image-Digest und die Baseline-Einstellungen im Runtime-Start festhalten.

### R02 · P1 · `trust: strict` schützt Regionsnamen, aber keine zugrunde liegenden Segmente

**Stellen:** [Registrierung](../../../crates/vig-gateway/src/service.rs), Zeile 810; [Region/Registry](../../../crates/vig-gateway/src/shm.rs), Zeile 61; Referenzprüfung in `service.rs:340`.

ALPHA registriert `name=alpha, key=/vig_private`. BETA darf danach `name=beta, key=/vig_private` registrieren. Der Besitzer wird nur am frei gewählten **Regionsnamen** geprüft; `Region` speichert den physischen Schlüssel gar nicht. Ein gemeinsamer `/vig_`-Präfix trennt die Aufrufer nicht. BETA kann anschließend seinen eigenen Alias in der Referenzprüfung verwenden.

**Nachweis:** `review_20260915_strict_rejects_foreign_segment_alias` zeigt, dass beide Registrierungen das Gateway passieren. Das Mock-Backend bestätigt sie; ein tatsächliches Lesen fremder Bytes in Triton wurde nicht ausgeführt.

**Beheben:** Physische Segmente und ihre Besitzer erfassen, fremde Aliase/Überlappungen verweigern und Registrierungsrechte an vertrauenswürdig zugewiesene Segmente oder aufruferspezifische Namensräume binden. Allein eine nachträgliche Alias-Tabelle löst den Fall „Angreifer registriert fremden Schlüssel zuerst“ nicht. Statusinformationen entsprechend begrenzen.

### R03 · P1 · Autotuning kann einen geschützten Stream deutlich verschlechtern

**Stellen:** [Zielgröße und Entscheidung](../../../crates/vig-cli/src/autotune/tune.rs), Zeilen 260 und 531.

`objective()` reduziert alle geschützten Streams und Lastpunkte auf ein Maximum. `decide()` prüft nur dieses Maximum. Damit wird beispielsweise akzeptiert:

| Geschützter Stream | Vorher, verfehlte Takte | Nachher |
|---|---:|---:|
| A | 200 ‰ | 100 ‰ |
| B | 0 ‰ | 100 ‰ |

Das Maximum verbessert sich von 200 auf 100 ‰; B verliert aber neu jeden zehnten Takt. Die Gegenprobe verwendet 10.000 Takte, sodass dies kein Rundungs-/Kleinstichprobenfall ist. Auch die Bestätigungspaare wenden dieselbe verdichtete Entscheidung an.

**Nachweis:** `protected_stream_regression_must_not_be_hidden_by_maximum` erhält `Ok("protected streams 100 ‰ instead of 200 ‰ ...")`.

**Beheben:** Vor dem Vergleich der Gesamtzielgröße pro `(Stream, Lastpunkt)` eine Regressionsgrenze prüfen. Erlaubte Verschlechterungen müssen aus dem jeweiligen Vertrag kommen. Zusätzlich längste Versorgungslücken und gegebenenfalls M/K/L-Budgets berücksichtigen. Ein besseres Gruppenmaximum genügt nicht für die Zusage „geschützte Streams werden nicht schlechter“.

### R04 · P1 · `vig-fit` erklärt fehlende Inferenzlieferungen zu einem belastbaren Ergebnis

**Stellen:** [workload.rs](../../../crates/vig-bench/src/workload.rs), Zeile 244; [vig-fit.rs](../../../crates/vig-bench/src/bin/vig-fit.rs), Zeilen 369 und 563.

`drive()` erzeugt für jeden konfigurierten Stream einen `StreamReport`, auch wenn Verbindungsaufbau oder sämtliche Aufrufe scheitern. `vig-fit` prüft anschließend das Vorhandensein dieser Berichtszeilen, nicht `delivered`. Das JSON setzt `conclusive` auf `!rows.is_empty()`.

**Nachweis:** `no_deliveries_must_not_be_conclusive` verwendet einen geschlossenen lokalen Port: null Lieferungen, aber eine Zeile mit 1000 ‰ Ausfall und **`conclusive: true`**. Der Bericht wertet das als Ergebnis gegen den Governor. In anderen Fehlerkonstellationen kann auch der Vergleich zwischen den Armen verfälscht werden.

**Beheben:** Verbindungs-, Protokoll-/Modellfehler, absichtliche Governor-Ablehnungen und tatsächliche Inferenzlieferungen getrennt im Ergebnis führen. Ein Integrationsfehler muss die Zelle ungültig machen; legitimes Aushungern unter Last bleibt dagegen ein gültiger negativer Befund. Ergebnis nur für eine vollständig valide Vergleichsmatrix bewerten. Entsprechende maschinenlesbare Zustände und Exitcodes durch `autotune` erhalten.

### R05 · P1 · `autotune --endpoint` kann eine andere Maschine nennen als die vermessene

**Stellen:** [CLI-Flag](../../../crates/vig-cli/src/main.rs), Zeile 60; [discover/measure](../../../crates/vig-cli/src/autotune.rs), Zeilen 1062 und 1089.

Das Flag bezeichnet laut Hilfe den zu vermessenden Inferenzserver. Existiert die Konfigurationsdatei bereits, liest `discover()` nur deren Modelle. `measure()` reicht dieselbe Datei an `calibrate()` weiter; dieses benutzt `backend.grpc_endpoint` aus der Datei. Das Flag bleibt dagegen Grundlage von Bericht und Resume-Fingerabdruck.

**Nachweis:** Eine gültige Gate-M3-Konfiguration mit `127.0.0.1:8001` wird trotz explizitem `different-machine.invalid:8001` akzeptiert. Weder Konflikt noch Überschreibung erfolgen. Test: `explicit_endpoint_must_not_silently_disagree_with_config`.

**Beheben:** Eine eindeutige Regel festlegen: explizite CLI-Adresse in eine effektive Arbeitskonfiguration übernehmen oder Abweichung ablehnen. Default und ausdrücklich gesetztes Flag unterscheidbar machen. Berichte müssen die tatsächlich verwendeten Endpunkte je Ressourcendomäne tragen.

### R06 · P1 · TFLite verliert Abschlussnachweise, wenn der wartende Request abbricht

**Stelle:** [models.rs](../../../backends/android-tflite/src/models.rs), Zeilen 107 und 145.

Der Modellthread berechnet den Auftrag unabhängig vom wartenden HTTP/gRPC-Aufruf. `stats.record()` läuft jedoch erst nach `rx.await` im aufrufenden Future. Wird dieses Future abgebrochen, rechnet der Thread fertig, aber der Abschlusszähler steigt für diesen Auftrag nie.

Der Governor verwendet genau diese Statistik zur Auflösung unklarer Ausführungen. Verlorene Abschlüsse können deshalb Kredite dauerhaft blockieren. Die unbeschränkte Modellwarteschlange ist zusätzlich ein Robustheitsthema für direkte Backendaufrufe.

**Nachweis:** `review_20260915_cancelled_call_still_counts_completed_gpu_work`: ersten Auftrag nach dessen Start abbrechen, ihn fertig rechnen lassen, zweiten Auftrag vollständig ausführen. Zwei abgeschlossene Berechnungen, Zähler **1 statt 2**.

**Beheben:** Statistik im besitzenden Modellthread nach Ende des Auftrags und vor der optionalen Antwortzustellung führen. Auch ein verschwundener Empfänger muss ein gezähltes Ende hinterlassen. Warteschlange und Nutzlastaufnahme begrenzen.

### R07 · P2 · Parallele Registrierungen überschreiten das Shared-Memory-Limit

**Stellen:** [admit/record](../../../crates/vig-gateway/src/shm.rs), Zeilen 123 und 139; Aufrufabfolge in `service.rs:813`.

Prüfung und Eintragung liegen beiderseits eines Backend-`await`. Mehrere Requests können gleichzeitig den noch freien Platz sehen. Die einzelnen Mutex-Zugriffe schützen die Map, aber nicht diese Transaktion.

**Nachweis:** `review_20260915_parallel_registrations_obey_limit`: Limit **1**, zwei parallele Registrierungen unterschiedlicher Namen, beide erfolgreich, Registry-Länge **2**.

**Beheben:** Plätze/Schlüssel vor dem Backendaufruf atomar reservieren; Erfolg bestätigt die Reservierung, Fehler oder Cancellation geben sie über einen Guard zurück. Auch Rennen zwischen Registrierung und Abmeldung prüfen.

### R08 · P2 · Resume vertraut erledigten Schritten trotz fehlender Ergebnisdatei

**Stellen:** [fingerprint/run](../../../crates/vig-cli/src/autotune.rs), Zeilen 1431 und 1697.

Sind alle Schrittnamen erledigt, kehrt der Befehl erfolgreich zurück, ohne Existenz oder Hash der eingefrorenen Konfiguration zu prüfen. Der Fingerabdruck enthält nur CLI-Endpunkt und Eingabedatei; Messoptionen, Binärversion, Backendidentität und Zustand der Ergebnisdateien fehlen.

**Nachweis:** `absent_frozen_artifact_must_invalidate_completed_state` verwendet eine vollständige gültige Beispielkonfiguration, erledigte Schritte und eine fehlende Messdatei. Ergebnis: `Nothing left to do`, Exitcode **0**.

Das ist in dieser Runtime besonders relevant: ältere JSON-Berichte tragen nach dem Ordnerumzug weiterhin absolute Pfade ohne das neue `messungen/`-Verzeichnis. Beispiel: `messungen/autotune-laptop-2026-09-15f/qualification.json`.

**Beheben:** Ergebnisartefakte samt Hash vor Wiederverwendung prüfen; Messparameter und Versionen in die Laufidentität aufnehmen. Hardware-/Backendwechsel am gleichen Port müssen einen expliziten Neuabgleich erfordern. Historische Rohberichte unverändert lassen und über ein Umzugsmanifest zuordnen. Berichte/Zustand atomar schreiben; Persistenzfehler nicht nur ausgeben und anschließend Erfolg melden.

### R09 · P2 · Das TFLite-Backend prüft die Tensorform nicht

**Stelle:** [prepare_inputs](../../../backends/android-tflite/src/service.rs), Zeile 115.

Name, Datentyp und Bytezahl werden geprüft, `given.shape` dagegen nicht. Eine anders angeordnete Form mit derselben Elementzahl wird unter der fest geladenen Modellform interpretiert.

**Nachweis:** `[1,3,2,2]` wird für ein Modell mit `[1,2,2,3]` akzeptiert, wenn beide 12 UINT8-Bytes tragen. Test: `review_20260915_rejects_wrong_shape_even_with_same_byte_count`.

**Beheben:** Form und Rang gegen die Metadaten prüfen; falls dynamische Formen unterstützt werden sollen, deren Zulässigkeit und Interpreter-Resize ausdrücklich implementieren. Unterschiedliche Layouts nicht aus gleicher Bytezahl ableiten.

## Weitere technische und betriebliche Verbesserungen

1. **Multi-Domain-Fit ist noch kein fairer Vergleich.** `vig-fit.rs:313` schickt den direkten Arm insgesamt an `base.backend_endpoint`, während der Governor die Modell-Endpunkte auflöst. Zusätzlich baut `:248` nur die erste Modelleingabe. Für Multi-Endpoint-, Multi-Input- und generative Modelle entweder beide Arme korrekt abbilden oder den Umfang vor Messbeginn ausdrücklich ablehnen. Dies wurde statisch festgestellt, nicht separat auf Hardware reproduziert.
2. **Qualifikationsstatus muss bis in die Konfiguration reichen.** Verworfene Soloprofile bleiben laut bestehender Dokumentation unmarkiert in `measured.yaml`. Ein daneben liegender ablehnender Bericht schützt nicht davor, diese Datei später allein an `serve` zu geben. Messstatus pro Profil und eine durchgesetzte Gültigkeitsregel fehlen weiterhin.
3. **Backend-Erholung bleibt eine Produktlücke.** [ADR-0042](../../adr/0042-an-end-is-proven-not-assumed.md) beschreibt den dauerhaften Verlust quarantänisierter Slots. Boot-/Instanzkennung und Ausführungstickets sind der saubere nächste Vertrag zwischen Gateway und Backend. Slots allein nach einem Timer freizugeben wäre keine tragfähige Reparatur.
4. **Die CI-Grenze stimmt nicht mit der Produktgrenze überein.** Android ist aus dem Workspace ausgeschlossen, ROS liegt außerhalb von Cargo. Hosttests für Android, Python-/Transporttests und einen geeigneten ROS-Smoke-Test separat in CI aufnehmen. Der aktuelle ROS-Livetest in der Dokumentation ist ein Fortschritt, ersetzt aber kein wiederkehrendes Gate.
5. **Betriebsskripte versionieren.** Relevante Start-, Mess- und Sperrlogik aus der unversionierten Runtime in gepflegte Werkzeuge überführen; schwere Modelle/Logs weiterhin extern halten. Ein Manifest muss Quellcommit, Binärhash, Modellhash, Konfiguration und tatsächlich gestartete Containeroptionen verbinden.
6. **Sperrprotokoll vereinheitlichen.** `autotune-tuned-laptop.sh` startet Builds direkt und setzt nur `measure-pending`, ohne anschließend die exklusive `quiet.lock` zu halten. Ein zuvor begonnener Build kann weiterlaufen. Alle Mess-/Buildpfade sollten dieselben Sperren nutzen; Flags allein ersetzen den gegenseitigen Ausschluss nicht.
7. **Verträge und Gegenproben statt weiterer Historienkommentare.** Die Trennung Kern/Actor/Backend ist gut. Dateien wie `actor.rs` und `autotune.rs` tragen inzwischen mehrere Lebenszyklen und umfangreiche Historie. Nach den Fehlerkorrekturen Persistenz, Messprotokoll und Backendnachweise in klar begrenzte Komponenten auslagern; die hier gezeigten Integrationsinvarianten erhalten.

## Aussagekraft der bisherigen Messungen

Die offenen Gegenmessungen, dokumentierten Nachteile und Korrekturen sind eine Stärke. Die Zahlen belegen dennoch nur einzelne Konfigurationen.

Besonders wichtig ist der **spätere Pixel-2-Lauf mit längeren Fenstern** in `InferenceQoS-runtime/messungen/pixel2-2026-09-15-usecase-fenster.log`. Er verwendet als Eingabe `vig-slots2-saturated.yaml`, vermisst erneut und meldet bei 90 % Last **293 → 99 ‰** geschützte Ausfälle. Bei 100 % stehen **4 → 4 ‰**, bei 110 % **4 → 0 ‰**, bei 125 % **3 → 3 ‰**. Die Pose verschlechtert sich in diesen vier Punkten. Das ist keine identische Wiederholung des alten 10-Sekunden-Fensters und widerlegt dessen Einzelbeobachtung nicht; es zeigt aber, dass **208 → 0 ‰ kein allgemeines Geräteversprechen ist**. Auch die Nichtmonotonie der Lastpunkte verlangt Wiederholungen und thermische Kontrolle.

**Abgleich zum Abschluss:** Die parallel bearbeitete README nennt inzwischen die längeren Fenster und ersetzt die alte Telefonzahl. Der ergänzte [Validierungsbericht](../../benchmark/validierung-autotune.md) nennt für Pixel 5 bei 90 % **497 → 208 ‰**, aber bei 125 % eine Verschlechterung von **3 → 32 ‰**. Auf beiden Telefonen wurde keine Tuningänderung übernommen. Diese Dokumentationskorrektur ist bereits erfolgt und kein noch offener Reviewauftrag. Offen bleibt im Code die Zusammenfassung: `vig-fit::finding()` wählt nur den ersten Lastpunkt, an dem die direkte Versorgung die Schwelle überschreitet; spätere Nachteile erscheinen nicht im Urteilssatz. Das Eignungsurteil sollte sämtliche relevanten Lastpunkte und deren Zielkonflikte zusammenfassen.

Empfehlungen für die nächste belastbare Evidenz:

- Stärkste Triton-Baseline nach sämtlichen Messkorrekturen erneut fahren, einschließlich Ressourcenlimits, Queue-Timeouts und passenden Clientregeln.
- Gleiche gültige Requests, gleiche Modelle/Varianten, gleiche Endpunkte und Hintergrundanforderungen je Vergleich; Integrationsfehler entwerten die Zelle.
- Reihenfolge zwischen Läufen tauschen/randomisieren; wiederholte gepaarte Läufe, längere Lastphasen und Streuung berichten. Zwei immer gleich angeordnete Paare beseitigen thermischen Drift nicht.
- Aktualitätsabdeckung, längste Lücke, Erkennungsqualität, Hintergrundfortschritt und End-to-End-Latenz gemeinsam bewerten. P99-Profilwerte sind keine harte Laufzeitgarantie.
- Messlast und fremde Last während des Laufs beobachten. Ruhe davor/danach beweist keine Ruhe währenddessen; bei Remote-Backends muss auch das Ziel beobachtet werden.
- Einen realen Jetson-Betriebspunkt sowie eine zweite repräsentative Kundenplattform qualifizieren. Android bestätigt die Backendnaht, qualifiziert aber keinen Jetson.

## Markt und Wettbewerb

### Gibt es einen Markt?

**Für das Problem ja; Zahlungsbereitschaft für dieses Produkt ist noch offen.** IFR meldet für 2024 knapp 200.000 verkaufte professionelle Serviceroboter, darunter 102.900 in Transport/Logistik (+14 %). Die Erhebung basiert auf einer Stichprobe von 294 Lieferanten. Diese Zahlen zeigen ein relevantes Umfeld, sind aber weder Vigilants adressierbarer Markt noch ein Beleg, dass diese Roboter konkurrierende GPU-Modelle betreiben. [IFR, World Robotics 2025](https://ifr.org/news/service-robots-see-global-growth-boom/1).

**Mein bevorzugter Erstkunde:** Hersteller mobiler Industrie-/Serviceroboter mit ungefähr 100–2.000 geplanten Geräten, mehreren Kamera-/Wahrnehmungsmodellen, optionaler Berichterstellung und messbaren Versorgungsproblemen auf einem gemeinsamen Rechner. Ansprechpartner: Leitung Embedded/Perception/Robotik und der Verantwortliche für Stückkosten oder Einsatzzuverlässigkeit. Diese Stückzahl ist ein Suchprofil, keine Marktstatistik.

**Zweite Wahl:** industrielle Vision-Edgeboxen mit mehreren unterschiedlich wichtigen Streams. Reine Videoanalyse trifft allerdings auf sehr starke kostenlose NVIDIA-Werkzeuge. Automotive und Medizintechnik wären wegen Qualifizierung, Haftung und langen Beschaffungswegen ein späterer Markteintritt; die vorhandene Messbasis trägt derzeit kein entsprechendes Einsatzversprechen.

### Konkurrenz nach tatsächlicher Kaufalternative

Stand der Quellenprüfung: 15.09.2026. „Kostenlos“ bezieht sich auf die Softwarebasis, ohne Hardware, Integration und Support. Funktionsnähe ist keine Aussage über bessere gemessene Leistung.

| Alternative | Preis-/Lizenzbasis | Stärken und Verhältnis zu Vigilant |
|---|---|---|
| **Triton + gute Konfiguration + Clientlogik** | Serverquellcode ohne Lizenzgebühr, BSD-3-Clause; Containerbestandteile separat betrachten | Modellübergreifende Ressourcen/Prioritäten; Dynamic Batcher mit Queuegröße, Prioritäten und Timeouts. Wichtigste vorhandene Baseline. Vigilant muss den zusätzlichen Wert von Aktualitätsverträgen, Ersetzbarkeit und Vorhersage modellübergreifend beweisen. [Lizenz](https://github.com/triton-inference-server/server/blob/main/LICENSE), [Rate Limiter](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/rate_limiter.html), [Queue Policies](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/batcher.html) |
| **NVIDIA Holoscan** | SDK-Quellcode Apache-2.0; Integration und weitere Komponenten gesondert | Streaminggraph, verschiedene Scheduler, asynchrone Puffer mit „latest frame wins“. Direkter Konkurrenzdruck bei Aktualität; noch kein fairer Vergleich im Projekt. Der mögliche Vigilant-Vorteil ist Nachrüstung vor vorhandenen OIP-Servern und deren modellübergreifende Verträge. [SDK/Lizenz](https://github.com/nvidia-holoscan/holoscan-sdk), [Scheduler](https://docs.nvidia.com/holoscan/sdk-user-guide/components/schedulers) |
| **ROS 2 QoS + Executor-/Anwendungslogik** | Keine zusätzliche Vigilant-Lizenz; eigener Entwicklungsaufwand | Keep-last, Deadline und Lifespan lösen Teile des Problems im Nachrichtentransport. Sie sind kein automatischer Nachweis einer GPU-Ausführungsdeadline. Der Kunde wird zuerst prüfen, ob diese vorhandenen Mittel genügen. [ROS-QoS](https://docs.ros.org/en/humble/Concepts/Intermediate/About-Quality-of-Service-Settings.html) |
| **XSched** | Apache-2.0 | XPU-Scheduling und Präemption, Triton-Integration. Sowohl technischer Baustein als auch Alternative für Priorität/Blockierung; dessen Existenz macht reine Präemption nicht zu einem exklusiven Verkaufsargument. Vigilants Wert läge darüber in fachlichen Aktualitätsverträgen und Qualifizierung. [Projekt](https://github.com/XpuOS/xsched), [Lizenz](https://github.com/XpuOS/xsched/blob/main/LICENSE) |
| **NVIDIA DeepStream** | NVIDIA beschreibt die aktuelle Softwarebasis als frei zugängliches Open-Source-Angebot; Bedingungen je ausgeliefertem Artefakt prüfen | Umfangreiche Video-/Multi-Sensor-Pipelines, starke Integration und Skalierung. Für Visionkunden oft die naheliegende Gesamtplattform; Vigilant ist ein schmalerer Scheduling-Baustein. [Herstellerangebot](https://developer.nvidia.com/deepstream-sdk) |
| **NVIDIA Run:ai** | Auf der geprüften Produktseite kein öffentlicher Standardpreis | GPU-Ressourcenverwaltung und Orchestrierung größerer Infrastruktur. Angrenzend; keine direkte Gleichsetzung mit Aktualitätsschutz innerhalb eines einzelnen Roboters. [Herstellerangebot](https://www.nvidia.com/en-us/software/run-ai/) |
| **NVIDIA AI Enterprise** | Listenpreis **4.500 USD/GPU/Jahr**, Standard-Support eingeschlossen; Sonderkonditionen vorhanden | Breites Enterprise-Software-/Supportangebot. Ein Preisanker für Unternehmenskunden, aber kein gleichartiger Einzelgeräte-Scheduler und kein fairer alleiniger Vergleichsmaßstab. [Offizielle Preisliste](https://docs.nvidia.com/ai-enterprise/planning-resource/licensing-guide/latest/pricing.html) |

**Folgerung:** Der stärkste Wettbewerb ist die bestehende kostenlose Plattform plus kompetenter Integration. Die schwerer kopierbare Differenzierung muss aus nachweislich besserer Versorgung bei vertretbarem Hintergrundverlust, reproduzierbarer Qualifizierung, verlässlichem Betrieb und geringem Integrationsaufwand entstehen. Ein neues Scheduling-Verfahren allein schafft noch keinen dauerhaften Wettbewerbsvorteil.

## Preisbewertung

Aktuelle Richtpreise laut [LICENSING.md](../../../LICENSING.md): Pilot ab 25.000 € für 8–12 Wochen; weitere Plattform ab 15.000 €; 15/10/6 € je Gerät und Monat; Dauerlizenz 350/240/140 €; OEM-Produktlinie ab 60.000 €/Jahr. Evaluation ist frei, bis drei eigene Produktionsgeräte sind nach der Zusammenfassung frei; OEM-Auslieferung bleibt separat lizenzpflichtig.

**Bewertung:** Der laufende Gerätepreis ist für hochwertige Industriehardware grundsätzlich plausibel. Die 25.000 € Einstiegshürde ist für einen noch nicht unabhängig belegten Nutzen hoch. Eine pauschale Aussage „billiger als die Konkurrenz“ wäre falsch: die wichtigsten Softwarealternativen beginnen bei null Lizenzgebühr, ihre Integrationskosten sind jedoch nicht null.

Beispielrechnung über drei Jahre, gleichbleibende Flotte, 25.000 € Pilot, monatliche Gerätepreise der Tabelle, ohne OEM-Angebot, Steuern, zusätzliche Plattform und Kundenintegration:

| Geräte | Lizenzkosten pro Gerät, 36 Monate | Pilotanteil pro Gerät | Summe pro Gerät |
|---|---:|---:|---:|
| 10 | 540 € | 2.500 € | **3.040 €** |
| 100 | 540 € | 250 € | **790 €** |
| 1.000 | 360 € | 25 € | **385 €** |

Die eigene Verkaufsargumentation nennt hypothetisch 700–900 € Hardwareersparnis je Gerät. Bei 100 Geräten würde diese Ersparnis nach obiger Rechnung allein kaum überzeugenden Überschuss schaffen. **Die Hardwareeinsparung selbst ist auf Jetson noch nicht nachgewiesen.** End-to-End-Kosten inklusive Entwicklung, Energie, Ausfällen und Wartung messen; eingesparte Hardware nicht vorwegnehmen.

Weitere Preisprobleme:

- Dauerlizenz entspricht nur rund **23–24 Monatsraten**. Gleichzeitig heißt es pauschal „Support eingeschlossen“. Dauer, Updates, unterstützte Versionen und Plattformwechsel fehlen in der Darstellung; unbegrenzter Support zu einem einmaligen Kleinstpreis wäre wirtschaftlich schwer tragbar.
- Mengenstaffeln sind unklar: Gilt der niedrigere Satz für alle Geräte, sinkt der Jahrespreis bei 100 → 101 Geräten von 18.000 auf 12.120 €. Bei 1.000 → 1.001 fällt er von 120.000 auf 72.072 €. Staffelung oder individuelle Mengenangebote ausdrücklich definieren.
- Bei 1.000 Geräten kostet die monatliche Einzelgerätelizenz 120.000 €/Jahr; das OEM-Angebot startet bei 60.000 €. Gleichen Umfang und Support vorausgesetzt, muss die Angebotslogik diese Wahl einfach erklären.

**Vorschlag zum Validieren, keine recherchierten Marktpreise:**

1. Ein klar begrenzter Eignungscheck für **3.000–5.000 €**, etwa 1–2 Wochen, vorhandene Modelle und genau eine Plattform, ohne neuen Backendbau. Ergebnis: reproduzierbarer Vergleich und belastbares Go/No-go. Bei anschließendem Pilot anrechnen.
2. Qualifizierung für **15.000–25.000 €** nur bei positivem Eignungscheck, mit kundenseitigen Erfolgskriterien und klar begrenztem Engineeringumfang; Umfangreiche Integrationen höher anbieten.
3. Gerätepreise zunächst als Hypothese beibehalten, Supportgrenzen und Plattformabdeckung konkretisieren. OEM-Modell an Flotte, Integrationsaufwand und dokumentiertem Kundennutzen prüfen.
4. Preisgespräche mit echten Budgetverantwortlichen führen. Kostenloses technisches Interesse ist kein Zahlungsnachweis.

Zur Größenordnung: 10 Produktlinien zu je 60.000 € ergeben **600.000 € jährlichen Lizenzumsatz**, 50 ergeben **3 Mio. €**, jeweils vor Rabatten und Betreuungskosten. Das kann ein attraktives Spezialgeschäft sein. Ein sehr großer Plattformanbieter erfordert deutlich mehr Verbreitung und wesentlich weniger individuellen Aufwand pro Kunde; aus den bisherigen Messungen folgt dieses Skalierungspotenzial noch nicht.

## Was für einen größeren Erfolg fehlt

- **Ein präziser Nutzen:** beispielsweise „bestehende Wahrnehmung bleibt unter Hintergrundlast innerhalb ihrer Aktualitätsgrenzen auf derselben Hardware“. Hintergrundfortschritt und Modellqualität gehören in denselben Vertrag.
- **Drei voneinander unabhängige Kundenfälle** mit wiederholtem Nutzen und mindestens einem zahlenden Referenzkunden. Die Zielzahl ist ein vorgeschlagenes Entscheidungsziel.
- **Ein geeigneter Baselinevergleich:** Holoscan sowie bestmöglich konfigurierter Triton und einfache Clientlogik.
- **Zuverlässige Recovery und Messartefakte:** Ein Gerät muss nach Fehlern kontrolliert in einen bekannten Zustand zurückkehren, ohne unklare GPU-Arbeit oder Puffer voreilig freizugeben.
- **Kurzer Einstieg:** installierbares Paket ist bereits vorgesehen, ROS-Brücke existiert. Jetzt einen vollständigen unterstützten Beispielpfad mit guten Fehlermeldungen, minimalen Verträgen und nutzbarem Ergebnis qualifizieren.
- **Gepflegte Produktwahrheit:** Supportmatrix, README, Preise und Berichte widersprechen sich teilweise im Stand. Beispiele: „eine Maschine“ trotz Telefonmessungen; bereits gemessene Features noch als ungemessen; Rust 1.90 im Manifest gegenüber 1.98 in der Supportmatrix. Quellenstand und freigegebene Konfiguration maschinenlesbar führen.
- **Skalierbarer Support und Vertrieb:** dokumentierte Plattformgrenzen, Upgrade-/Rollbackpfad, messbarer Integrationsaufwand und nachvollziehbare Angebote. Kubernetes-/Fleet-Funktionen erst bauen, wenn zahlende Kunden sie benötigen.

## Wie weiter: vorgeschlagene 90 Tage

| Zeitraum | Arbeit | Fertig, wenn … |
|---|---|---|
| **Tag 1–14** | R01–R09 beheben; Android-/ROS-Gates ergänzen; Runtime-Start und Messsperren vereinheitlichen | alle acht Gegenproben grün; bestehende Suite weiter grün; Ports wie vorgesehen; Messfehler können keine belastbare Bewertung erzeugen |
| **Tag 15–30** | Jetson-Ziel auswählen; stärkste Triton-/Client-Baseline und Holoscan für denselben begrenzten Anwendungsfall vergleichen | Messpaket auf Zielhardware reproduzierbar; Streuung und Nebenwirkungen offen; echte Modellqualität und Hintergrundfortschritt erfasst |
| **Parallel, erste 30 Tage** | 15–20 Gespräche mit passenden OEM-/Integrationsverantwortlichen; konkrete Traces, Hardwarekosten und Budget prüfen | etwa drei geeignete Designpartner und mindestens ein bezahlter Eignungscheck; andernfalls Zielgruppe/Nutzen ändern |
| **Tag 31–60** | Einen bezahlten Kundenpilot einschließlich Abbruch-/Restarttests, Dauerbetrieb und Upgrade erarbeiten | vorab vereinbarte Aktualitäts-, Qualitäts- und Hintergrundkriterien auf Kundenpipeline erfüllt; Aufwand und wirtschaftlicher Nutzen dokumentiert |
| **Tag 61–90** | Zweiten unabhängigen Fall reproduzieren; Angebot/Supportpaket und Referenzfall abschließen | Nutzen wiederholt, Kunde will nach Pilot weiterbezahlen, Integration lässt sich mit begrenztem Aufwand wiederholen |

**Entscheidungsregel:** Schlägt Vigilant bei relevanten Fällen die vorhandene kostenlose Lösung nicht deutlich genug oder hängt jeder Erfolg an individueller Modellchirurgie, die Positionierung verkleinern: Qualifizierungs-/Optimierungsdienstleistung mit Runtime-Baustein. Wenn mehrere Kunden denselben Vorteil auf demselben Integrationspfad kaufen, genau diesen Pfad zum Produkt ausbauen.

## Gegenproben selbst ausführen

Nur in einer neuen, temporären Quellkopie; das Skript fügt absichtlich fehlschlagende Tests hinzu. Keine Korrekturen sind in diesem Review enthalten.

```bash
mkdir -p /tmp/vig-review-repro/source
git archive 90f517c | tar -x -C /tmp/vig-review-repro/source
python3 docs/reviews/2026-09-15/reproduce.py /tmp/vig-review-repro/source

# Buildausgaben auf die NVMe bzw. nach /tmp, nicht auf das NTFS-Projektlaufwerk.
# Auf diesem Rechner zusätzlich die vorhandene quiet-build-Sperre verwenden.
export CARGO_TARGET_DIR=/tmp/vig-review-repro/target
cargo test --manifest-path /tmp/vig-review-repro/source/Cargo.toml \
  -p vig-cli -p vig-gateway -p vig-bench --locked --offline \
  --no-fail-fast review_20260915 -- --nocapture
cargo test --manifest-path /tmp/vig-review-repro/source/backends/android-tflite/Cargo.toml \
  --locked --offline --no-fail-fast review_20260915 -- --nocapture
```

Erwartung am geprüften Stand: sechs fehlgeschlagene Gegenproben im ersten Befehl, zwei im zweiten. Offline benötigt bereits vorhandene Cargo-Abhängigkeiten. Die Originaltests bleiben grün; gerade diese Lücke zwischen Unit-Test-Abdeckung und komponentenübergreifendem Verhalten ist das wesentliche technische Ergebnis dieses Reviews.
