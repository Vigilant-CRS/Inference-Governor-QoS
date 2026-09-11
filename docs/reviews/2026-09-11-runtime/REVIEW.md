**Review von InferenceQoS und InferenceQoS-runtime — 11.09.2026**

Geprüfter Repository-Stand: `b201c57807131926782a7b0a4a743a2cd7e20326`.
Der Runtime-Ordner ist kein eigenes Git-Repository. Die laufende Messkette
`measure-chain-2026-09-11d` nennt als Build-Ausgangspunkt `b0b36b5`.
Die Ergebnisse dieses Laufs sind daher nicht ohne Weiteres Ergebnisse des
aktuellen HEAD mit der neuen Mehrdomänenunterstützung.

**Urteil:** Der Ansatz ist sinnvoll für mehrere Inferenzströme, die sich
eine GPU teilen und unterschiedliche Anforderungen an Frische und Fortschritt
haben. Der deterministische Kern, der Actor als alleiniger Kapazitätsbesitzer
und die Backend-Abstraktion sind eine brauchbare Grundlage. Einen Rewrite
empfehle ich nicht. Vor einer Produktionsfreigabe müssen die Lebensdauerfehler
beim Ausführungsnachweis und bei Shared Memory behoben werden. Vor weiteren
Leistungsversprechen muss die Messpipeline verlässliche, abgeschlossene und
eindeutig zuordenbare Experimente liefern.

Der Schwerpunkt lag auf Komponentenübergängen, Messskripten, Pilot,
LLM-Zerlegung, Abschlussabgleich, ROS-Anbindung sowie CI und dokumentierten
Ergebnissen. Dies ist kein vollständiger Audit sämtlicher Rust-Zeilen oder
der vendorten XSched-Abhängigkeiten. Frühere Review-Befunde wurden mit dem
jetzigen Code abgeglichen; behobene Fehler werden hier nicht pauschal erneut
als offen geführt.

Ausgeführt wurden kleine Gegenproben mit unverändert extrahierten Rust-Methoden,
dem unverändert extrahierten Python-Callback und den tatsächlichen Shellskripten
mit ersetzten Docker-/HTTP-/Wartebefehlen. Die Rust-Gegenprobe verwendet
Ersatztypen für Actor und Scheduler; sie ist kein Gateway-Integrationstest.
Die SHM-Ring-Gegenprobe demonstriert eine zulässige Ereignisfolge, keinen
beobachteten GPU-Datenfehler. Code, Verfahren und Ausgabe stehen in
[probes.py](probes.py) und [probe-results.txt](probe-results.txt).

Eine vollständige Cargo-Suite, neue GPU-Experimente und ein kompletter
ROS-Integrationstest wurden in diesem Review nicht ausgeführt. Die laufende
GPU-Messung wurde nicht angehalten. `pytest` ist im hier verwendeten
System-Python nicht installiert. Produktcode, Modelle und Container wurden
durch diesen Review nicht geändert.

**R01 · P1 · Nach einem Backend-Neustart kann neue Arbeit vorzeitig freigegeben werden.**

Stellen: [actor.rs](../../../crates/vig-gateway/src/actor.rs), insbesondere
`start_reconciliation` Zeilen 1696–1724 und `on_completion_evidence`
Zeilen 2026–2042.

Der Poller merkt sich `highest = max(highest, completed)` und bezeichnet
jeden niedrigeren Wert als Neustart. Nach der Folge `100 → 0 → 0` ist deshalb
auch die letzte Meldung noch `restarted=true`. Der Actor akzeptiert diesen
Zustand ohne Prüfung seines Zählerziels und gibt alle nicht als `Running`
markierten Leases dieses Modellnamens frei. Dazu gehören neue `TimedOut`-
Aufträge, deren Backend-Ausführung noch läuft.

Die Gegenprobe mit den extrahierten Produktionsmethoden gibt erst den alten
Auftrag 1 und anschließend den neuen Auftrag 2 frei. Auftrag 2 wurde erst
nach dem Reset angelegt. Ein Neustartnachweis für eine alte Ausführung ist
kein Endnachweis für diese neue Ausführung.

