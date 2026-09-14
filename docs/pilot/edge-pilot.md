# Vig-Edge-Pilot — der interne Referenzpilot

Stand: 11.09.2026 · **Abnahmekriterien festgelegt vor der ersten Messung.**
Wer dieses Dokument nach einer Messung ändert, schreibt dazu, was und warum.

## Warum ein eigener Pilot

Es gibt keinen Entwicklungspartner (NV-19). Ohne einen benannten Lastfall ist
jede weitere Zahl eine Vermutung darüber, was ein Kunde braucht. Dieser Pilot
bildet den Fall nach, für den der Governor gebaut ist: ein
**Edge-Wahrnehmungsknoten** mit mehreren Kameras, einem schnellen,
geschützten Alarmpfad und einem langsameren semantischen Berichtspfad auf
derselben GPU — mit der Zusage, dass der Bericht den Alarm nie blockiert.

Er ersetzt keinen Kundennachweis. Er ist die ehrlichste Näherung, die ohne
einen Kunden möglich ist: echte Modelle, echte annotierte Aufnahmen, eine
fachliche Kennzahl — und Kriterien, die vor der Messung feststehen.

## Der Aufbau

```
4 Kameras ─▶ Detektor (RF-DETR, 23 Klassen) ─▶ Alarm         protected
                                      │
Lagebericht ◀── Qwen3-0.6B (vLLM) ◀───┘                       best_effort
```

| | |
|---|---|
| Detektor | ein interner RF-DETR-Detektor mit 23 Klassen, Eingang 768×768, als Kopie in einem eigenen Triton-Modellverzeichnis (`edge_detector`). **Nicht im Repository und nicht öffentlich.** |
| Zielklassen | die Alarmklassen dieses Detektors; das Werkzeug prüft im Referenzdurchlauf, auf welche Klassen er bei annotierten Alarmobjekten tatsächlich antwortet |
| Berichtspfad | Qwen3-0.6B über das vLLM-Backend, eigener Tritonprozess, `best_effort`, kooperative Zerlegung wie in WP26; der Prompt nennt, was die Kameras zuletzt geliefert haben |
| Kameras | vier Wiedergabelisten aus Clips zweier Objektklassen und Clips ohne Alarmobjekt, in Echtzeit abgespielt |
| Maschine | RTX 3070 Laptop (8 GB), Treiber 580.178.04, Triton 2.70 |

Nicht Teil dieses Stands: eine zusätzliche Erkennungsstufe (`high`) auf
Ausschnitten des Detektors. Sie wäre die naheliegende Erweiterung; der Pilot
beantwortet zuerst die Frage des Alarmpfads.

## Die Daten

**Primär:** ein öffentlicher, Bild für Bild annotierter Videodatensatz mit
Clips zweier Objektklassen und Clips ohne Alarmobjekt, Annotationen im
COCO-Format. Name und Quelle stehen im lokalen Aufbereitungsskript, nicht im
Repository.

**Lizenz: CC BY-NC 3.0.** **Nur für diesen internen Test, keine Weitergabe**
von Bildern, Annotationen oder daraus abgeleiteten Dateien. Alles
Abgeleitete liegt außerhalb des Repositorys; der Pfad zum Datensatz wird dem
Aufbereitungsskript übergeben.

Was bei der Aufbereitung geprüft wurde, und was daraus folgt:

- **Nur Aufnahmereihe C1:** 640×480, genau 25 fps, Annotation in
  Videogröße. Reihe C2 mischt 30 und 29,92 fps mit Annotationen in 1920×1080;
  eine Wiedergabeliste daraus müsste umtakten und verschöbe die Zuordnung.
- **`image_id` k ist Frame k−1.** In jeder der 41 geprüften Sequenzen ist die
  Zahl der Annotationsbilder gleich der Zahl der Videoframes. Annotiert sind
  die Frames, auf denen das Alarmobjekt zu sehen ist.
