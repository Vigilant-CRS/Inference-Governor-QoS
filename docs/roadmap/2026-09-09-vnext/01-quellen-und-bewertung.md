# 01 — Quellenprüfung und Forschungsbewertung

Stand: 2026-09-09. Aussagen über fremde Systeme sind keine Messungen von Vigilant.
Zielentwurf und technische Folgerungen sind unsere eigene Bewertung.

## 1. Zentrales Paper: relevanter Befund, begrenzte Garantie

[S01: Kang, Edge-Inference Governors Need Memory-Clock State, v3 vom 08.07.2026](https://arxiv.org/html/2606.16106v3).
Geprüft: Haupttext einschließlich Methodik, Zulassung, End-to-End-Auswertung,
Limitations und Messfallen; nicht sämtliche Rohdaten unabhängig reproduziert.

Kompakter Quellenbefund:

- Zwei Orin-Varianten; der hervorgehobene Vergleich erreicht bei engen
  Deadlines 25–28 % Misses ohne EMC-Zustand und höchstens etwa 0,9 % mit ihm.
- TensorRT beseitigt den untersuchten Speichertakteinfluss nicht.
- Die Reparatur nutzt diskrete Zustände; einfache Frequenzterme sind nicht
  allgemein ausreichend. Dekodierhorizont und Nebenlast ergänzen das Modell.
- Die Taktreaktionen von ungefähr 1/5/8 ms sind Messwerte des Versuchsaufbaus,
  keine portablen Konstanten.
- Die endgültige Bandregel erreicht im gehaltenen Test 1,19 % aggregierte
  Misses, aber einzelne Läufe bis 2,9 % bei 2 % Budget. Zusätzliche Reserve
  verbessert die beobachtete Einzellaufkonformität.
- Die Studie beansprucht weder einen Produktionsgovernor noch eine
  weakly-hard-Garantie. Referenzprobe, frische Probe und neuer Lauf sind
  unterschiedliche Validierungsstufen.

**Unsere Schlussfolgerung:** Zustandsabdeckung und unabhängige Validierung
haben Vorrang vor einem komplexeren Optimierer. Das Paper ist ein guter
Versuchsplan-Impuls, keine übernehmbare Sicherheitsbegründung.

## 2. Bewertung der vorgeschlagenen Änderungen

Die Prioritäten unten sind unsere Produktentscheidungen, keine Empfehlung
der jeweiligen Autoren. Abnahmen stehen in Dokument 05/06.

| Vorschlag aus der Diskussion | Bewertung am aktuellen Code | Konsequenz |
|---|---|---|
| Hardwarezustand als zusätzliche Eingabe | Sinnvoll; bisherige Metadaten reichen nicht für frühe Zustandswechsel-Erkennung | Erst beobachten und Profile invalidieren; später aktiv steuern |
| Backend-spezifische Profile | Richtig, aber Triton ist eine Serverebene, nicht derselbe Typ wie TensorRT | Gesamten Ausführungspfad samt Messgrenze identifizieren |
| p95/p99 statt Mittelwert | Bereits vorhanden | Stichprobenqualität, Kontext und unabhängige Validierung ergänzen |
| `InferenceContract` einführen | Wesentliche Verträge existieren schon | Versionierte Zusatzbedingungen statt paralleles Vertragsmodell |
| Miss-Bursts | Hoher Nutzen, aber Nenner und Zeittakt müssen zuerst definiert sein | Zunächst beobachten; Durchsetzung getrennt qualifizieren |
| Vollständige Interferenzmatrix | Nicht als erster Schritt; teuer, asymmetrisch, nicht generell komponierbar | Sparse-Messungen bekannter Kombinationen und konservative Rückfälle |
| Vorausschauende Taktwahl | Separates Risiko: globale Stellgröße und langsame Wirkung | Optionaler Aktuator mit exklusivem Besitzer und Readback |
| CUDA Graphs / warme Profile | Nach korrektem nativen Lebenszyklus | Beschleunigungsoption; Messzustand und Speicherbudget berücksichtigen |
| Semantic Freshness | Forschungs- und kundenspezifische Erweiterung | Autorisierte Anwendung liefert begrenzte Gültigkeitshinweise |
| Green-Context-Garantien | In dieser Form zu stark | Partitionierungsfähigkeit, keine pauschale Deadline-Garantie |

Eine Zahl wie „99,5 % Wahrscheinlichkeit“ wird nicht dadurch belastbar, dass
die API eine Verteilung zurückliefert. Ebenso macht ein Feld für erlaubte
Miss-Bursts deren Einhaltung nicht automatisch beweisbar.

## 3. XSched: prüfen, nicht voraussetzen

[S02: OSDI 2025](https://www.usenix.org/conference/osdi25/presentation/shen-weihang)
trennt Policy und Ausführung über präemptierbare XQueues. Das
[aktuelle Projekt](https://github.com/XpuOS/xsched) nennt CUDA-Graph-Support,
weist aber unterschiedliche Implementierungsstände je Hardware und Ebene aus;
bei sm86 sind Level 2/3 in der Tabelle noch als in Arbeit markiert.

Die [Triton-Integration](https://github.com/XpuOS/xsched/tree/main/integration/triton)
ist ein konkretes Beispiel mit Patch für `tensorrt_backend r22.06`, keine
nachgewiesene Drop-in-Kompatibilität mit unserem Triton 2.70.

Unsere Entscheidung: begrenzter Kompatibilitätsversuch `NV-15`. Er muss
Treiber-/GPU-/Graph-Kombination, tatsächlich erreichbare Unterbrechungsgrenze,
Fortsetzungssemantik und Wartungskosten belegen. Scheitert das, bleibt der
Standardpfad vollständig benutzbar. Ein boolesches `supports_preemption`
wäre als Schnittstelle zu ungenau; siehe Dokument 03.

## 4. REEF: Konzeptquelle, kein vorhandenes Jetson-Backend

[S03: OSDI 2022](https://www.usenix.org/conference/osdi22/presentation/han)
und das [öffentliche Artefakt](https://github.com/SJTU-IPADS/reef)
zeigen resetbasierte Präemption und gesteuerte Kernel-Nebenläufigkeit.
Das Artefakt nennt AMD MI50 als unterstützte Hardware und verwendet
transformierte Kernel. Das ist nicht mit unveränderten TensorRT-Engines auf
Jetson gleichzusetzen.

Der zusätzlich genannte [TOCS-DOI](https://doi.org/10.1145/3768622) war in
dieser Prüfung nicht abrufbar. Dessen konkrete Leistungszahlen und die
behauptete spezielle Speicherbandbreiten-Limitation werden hier deshalb
nicht als überprüfte Befunde übernommen.

Unsere Entscheidung: von den Trennlinien zwischen Policy, Unterbrechung und
Ko-Ausführung lernen. Kein REEF-Port im verbindlichen Pilotpfad.

## 5. Robotikarbeiten: Kontext ernst nehmen, Grenzen bewahren

Die folgenden Quellen wurden auf Titel-/Versions-/Abstract-Ebene geprüft.
Das ist eine belastbare thematische Einordnung, kein vollständiger
Methoden- oder Artefaktaudit dieser drei Arbeiten.

| Quelle | Tatsächlicher Gegenstand | Unsere mögliche Verwendung |
|---|---|---|
| [S04: Jetson-PI v5](https://arxiv.org/abs/2607.12659v5) | VLA-Ausführung mit gelernter Zukunftskorrektur, asynchronen Aktionen und confidence-basiertem Scheduling | Anwendungsseitige Gültigkeit und Aktionshorizont als optionale Metadaten; kein Austausch unseres Cores durch ein VLA-Verfahren |
| [S05: Armory](https://arxiv.org/abs/2608.00337) | Remote-Policy-Serving mehrerer Roboter mit unterschiedlich schnell verbrauchten Aktionsblöcken | Verbraucherbedarf statt bloßer Ankunftsreihenfolge modellieren; späterer Mehrgerätefall |
| [S06: Speedup Paradox v2](https://arxiv.org/abs/2606.28529v2) | Unterschied zwischen Inferenzbeschleunigung und Gesamtaufgabenerfolg | Varianten an Aufgabenqualität und Zeitverhalten messen, nicht allein an Latenz |

Datierung: Jetson-PI wurde im Juli eingereicht und im September überarbeitet.
Die Aussage „neues September-Paper“ verdeckt diese Versionsgeschichte.

Wir übernehmen weder deren Modellgewichte noch Methoden als schon
implementiert. Ein allgemeiner Governor kennt die Welt nicht: Die Anwendung
muss erklären, welches Ergebnis noch nutzbar ist. Selbsteingeschätzte
Modell-Confidence darf nicht allein eine harte Frischegrenze lockern.

## 6. NVIDIA-Funktionen: Werkzeuge und Konkurrenz

[S07: Green Contexts](https://docs.nvidia.com/cuda/cuda-programming-guide/04-special-topics/green-contexts.html)
partitionieren bestimmte SM- und Work-Queue-Ressourcen. NVIDIA schließt eine
allgemeine Zusage gleichzeitiger Ausführung ausdrücklich aus. Ein freier
SM-Anteil beweist daher keine End-to-End-Deadline und keine Isolation aller
Ressourcen. Die Runtime-API-Darstellung ab CUDA 13.1 ist von älteren
Driver-API-Pfaden zu unterscheiden.

[S08: TensorRT-Optimierung](https://docs.nvidia.com/deeplearning/tensorrt/latest/performance/optimization.html)
beschreibt konkurrierende Execution Contexts, steuerbare Auxiliary Streams
und Kosten des ersten Enqueue nach Shape-/Profilwechsel. Unsere Folgerung:
solche Pfade brauchen eigene Messbedingungen und explizite Besitzer.

Holoscan besitzt bereits
[S09a: Green-Context-Pools](https://docs.nvidia.com/holoscan/sdk-user-guide/api-reference/holoscan/classes/cudagreencontextpool)
und [S09b: Latest-Frame-Semantik](https://docs.nvidia.com/holoscan/sdk-user-guide/components/schedulers).
TensorRT, Latest und Partitionierung zusammen sind damit kein gesicherter
Alleinstellungsanspruch. Die zusätzliche Wirkung der Vigilant-Policy muss
gegen einen passenden Kundenstack sichtbar werden.

## 7. Messharness wiederverwenden?

[S10: jetson-latency-lab](https://github.com/dankang21/jetson-latency-lab)
enthält Harness, Mess-/Reproduktionsdateien und verweist für große Traces auf
Zenodo. [LICENSE](https://github.com/dankang21/jetson-latency-lab/blob/main/LICENSE)
und [NOTICE](https://github.com/dankang21/jetson-latency-lab/blob/main/NOTICE)
wurden eingesehen: Apache-2.0 mit Urheberhinweisen; Modelle sind gesondert zu
beziehen. Das Repository warnt unter anderem vor falsch bestätigten EMC-Locks
und vor einem blockierenden Board bei ungünstiger Taktfixierung.

Unsere Entscheidung: zuerst offline Messformate und Versuchsstruktur nutzen.
Vor einer Codeübernahme: Commit pinnen, Datei-/Abhängigkeitslizenzen und
Privilegien prüfen, Attribution erhalten, Änderungen markieren. Keine
Benchmarkscripte ungeprüft mit Rootrechten auf Kundenhardware ausführen.
Es wurde in diesem Schritt nichts importiert oder geforkt.

## 8. Weitere Lernfelder ohne zusätzlichen Produktballast

- [S11: AoI-Grundlagen](https://arxiv.org/abs/2007.08564): Aktualität als
  Verbraucherzustand, nicht nur Antwortstatistik. Unsere genaue Semantik
  steht in Dokument 04.
- [S12: Clockwork](https://www.usenix.org/conference/osdi20/presentation/gujarati):
  Vorhersagbarkeit und begrenzte Ausführung als relevante Vergleichsrichtung;
  kein übertragener Benchmarkfaktor.
- Kontrolltheorie, Ressourcenbuchhaltung und Messstatistik werden im
  Zielentwurf als überprüfbare Annahmen behandelt, nicht als Gütesiegel.

## Gesamturteil

Übernehmen: bessere Beobachtung, eindeutige Profile, Verbrauchermetriken,
strengere Trennung von Wissen und Schätzung. Erproben: native Ausführung,
gezielte Interferenzplanung und echte Unterbrechungsmechanismen.
Zurückstellen: globale Optimierung über alle Hardware- und Weltzustände.
Die Liste von Forschungsrichtungen ist keine Liste bereits gelöster Probleme.
