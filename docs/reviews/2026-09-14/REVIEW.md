**Erneutes Code- und Produktreview — 14.09.2026**

Ausgangspunkt: `311f1c7e02b433e2c62035d5adcb7443b53ec058`, lokales `main`.
Geprueft wurden insbesondere die Aenderungen seit `b201c57`: Scheduler,
Versorgungsschutz, Varianten, Abgleich von Ausfuehrungen, Pilot und Messablage,
Abnahmeberichte, Support- und Releaseaussagen. Die Runtime-Rohdaten vom 12.09.
wurden lesend mit den Berichten abgeglichen. Dies ist kein vollstaendiger
Security-Audit und keine neue Hardwarequalifikation.

**Verkaufsurteil:** Ein technisch substanzieller Produktkern ist vorhanden.
Ein allgemeines, produktionsreifes Versprechen fuer gemeinsame Kamera- und
LLM-Inferenz auf Edge-GPUs ist noch nicht belegt. Ein bezahltes, klar begrenztes
Qualifizierungsprojekt im Replay/Labor ist ein plausibles Angebot; ein Einsatz
mit Betriebszusagen braucht zuvor die offenen Lebensdauerfehler und eine
erfolgreiche Abnahme der konkreten Kundenpipeline. Ein zahlender Pilotpartner
ist durch dieses Review nicht nachgewiesen.

| Angebot | Urteil am Reviewtag | Begruendung |
|---|---|---|
| Technische Qualifizierung einer vorhandenen Pipeline | Als begrenztes Entwicklungsangebot vertretbar | Reale Modelle, Vergleichsmethoden und ein funktionierender Runtime-Kern sind vorhanden; auch ein negativer Eignungsbefund ist ein vereinbartes Ergebnis |
| Produktiver Betrieb auf exakt qualifizierter Hardware | Noch nicht freigeben | Offene Ausfuehrungs-/Pufferlebensdauer und kein bestandener Kunden-Anwendungsfall |
| Allgemeiner Schutz von Erkennung bei gleichzeitig garantiertem LLM-Fortschritt | Nicht belegt | Im Pilot hungert der Bericht aus; das vorgesehene vLLM-Backend laedt unter XSched nicht |
| Jetson-Produkt mit uebertragbaren Leistungszahlen | Nicht belegt | Die Telefonmessung prueft eine zweite Architektur, qualifiziert aber keinen Jetson |

**Was seit dem letzten Review tatsaechlich besser geworden ist.**

Die zuvor gefundenen Fehler wurden nicht nur umformuliert: Endpunkte werden
im Abgleich getrennt, Poller sind begrenzt, wiederholte identische Resetwerte
erzeugen nicht mehr denselben alten Fehler, und ein lokaler Pufferpool verhindert
die fruehere Modulo-Wiederverwendung innerhalb eines Messarms. Samplingparameter
werden als JSON verarbeitet. Die Hauptsuite bestand im unveraenderten
Ausgangsstand: **803 Tests bestanden, 0 fehlgeschlagen, 6 ignoriert**.
[Vollstaendige Ausgabe](tests-before.log).

Die abgeschlossenen Berichte sagen mehr als der kopierte Zwischenstand:

- Pipelining haelt Gate M3 in drei Wiederholungen und vermindert die
  Dispatch-Luecke. Das ist eine sinnvolle Empfehlung fuer den gemessenen Aufbau.
- Der Versorgungsschutz allein fuehrt bei 110 % Last zu **998 Promille
  unabgedeckten Perioden des schlechtesten Stroms**. Die Rohdatei sagt
  ausdruecklich „schlechtester Strom je Lauf“; die Spalte „alle Stroeme“ ist
  kein Durchschnitt aller Stroeme und keine direkte GPU-Auslastung.
- Schutz plus gelernte Marge erreicht bei 110 % bessere Werte in beiden
  berichteten Spalten. Bei 125 % ist diese Kombination fuer den Detektor
  schlechter als die feste Marge ohne Schutz: 45 gegen 21 Promille.