Zusätzlich endet ein gestarteter Poller nach erfolgreicher Versöhnung nicht.
Jeder weitere Transportabbruch kann einen zusätzlichen dauerhaften Poller
erzeugen. Auch die alten Zählerbasislinien werden beim Reset nicht in eine
neue Epoche überführt.

Änderung: Ausführungsepoche in Lease und Nachweis; ein begrenzter Poller je
Backend-/Modell-/Versionsidentität; alte Poller bei Epochenwechsel beenden;
Zähler und Basislinie kontrolliert neu initialisieren. Ein Reset darf nur
Ansprüche der betroffenen alten Epoche beenden. Tests müssen Reset,
verspätete Meldung, neuen Timeout und wiederholte Verbindungsabbrüche
zusammen auslösen.

**R02 · P1 · Der Abschlussabgleich verliert Endpunkt und Modellversion.**

Stellen: [actor.rs](../../../crates/vig-gateway/src/actor.rs),
`dispatched_per_model`, `reconcile_baseline`, `backend_for_model_name`
Zeile 2186 und `spawn_baseline_probe`; [client.rs](../../../crates/vig-backend-triton/src/client.rs),
`completion_evidence` Zeile 350.

Ein Request wird anhand seines logischen Modells an den richtigen Endpunkt
geschickt. Für den späteren Abgleich wird dagegen nur der physische Modellname
gespeichert bzw. nachgeschlagen. Bei zwei Triton-Prozessen auf derselben GPU,
die beide ein Modell `m` anbieten, wählt `.position(...)` immer den ersten.
Zähler aus getrennten Servern können so mit einer gemeinsamen Dispatchsumme
verglichen werden. Separate GPU-Domänen besitzen inzwischen separate Actors;
das löst die Mehrprozesskonstellation innerhalb derselben Domäne nicht.

Die Statistikabfrage setzt außerdem `version: ""` und nimmt den ersten Eintrag
mit passendem Namen. Triton liefert ohne Versionsfilter Statistiken für alle
Versionen; die Zähler sind pro Modellversion aggregiert, nicht global über
alle Versionen. Das bestätigt die
[NVIDIA-Statistikdokumentation](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/protocol/extension_statistics.html).

Folgen sind ein falscher oder unerreichbarer Freigabenachweis. Nebenbefund:
`baselines_missing` zählt logische Varianten, zieht aber die Anzahl eindeutiger
physischer Modellnamen ab. Vier Kamera-Aliase auf ein Modell erscheinen dadurch
als drei fehlende Basislinien.

Änderung: durchgängiger Schlüssel aus Domäne, Endpunkt, Modell, Version und
Epoche; tatsächliche Version beim Dispatch festlegen. Nicht unterstützte
mehrdeutige Konfigurationen vorläufig ablehnen. Nachweise und
Bereitschaftszähler über dieselbe Menge eindeutiger Identitäten führen.
Dieser Befund wurde anhand der Aufrufpfade geprüft, nicht durch zwei echte
Triton-Prozesse reproduziert.

**R03 · P1 · Der Pilot kann noch benutzte Bildpuffer überschreiben.**

Stellen: [pilot.rs](../../../crates/vig-bench/src/pilot.rs), Zeilen 1019–1036;
[edge-pilot.rs](../../../crates/vig-bench/src/bin/edge-pilot.rs), Zeile 637.

Die Semaphore begrenzt nur die Anzahl offener Requests. Der konkrete SHM-Puffer
wird unabhängig davon als `sequence_number % regions.len()` ausgewählt.
`max(cap)+1` Puffer sind kein Nachweis, dass der nächste Puffer frei ist.

Gegenbeispiel bei vier erlaubten Requests und fünf Puffern: A hält Puffer 0;
B bis E benutzen 1 bis 4 und werden schnell fertig; F bekommt wieder 0,
während A noch wartet. Die Anzahl offener Requests bleibt dabei jederzeit
unter der Grenze. Besonders relevant ist das bei schneller Supersession
wartender Requests neben einem langsamen laufenden Request.

Damit können Bildinhalt, Capture-ID und Ground Truth auseinanderfallen.
Das betrifft die Aussagekraft von Recall, Alarmzeit und Qualitätsvergleich.
Die normale Gate-M3-Last mit konstanten Eingaben ist dadurch nicht automatisch
ebenfalls betroffen.