- **Die Alarmobjekte sind klein:** typisch rund 20×20 px bei 640×480, nach
  der Skalierung auf 768×768 rund 24 px.
- **Die Clips ohne Alarmobjekt haben keine Annotation.** Sie sind die
  Fehlalarmseite: jede Alarmdetektion dort ist falsch. In `clips.csv` tragen
  sie `kind = negative`.
- **Aufbereitet:** vier Kameras mit 500, 600, 675 und 625 Frames (20–27 s),
  194–298 Objektboxen je Kamera. Keine zwei Kameras zeigen denselben Clip.

**Optional:** MOT16 (MOTChallenge, CC BY-NC-SA 3.0), vier Sequenzen mit
Fußgängern, drei feste Kameras und eine bewegte — ein Personenszenario mit
demselben Detektor (Klasse `person`, IoU 0,5; nur Personen, die mindestens
halb sichtbar und 20 von 512 px hoch sind). Aufbereitet mit
`tools/pilot/prepare-data.sh`; **kein Teil der Abnahme**. Für den
768er-Detektor mit `SIZE=768` aufbereiten.

## Die Arme

| Arm | Was er ist |
|---|---|
| **Referenz** | Jedes Frame einzeln, ohne Konkurrenz, gegen denselben Detektor. Keine Wartezeit — die Obergrenze, gegen die beide anderen gemessen werden. |
| **Triton direkt** | Kameras sprechen direkt mit dem Detektor-Triton, der Bericht direkt mit dem vLLM-Triton: zwei Prozesse, die GPU verteilt der Treiber. |
| **Vigilant** | Dieselben Kameras, derselbe Bericht, dieselben Bilder zu denselben Zeitpunkten — über den Governor. |
| **Vigilant ohne Bericht** | Wie Vigilant, ohne Berichtspfad — die Gegenprobe für K5. |

Welche Bilder eine Kamera sendet, bestimmt allein die Uhr seit Laufbeginn.
Alle Arme sehen dieselbe Folge. Jeder Arm wird mit den Client-Puffertiefen
1 und 4 gefahren, je Arm zählt die bessere (Spec 19.1): niedrigere
p95-Alarmlatenz, bei Gleichstand weniger verpasste Ereignisse.

## Lastpunkte

Nach **serialisierter Auslastung des Detektors**, nicht nach fester Rate: die
Rate ergibt sich aus dem im Referenzdurchlauf gemessenen Median,
`Rate = Auslastung / (Kameras × Median)`, höchstens 25 Hz (die native Rate).
Eine feste Rate hieße bei einem 40-ms-Detektor 80 % auf dem einen und 400 %
auf dem anderen Punkt — keine vergleichbaren Punkte.

| Punkt | Kameras | Auslastung des Detektors | Bericht |
|---|---:|---:|---|
| A | 2 | 50 % | ein Bericht nach dem anderen, Mindestabstand 2 s |
| B | 4 | 90 % | ebenso |
| C | 4 | 125 % | ebenso |
| D | 4 | 200 % | ebenso |

Rate und effektive Auslastung stehen im Protokoll jedes Laufs. Die Last des
Sprachmodells kommt zur Detektorauslastung hinzu. 60 s je Lauf, drei
Wiederholungen; berichtet werden Median und Spannweite.

## Die Kennzahlen

- **Alarmlatenz.** Je Ereignis — die erste annotierte Sichtbarkeit eines
  Alarmobjekts in einem Clip, mindestens eine halbe Sekunde lang sichtbar — die Zeit
  bis zur ersten **gelieferten** Detektion einer Zielklasse, die eine
  Annotation dieses Frames trifft (IoU ≥ 0,3). p50, p95, Maximum und der
  Anteil nie alarmierter Ereignisse. Ein Clipwechsel ist ein Szenenwechsel:
  ein Objekt, das dort schon zu sehen ist, ist ein neues Ereignis. Ereignisse,
  die weniger als 2 s vor Laufende liegen, zählen nicht.