- XSched verlangsamt den Detektor im spaeteren Pilot um 17–20 %. vLLM laedt
  unter dem Shim nicht. Eine dennoch deklarierte Lane steigert bei B den
  Bericht von 2 auf 30 pro Minute, schadet aber der Erkennung; bei D steigt
  Alarm-p95 gegen den Lauf ohne Bericht um 28 %.

Belege: [Abnahme](../../benchmark/abnahme-2026-09-12.md),
[Pilot mit Praemption](../../benchmark/pilot-praemption-2026-09-12.md).
Die Rohdaten liegen unter `InferenceQoS-runtime/measure-abnahme-2026-09-12/`
und `measure-pilot-praemption-2026-09-12/`; dort wurden unter anderem
`c-ramp-supply.txt`, `manifest-neu.txt` und `plain/summary.json` gelesen.
Eine erneute GPU-Messung wurde fuer dieses Review nicht gestartet.

**R01 · P1 · Der Look-ahead prognostizierte eine nicht freigegebene Variante. BEHOBEN.**

Stelle: [scheduler.rs](../../../crates/vig-core/src/scheduler.rs),
`build_forecast`, zuvor fest `contract.variants.get(0)` und `VariantIdx(0)`.
Die tatsaechliche Variantenwahl beachtet dagegen die Freigabeliste. Ausserdem
ist Qualitaetsreihenfolge keine Laufzeitreihenfolge; das erkennt `variant.rs`
bereits an anderer Stelle an. Die Vorverarbeitung fehlte ebenfalls im Forecast.

Reproduktion mit dem echten Scheduler, ohne GPU: geschuetzter Strom alle 30 ms,
Deadline 20 ms; Variante 0 braucht 5 ms, ist aber gesperrt, Variante 1 braucht
20 ms und ist als einzige freigegeben. Nach der ersten Fertigstellung bei 20 ms
kommt Hintergrundarbeit bei 25 ms mit 20 ms Laufzeit. Der alte Forecast laesst
sie starten: `45 + 5 = 50`, scheinbar genau rechtzeitig. Tatsaechlich braucht
der naechste erlaubte Detektor `45 + 20 = 65 ms` bei Deadline 50 ms.
Die Gegenprobe zeigt den falschen `Dispatch` unmittelbar.

Korrektur: Forecast aus den fuer den Dispatch erlaubten Varianten, mit deren
Marge, Vorverarbeitung und Restblockierung. Bei automatischer Auswahl wird die
laengste zulaessige Prognose verwendet; bei festgelegter Auswahl nur die erste
freigegebene Variante. Das ist bewusst konservativ und kann bei nicht monotonen
Variantenreihen zusaetzliche Vetos erzeugen. Der Einvariantenfall ohne
Vorverarbeitung behaelt seine bisherige Rechnung. Zwei neue Integrationstests
decken Freigabe und Vorverarbeitung ab. Das ist kein neuer allgemeiner
Schedulability-Beweis fuer beliebige Varianten und mehrere Slots.

Weitere Integrationsgrenze: Der Dispatch kann im aktiven Prognosemodus eine
zustandsbezogene Zelle lesen, waehrend der Look-ahead weiterhin den Estimator
verwendet. Die vereinheitlichte Schaetzung und ein Nachweis fuer diese
Kombination stehen aus. Die hiesigen Gegenproben belegen Freigabe und
Vorverarbeitung; sie qualifizieren nicht jede Kombination der Lernverfahren.

**R02 · P2 · Die Hysterese konnte die Versorgungskorrektur wieder aufheben. BEHOBEN.**

Stelle: [variant.rs](../../../crates/vig-core/src/variant.rs),
`apply_hysteresis`. Nach der versorgungsbewussten Auswahl durfte eine bisherige
Variante allein deshalb gehalten werden, weil sie ihre Deadline noch einhaelt.

