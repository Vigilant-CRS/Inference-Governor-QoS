# Vigilant vNext — Bewertung und modularer Maximalausbau

Stand: 2026-09-09. Ausgangscode: `fa4c909339e5cc6851b309c0ea8e8d451c6fab34`.
Status: ausgearbeiteter Architektur- und Umsetzungsentwurf, **nicht implementierte Produktversion**.

> **Dieser Ordner ist der Plan, nicht der Stand.** Er wird nicht
> fortgeschrieben — sonst waere hinterher nicht mehr zu erkennen, was am
> 09.09.2026 geplant und was spaeter entschieden wurde. Was heute umgesetzt
> ist, steht in [docs/STATUS.md](../../STATUS.md); warum es so umgesetzt
> wurde, in den ADRs ab [0019](../../adr/0019-profile-identity-beyond-a-metadata-hash.md).

## Entscheidung in einem Absatz

Vigilant weiterentwickeln, den deterministischen Rust-Kern und die Triton-Anbindung
erhalten. Zuerst Ausführungswissen, Messsemantik und Profilgültigkeit absichern;
dann zustandsabhängige Planung und einen schmalen TensorRT-Direct-Pfad ergänzen.
Hardwareaktuation, Präemption, LLM-Fortschritt und Anwendungssemantik bleiben
separat aktivierbare Ausbaustufen. Der Maximalausbau ist eine Landkarte, keine
Voraussetzung für erste Kundengespräche oder ein begrenztes Pilotprojekt.

## Dokumente und Lesereihenfolge

| Dokument | Zweck |
|---|---|
| [01 — Quellen und Bewertung](01-quellen-und-bewertung.md) | Was die Forschung tatsächlich stützt; Korrekturen am vorgeschlagenen Ausbau |
| [02 — Ist-Stand und Lücken](02-ist-stand-und-luecken.md) | Wiederverwendung, aktuelle Quelltextbefunde, Grenzen der Prüfung |
| [03 — Zielarchitektur](03-zielarchitektur.md) | Module, Schnittstellen, Zustände, Ressourcenbesitz und Migration |
| [04 — Theorie und Verträge](04-theorie-und-vertraege.md) | Präzise Zeit-, Erfolgs-, Risiko- und Zulassungssemantik |
| [05 — Arbeitspakete](05-arbeitspakete.md) | Abhängigkeiten, Liefergegenstände, Abnahme, Aufwand und Rückfallwege |
| [06 — Verifikation und Pilot](06-verifikation-und-pilot.md) | Experimente, Baselines, Freigabekriterien und Kundenarbeit |
| [07 — Abschlussbewertung](07-abschlussbewertung.md) | Erneute kritische Prüfung des Gesamtentwurfs und Empfehlung |

## Was in diesem Schritt erledigt wurde

- Primärquellen geprüft; das zentrale Paper anhand der Version v3 bewertet.
- Architekturbezogene Quelltextprüfung auf dem angegebenen Commit durchgeführt.
- Bestehende Workspace-Tests ausgeführt: **217 Tests, kein Fehlschlag**.
- `cargo fmt --all -- --check` und Clippy mit `-D warnings`: erfolgreich.
- Dokumentprüfung: acht Dateien, 26 lokale Links auf vorhandene Ziele,
  13 korrekt geschlossene Codeblöcke, keine nachgestellten Leerzeichen.
  Alle 25 Arbeitspakete besitzen Detailabschnitte; ihre Abhängigkeiten sind
  azyklisch. Die Aufwandssumme wurde rechnerisch geprüft (156–295 PT).
- Diese Dokumente erstellt; kein Produktcode, keine Lizenz und keine
  Hardwareeinstellung verändert. Keine Abhängigkeit importiert, kein Push.

Die Tests qualifizieren weder Jetson noch TensorRT Direct. Neue
Fehlereinspritzungen und GPU-Messungen wurden in diesem Schritt nicht ausgeführt.
Die neuen Quelltextbefunde benötigen die in den Arbeitspaketen genannten Repros.

## Verhältnis zu bestehenden Dokumenten

Die [v1-Spezifikation](../../../Vigilant_Inference_Governor_Specification_v1.0.md)
und [ADRs](../../adr/README.md) bleiben erhalten. Dieser Entwurf ersetzt sie
nicht rückwirkend. Künftige Implementierungen ergänzen gezielte ADRs und eine
Kompatibilitätsnotiz. Historische Benchmarkdaten werden nicht umbenannt oder
als Nachweis für neue Backends ausgegeben.

Insbesondere ist [STATUS.md](../../STATUS.md) ein älterer Arbeitsstand, keine
zuverlässige aktuelle Feature- oder Freigabeliste. Die Bestandsaufnahme in
Dokument 02 gilt nur für den oben genannten Commit.

## Nächster sinnvoller Schritt

`NV-00` bis `NV-03` vorbereiten bzw. umsetzen und parallel `NV-19`
(Kunden-/Pilotvalidierung) beginnen. Erst danach den nativen Produktpfad
ausbauen. Kein Warten auf die Forschungsoptionen `NV-13` bis `NV-18`.