- **Lagebild-Trefferquote.** Alle 100 ms: welcher Anteil der **jetzt**
  sichtbaren Alarmobjekte wird von der zuletzt gelieferten Detektion getroffen.
  Daneben die Quote der Referenz auf demselben Frame.
- **Fehlalarme.** Anteil der gelieferten Detektionen auf Clips ohne
  Alarmobjekt, die mindestens ein Alarmobjekt melden.
- **Abdeckung, AoI, längste Lücke** je Kamera, wie in Gate M3.
- **Berichtspfad.** Berichte je Minute, Dauer je Bericht, Alter der
  Detektion, auf der ein Bericht beruht.

**Warum IoU 0,3 und nicht 0,5.** Bei rund 24 px großen Boxen kostet ein
Versatz von zwei Pixeln bereits über 0,15 IoU. 0,5 würde die
Lokalisierungsgenauigkeit des Detektors messen, nicht die Frische. Die
Schwelle gilt für alle Arme gleich; das Personenszenario nutzt 0,5.

**Der Detektor wird nicht bewertet.** Auf so kleinen Objekten ist seine
Trefferquote auch ohne jede Konkurrenz unvollständig. Alle Arme werden
**relativ zur Referenz** beurteilt: gemessen wird, was sie gegenüber dem
unbelasteten Detektor verlieren.

## Abnahmekriterien

**Zwei Gruppen, seit dem 12.09.2026.** Die ursprünglichen Kriterien K1–K7,
festgelegt am 11.09. vor der ersten Messung, maßen zwei Dinge in einem
Urteil: was der Governor entscheidet, und was Detektor, Datensatz und
Bildrate überhaupt hergeben. Der [erste vollständige
Lauf](#erster-vollständiger-lauf-12092026-verfehlt) verfehlte K1 (Alarm
≤ 300 ms) und K4 (Trefferquote) um ein Vielfaches — **und die Referenz ohne
jede Konkurrenz ebenso**. Ein Kriterium, das die Referenz selbst nicht
erfüllt, kann kein Urteil über die Planung tragen. Das war ein Fehler im
Entwurf der Kriterien, und so ist er korrigiert:

- **Gruppe Planung (P):** der Vergleich gegen den Arm „Backend direkt" unter
  derselben Last. Nur sie entscheidet über den Exitcode.
- **Gruppe Anwendung (A):** die absoluten Schwellen. Sie gelten zuerst für
  die Referenz ohne Konkurrenz; verfehlt die schon, lautet das Urteil
  **nicht anwendbar: Erkennungsqualität** — ausgewiesen, aber nicht
  gewertet.

Beide Gruppen stehen getrennt im Bericht und in `summary.json`; K8 kommt
weiter aus `shm-latency`.

### Gruppe Planung

| # | früher | Kriterium | Bestanden, wenn |
|---|---|---|---|
| P1 | K2 | **Alarm unter Last** (C, D) | Vigilants p95-Alarmlatenz ist mindestens 30 % kürzer als die des Backends direkt, **oder** das Backend direkt erfüllt A1 selbst — dann gibt es dort nichts zu gewinnen. **Anwendbar, solange** der Vergleichsarm mindestens 500 ‰ der Perioden versorgt |
| P2 | K3 | **Kein Schaden ohne Last** (A) | Vigilants p95 höchstens 5 % oder 10 ms schlechter als direkt (Spec 19.8), das Größere von beiden |
| P3 | neu | **Aufgabenqualität unter Last** (C, D) | Vigilants Trefferquote höchstens 2 % schlechter als die des Backends direkt |
| P4 | neu | **Versorgung unter Last** (C, D) | Abdeckung des Alarmpfads höchstens 2 % schlechter und längste Lücke höchstens 10 % (oder 10 ms) länger als direkt |
| P5 | K5 | **Entkopplung** (alle Punkte) | Der Berichtspfad verschlechtert Vigilants Alarmlatenz nicht: p95 mit Bericht höchstens 10 % (oder 5 ms) über dem Lauf ohne Bericht |
| P6 | K6 | **Der Bericht lebt** (A, B) | mindestens zwei fertige Lageberichte je Minute |