Änderung: konkrete freie Puffer aus einem Pool ausleihen und die Puffer-ID
mit dem Request führen; Wiederverwendung erst nach sicherem Ende aller Leser.
Ein Test muss einen alten Leser halten, während mehrere jüngere Requests
beendet oder verdrängt werden. Ein realer Korruptionsfall wurde hier nicht
auf der GPU provoziert.

**R04 · P1 · Die ROS-Brücke gibt SHM nach einem Timeout wieder frei.**

Stellen: [node.py](../../../integrations/ros2/vig_bridge/vig_bridge/node.py),
`_on_result` Zeilen 191–195; [oip.py](../../../integrations/ros2/vig_bridge/vig_bridge/oip.py),
`infer_async` und `read_result`.

Die ROS-Brücke hat bereits einen Pool konkreter Puffer. Sie gibt einen Puffer
aber bei jeder Client-Rückmeldung frei, noch vor der Auswertung des Outcomes.
Das schließt `BACKEND_TIMEOUT`, `EXECUTION_UNKNOWN` und Transportabbrüche ein.
Der Governor hält bei einem Timeout absichtlich den Ausführungskredit,
weil die GPU weiterarbeiten kann. Sein Client überschreibt zugleich womöglich
die zugehörigen Eingabedaten.

Der unveränderte Callback wurde ohne ROS mit einem echten `SlotRing(1)`
ausgeführt: Nach `BACKEND_TIMEOUT` lässt sich derselbe Puffer unmittelbar
wieder ausleihen. Das belegt die Freigabelogik; ein echter Backend-Leser wurde
in dieser Gegenprobe nicht gestartet.

Änderung: sichere Vorabablehnung, belegte Completion und unbekanntes
Ausführungsende im Clientvertrag unterscheiden. Bei unbekanntem Ende Puffer
quarantänisieren, bis ein expliziter End-/Epochennachweis vorliegt. Ein
zusätzlicher Warte-Timer allein ist kein solcher Nachweis. Diese Lebensdauer
muss zwischen Gateway, Client und Backend zusammenpassen.

**R05 · P1 für generative Requests · Samplingparameter werden zu ungültigem JSON.**

Stelle: [cooperative.rs](../../../crates/vig-gateway/src/cooperative.rs),
`sampling_for` Zeile 188 und `strip_max_tokens` Zeile 337.

Aus der gültigen Eingabe `{"temperature":0.7,"max_tokens":64}` wird beim
Zuschneiden auf acht Token `{"max_tokens": 8, "temperature":0.7,}`.
Das abschließende Komma ist ungültig. Steht `max_tokens` zuerst, funktioniert
derselbe Request. Die vorhandene Gegenprobe im Produkt testet genau diese
günstige Feldreihenfolge und prüft keinen vollständigen JSON-Roundtrip.

Reproduziert mit den unverändert extrahierten Rust-Funktionen und anschließend
mit einem JSON-Parser geprüft. Ursache: `strip_max_tokens` entfernt beim
letzten Feld auch die schließende Klammer; die Komma-Bereinigung erkennt
daraufhin das verbliebene Schlusskomma nicht.

Änderung: JSON strukturiert parsen, nur das oberste Feld `max_tokens`
ersetzen, Objekt serialisieren. Feldreihenfolgen, verschachtelte Werte und
Strings mit ähnlichen Inhalten testen. Die Bedeutung unbekannter Felder
bleibt beim normalen JSON-Objektroundtrip erhalten.

**R06 · P1 für den Vergleich · TSG wird standardmäßig mit dem falschen Level gestartet.**

Stellen: Runtime `xsched-triton-alt.sh` Zeile 28,
[triton-two-process.sh](../../../deploy/xsched/triton-two-process.sh) Zeile 34;
Runtime `measure-chain-2026-09-11d.sh` Zeile 145.

