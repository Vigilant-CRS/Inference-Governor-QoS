<!--
SPDX-FileCopyrightText: 2026 Vigilant e.K.
SPDX-License-Identifier: BUSL-1.1
-->

# Das Demo-Video, reproduzierbar

Ein Video ist eine Behauptung, die niemand nachrechnen kann — es sei denn, es
entsteht aus denselben Quellen wie die Berichte. Deshalb steht hier der Bauplan
und nicht die fertige Datei.

```bash
taskset -c 0-7 nice -n 10 python3 tools/video/make_video.py
```

Ergebnis in `InferenceQoS-runtime/video/`: das Video, eine Fassung mit
eingebrannten Untertiteln, ein Kurzschnitt, die SRT-Datei, ein Vorschaubild und
`youtube.md` mit Titel, Kapiteln und Belegen. Die großen Dateien gehören
bewusst **nicht** ins Repository.

## Was woher kommt

| Teil | Quelle |
|---|---|
| Sprechertext, Szenenfolge | `script.py` — eine Datei, aus der Stimme *und* Untertitel entstehen, damit beide nicht auseinanderlaufen |
| Die Animation | `sim.py` — ein kleiner Simulator mit Kameraperiode, Detektorlaufzeit und dem unteilbaren Hintergrundblock. Die Balken sind sein Ergebnis, nicht gezeichnet |
| Terminalbilder | echte Messprotokolle aus `InferenceQoS-runtime/`, zeilenweise gerendert |
| Tabellenzahlen | die Berichte unter `docs/benchmark/`, in `script.SOURCES` benannt |
| Stimme | `piper`, Modell `en_US-lessac-high`, `length_scale 1.15` (≈ 134 Wörter je Minute) |

Die Regel aus dem Nachbarprojekt gilt hier genauso: **Ein Bild, das eine
Behauptung illustriert, wird aus der Sache erzeugt, die es behauptet.** Eine
Zahl im Video ändert sich, wenn die Messung sich ändert — oder gar nicht.

## Voraussetzungen

`ffmpeg`, `python3` mit Pillow, `piper` mit einer englischen Stimme unter
`~/.local/opt/piper/`. Kein Chrome, kein Netz, keine Schriftinstallation:
DejaVu reicht.

## Wenn eine Messung läuft

Das Skript bricht ab, solange `InferenceQoS-runtime/measure-pending` existiert.
Rendern kostet alle Kerne, und eine Latenzmessung daneben wäre wertlos.

## Was ein Mensch vor der Veröffentlichung prüfen muss

- **Hören.** Synthetische Sprache betont gelegentlich falsch; Zahlen und
  Eigennamen sind die üblichen Verdächtigen.
- **Die Zahlen gegen die Berichte lesen.** Das Skript erzwingt Belege, aber
  nicht, dass der gesprochene Satz sie richtig zusammenfasst.
- **Untertitel lesen.** Sie sind automatisch geschnitten.
- **Die Preis-Szene prüfen.** Sie zeigt zwei verschiedene Läufe: oben die
  Null-Prozent-Abdeckung des Sprachmodells aus dem Gate-M3-Lauf, unten den
  Kompromiss aus WP26 mit anderem Modell und anderem Aufbau. Die Fußnote sagt
  das; wer den Text ändert, muss es weiterhin sagen.