P3 und P4 sind neu, weil die Trennung sie verlangt: Die absoluten Schwellen
wandern in die andere Gruppe, und ohne sie stünde für Trefferquote und
Versorgung gar kein Maß mehr da. Ihr Maßstab ist derselbe Lauf auf derselben
Karte, nur ohne Governor.

#### P1 braucht einen Vergleichsarm, der die Aufgabe noch erfüllt

Der Lauf unter XSched vom 12.09. zeigt eine Lücke im Entwurf von P1. Bei den
Lastpunkten C und D liefert der Arm „Backend direkt" gar keine Versorgung
mehr: 0 ‰ Abdeckung, längste Lücke 45 s, Trefferquote 0 ‰. Seine Alarmzeit
ist dann die Zeit bis zu einem zufälligen Treffer und kein Maßstab. An ihr
gemessen verfehlt Vigilant die 30-Prozent-Schwelle (1778 gegen 2317 ms, also
23 % kürzer) — obwohl es der einzige Arm ist, der die Aufgabe überhaupt noch
erfüllt.

**Ein relatives Kriterium braucht einen Vergleichsarm, der die Aufgabe noch
erfüllt.** Seit dem 14.09. gilt P1 deshalb nur, solange der Vergleichsarm
**mindestens 500 ‰ der Perioden versorgt**; darunter steht es als *nicht
anwendbar* im Bericht, und P3 und P4 tragen das Urteil.

**Woher die 500 ‰ kommen.** Nicht aus den Messwerten: Die Grenze ist
dieselbe wie bei A2 und aus demselben Grund gewählt. Versorgt ein Arm
weniger als die **Hälfte** der Perioden mit einem frischen Ergebnis, misst
seine Alarmzeit nicht mehr seine Latenz, sondern die Zeit bis zu einem
zufälligen Treffer. Ein Arm, der die Aufgabe nicht mehr erfüllt, ist kein
Maßstab dafür, wie gut sie erfüllt wird. Die Schwelle steht damit vor der
nächsten Messung fest und wurde nicht an einen vorhandenen Lauf angepasst.

**Der Lauf vom 12.09. wird nicht umgewertet.** Er bleibt „Planung verfehlt".
Zur Einordnung, was die neue Fassung dort geändert hätte: Der Vergleichsarm
lag bei C und D bei 0 ‰ Abdeckung, P1 wäre also *nicht anwendbar* gewesen,
und das Urteil hätte auf P3 und P4 beruht — die er an beiden Punkten
bestanden hat. Das ist eine Aussage über das Kriterium, kein neues Ergebnis:
Gemessen wurde nichts neu, und die Zahlen des Laufs stehen unverändert im
[Messbericht](../benchmark/messkette-2026-09-12.md).

### Gruppe Anwendung

| # | früher | Kriterium | Bestanden, wenn | Anwendbar, wenn |
|---|---|---|---|---|
| A1 | K1 | **Alarm unter Last** (C, D) | p95 ≤ 300 ms **und** nie alarmierte Ereignisse höchstens Referenz + 2 Prozentpunkte | die Referenz ohne Konkurrenz selbst ≤ 300 ms liegt |
| A2 | K4 | **Lagebild** (C, D) | Trefferquote mindestens 90 % der Referenzquote | die Referenzquote mindestens 500 ‰ der annotierten Objekte erreicht |
| A3 | K7 | **Keine erfundenen Alarme** | Fehlalarmquote höchstens Referenz + 1 Prozentpunkt | immer |
| K8 | K8 | **Datenpfad** | das Budgeturteil aus `docs/datapath-budgets.md` auf derselben Maschine: bestanden | immer |

