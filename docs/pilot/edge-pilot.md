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

Festgelegt am 11.09.2026, vor der ersten Messung. Das Werkzeug druckt am Ende
ein Urteil zu K1–K7; K8 kommt aus `shm-latency`.

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

**Was als Scheitern der Produktaussage zählt.** K1 oder K4 verfehlt, obwohl
Triton direkt sie erfüllt; oder K3 verfehlt. Dann schützt der Governor den
schnellen Pfad nicht besser als der Treiber, oder er schadet dort, wo nichts
zu schützen ist — und das steht dann in STATUS und README, nicht in einer
Fußnote.

**Was kein Scheitern ist.** Eine schwache Referenz-Trefferquote auf 24-px-
Objekten (eine Aussage über den Detektor), und K6 auf den Punkten C und D:
wenn der Detektor die Karte auslastet, soll der Bericht warten — das ist die
Entkopplung, nicht ihr Versagen.

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
