# 07 — Erneute Bewertung des Ausbauplans

Stand: 2026-09-09. Bewertet wird der Entwurf in Dokumenten 01–06, nicht eine
bereits implementierte vNext-Version. Ausgangsstand: Commit `fa4c909`.

## 1. Wird Vigilant dadurch besser?

**Plausibel ja, aber nicht schon durch das Schreiben dieses Plans.** Die
wertvollsten Änderungen verbessern die Voraussetzungen jeder Policy:
korrektes Ausführungswissen, passende Profile, ausdrücklicher Verbraucherbedarf
und reproduzierbare Messungen. Sie adressieren konkrete Lücken des heutigen Codes.

Ein größerer Predictor oder mehr Stellgrößen sind dagegen nicht automatisch
besser. Falls sie nur Ablehnungen erhöhen, schlechter erklärbar sind oder
den eigenen Hot Path verlangsamen, kann das Gesamtsystem schlechter werden.
Jede Erweiterung muss deshalb den nächsteinfacheren funktionsfähigen Pfad schlagen.

## 2. Ist der Entwurf allgemeiner und modular?

Ja, sofern die vorgesehenen Grenzen eingehalten werden:

- Der Core bleibt ohne Backend-/Hardwareabhängigkeiten replayfähig.
- Ausführungswissen und Ressourcenbesitz gelten gleichermaßen für Triton,
  native GPU-Worker und weitere passende Backends.
- Plattformen können Fähigkeiten ausdrücklich nicht besitzen; fehlende
  Telemetrie oder Präemption blockiert nicht automatisch den Basisbetrieb.
- Anwendungssemantik ist optional und autorisiert, kein eingebautes Robotik-Weltmodell.
- Ein zweiter Adapter muss die Abstraktion praktisch bestätigen; Diagramme
  allein beweisen keine gute Schnittstelle.

Der Preis der Generalisierung sind mehr Zustände und eine größere Testmatrix.
Die Architektur vermeidet diesen Preis nicht, sie macht ihn begrenzbar.

## 3. Gegenprüfung mit schwierigen Fällen

| Gegenbeispiel | Muss der Entwurf bestehen? | Antwort / verbleibendes Risiko |
|---|---|---|
| Kunde hat nur Triton und keinen Hardwareagenten | Ja | Legacy-/qualifizierter Envelope möglich; keine erfundene EMC-Zusage |
| GPU arbeitet nach RPC-Abbruch weiter | Ja | Unknown-Lease bis End-/Fencingnachweis; Availability kann eingeschränkt bleiben |
| Neuere Aufnahme macht einen alten Eventbeleg nicht wertlos | Ja | Event-/Stateful-Semantik und DAG-Verbraucher, keine blinde Latest-Regel |
| Clock-Wunsch wird von Plattform überschrieben | Ja | Pending/Observed-State; alter oder konservativer Übergangszustand bleibt maßgeblich |
| Hoher Frischescoring-Gewinn, Hintergrundarbeit verhungert | Nein, wenn Fortschritt vereinbart | G2 verlangt beide Vertragsseiten |
| p99 wird besser, vier Zyklen hintereinander fehlen | Nicht automatisch ausreichend | Consumer-Lücken und M/K/L separat bewerten |
| Paardaten passen, drei Modelle stören sich stark | Ja | unqualifizierte Kombination nicht mit addierten Paarwerten freigeben |
| Green Contexts oder XSched bringen auf Jetson keinen Nutzen | Ja | Feature bleibt aus; R1/R2 bleiben eigenständig lieferbar |
| Nur schnellere Engine, schlechtere Aufgabenlösung | Kein Produktgewinn | Semantik-/Qualitätsgrenzen und reale Task-Metrik erforderlich |

Wichtige Konsequenz: Kein Fall wird durch automatische Lockerung von
Deadlines, Alter oder Qualität „gelöst“. Wo ein Vertrag nicht tragbar ist,
benennt Vigilant den Konflikt und fordert eine Betreiberentscheidung.

## 4. Was nach diesem Plan weiterhin nicht bewiesen ist

- Überlegenheit gegenüber Holoscan, einfachen kundeneigenen Latest-Policies
  oder allen modernen Inferenzschedulern.
- Ein bestimmter Nutzen auf Jetson oder mit nativer TensorRT-Ausführung.
- Kalibrierte Deadline-Wahrscheinlichkeiten und weakly-hard-Zusagen.
- Universelle Präemption, vollständige GPU-Isolation oder funktionale Sicherheit.
- Eine bestimmte Zahlungsbereitschaft oder ein wirtschaftlich tragbarer
  Supportaufwand pro installierter Hardwarekombination.

Es bleibt ein Unterschied zwischen guter Softwarearchitektur, guter
Forschungsarbeit und einem vom Kunden tatsächlich benötigten Produkt.
Alle drei müssen gesondert nachgewiesen werden.

## 5. Sind wir „elite gut“?

Für „weltweit führend“ oder „besser als der Markt“ fehlt ein fairer, unabhängiger
Nachweis. Das wäre heute ein Marketingurteil, kein technisches Ergebnis.

Die vorhandene Basis ist ernstzunehmend: deterministischer Core, getrennte
Payloads, konkrete Überlastsemantik, Regressionstests und dokumentierte
Gegenbefunde. Gleichzeitig zeigen Ausführungsleases, Messdefinitionen und die
Co-Run-Begründung, dass Tests und anspruchsvoller Code allein keine Fehlerfreiheit
oder starke Theorie garantieren.

Der Weg zu einem Spitzenprodukt wäre: eine eng definierte Robotikaufgabe
nachweislich zuverlässiger versorgen als eine gut konfigurierte Alternative,
mit einfacher Integration, nachvollziehbaren Grenzen und stabilem Betrieb.
Nicht: möglichst viele neue Forschungsbegriffe gleichzeitig implementieren.

## 6. Gesamtentscheidung

**Weiterentwickeln, ohne Rewrite.** R0/R1 haben Vorrang; TensorRT Direct ist
eine plausible nächste Vertikale. Regler, Partitionierung, Präemption und
Semantik kommen nur nach eigenen Gates. Der gesamte Maximalausbau ist kein
kurzfristiges Pilotversprechen.

**Mit Herstellern jetzt sprechen.** Einen Entwicklungspartner und eine
messbare Aufgabe suchen; keine pauschale Produktions- oder Safety-Freigabe
behaupten. Erst der gewählte Stack entscheidet, welche Module zuerst Geld
und Engineeringzeit rechtfertigen.

## 7. Was jetzt tatsächlich geliefert ist

Acht lokale Planungs-/Bewertungsdokumente einschließlich Index. Bestehende
217 Tests erfolgreich, Format und Clippy sauber. Produktcode und Lizenz
unverändert durch diesen Arbeitsgang; keine fremden Bibliotheken eingebaut,
keine GPU-/Clock-Experimente ausgeführt und nichts veröffentlicht.

Der Plan umfasst 25 Arbeitspakete. Lokale Links, Codeblock-Abschlüsse,
eindeutige Paketabschnitte, azyklische Abhängigkeiten und die Aufwandssumme
wurden zusätzlich mechanisch geprüft. Das ist eine Dokumentkonsistenzprüfung,
kein Ersatz für die späteren technischen Abnahmen.

Nächste Neubewertung nach G0 und G1: konkrete Repros, gültige Zustandsprofile
und ein bestätigter Pilotbedarf. Bis dahin lautet der Status:
**gute Grundlage mit präziserem Ausbauplan, nicht fertiger Maximalausbau.**