Die Anwendbarkeitsschwelle von A2 (500 ‰) ist eine Setzung mit einem Grund:
Ein Detektor, der auf diesem Datensatz nur ein Fünftel der annotierten
Objekte überhaupt findet, trägt keine absolute Qualitätsaussage — die Zahl
beschreibt dann ihn und nicht die Planung.

### Was das Werkzeug daraus macht

Das Binary druckt beide Gruppen getrennt und setzt den Exitcode allein nach
der Gruppe Planung: **0** bestanden, **1** sauber gelaufen und Planung
verfehlt, **2** Aufbau oder Lauf kaputt, **3** kein Urteil (keine
auswertbare Zelle). Eine verfehlte oder nicht anwendbare Anwendungsgruppe
steht im Bericht und in `summary.json`, macht aber keinen Fehlschlag daraus.

#### Das Freigabeurteil: fünf getrennte Fragen

Ein bestandener Planungsstatus ist **keine Abnahme**. Er sagt nur: auf den
gemessenen Zellen entschied der Governor besser als das Backend direkt. Bis
zum 14.09. lieferte ein Lauf ohne jedes auswertbare Kriterium Exitcode 0 und
sah damit aus wie ein bestandener; das war der Befund R05 des Reviews. Seit
dem 14.09. steht deshalb neben dem Status ein Freigabeurteil aus fünf
Feldern, in der Ausgabe und in `summary.json` unter `freigabe`:

| Feld | Werte | Woher |
|---|---|---|
| `ablauf` | vollständig, unvollständig, kaputt | gemessene gegen geplante Zellen |
| `planung` | bestanden, verfehlt, kein_urteil | Gruppe P |
| `anwendung` | bestanden, verfehlt, kein_urteil | Gruppe A, nur anwendbare Kriterien |
| `qualifikation` | immer `nicht_bewertet` | Hardware und Backend; ein Messlauf stellt das nicht über sich selbst fest |
| `freigabe` | empfohlen, nicht_empfohlen | nur wenn **alle vier** darüber bestätigt sind |

Weil `qualifikation` aus eigener Kraft nie „bestätigt" lautet, kann dieses
Binary **keine** Freigabe aussprechen — es kann sie nur verweigern und den
Grund nennen. Eine Teilmatrix, eine nicht anwendbare Anwendungsgruppe oder
ein abgebrochener Lauf erzeugen nie ein positives Gesamturteil.

| # | Kriterium | Bestanden, wenn |
|---|---|---|
| K1 | **Alarm unter Last** (Punkte C und D) | Vigilant: p95 der Alarmlatenz ≤ 300 ms **und** nie alarmierte Ereignisse höchstens Referenz + 2 Prozentpunkte |
| K2 | **Vorteil unter Last** (C, D) | Vigilants p95-Alarmlatenz ist mindestens 30 % kürzer als die von Triton direkt, **oder** Triton direkt erfüllt K1 selbst — dann gibt es auf diesem Punkt nichts zu gewinnen, und das steht so im Bericht |
| K3 | **Kein Schaden ohne Last** (A) | Vigilants p95-Alarmlatenz höchstens 5 % oder 10 ms schlechter als Triton direkt (Spec 19.8), das Größere von beiden |
| K4 | **Lagebild** (C, D) | Vigilants Trefferquote mindestens 90 % der Referenzquote |
| K5 | **Entkopplung** (alle Punkte) | Der Berichtspfad verschlechtert Vigilants Alarmlatenz nicht: p95 mit Bericht höchstens 10 % (oder 5 ms) über dem Lauf ohne Bericht |
| K6 | **Der Bericht lebt** (A, B) | mindestens zwei fertige Lageberichte je Minute |
| K7 | **Keine erfundenen Alarme** | Fehlalarmquote auf Vigilant höchstens Referenz + 1 Prozentpunkt |
| K8 | **Datenpfad** | das Budgeturteil aus `docs/datapath-budgets.md` auf derselben Maschine: bestanden |