Die Auswahl `xsched TSG` ändert `XSCHED_CUDA_LV3_IMPL`, lässt aber
`XSCHED_AUTO_XQUEUE_LEVEL=${LEVEL:-2}` unverändert. Der Runner setzt kein
`LEVEL=3` für den TSG-Arm. Der lokale XSched-Code ruft `Interrupt()` erst ab
`kPreemptLevelInterrupt` auf (`xsched/preempt/src/xqueue/async_xqueue.cpp`,
Zeile 111). Die lokale vLLM-Anleitung fordert ebenfalls Level 3 für TSG.

Beide tatsächlichen Startskripte wurden mit Docker-Stubs ausgeführt. Ohne
von außen gesetztes `LEVEL` erzeugen beide Level 2, obwohl sie „bereit:
xsched TSG“ melden. Das ist kein belastbarer Test der TSG-Unterbrechung.
Ein bereits extern gesetztes `LEVEL=3` könnte einen einzelnen Lauf ändern;
der Skriptstandard und die Messkette stellen es nicht sicher.

Änderung: Implementierung und Level gemeinsam aus dem Modus ableiten,
inkonsistente Kombinationen ablehnen, tatsächlich gestartete Parameter und
eine Präemptions-Funktionsprobe im Runmanifest speichern.

**R07 · P1 für die Messkette · Der vollständige Pilot wird zu früh abgebrochen.**

Stellen: Runtime `measure-chain-2026-09-11d.sh` Zeile 196;
[edge-pilot.rs](../../../crates/vig-bench/src/bin/edge-pilot.rs), Optionen
Zeilen 99–105 und Schleifen ab Zeile 726.

Der Runner erlaubt 1800 Sekunden. Die Defaults verlangen bereits
`4 Lastpunkte × 3 Wiederholungen × 3 Arme × 2 Puffertiefen × 60 Sekunden`
= 4320 Sekunden. Referenz, Verbindungsaufbau und Drain kommen hinzu.
Ein vollständiger Standardlauf passt prinzipiell nicht in die Frist.
Die äußere Schleife wiederholt diesen ohnehin dreifach wiederholten
Gesamtlauf nochmals dreimal.

Änderung: eine einzige Experimentmatrix mit daraus abgeleiteter Zeitplanung;
pro Zelle ein persistiertes Ergebnis und Wiederaufnahme nach Abbruch.
Keine bereits im Binary enthaltene Wiederholungsdimension unbemerkt im
Shellrunner verdreifachen. Der Befund folgt aus den Defaults, kein
72-Minuten-Lauf war dafür erforderlich.

**R08 · P2 · Fehlgeschlagener Start kann als Erfolg zurückkommen.**

Stellen: Runtime `xsched-triton-alt.sh` Zeilen 59–60 und
[triton-two-process.sh](../../../deploy/xsched/triton-two-process.sh), gleiche
`wait_ready A && wait_ready B; echo ...`-Folge.

Wenn die erste Bereitschaftsprobe fehlschlägt, beendet `set -e` diese
AND-Verknüpfung nicht. Das folgende `echo` läuft und liefert Exitcode 0.
Beide tatsächlichen Skripte meldeten in der Stub-Gegenprobe trotz dauerhaft
fehlgeschlagener Health-Checks „bereit“ und Exitcode 0.

Außerdem wertet die Messkette den Exitcode von `xsched-infer-check.py` nicht
als Voraussetzung der folgenden Gates aus. `edge-pilot` schreibt seine
Kriterien nur als Text „VERFEHLT“/„bestanden“ und endet regulär. Der äußere
Runner führt Fehlversuche weiter aus, ohne am Ende ein aggregiertes
maschinenlesbares Gesamturteil zu liefern.

Änderung: Bereitschaft explizit abbrechen; Voraussetzungen einer Messzelle
prüfen; Ausführungsstatus, Ergebnisgültigkeit und fachliches Gate separat
speichern. Ein komplett durchgelaufener, fachlich negativer Versuch ist
wertvolle Evidenz und muss anders aussehen als ein kaputter Aufbau.

**R09 · P2 · Fremdlast wird beobachtet, aber nicht in die Auswertung übernommen.**