Reproduktion: Periode 10 ms, Deadline 20 ms, max_age 20 ms. Die bisherige Variante
braucht 15 ms. Eine qualitativ bessere braucht 5 ms. Innerhalb der Verweildauer
waehlte der Code weiterhin 15 ms, obwohl nur 5 ms die Versorgung halten.
Eine hoehere Qualitaet muss auf konkreter Hardware nicht langsamer sein.

Korrektur: Ein Wechsel, der die Versorgung rettet, darf auch waehrend der
Verweildauer stattfinden. Wenn auch der Wechsel die Versorgung nicht retten
kann, bleibt die Hysterese erhalten. Beide Faelle sind als Regressionstests
in [variant_supply.rs](../../../crates/vig-core/tests/variant_supply.rs).

**R03 · P1 · Die Pufferkorrektur endet am Messarm; alte Leser koennen weiterleben. OFFEN.**

Stellen: [pilot.rs](../../../crates/vig-bench/src/pilot.rs), `run_arm`:
neuer `RegionPool` je Aufruf, ungesammelte innere `tokio::spawn`-Aufgaben,
anschliessend pauschal 500 ms Warten. Der CLI-Pilot reicht in der naechsten
Zelle dieselben registrierten Regionen erneut hinein.

Zwei Gegenproben:

1. Ein 100-ms-Messarm gegen ein 2000-ms-Mockbackend kehrt bereits zurueck,
   obwohl das Backend seinen einzigen Slot noch haelt. Der Bericht nennt
   einen gesendeten, null gelieferten und null abgelehnten Request. Ein
   Messende ist hier kein Ende aller Inferenzaufgaben.
2. Ein im ersten `RegionPool` quarantinierter Puffer wird von einem neuen
   Pool ueber dieselben Regionen sofort wieder ausgegeben. Genau diese
   Neuerzeugung passiert zwischen den Armen. Auch beendete RPCs mit unklarem
   Backend-Ende sind daher betroffen; bloss alle Clienttasks abzuwarten reicht
   nicht fuer die Quarantaene.

Moegliche Folgen: ueberlappende Messzellen, verfremdete Laufzeiten, erneute
SHM-Wiederverwendung vor Ende eines Lesers. Reproduziert wurden offenes
Mockbackend und verlorene Poolquarantaene, keine reale GPU-Speicherkorruption.
Die vorhandenen GPU-Ergebnisse sind deshalb nicht pauschal falsch; ihre
Zellenisolierung ist auf diesen Fehlerpfaden aber nicht abgesichert.

Erforderliche Aenderung: Poolbesitz ueber die gesamte Registrierung behalten,
alle gestarteten Kameraaufrufe in einer begrenzten Taskverwaltung fuehren,
pro Arm explizit stoppen und entleeren. Bei ungeklaertem Leserende die Region
gesperrt halten und den naechsten Arm mit diesen Regionen nicht starten.
Zeitueberschreitung muss einen ungueltigen Lauf ergeben. Ein neuer Pool oder
ein weiterer Timer ist kein Endnachweis. Bei Armwechsel darf keine unzugeordnete
Backendarbeit uebrig sein.

**R04 · P1 · Ein spaet erkannter Reset kann weiterhin neue Arbeit freigeben. OFFEN, bereits dokumentierte Grenze jetzt reproduziert.**

Stelle: [actor.rs](../../../crates/vig-gateway/src/actor.rs),
`on_backend_restart`. Der Code gibt bei einem Zaehlerabfall alle Ansprueche
in `Reconciling`/`AwaitingBaseline` frei. Er kann nicht feststellen, ob der
betroffene Aufruf vor oder nach dem tatsaechlichen Neustart gestartet wurde.

Gegenprobe mit echtem Actor und FakeExecutor: Basislinie 100; Backendzaehler
faellt auf 0; neuer Aufruf im neuen Prozess verliert seine RPC-Verbindung;
erst danach sieht der Poller den Reset. Ergebnis: `quarantined = 0`,
`reconciled = 1`, obwohl das Ende dieser neuen Ausfuehrung unbekannt ist.
Der Quelltext und ADR-0040 beschreiben diesen Restfall bereits. Damit ist
„alle Review-Befunde behoben“ fuer eine Produktionsfreigabe zu weit gefasst.