**Was als Scheitern der Produktaussage zählt.** Jedes verfehlte Kriterium der
Gruppe Planung: P1 oder P3 oder P4 verfehlt heißt, der Governor liefert unter
Last weder schnellere Alarme noch bessere Aufgabenqualität als das Backend
direkt; P2 verfehlt heißt, er schadet dort, wo nichts zu schützen ist. Das
steht dann in STATUS und README, nicht in einer Fußnote.

**Was kein Scheitern ist.** Eine schwache Referenz-Trefferquote auf 24-px-
Objekten — sie ist eine Aussage über den Detektor und macht A2 unanwendbar,
nicht den Governor schlecht. Ebenso P6 auf den Punkten C und D: wenn der
Detektor die Karte auslastet, soll der Bericht warten, das ist die
Entkopplung und nicht ihr Versagen. Deshalb wird P6 nur auf A und B geprüft.

## Erster vollständiger Lauf, 12.09.2026: verfehlt

Vier Lastpunkte, drei Wiederholungen, 60 s je Zelle, RTX 3070 Laptop, Stand
`49f3639`; Exitcode 1, also sauber gelaufen und fachlich verfehlt. Zahlen und
Einordnung: [messkette-2026-09-12.md](../benchmark/messkette-2026-09-12.md).

- **Relativ gewinnt der Governor in jedem Lastpunkt:** Alarm p95 1343 gegen
  1675 ms, Trefferquote 133 gegen 106 ‰, Abdeckung 969 gegen 854 ‰ (Punkt B).
- **Absolut verfehlt er K1 und K4 um ein Vielfaches.** Beide Arme liegen bei
  1,3–1,9 s Alarmzeit statt 300 ms, und die Referenz ohne Konkurrenz trifft
  selbst nur 223 von 1000. Diese beiden Kriterien messen die Erkennungs-
  qualität mit; sie gehören getrennt, bevor sie ein Urteil über die Planung
  tragen.
- **Der Berichtspfad verhungert** (1–2 statt 30 Berichte je Minute, ADR-0012).
  Der Lauf gehört mit präemptierbarer Lane wiederholt.
- **Bestanden:** K5 außer im härtesten Punkt, K6 bei moderater Last, K7
  überall.

## Reproduzieren

```bash
# Daten (einmalig; SRC zeigt auf den entpackten Datensatz)
# Das Aufbereitungsskript liegt lokal neben den Daten, nicht im Repository.
SRC=/pfad/zum/datensatz SIZE=768 "$VIG_PILOT_DIR/prepare-alarm.sh" "$VIG_PILOT_DIR/alarm"

# Detektor-Triton (eigener Port, eigenes Modellverzeichnis `edge_detector`)
docker run -d --name edge-pilot-triton --device nvidia.com/gpu=all \
  -p 8201:8000 -p 8202:8001 -p 8203:8002 --ipc=host \
  -v "$VIG_PILOT_DIR/models:/models:ro" nvcr.io/nvidia/tritonserver:26.06-py3 \
  tritonserver --model-repository=/models --allow-client-shm=true

# Berichts-Triton (wie WP26)
docker run -d --name onetimer-vllm --device nvidia.com/gpu=all \
  -p 8010:8000 -p 8011:8001 -v "$LLM_MODELS:/models:ro" --ipc=host \
  nvcr.io/nvidia/tritonserver:26.06-vllm-python-py3 \
  tritonserver --model-repository=/models

cargo build --release -p vig-bench
VIG_PILOT_DIR=… taskset -c 8-15 target/release/edge-pilot --smoke   # Funktionsprobe, ~2 min
VIG_PILOT_DIR=… taskset -c 8-15 target/release/edge-pilot           # alles, ~80 min
```

Optionen: `--scenario A,C`, `--seconds`, `--repeats`, `--caps 1,4`,
`--no-llm`, `--triton HOST:PORT`, `--model NAME`, `--llm HOST:PORT`,
`--dataset mot`, `--out DIR`, `--fresh`.

