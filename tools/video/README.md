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

**Reproduzierbar heißt hier: inhaltlich, nicht bitgenau.** Zwei Läufe mit
demselben Text, derselben Stimme und denselben Einstellungen ergeben nicht
dieselbe Datei. Gemessen am 14.09.2026: Sprechzeiten schwanken um Zehntel
(`measured` 20,8 s gegen 19,9 s), die Gesamtlänge um rund 0,3 s. Da die
Szenendauer aus der gemessenen Sprechlänge folgt, verschieben sich Kapitel-
und Untertitelmarken entsprechend. Wer zwei Läufe byteweise vergleicht, findet
Unterschiede — das ist Piper, nicht ein Fehler im Bauplan. Was garantiert ist:
jede gezeigte Zahl stammt aus der genannten Datei.

## Was woher kommt

| Teil | Quelle |
|---|---|
| Sprechertext, Szenenfolge | `script.py` — eine Datei, aus der Stimme *und* Untertitel entstehen, damit beide nicht auseinanderlaufen |
| Die Animation | `sim.py` — ein kleiner Simulator mit Kameraperiode, Detektorlaufzeit und dem unteilbaren Hintergrundblock. Die Balken sind sein Ergebnis, nicht gezeichnet |
| Terminalbilder | echte Messprotokolle aus `InferenceQoS-runtime/`, gelesen und gerendert von `report.py` |
| Tabellenzahlen | die Berichte unter `docs/benchmark/`, in `script.SOURCES` benannt |
| Stimme | `piper`, Modell `en_US-lessac-high`, `length_scale 1.0`. Eine frühere Fassung stand auf `1.15` und klang gedehnt; das Tempo kommt jetzt aus den Pausen zwischen den Sätzen, nicht aus gedehnten Silben |

Die Regel aus dem Nachbarprojekt gilt hier genauso: **Ein Bild, das eine
Behauptung illustriert, wird aus der Sache erzeugt, die es behauptet.** Eine
Zahl im Video ändert sich, wenn die Messung sich ändert — oder gar nicht.

## Deutsche Werkzeuge, englisches Video

Die Terminalbilder zeigen **übersetzte Spaltenköpfe über unveränderten Zahlen**.
`vig` gibt deutsch aus, das Video ist englisch, und die Tabelle für das Bild neu
zu tippen hätte genau die Zahl erzeugt, die niemand mehr gegen eine Datei prüfen
kann.

Stattdessen liest `report.py` die im Bild genannte Protokolldatei, parst die
Werte heraus und ersetzt ausschließlich die Wörter — nach Regeln, die in
derselben Datei offen stehen. Bleibt nach der Ersetzung ein deutsches Wort
übrig, **bricht der Bau ab**, statt ein halbdeutsches Bild zu rendern. Jedes
Terminalbild trägt Dateiname und Messdatum, damit jede Zahl gegen genau diese
Datei prüfbar bleibt.

**Nicht jede Quelle braucht eine Übersetzung, und das Bild sagt welche.** Die
Qualifikationsdatei von `vig autotune` ist in den Feldern, die das Video zeigt,
bereits englisch; dort wird nichts ersetzt. Die Quellenzeile trägt deshalb
einen Parameter statt einer festen Formel: unter den Protokollen von `gate-m3`
und `vig doctor` steht „only the German headings translated", unter der
Autotune-Szene „shown unchanged". Eine Übersetzung zu behaupten, die nicht
stattfand, wäre eine falsche Angabe zur Methode — klein, aber in einem Projekt,
das Belegbarkeit verkauft, an der falschen Stelle.

Der sauberere Weg wäre eine Sprachumschaltung in `vig` selbst. Die gibt es
nicht, und `report.py` tut nicht so, als gäbe es sie. Solange das so bleibt,
gibt `vig-fit` sein Urteil auch in einem sonst englischen Qualifikationsbericht
deutsch aus — dokumentiert in ADR-0044, und der Grund, warum das Video die
JSON-Felder liest und nicht die Prosa daneben.

## Die austauschbare Szene

Die Kernzahl hängt am gezeigten Lauf, und der Lauf wird sich ändern. Deshalb
steht die Messszene in `script.py` als klar markierter Block. Zum Tauschen
genügen drei Stellen:

1. `SOURCES["run"]` auf das neue Protokoll,
2. `SOURCE_DATES["run"]` auf dessen Datum aus dem zugehörigen Bericht,
3. die Zahlen im Sprechertext der Szene `measured`.

Tabelle, Faktor, Fußnote und Quellenangabe im Bild ziehen von selbst nach — sie
lesen alle aus der Datei. Wer nur die Datei tauscht und den Sprechertext
vergisst, hört es sofort: Bild und Stimme nennen dann verschiedene Zahlen.

Heute zeigt die Szene `gate-m3-r03/gate-r1.txt`: echter Detektor (RF-DETR
512 px) gegen einen nicht unterbrechbaren Block von rund 95 ms (ResNet-50
Batch 48, siehe `docs/benchmark/gate-m3.md`). **Kein Sprachmodell.** Sobald das
Reproduktionspaket mit echtem Detektor *und* echtem lokalem Sprachmodell
gemessen ist, gehört sein Protokoll hierher — erst dann darf im Text
„language model" stehen.

## Die Autotune-Szene, und warum sie auch die Verweigerung zeigt

Diese Szene lag lange als Konstante neben `SCENES` und hing bewusst nicht im
Video: den Befehl gab es nicht, und eine Funktion zu versprechen, die niemand
starten kann, wäre genau der Fehler, den der Rest dieses Skripts vermeidet.
Seit `vig autotune` existiert, hängt sie zwischen `limits` und `try`.

Ihr Bild liest `qualification.json` eines echten Laufs — nicht den Bericht
daneben. Gezeigt wird dabei ausdrücklich auch, was der Lauf **nicht**
behauptet: wie viele Messreihen verworfen wurden und ob er eine Freigabe
verweigert. Auf dem Laptop steht dort heute „1 of 4 usable" und „release
refused", weil die Karte unter `SwPowerCap` den Takt während der Messung
wechselt.

Das ist kein Schönheitsfehler, den man wegschneidet. Ein Werkzeug, das auf
verworfenen Daten nichts freigibt, ist genau deshalb zu glauben — und ein
Video, das nur den gelungenen Teil zeigt, wäre Werbung. Fällt später ein
sauberer Lauf an, zeigt dasselbe Bild dessen Zahlen von selbst: Es liest die
Datei, es merkt sie sich nicht.

Ein unbekannter Schritt oder fehlende Serienzahlen brechen den Bau ab, statt
stillschweigend eine kürzere Zeile zu zeichnen — ein Bild, das einen Schritt
wegließe, behauptete einen kürzeren Lauf als den stattgefundenen.

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
- **Die Einsatzbilder freigeben.** Die Szene „Where this belongs" zeigt einen
  humanoiden Roboter und Fahrerassistenz in der Vorentwicklung. Das sind
  *plausible Bilder*, keine Kundeninstallationen, und das Bild sagt das auch.
  Trotzdem ist es eine Aussage über Märkte — wer das Video veröffentlicht,
  muss sie vertreten wollen.
- **Die Preis-Szene prüfen.** Sie zeigt zwei verschiedene Läufe: oben die
  Null-Prozent-Abdeckung des Sprachmodells aus dem Gate-M3-Lauf, unten den
  Kompromiss aus WP26 mit anderem Modell und anderem Aufbau. Die Fußnote sagt
  das; wer den Text ändert, muss es weiterhin sagen.