Zusaetzlich werden mittlerweile die Zaehler aller Modellversionen summiert.
Das korrigiert die fruehere Auswahl nur des ersten Statistikeintrags, ersetzt
aber keine Versions-/Ausfuehrungsidentitaet: Eine sinkende Summe belegt nicht
das Ende jeder anderen Version. Triton dokumentiert die Statistik pro Modell
und Version als Aggregat, nicht als Ausfuehrungsticket.
[NVIDIA-Statistikprotokoll](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/protocol/extension_statistics.html).
Dieser Mehrversionsfall wurde hier nicht mit echtem Triton nachgestellt.

Erforderliche Aenderung: Backendinstanz/Boot-ID, Modellversion und eindeutiges
Ausfuehrungsticket durchgaengig fuehren. Ein aggregierter Zaehlerabfall allein
darf keinen noch moeglichen Leser beenden. Ohne diese Evidenz bleibt der
Anspruch gesperrt; Wiederherstellung erfordert einen expliziten, belegbaren
Stillstand des betroffenen Backends. Die Startsequenz braucht zudem eine
Basislinie vor der ersten Auslieferung: `on_baseline` verwirft spaete
Basislinien dauerhaft, waehrend die Bereitschaft sie nicht voraussetzt.
Dieser Startfall ist hier eine Quelltextfeststellung, keine sechste Gegenprobe.

**R05 · P1 fuer Verkaufsfreigabe · Ein gruener Pilotstatus bestaetigt nicht die Anwendung. OFFEN.**

Stelle: [edge-pilot.rs](../../../crates/vig-bench/src/bin/edge-pilot.rs),
`RunStatus::exit` und Urteilsbildung. Nur anwendbare Planungskriterien entscheiden
ueber `bestanden`. Anwendungsfehler und ausgeschlossene Anwendungskriterien
aendern den Exitcode nicht. Auch `NoVerdict` liefert 0. Das ist dokumentiertes
Verhalten, kein heimlicher Codefehler; gefaehrlich ist seine Nutzung als
Produkt- oder Releaseabnahme.

Das vorhandene `plain/summary.json` bestaetigt: Status bestanden, Exit 0,
vier Anwendungskriterien nicht anwendbar. Ein Detektor, der die Aufgabe schon
allein nicht ausreichend erfuellt, kann eine Scheduling-Ursachenanalyse
ermoeglichen. Er kann keine erfolgreiche Anwendung demonstrieren.

Die Trennung von Planung und Anwendung ist richtig. Der Schluss „die Anwendung
zaehlt nicht mehr, also ist das Produkt fertig“ ist falsch. Auch ein relativer
Vergleich gegen einen Arm mit fast null Abdeckung reicht nicht. Eine neue
500-Promille-Schwelle fuer den Vergleichsarm sollte nicht aus den bereits
bekannten Ergebnissen abgeleitet werden. Besser: zensierte/verpasste Ereignisse
vollstaendig bilanzieren und fachliche Mindestanforderungen vorab festlegen.

Erforderliches Releaseurteil: getrennte Felder fuer Ablaufgueltigkeit,
Planungsvertrag, Anwendungsvertrag, Hardware-/Backendqualifikation und
Freigabeumfang. `Nicht anwendbar`, fehlende Zellen oder nur ein Teilversuch
koennen Diagnose erlauben, aber keine positive Gesamtfreigabe erzeugen.
P6 prueft derzeit nur A/B und mindestens zwei Berichte pro Minute. Eine
behauptete Hintergrundzusage bei C/D wird dadurch nicht geprueft.

**R06 · P2 · Das Berichtsbasisalter zeigt die frischeste beteiligte Kamera. OFFEN.**