### Matrix und Dauer

Ein Lauf ist **eine** Matrix: Lastpunkte × Wiederholungen × Arme ×
Puffertiefen, jede Zelle `--seconds` lang. Voreinstellung: 4 × 3 × 3 × 2 = 72
Zellen à 60 s, dazu rund 3 s je Zelle für Gatewaystart und Nachlauf und der
Referenzdurchlauf (1–2 min): **rund 80 Minuten**. Das Werkzeug druckt die
Schätzung vor dem Start und nach dem Referenzdurchlauf. Die Wiederholungen
stecken schon in der Matrix; ein äußerer Runner fährt den Lauf **einmal**
und gibt ihm mindestens 100 Minuten Frist. Bis zum 11.09. brach die
Messkette ihn nach 30 Minuten ab und wiederholte ihn dreimal (Review R07).

Kleiner geht es mit `--scenario`, `--repeats`, `--seconds` und `--caps`;
eine verkleinerte Matrix beurteilt nur die Kriterien, deren Lastpunkte sie
enthält.

### Ablage und Wiederaufnahme

`--out` (Voreinstellung `$VIG_PILOT_DIR/results/edge-pilot`) enthält:

| Datei | Inhalt |
|---|---|
| `manifest.json` | der Aufbau (Datensatz, Detektor, Sprachmodell, Sekunden je Zelle) und die Referenzwerte des ersten Starts |
| `cells.jsonl` | je Zelle eine Zeile, sofort geschrieben: Schlüssel, Gültigkeit, Kennzahlen, gesendet/geliefert/abgewiesen, erschöpfte und gesperrte Puffer |
| `summary.json` | Status, Exitcode, jedes Kriterium mit Ergebnis |

Ein erneuter Start mit derselben Ablage überspringt gültige Zellen und rechnet
Raten und Profile mit den Referenzwerten des ersten Starts, damit spätere
Zellen mit früheren vergleichbar bleiben. Ein anderer Aufbau in derselben
Ablage wird abgelehnt; `--fresh` verwirft sie. Eine Zelle ohne eine einzige
Lieferung gilt als Aufbaufehler: sie wird als ungültig abgelegt, der Lauf
endet, und ein Neustart misst sie neu.

### Exitcode

| Code | Bedeutung |
|---|---|
| 0 | jedes ausgewertete Kriterium erfüllt, oder keines auswertbar (Teilmatrix) |
| 1 | sauber gelaufen, mindestens ein Kriterium verfehlt — ein negatives Ergebnis, kein Fehler |
| 2 | Aufbau oder Lauf kaputt: Backend nicht erreichbar, Zelle ohne Lieferung, Ablage nicht beschreibbar, Panik |

Die Funktionsprobe (`--smoke`) kennt nur 0 und 2: zwölf Sekunden sind keine
Abnahme.

### Die Bildpuffer

Jede Kamera hat so viele Shared-Memory-Puffer wie die größte Puffertiefe plus
eins. Ein Puffer wird verliehen, reist mit dem Auftrag und kommt erst zurück,
wenn sein Leser sicher fertig ist: bei einer Antwort, einer Ablehnung vor der
Weitergabe oder einem Fehler, den das Backend selbst meldet. Nach einem
Timeout oder einem unbekannten Ausgang bleibt er gesperrt. Ist kein Puffer
frei, wird das Frame nicht gesendet (`buffers_exhausted`), statt eines zu
überschreiben, das noch gelesen wird. Bis zum 11.09. wurde der Puffer reihum
gewählt, und ein langsamer Leser konnte sein Bild verlieren (Review R03).

Auf ruhiger Maschine, nicht neben einem Build. Ein weiterer Triton daneben
belegt GPU-Speicher; für die Messung besser beenden. Die Ergebnisse gehören
in `docs/pilot/edge-pilot-ergebnis.md`, mit dem Stand und dem
Taktmitschnitt; Rohprotokolle bleiben außerhalb des Repositorys.