Stelle: Runtime `measure-chain-2026-09-11d.sh`, Lastwächter ab Zeile 119 und
Aufrufe der Messblöcke. Im während des Reviews gelesenen Log stehen unter
anderem `16:09:29 p2-frontier ... FREMD java(397.0%)` und
`16:15:02 p2-frontier ... FREMD java(496.0%)`. Auch Burst- und Rampenblöcke
enthalten markierte Chrome-Last.

Nach dem eigenen Skriptkommentar sind solche Blöcke verschmutzt. Die
Messausgaben enden trotzdem mit `exit=0`; der Wächter liefert keine
automatisch ausgewertete Gültigkeitsmarkierung. Namen wie `python3` sind
pauschal erlaubt, obwohl darüber fremde Last laufen kann. Eine CPU-Maske
reserviert diese Kerne zudem nicht exklusiv gegenüber anderen Prozessen.

Änderung: PID-/Prozessgruppen- oder cgroup-basierte Zuordnung; je Messzelle
Status `valid`, `contaminated`, `failed` oder `incomplete`. Rohdaten behalten,
ungültige Zellen nicht in eine qualifizierte Vergleichsaussage übernehmen.
Der jetzige Logbefund beweist nicht, wie groß der Einfluss auf jede einzelne
Zahl war; er verhindert aber die Behauptung eines ungestörten Laufs.

**Was die vorhandenen Zahlen für den Ansatz bedeuten.**

Die sechs dokumentierten Gate-M3-Läufe liefern gute Evidenz für den
konkreten Betriebspunkt: wesentlich weniger unversorgte Detektorzyklen.
Die dokumentierte vollständige Verdrängung der Hintergrundarbeit ist
jedoch Teil desselben Ergebnisses. Auch der DIY-Vergleich zeigt auf der
Governor-Seite hohe Verluste der anderen Ströme. „47× besser“ beschreibt
dort eine bestimmte Detektormetrik und keine 47-fach bessere Gesamtpipeline.
Siehe [DIY-Baseline](../../benchmark/diy-baseline.md) und
[R04](../../benchmark/gate-m3-r04.md).

Die durchschnittliche GPU-Auslastung allein entscheidet nicht über den
Nutzen. Ein langer nicht unterbrechbarer Job kann eine kurze Frist auch
unterhalb 100 % Last verletzen. Der eigene
[TensorRT-Bericht](../../benchmark/tensorrt.md) beschreibt genau so einen
Betriebspunkt bei 76 %. Die pauschale README-Empfehlung „GPU below saturation:
No governor“ ist daher zu breit. Entscheidend sind Blockadedauer,
Ankunftsmuster, Latenzstreuung, Zeitbudget und Prioritäten der Verbraucher.

Der aktuelle SHM-Datenpfad wirkt für den Laptop brauchbar: Der während des
Reviews eingesehene abgeschlossene 11d-Teilversuch berichtet bei Pose
+159 µs Median und +99 µs Differenz der p99-Quantile. Der Mockversuch
meldet seine budgetierten Zeilen als bestanden. Beim 6,2-MB-Copy-Pfad
stehen dagegen +4771 µs Median. Differenzen zweier p99-Quantile sind
keine p99-Verteilung paarweiser Zusatzkosten. Diese Ergebnisse stammen
aus der bestehenden Messung, nicht aus einem neuen Lasttest dieses Reviews.
Die eingesehenen Ausschnitte sind in
[observed-run-excerpts.txt](observed-run-excerpts.txt) festgehalten;
das Ablaufprotokoll ist eine Momentaufnahme des noch laufenden Versuchs.

Die ARM-Dokumentation unterscheidet sinnvoll Kernkosten und Gesamtkosten.
Ein schneller Scheduler allein belegt keinen günstigen Datenpfad. Die
Android-Zahlen lassen sich ebenso wenig unmittelbar auf Jetson übertragen
wie die Laptop-Zahlen. Die Qualifikation gehört auf die wirkliche Zielhardware
mit deren Transport, Energiezuständen und Verbrauchern.

**Empfohlene Engineering-Reihenfolge.**

1. **Lebensdauer und Identität schließen.** R01–R05 zuerst: Ausführungsende,
   Speicherbesitz, Modell-/Versions-/Epochenidentität und gültige Requests.
   Ausführungskredit, Payload-Permit und SHM-Leselease müssen dieselben
   Endzustände respektieren. Die vorhandenen Core-Invarianten weiterverwenden.
