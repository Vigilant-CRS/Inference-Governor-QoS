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
verweigert. Seit dem 15.09. zeigt die Szene `autotune-laptop-2026-09-15f`,
den Lauf mit dem ausgelieferten Stand (allein auf der Maschine,
`contaminated: false`): „2 of 4 usable" und „release refused", weil die Karte
unter `SwPowerCap` in zwei Reihen den Zustand wechselte. Die Läufe `15`, `15c`
und `15d` sind verschmutzt; `15e` ist sauber, aber älter als die Reparaturen.

Dazu kommen zwei `FIT`-Zeilen: die vier Promillezahlen aus `fit_verdict`
(geschützter Strom direkt 996 → Governor 0, nachrangige 559 → 1000). Gelesen
werden nur die Zahlen, nie der Satz — ältere Läufe tragen ihn deutsch, neuere
englisch. Stehen nicht genau vier Zahlen da, bricht der Bau ab. Die
Quellenzeile sagt deshalb „shown unchanged, FIT from fit_verdict".

**Einstieg und Schluss.** Der erste Satz benennt den Schaden („part of that
work is on frames your robot has already thrown away"), der letzte die
Handlung („Faster GPUs compute the past faster. Vigilant stops computing it.
Run vig autotune on the machine you already have"). Beide trägt der Film
selbst: die Animation zeigt verworfene Frames, die Autotune-Szene den Lauf.

Das ist kein Schönheitsfehler, den man wegschneidet. Ein Werkzeug, das auf
verworfenen Daten nichts freigibt, ist genau deshalb zu glauben — und ein
Video, das nur den gelungenen Teil zeigt, wäre Werbung. Fällt später ein
sauberer Lauf an, zeigt dasselbe Bild dessen Zahlen von selbst: Es liest die
Datei, es merkt sie sich nicht.

Ein unbekannter Schritt oder fehlende Serienzahlen brechen den Bau ab, statt
stillschweigend eine kürzere Zeile zu zeichnen — ein Bild, das einen Schritt
wegließe, behauptete einen kürzeren Lauf als den stattgefundenen.

## Die Fassung vom 15.09.2026: was es kann

Die fruehere Fassung erzaehlte die Grenzen im Film selbst: drei Faelle, in
denen man es nicht braucht, den Preis des Hintergrundblocks, eine Abgrenzung
zu Zertifizierung und Echtzeit, den Hinweis auf die Logs im Repository. Das
war ehrlich und fuer einen Drei-Minuten-Film das Falsche: Wer das Video sieht,
will wissen, **was das System kann** und **wie er herausfindet, ob es fuer ihn
ist**.

Die Szenenfolge ist deshalb jetzt: Problem → was der Governor tut → die vier
Entscheidungen (`capabilities`) → wofuer er gebaut ist → die Messung gegen
Triton → drei Plattformen (`devices`) → `vig autotune` → ausprobieren →
Schluss. Die Szenen `stale`, `price` und `limits` sind aus `SCENES`
genommen; ihre Renderer bleiben fuer eine laengere Fassung erhalten.

Was sich **nicht** geaendert hat: Jede gezeigte Zahl kommt aus einer Datei.
Die Geraeteszene liest Serienzahl und Urteil aus den drei
`qualification.json` (Laptop `15f`, Pixel 2, Pixel 5); Name, Chip und Backend
stehen in `script.DEVICES`. Die Grenzen stehen weiter vollstaendig in README,
STATUS und den Berichten — nur nicht mehr im Sprechertext.

Das Video liegt seit dieser Fassung zusaetzlich unter `site/assets/`, damit die
Projektseite es abspielen kann (`<video>` mit WebVTT-Untertiteln). Die
GitHub-README verlinkt ein Vorschaubild dorthin; ein Player direkt in der README
braucht einen Upload ueber die Weboberflaeche von GitHub.

## Die Demo-Szene: echtes Bildmaterial

Seit dem 15.09. zeigen beide Schnitte aufgezeichnete Demo-Clips
(`tools/demo/render.py`, Bericht `docs/benchmark/demo-2026-09-15.md`): links
„NVIDIA Triton alone", rechts „with Vigilant", vier Kameras und ein
Sprachmodell auf einer Laptop-GPU. Im Erklaervideo folgt direkt **nach**
`measured` eine Folge aus drei Szenen: `demo` (Fahrzeug, Krakau, mit Preis
des Sprachmodells), `demo-sidewalk` (Lieferroboter, Edinburgh) und
`demo-humanoid` (Kopfkamera eines Humanoiden, TUM RGB-D). Die letzte endet mit
der Grenze: ohne Ueberlastung hilft der Governor nicht. Die beiden kurzen
Szenen tragen `chapter_break=False` und gehoeren zum Kapitel „On camera" —
YouTube verlangt je Kapitel mindestens zehn Sekunden. Im Werbe-Cut steht nach
`measured` nur der Krakau-Clip als kurze Fassung (rund 8 s).

**Warum nach und nicht statt `measured`.** Die beiden Szenen belegen
Verschiedenes. `measured` ist die Zahl gegen einen *getunten* Triton mit einem
unteilbaren Hintergrundblock (85 → 99 %), die Demo ein anderer, deutlich
ueberlasteter Aufbau. Die Demo ersetzt die Tabelle nicht, sie zeigt, wie das
Problem aussieht. Ersetzt man `measured`, faellt die einzige Zahl gegen einen
getunten Triton aus dem Film — und die Beschreibung nennt sie trotzdem.

**Was woher kommt.**

| Teil | Quelle |
|---|---|
| Clip und Zeitachsen | `script.DEMOS`: Laufordner unter `InferenceQoS-runtime/`, `demo.mp4`, `direct.jsonl`, `governed.jsonl`. Referenziert, nicht kopiert — ein neu gerenderter Clip zieht beim naechsten Bau nach |
| Zahlen in Sprechertext, Kapitel, Leiste, Beschreibung | `report.demo_facts`: laedt `tools/demo/render.py` und rechnet mit dessen `Timeline`/`ArmState` genau die Zusammenfassung, die der Clip am Ende zeigt (Fenster bis zum letzten Bild). Dazu Kameraanzahl aus dem Kopf und Median des Detektoralters bei Ankunft |
| Sprechertext | Vorlage in `script.py` mit Feldern wie `{Cameras}`, `{left_age_spoken}`, `{right_answers_spoken}`; Zahlen werden ausgeschrieben (`report.number_words`), 336 ms werden „a third of a second" (`report.spoken_ms`) |
| Namensnennung | `DEMOS[...]["attribution"]`, in der Leiste der Szene (zweite Zeile) und unter „Credits" in `youtube.md` / `youtube-promo.md` |

**Was der Text behaupten darf.** `report.assert_demo_claims` bricht den Bau ab,
wenn der direkte Arm mehr als 5 % frische Takte hat oder der Governor-Arm unter
95 % faellt: dann stimmen „effectively blind" und „stays fresh" nicht mehr,
und die Szene muss fuer diesen Lauf neu geschrieben werden. Der letzte Satz
(„With room to spare on the chip, Triton alone keeps up") bleibt stehen, weil
der Bericht genau das misst: ohne Ueberlastung hilft der Governor nicht.

**Zaehlweise.** Gezaehlt wird wie im Clip: Antworten des Sprachmodells, die bis
zum letzten Bild ankamen. Das ergibt fuer `krakow-cams4-1825` **222 → 1**. Der
Bericht nennt 223 → 2, weil er auch die zwei Antworten mitzaehlt, die erst nach
Sekunde 40 eintrafen (40,03 s und 40,18 s). Frische Takte: 0,17 % → 100 %.
Leiste, Sprechertext und Beschreibung zeigen eine Nachkommastelle
(`report.percent_text`: „0.2 %", „99.8 %") und runden nie auf 100 hoch oder
auf 0 herunter; der Clip selbst zeigt ganze Prozent.

**Der Ausschnitt.** `clip_segment` in `make_video.py` dekodiert den Clip ab
`start_s` mit ffmpeg nach RGB (BT.709), verkleinert ihn um knapp 8 % unter eine
84 Pixel hohe Leiste (die Kopfzeile des Clips mit Modell, Takt und
Blind-Schwelle bleibt so sichtbar) und kodiert wie die uebrigen Szenen. Ist die
Sprechzeit laenger als der Rest des Clips, bleibt das letzte Bild stehen —
keine Schleife, sonst sprange die Uhr im Clip zurueck. Mit `align_end` endet
der Ausschnitt mit dem Clip, damit die Zusammenfassungskarte unter dem Satz
ueber den Preis steht; `start_s` ist dann der frueheste Anfang.

**Ein weiterer Clip** (Lieferroboter, Humanoid): ein Eintrag in `DEMOS` mit
demselben Ordneraufbau und eine Szene mit `visual="demo_clip"`,
`data={"demo": <Schluessel>, "start_s": ...}`. Traegt der Lauf einen anderen
Ausgang, schlaegt `assert_demo_claims` an — dann den Sprechertext anpassen,
nicht die Schwelle.

## Probeschnitt und Rechenbudget

```bash
# nur die Demo-Szenen, in ein Arbeitsverzeichnis, zwei Prozesse, Kerne 6-7
nice -n 19 taskset -c 6-7 python3 tools/video/make_video.py \
    --out /tmp/video-preview --scenes demo --jobs 2
```

`--scenes` baut beide Schnitte nur aus den genannten Szenen und schreibt
`<stem>-preview.*`; `youtube*.md`, `thumbnail.png` und `build.json` bleiben
unberuehrt. `--jobs` begrenzt die Prozesse fuer die Einzelbilder (Voreinstellung
min(8, Kerne)). Laeuft auf dem Rechner parallel eine Latenzmessung ohne
`measure-pending`-Marke, gehoert der Bau auf zwei Kerne: `--jobs 2` und
`taskset -c 6-7`.

## Schwarze Bilder: was `blackdetect` meldet

Gemeldet war „kurze schwarze Bilder, wie Aussetzer der Kamera". Gemessen am
15.09.2026 an beiden Schnitten:

- `blackdetect=d=0.03:pix_th=0.10` meldet im Erklaervideo 0–4,4 s, 27,5–27,8 s
  und 45,7–49,7 s, im Werbe-Cut 0–3,7 s, 15,6–15,8 s und 26,7–28,4 s. Das sind
  **keine schwarzen Bilder**: der Grund aller Szenen (#0d1117) hat eine Luma um
  30 und liegt unter der 10-%-Schwelle; gemeldet werden Szenen, deren Bild sich
  gerade erst aufbaut (`trend`, `governor`, `capabilities`). YMIN 23, YMAX um
  225 — Text auf dunklem Grund. Die Paketzeitstempel laufen ohne Luecke durch.
- Echte Aussetzer stecken **im Demo-Clip selbst**, nicht in dieser Strecke:
  `tools/demo/render.py` dimmt eine Seite fuer jedes Bild, in dem das neueste
  Ergebnis aelter als `max_age_ms` ist. Rechts („with Vigilant") trifft das in
  `krakow-cams4-1825` 37 einzelne Bilder (Alter kurz ueber 100 ms, laengste
  Luecke 11 ms, also kuerzer als ein Bild) — abgedunkelt, roter Rahmen, BLIND,
  ein Bild lang. Das sieht aus wie ein Kameraaussetzer. Die Taktstatistik (100 %)
  ist davon nicht beruehrt; die Darstellung liegt in `tools/demo/render.py`.
  Die am 15.09. abends neu gerenderten Clips (alle drei Laeufe) enthalten
  diese Ein-Bild-Blitze laut Demo-Renderer nicht mehr; das Video liest die
  Clips per Pfad und uebernimmt die Korrektur beim naechsten Bau.

`build_cut` prueft jedes fertige Video mit `pix_th=0.02` (Luma unter rund 20,
also echtes Schwarz), schreibt Funde als `WARNING` und nach `build.json`
(`black`, `promo_black`). Ausserdem wird die Szenendauer jetzt vor dem Bau auf
ganze Bilder gerundet, damit Bild, Ton, Untertitel und Kapitel dieselbe Laenge
rechnen.

Ein echter Fehler fand sich dabei doch, nur nicht schwarz: Das Zusammenfuehren
von Bild und Ton lief mit `-shortest` und schnitt im Werbe-Cut vom 15.09. die
letzten vier Bilder ab (2131 Bilder im Schnitt, 2127 im Ergebnis, Bild 70,90 s
gegen Ton 71,03 s) — am Ende der Schlusskarte, also ohne Versatz in der Mitte.
Jetzt wird der Ton aufgefuellt und beide Stroeme werden auf die Szenenuhr
geschnitten (`-af apad -t <Dauer>`).

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