Stelle: [pilot.rs](../../../crates/vig-bench/src/pilot.rs), Reporter,
Zusammenfuehrung mit `Some(a.max(b))`. Der Prompt enthaelt jedoch die
Detektionen aller Kameras. Bei einer alten und einer neuen Kamera wird der
Zeitstempel der neuen als Basis verwendet. Das ausgewiesene Alter verdeckt
damit die alte Information. Ein Maximalalter wird beim Bau dieses Prompts
nicht durchgesetzt.

Beispiel aus der Rechnung: Kamerastempel 1 s und 10 s, Bericht bei 11 s.
Gemeldet wird 1 s Basisalter; ein Teil der Aussage beruht auf 10 s alten
Daten. Zu korrigieren sind Alter pro Kamera beziehungsweise der aelteste
verwendete Zeitstempel und eine erkennbare Kennzeichnung fehlender/veralteter
Quellen. Dieser Befund wurde im Quelltext geprueft, nicht mit einem echten
Sprachmodell reproduziert.

**R07 · Produktentscheidung · Mindestfortschritt laesst sich nicht einfach ueber den Guard stellen. OFFEN.**

`minimum_background_progress_pct` wird als Missbudget beobachtet. Der Guard
nimmt es nicht als Grund, ein Veto zurueckzunehmen. Die missbewusste Policy
ersetzt keine solche Machbarkeitspruefung. Ein konfiguriertes Minimum ist
damit keine allgemeine Ausfuehrungszusage.

Die im ADR vorgeschlagene Deckelung ist nicht automatisch die richtige Loesung:
Wenn ein langer, unteilbarer Auftrag die geschuetzte Frist notwendig verletzt,
kann ein Mindestfortschrittsregler diese Arbeit nur auf Kosten des Schutzes
starten. Dann muss entweder ein ausdrueckliches Missbudget diesen Tausch erlauben,
die Arbeit echt geteilt/unterbrochen, ihre Rate gesenkt, der Vertrag gelockert
oder eine weitere Ressource verwendet werden. Ein unmoeglicher Vertrag muss
als solcher angezeigt werden, statt abwechselnd zwei Zusagen zu verletzen.

Dabei ist ein gemessenes p99 keine obere Laufzeitschranke. Machbarkeit und
Missbudget brauchen eine ausdrueckliche Annahme ueber Laufzeitstreuung,
Ankunftsjitter und kontrollierte Ressourcen. Aus einem bestandenen Replay
entsteht keine harte Echtzeitgarantie fuer beliebige spaetere Last.

Auch die Erklaerung „laenger als der Abstand zweier Ankuenfte“ ist fuer die
Tabelle ungenau: 25 ms sind kleiner als 33 ms. Entscheidend ist das verbleibende
Budget inklusive bereits laufender Arbeit, geschuetzter Rechenzeit und
Versorgungsfrist. Im vereinfachten Fall: letzte Aufnahme bei 0, Ablauf bei
66 ms, aktueller Zeitpunkt 22 ms, naechste Ankunft bei 33 ms, Detektor 22 ms.
Wenn der Hintergrund die Ankunft ueberlappt, muss `22 + B + 22 <= 66` gelten;
ein 25-ms-Block passt nicht. Das ist eine deterministische Beispielrechnung,
keine neue allgemeine Schranke fuer GPU-Ausfuehrungen.

**R08 · P2 · Qualifikationsaussagen, Releasegate und Repositoryzustand auseinanderhalten.**

`STATUS.md` nennt Releasequalifikation fertig bis auf die Pilotfreigabe,
zugleich einen fachlich verfehlten Pilot. Teile der Support-Matrix sagen
weiter „noch nicht auf GPU gemessen“ fuer inzwischen gemessene Optionen und
stellen XSched breiter als qualifiziert dar, als es der spaetere vLLM-Pilot
traegt. Die beiden 8-Stunden-Laeufe ersetzen keinen Dauerlauf jeder neuen
Kombination aus Versorgungsschutz, Kalibrierung und Praemption.