2. **Eine versionierte Experimentpipeline bauen.** Runner ins Repo;
   Laufdaten extern. Ein Runmanifest enthält Commit, Dirty-Diff, Binary-Hash,
   Konfiguration, Modell- und Datensatz-Digests, Container-Digests, Treiber,
   Geräteidentität, aktive XSched-Parameter, Seeds und Messschema-Version.
   Aus einem eingefrorenen Build in ein laufbezogenes Binärverzeichnis
   kopieren. Die heutigen gemeinsam verwendeten `target/release`-Pfade sind
   kein unveränderliches Experimentartefakt.
3. **Kurze, wiederaufnehmbare Messzellen.** Vorbereitung, Preflight, Warmup,
   Referenz, randomisiert bzw. balanciert angeordnete A/B-Arme, Drain,
   Validierung und Auswertung trennen. JSONL/CSV pro Zelle unmittelbar
   schreiben. Alle getesteten Puffertiefen behalten; eine Auswahl anhand
   eines gesonderten Tuning-Laufs festlegen, anschließend separat bewerten.
   Das jetzige Best-of aus denselben Testdaten begünstigt Zufallssieger.
4. **Die richtigen Nahttests in CI aufnehmen.** Zählerreset plus neue
   Ausführung; doppelte Modellnamen an zwei Endpunkten; mehrere Versionen;
   SHM-Leser mit schnellen jüngeren Ablehnungen; Client-Timeout bei laufendem
   Backend; permutierte Sampling-JSON-Felder; falsche Health-Checks und
   unvollständige Experimentmatrizen. Für die hier demonstrierten
   Gegenbeispiele sind Regressionstests zu ergänzen. Die Cargo-CI erfasst die Python-Brücke und
   externen Runtime-Shellskripte nicht automatisch.
5. **Ein fokussiertes Produktziel qualifizieren.** Eine GPU, Triton, mehrere
   geschützte Kameraströme, expliziter Hintergrundvertrag, dokumentierte
   Betriebsgrenzen und eine reale Anwendung. LLM-Fortsetzung, aktive
   Hardwareprognose, komplexe Graphen und Mehr-GPU-Mechanismen jeweils erst
   mit einem benötigten Pilotfall weiter ausbauen.

Die fachliche Abnahme sollte zeitgewichtete Verbraucherabdeckung, längste
Versorgungslücke, Alarmverzögerung, Erkennungsqualität und tatsächlich
fertige Hintergrundaufgaben gemeinsam betrachten. Ein gemessenes p99-Profil
ist keine harte Worst-Case-Zusage. Varianten dürfen nur dann automatisch
gewechselt werden, wenn Semantik und erforderliche Qualität belegt sind.
Der eigene Frontier-Lauf nennt sein Paar ausdrücklich ein Laufzeitpaar mit
deklarierter Qualität; daraus folgt noch kein Qualitätsnachweis echter
Detektorvarianten.

Als zusätzliche Vergleichsarme bieten sich eine zentral koordinierte
LATEST-/EDF-Baseline und Holoscan mit geeignetem Puffer-/Schedulingaufbau an.
Holoscan bietet asynchrone Pufferverbindungen und konfigurierbare Scheduler;
die faire konkrete Konfiguration muss erst ermittelt werden. Das ist eine
Empfehlung für die nächste Vergleichsmessung, kein hier gemessener
Leistungsvergleich. Siehe
[Holoscan-Ressourcen](https://docs.nvidia.com/holoscan/sdk-user-guide/components/resources)
und [Scheduler](https://docs.nvidia.com/holoscan/sdk-user-guide/components/schedulers).

Der Produktwert liegt in einer überprüfbaren Entscheidung darüber, welche
Arbeit noch rechtzeitig nützt, einschließlich sichtbarer Kosten für andere
Ströme. Dafür ist das Projekt bereits weit genug, um einen fokussierten
Pilot zu rechtfertigen. Die derzeitigen Nachweis- und Messfehler rechtfertigen
noch keine allgemeine Produktions- oder Plattformfreigabe.