Der Releaseworkflow fuehrt fmt/clippy/tests aus, verlangt vor dem Publizieren
aber nicht alle CI-Gates, insbesondere nicht Gate S, cargo-deny und REUSE.
Eine vorhandene Workflowdatei ist zudem noch kein Beleg fuer ein gebautes,
signiertes und mit der gleichen Konfiguration qualifiziertes Releaseartefakt.

Lokal zeigt `origin` weiterhin auf `Vigilant-CRS/Inference-QoS`, `main` liegt
40 Commits vor dem gespeicherten `origin/main`. Das ist eine lokale
Trackinginformation, keine frisch gepruefte Aussage ueber GitHub. Die
GitHub-App lieferte fuer alten und gewuenschten neuen Namen 404; Existenz,
Privatstatus und Remote-HEAD konnten damit nicht bestaetigt werden.
Die alten Namen stehen auch in Cargo-Metadaten und der Signaturpruefanleitung.

Die zitierte Anweisung zu Jetsonkauf, privatem `Vigilant-Edge` und bereinigter
Historie wurde hier als Kontext des Reviews behandelt. Es wurden weder Kauf,
Umbenennung noch Umschreiben/Push der Historie ausgefuehrt. Vor einer
Historienbereinigung muessen Messbinaries, Quellbaeume, Lockfiles, Manifeste
und ihre Zuordnung erhalten bleiben. Sonst verlieren gerade die guten
Messungen ihre nachvollziehbare Herkunft.

**Welches Produkt ich aus diesem Stand entwickeln wuerde.**

Ein Runtime-Paket fuer **mehrere kurz laufende Inferenzstroeme mit verschiedenen
Frischeanforderungen auf einer konkret qualifizierten Edge-GPU**. Der erste
Pilot kann ein vorhandener Personen-/Fahrzeugdetektor mit nachrangiger
Ausschnittklassifikation oder Inspektionsaufgabe sein. Modelle, Bilddaten und
fachliche Grenzwerte kommen vom Entwicklungspartner. Das vermeidet, dass ein
ungeeigneter eigener Detektor den Scheduler-Nachweis verdeckt.

Der Lieferumfang sollte sein: installierbarer Governor, ein dokumentierter
Video-/OIP-Adapter, Instrumentierung bis zur Verbraucherstelle, ein geprueftes
Profil samt Container-/Modellidentitaeten, Konfigurationspruefung und ein
maschinenlesbarer Eignungsbericht. Der Kunde kauft die nachgewiesene Einhaltung
seiner Aufgabe auf seiner Hardware; die Zahl der ADRs oder ein isolierter
Faktor gegen eine Baseline ist kein solcher Nutzen.

```text
Kameras -> Aufnahmezeit/Frame-ID -> begrenzte Videoqueue -> Governor
                                                        |-> Detektor -> Tracking/Ereignis
                                                        |-> nachrangige Klassifikation
Verbraucher -> bestaetigte Verwendung/Alter -> Auswertung des Anwendungsvertrags
```

Decode, Vorverarbeitung, Tracking, Transport und fremde GPU-Auftraege gehoeren
in die Messung. Ein Request-Proxy kontrolliert sie nicht automatisch.
Frames koennen bei zustandslosem Detektieren entbehrlich sein, bei Tracking,
zeitlichen Modellen und Sensorfusion aber Teil eines notwendigen Zeitfensters.
Die Semantik muss der Adapter ausdruecklich kennen.

Das Problem ist real, aber die grundsaetzliche Idee ist nicht allein ein
Wettbewerbsvorteil: PAAM untersucht priorisierten Beschleunigerzugriff in ROS 2;
Holoscan besitzt Bedingungen zur Steuerung von Operatorausfuehrungen.
[PAAM](https://arxiv.org/abs/2404.06452),
[Holoscan-Dokumentation](https://docs.nvidia.com/holoscan/sdk-user-guide/components/conditions).
Die genaue Ueberlegenheit von Vigilant gegen diese Systeme ist nicht gemessen.
Die Differenzierung muss aus einfacher Integration, belastbaren Frischevertraegen,
Fehlerbehandlung und einer reproduzierbaren Kundenabnahme entstehen.

Zwei benannte Projekte wurden am Reviewtag nochmals anhand primaerer Quellen
geprueft. Meine Priorisierung fuer die Ansprache lautet:

| Organisation und Projekt | Oeffentlich belegte Pipeline | Passender naechster Schritt |
|---|---|---|
| Protex AI / FORT Robotics, Sicherheitsdemonstrator auf IGX Orin | IP-Kamera, DeepStream, TensorRT-Personenerkennung, Warn-/Stoppzonen | Engineering-Replay mit bestehender Erkennung und zusaetzlicher kurzer Klassifikation; Anschluss an den vorhandenen Inferenzpfad zuerst pruefen |
| Fogsphere, Safety-AI-Agenten fuer Saipem | Echtzeit-Erkennung von Personen unter schwebenden Lasten und Kohlenwasserstoffaustritten; Validierung auf AI-RAN | Spaeterer Mischlastpilot, sobald die langen Analyseaufgaben auf dem Zielbackend beherrscht werden |

Quellen: [NVIDIA zum Protex-/FORT-Projekt](https://developer.nvidia.com/blog/using-the-power-of-ai-to-make-factories-safer/),
[NVIDIA zum Saipem-Projekt](https://nvidianews.nvidia.com/news/nvidia-t-mobile-and-partners-integrate-physical-ai-applications-on-ai-ran-ready-infrastructure).
Die vorgeschlagenen Nebenaufgaben und der Governor-Einsatz sind unsere
Pilotentwuerfe. Die Quellen belegen weder ein ungeloestes Schedulingproblem
dieser Teams noch deren Bedarf an Vigilant. Diese Aussage braucht deren
Pipeline-Trace und ein Gespraech; aus einer Projektmeldung laesst sie sich
nicht serioes ableiten. Es wurde niemand kontaktiert.

**Priorisierte Entwicklung, jeweils mit einem konkreten Abschlusskriterium.**

1. **Ausfuehrungs- und Pufferlebensdauer schliessen.** R03/R04 und der
   Start-Basislinienpfad. Fertig bedeutet: Fehler vor/nach Restart,
   Transportabbruch und Armwechsel koennen weder Kredit noch Puffer vorzeitig
   freigeben; fehlende Evidenz wird betrieblich beherrschbar ausgewiesen.
2. **Vertraege auf Machbarkeit pruefen.** Drei ausdrueckliche Ziele:
   maximale Ergebnisalter/Luecken, zulassige Misses und minimale abgeschlossene
   Nebenarbeit. Unteilbare Laufzeit, belegte Restblockierung und gemessene
   Nebenlaeufigkeit muessen dazu passen. Keine stille Uebersteuerung eines
   Schutzvertrags zur Reparatur eines anderen.
3. **Einen unterstuetzten Betriebsmodus qualifizieren.** Zunaechst keine
   freie Kombination aller experimentellen Schalter. Pipelining 1 ist ein
   gemessener Ausgangspunkt auf der RTX 3070, kein portabler Defaultbeweis.
   Deklarierte Praemption bleibt ein Laborversuch, bis Backend und Restblockierung
   nachgewiesen sind. Ein fehlendes Interferenzprofil bedeutet unbekannt,
   nicht nachweislich null Interferenz.
4. **Abnahme automatisieren.** Identitaeten einfrieren, Aufbau pruefen,
   zeitgetreu wiedergeben, alle Leser beenden, Vollstaendigkeit pruefen und
   erst dann werten. Mindestfortschritt auf jedem zugesagten Lastpunkt;
   Ereignis-Recall und Fehlalarme neben Latenz und maximaler Beobachtungsluecke.
   Bei schwacher Referenz erst Modell/Datensatz/Rate reparieren.
5. **Partner und Zielhardware verbinden.** Kontakte aus der
   [belegten Projektrecherche](../../pilot/2026-09-11-use-cases-und-pilotpartner.md)
   sind Ansaetze fuer Protex-, Fogsphere- oder KION-Engineering, noch keine
   Bedarfs- oder Kaufbestaetigung. Ein Jetson ist ein Qualifikationsgeraet;
   die Auswahl muss zum Modell- und Speicherbedarf der konkreten Pipeline
   passen. Die alten Prozentzahlen werden nicht auf ihn uebertragen.

Ein moeglicher bezahlter Pilot liefert einen reproduzierbaren Vorher-/Nachher-
Vergleich und eine klare Entscheidung: vorhandene Hardware traegt die
vereinbarten Kameras und Nebenaufgaben, oder sie traegt sie nicht. Wirtschaftlicher
Nutzen waere beispielsweise eine zusaetzliche Inspektionsaufgabe ohne weitere
GPU bei unveraenderter Ereignisqualitaet. Ob dieser Nutzen entsteht, wird im
Pilot gemessen; er ist hier kein behauptetes Ergebnis.

**Pruefnachweise und Grenzen.**

Die fuenf Gegenproben sind im eigenen
[Review-Workspace](repros/tests/review.rs) gespeichert. Sie verwenden die
echten Bibliotheken, einen kontrollierten FakeExecutor beziehungsweise ein
lokales Mockbackend; sie benoetigen keine GPU. Auf dem Ausgangsstand schlagen
alle fuenf Assertions fehl: [Ausgabe vor den Fixes](repros-before.log).
Zwei davon werden durch die lokalen Variantenkorrekturen behoben. Die drei
Lebensdauer-Gegenproben bleiben bewusst rot und sind nicht Teil der regulaeren
Workspace-Suite. Ihr Fehler ist ein offener Befund, kein gruener Produktnachweis.

Die abschliessenden lokalen Pruefergebnisse stehen in
[validation.json](validation.json), einschliesslich Befehlen und Quellhashes:

| Pruefung | Ergebnis |
|---|---|
| Hauptworkspace nach den Korrekturen | 807 bestanden, 0 fehlgeschlagen, 6 ignoriert |
| fmt / Clippy mit Warnungen als Fehler | bestanden |
| Gate S, Releasebuild, simuliert | bestanden; [Bericht](gate-s-report.md) |
| Separater Android-TFLite-Workspace, Hosttests | 20 bestanden; keine neue Geraetequalifikation |
| Fuenf Review-Gegenproben | vorher 0 bestanden / 5 fehlgeschlagen; nachher 2 / 3 |
| cargo-deny, offline | bestanden mit Warnungen; Advisory-Datenbankstand 09.09.2026 |

Die ROS-Tests konnten mit den vorhandenen Python-Umgebungen nicht vollstaendig
ausgefuehrt werden; pytest/numpy fehlen im System, numpy auch in der geprueften
Review-Umgebung. Der unittest-Versuch ist kein bestandener ROS-/pytest-Lauf.
REUSE ist in den geprueften Umgebungen nicht installiert; REUSE-Lint und
aarch64-Emulation wurden nicht erneut ausgefuehrt. Die lokale Pruefung ist
deshalb keine Behauptung eines vollstaendig neu durchlaufenen GitHub-CI-Gates.

Die [Korrekturen als Patch](scheduler-fixes.patch) sind nicht committed oder
publiziert. Keine bestehenden Messdaten, Abnahmeschwellen, Container oder
Konfigurationen wurden fuer ein positiveres Ergebnis geaendert. Wegen der
konservativeren Variantenprognose muss eine neue GPU-Abnahme den verbleibenden
Hintergrundfortschritt pruefen; alte Prozentwerte gelten fuer ihre damaligen
Binaries, nicht automatisch fuer diesen geaenderten Quellstand.
