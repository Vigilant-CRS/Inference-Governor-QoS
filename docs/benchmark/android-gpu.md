# Gate M3 auf der GPU eines Telefons

Datum: 11./12.09.2026. Gerät: Pixel 2 (Snapdragon 835, Adreno 540,
Android 11, ungerootet, passiv gekühlt). Backend: `vig-tflite-server` (ADR-0039) mit
TFLite 2.16.1 und GPU-Delegate V2 über GLES. Governor, Lastgenerator und
Backend laufen auf dem Telefon; der Laptop schiebt nur Dateien. Rohdaten:
`InferenceQoS-runtime/messungen/android-gpu-2026-09-11/`.

## Die Frage

Hängt der Vorteil aus Gate M3 an Triton und an einer NVIDIA-Karte? Derselbe
Governor, unverändert, vor einer anderen GPU mit einem anderen Backend.

## Aufbau

| | |
|---|---|
| detector | EfficientDet-Lite0 mit NMS im Graphen (TensorFlow, Apache-2.0): 266 von 267 Knoten auf der GPU, die Nachbearbeitung auf der CPU, 25 Boxen in der Antwort |
| pose | Pose Landmarker lite, Landmarkenmodell (MediaPipe, Apache-2.0): 305 von 305 Knoten auf der GPU |
| depth | MiDaS v2.1 small (isl-org, MIT): 136 von 136 Knoten auf der GPU |
| Datenpfad | Kopie im Request (Android hat kein `/dev/shm`), auf beiden Seiten |
| Vergleich | `gate-m3` mit `VIG_GATE_COPY=1`: erst Backend direkt, dann über den Governor, je Puffertiefe 1 und 8, 30 s |
| Governor | Gold-Kerne (`taskset f0`), ohne Hardwarebeobachtung (`VIG_GATE_NO_HARDWARE=1`, kein `nvidia-smi`) |
| Beschaffung | `tools/android/fetch-assets.sh` (SHA-256 fest), Ablauf `tools/android/gate-on-phone.sh` |

MoveNet wäre das naheliegende Posenmodell, läuft aber nur zu einem Drittel
auf der GPU (97 von 297 Knoten, gemessen auf einem Pixel 5) und wäre kein
GPU-Stellvertreter.

## Die Profile: Rechenzeit ist nicht Laufzeit

`vig-tflite-server --bench` misst die Zeit im Interpreter, `vig profile`
misst über gRPC, wie der Governor es erlebt. Beide auf dem Telefon, 200
Läufe, Nulleingaben:

| Modell | Interpreter p50 / p99 | über gRPC p50 / p99 | Nutzlast hin / zurück |
|---|---:|---:|---|
| detector (ohne NMS, 19 206 Anker) | 108 / 122 ms | — | 1,2 MB / 7,2 MB |
| detector (mit NMS) | — | 225 / 287 ms | 0,3 MB / < 1 KB |
| pose | 34 / 40 ms | 90 / 120 ms | 0,8 MB / 0,9 MB |
| depth | 168 / 174 ms | 196 / 206 ms | 0,8 MB / 0,3 MB |

Beim Posenmodell sind zwei Drittel der Laufzeit Transport und gRPC auf dem
Telefon, nicht GPU. `arm-serve.md` hat denselben Preis schon gesehen: auf
dem Pixel 2 liegt der größte Teil im Kernel.

**Gleichzeitig** — alle drei Modelle ohne Governor nebeneinander, wie ein
Backend sie ohne Steuerung annimmt — teilt die GPU die Zeit auf, und das
kleine Modell verliert am meisten: Pose p50 34 → 207 ms, Detektor p95
116 → 433 ms, Tiefe p50 168 → 203 ms.

## Erster Versuch: mit der Rechenzeit geplant — der Governor schadet

Der erste Aufbau nahm die Interpreterzeiten als Profile und den Detektor
ohne NMS (7,2 MB Antwort). Unter Last (geplant 68 %), drei Läufe:

| Lauf | Detektor direkt / Governor | Pose direkt / Governor | verspätet | stale |
|---|---:|---:|---:|---:|
| 1 | 86 % / 63 % | 80 % / 39 % | 150 | 159 |
| 2 | 88 % / 56 % | 80 % / 38 % | 140 | 154 |
| 3 | 91 % / 65 % | 80 % / 39 % | 136 | 154 |

Der Governor plante mit 122 ms für einen Auftrag, der mit Transport über
200 ms dauerte. Er hielt den Slot für einen Plan, der nicht stimmte, gab
Aufträge zu spät frei und verwarf, was dabei alt wurde. **Ein Profil, das
nicht den Weg misst, den der Governor sieht, macht ihn schlechter als keinen
Governor** — hier um den Faktor 3 bis 4. Genau das verlangt Spec 13.5, und
genau das tut `vig profile`; die Interpreterzeit war der Fehler dieses
Versuchs, nicht des Governors.

## Mit gemessenen Profilen: kein Schaden, kein Einbruch, wenig Gewinn

Detektor mit NMS, Profile aus `vig profile`. Je drei Läufe; die Werte sind
Spannweiten über die Läufe. Abdeckung aus Verbrauchersicht (ADR-0005),
Lieferfenster nach der Legacy-Zählung.

**Unter Last** (`vig.yaml`, geplant 69 %):

| Strom | Abdeckung direkt / Governor | längste Lücke direkt / Governor | mittlere AoI direkt / Governor |
|---|---:|---:|---:|
| detector | 100 / 100 % | 414–423 / 256–310 ms | 795–816 / 752–768 ms |
| pose | 100 / 100 % | 285–336 / 349–420 ms | 399–418 / 483–506 ms |
| depth | 100 / 100 % | 284–296 / 618–661 ms | 1269–1285 / 1444–1569 ms |

**Über Last** (`vig-overload.yaml`, geplant 138 %):

| Strom | Lieferfenster direkt / Governor | Abdeckung direkt / Governor | längste Lücke direkt / Governor | mittlere AoI direkt / Governor |
|---|---:|---:|---:|---:|
| detector | 100 / 100 % | 100 / 100 % | 377–401 / 278–299 ms | 568–578 / 493–523 ms |
| pose | 75 / 53–57 % | 99 / 98–99 % | 297–325 / 356–380 ms | 291–364 / 374–382 ms |
| depth | 100 / 96–100 % | 100 / 100 % | 280–305 / 616–674 ms | 792–794 / 1065–1140 ms |

**Der Einbruch aus Gate M3 tritt hier nicht ein.** Auf dem Laptop verfehlt
ein getunter Triton bei 103 % jede sechste Detektorperiode; hier liefert das
Backend direkt auch bei geplanten 138 % jede. Die geplante Zahl ist
serialisiert gerechnet, mit Laufzeiten, die zu zwei Dritteln Transport und
CPU sind. Direkt überlappen sich diese Anteile verschiedener Modelle, die GPU
selbst ist nicht voll. Der Governor mit einem Slot überlappt nichts.

**Was der Governor trotzdem tut:** er hält die längste Lücke des geschützten
Detektors um ein Viertel kürzer und sein mittleres Alter um ein Zehntel
niedriger — und bezahlt mit älteren Ergebnissen bei Pose und Tiefe. Das ist
die Zusage, die er macht, aber auf diesem Gerät ist sie billig zu haben und
nicht viel wert.

## Schwere Überlast: der Governor wird selbst zum Engpass

`vig-overload-heavy.yaml` verdoppelt die Raten noch einmal (geplant 277 %).
Drei Läufe:

| Strom | Lieferfenster direkt / Governor | Abdeckung direkt / Governor | längste Lücke direkt / Governor |
|---|---:|---:|---:|
| detector | 85–89 / 55–67 % | 99 / 85–98 % | 397–412 / 428–659 ms |
| pose | 50–53 / 5–11 % | 74–78 / 6–13 % | 355–380 / 2410–5531 ms |
| depth | 100 / 83–91 % | 100 / 93–98 % | 277–331 / 536–644 ms |

Der Governor verwirft 426–471 Aufträge als veraltet und gibt 237–257 zu
spät frei. **Hier verliert er auf jedem Strom, auch auf dem geschützten.**
Die Ursache steht in der Konfiguration: der Detektor allein braucht bei 4 Hz
287 ms je 250-ms-Periode, 115 % eines seriellen Slots. Mit `slots: 1` ist
schon der geschützte Strom unmachbar, während das Backend direkt die
Transport- und CPU-Anteile verschiedener Aufträge überlappt und mehr
schafft. Ein Slot, der nicht der wirklichen Nebenläufigkeit des Backends
entspricht, ist auf diesem Gerät die falsche Beschreibung (ADR-0004).

**Ein Zusatzkredit rettet das nicht.** `vig-overload-pipelined.yaml` ist
`vig-overload.yaml` mit `pipelining_depth: 1` (geplant 138 %). Drei Läufe,
im Rahmen der Streuung dieselben Zahlen wie ohne: Detektor 100 / 100 %,
längste Lücke 386–424 / 279–307 ms; Pose-Lieferfenster 75 / 52–55 %, aus
Verbrauchersicht 99 / 65–99 %; Tiefe 100 / 93–100 %.

## Der zweite Betriebspunkt: zwei Slots

Der Befund oben verlangt die Konsequenz: ein Slot beschreibt dieses Backend
falsch. Der TFLite-Server gibt jedem Modell einen eigenen Thread mit eigenem
Interpreter — zwei **verschiedene** Modelle laufen dort wirklich
nebeneinander, zwei Auftraege desselben Modells nacheinander. Also `slots: 2`
und eine gemessene Interferenztabelle (ADR-0004, ADR-0026).

### Was `vig calibrate` auf dem Telefon misst

Erstmals gegen das TFLite-Backend gelaufen (11.09., 21:18–21:28, 200
Messungen je Stufe, `tools/android/gate-on-phone.sh` mit `STEPS=calibrate`).
Alle zwoelf Reihen qualifiziert: ohne `nvidia-smi` gibt es keinen
Hardwarezustand, der eine Reihe verwerfen koennte — die Kehrseite ist, dass
ein wandernder Takt hier **nicht** auffaellt.

| Modell | allein p50 | daneben laeuft | Verhaeltnis | Aufschlag |
|---|---:|---|---:|---:|
| pose (90 ms) | 89 815 us | depth | **2,64x** | +147 650 us |
| detector (222 ms) | 222 435 us | depth | 1,52x | +116 347 us |
| depth (200 ms) | 199 642 us | detector | 1,25x | +51 155 us |
| depth | — | pose | 1,25x | +50 063 us |
| detector | — | pose | 0,65x | 0 |
| pose | — | detector | 0,67x | 0 |

Zwei Dinge stehen darin, die eine symmetrische Regel verstecken wuerde:

- **Die Richtungen sind sehr verschieden.** Das lange `depth` verlaengert das
  kurze `pose` um das 2,6-fache, umgekehrt kostet `pose` nur ein Viertel.
  `vig calibrate` schlaegt deshalb `no_corun: [pose, depth]` vor, und die
  gemessenen Konfigurationen uebernehmen es.
- **Zwei Paarungen sind schneller als allein** (0,65x und 0,67x). Das ist
  kein Messfehler, sondern der Frequenzregler des Telefons: unter Last
  taktet der SoC hoch, und eine Einzelmessung laeuft im Sparmodus. Der
  Kalibrator schreibt fuer solche Richtungen keinen Aufschlag (`added_us` 0),
  und das ist die richtige Antwort — negative Interferenz gibt es nicht.

**Die Laststufe `under_load` ist in den Messkonfigurationen von Hand
entfernt.** Der Kalibrator belegt den zweiten Slot mit **demselben** Modell;
auf diesem Server wartet dieser Auftrag im Modellthread, statt nebenher zu
laufen (depth 2,11x, detector 1,82x — und pose 0,90x, wieder der Takt). Das
ist eine Warteschlange, keine Nebenlaeufigkeit, und als Belegungsprofil
waere es doppelt gezaehlt: die Interferenztabelle sagt dasselbe genauer.
**Fuer Backends mit einem Thread je Modell misst die Belegungsstufe des
Kalibrators die falsche Groesse** — ein Befund fuer `vig calibrate`, kein
Fehler dieser Messung.

### Gate M3 mit einem und mit zwei Slots

Dieselben 30-Sekunden-Laeufe wie oben, je drei, abwechselnd ein und zwei
Slots, mit 120 s Abkuehlpause; 11.09., 21:31–22:43. Angegeben ist die
Verbrauchersicht (Abdeckung nach ADR-0005 und laengste Lueckenspanne),
jeweils direkt / ueber den Governor.

**Unter Last (geplant 69 %):**

| Strom | direkt | ein Slot | zwei Slots |
|---|---|---|---|
| detector | 100 %, Luecke 404–412 ms | 100 %, **252–297 ms** | 100 %, 413–452 ms |
| pose | 100 %, 306–352 ms | 100 %, 373–402 ms | 100 %, 393–433 ms |
| depth | 100 %, 288–305 ms | 100 %, 602–650 ms | 100 %, **335–352 ms** |

**Ueber Last (geplant 138 %, mit zwei Slots 70 % je Slot):**

| Strom | direkt | ein Slot | zwei Slots |
|---|---|---|---|
| detector | 100 %, 379–405 ms | 100 %, **163–254 ms** | 100 %, 244–455 ms |
| pose (Lieferfenster) | 75 % | 52–65 % | **80–83 %** |
| depth | 100 %, 266–291 ms | 83–100 %, 370–1553 ms | 100 %, 313–422 ms |

**Schwere Ueberlast (geplant 277 %, mit zwei Slots 141 % je Slot):**

| Strom | direkt | ein Slot | zwei Slots |
|---|---|---|---|
| detector | 99 %, 381–412 ms | 98–100 %, **190–434 ms** | 96–99 %, 266–738 ms |
| pose | 71–78 %, 352–364 ms | 8–13 %, 3102–5548 ms | **37–45 %**, 883–1367 ms |
| depth | 100 %, 281–296 ms | 43–76 %, 2103–4302 ms | **85–95 %**, 785–2262 ms |

**Der zweite Slot behebt den Nachteil zur Haelfte.** Ueber Last liefert der
Governor mit zwei Slots dem `pose`-Strom mehr Lieferfenster als das Backend
direkt (80–83 gegen 75 %) und haelt alle drei Stroeme bei 100 % Abdeckung;
mit einem Slot verlor `pose` ein Drittel. In schwerer Ueberlast steigt
`pose` von 8–13 auf 37–45 % und `depth` von 43–76 auf 85–95 %. Der Governor
verwirft dort halb so viel als veraltet (206–227 statt 412–446) und stellt
dafuer viermal so viel zurueck.

**Bezahlt wird mit dem geschuetzten Strom.** Genau das, was ein Slot dem
Detektor gab, gibt der zweite Slot wieder her: seine laengste Luecke steigt
unter Last von 252–297 auf 413–452 ms, ueber Last von 163–254 auf
244–455 ms, in schwerer Ueberlast von 190–434 auf 266–738 ms. Mit zwei
Slots liegt er ungefaehr dort, wo das Backend ihn ohnehin liefert.

**Und in schwerer Ueberlast bleibt das Backend direkt vorn.** 85–95 % gegen
100 % bei `depth`, 37–45 % gegen 71–78 % bei `pose`. Zwei Slots sind auf
diesem Geraet also die bessere Beschreibung, aber sie machen den Governor
nicht zum Gewinner: wo der Engpass Transport und CPU sind, gewinnt, wer
ueberlappt, und das Backend ueberlappt mit drei Threads mehr als der
Governor mit zwei Slots. Die ehrliche Empfehlung fuer dieses Geraet bleibt:
**so viele Slots wie das Backend nebenlaeufig rechnen kann**, hier drei, und
die gemessene Interferenz dazu.

**Thermik.** Ueber die 18 Laeufe blieb der SoC zwischen 35 und 43,7 °C, die
Rueckseite zwischen 33 und 36 °C (Thermal-HAL, 19 Proben, je 120 s
Abkuehlpause). Eine Drosselung ist in diesen Zahlen nicht zu sehen. Die
Kalibrierung davor war haerter: zehn Minuten Dauerlast ohne Pause brachten
den SoC von 37 auf 58 °C — die Profile stammen also aus einem waermeren
Geraet als die Gate-Laeufe.

### Mit echten Bildern statt Nullen

Der Detektor traegt seine NMS im Graphen, und die haengt an der Zahl der
Kandidaten: auf Nulltensoren findet er nichts und sortiert nichts. `gate-m3`
schickt deshalb auf Wunsch echte Bilder (`VIG_GATE_FRAMES`, hier 16 Frames
aus MOT16-02, 512x512 RGB24, oeffentlicher Datensatz), reihum, auf beiden
Seiten dieselben. Ueber Last, je drei Laeufe, 12.09. 03:38–04:02:

| Strom | direkt | ein Slot | zwei Slots |
|---|---|---|---|
| detector | 100 %, Luecke 405–425 ms | 100 %, **268–310 ms** | 100 %, 279–345 ms |
| pose (Lieferfenster) | 75 % | 50–57 % | **79–82 %** |
| depth | 100 %, 277–287 ms | 80–86 %, 1531–1568 ms | 100 %, 381–469 ms |

**Die Richtung aendert sich nicht, die Zahlen werden etwas haerter.** Mit
Bildern liegt die laengste Detektorluecke auf beiden Seiten rund 60 ms
hoeher als mit Nullen, und der Ein-Slot-Governor verliert bei `depth` mehr
(80–86 statt 83–100 % Abdeckung). Der zweite Slot haelt auch hier alle drei
Stroeme bei voller Abdeckung und gibt `pose` mehr Fenster als das Backend
direkt. Eine eigene Messung der NMS-Zeit auf Nullen gegen Bilder steht aus;
`vig calibrate` misst weiter mit Nulltensoren, die Profile sind also
optimistisch.

## Detektor plus kleines Sprachmodell

Qwen2.5-0.5B-Instruct (Q4_K_M, Apache-2.0) in llama.cpp auf den vier
Gold-Kernen (`llama-bench`, nur Generierung, 64 Token je Durchgang, 4
Threads), daneben der Detektor auf der GPU hinter dem Governor
(`vig-llm.yaml`, 2 Hz, geplant 63 %), Governor und Last auf den
Silver-Kernen. **Das Sprachmodell steht nicht hinter dem Governor.** Die
Konkurrenz ist CPU, Speicherbandbreite und Wärme, nicht GPU-Zeit.
`tools/android/llm-neighbour.sh`, je drei Läufe.

| | ohne Sprachmodell | mit Sprachmodell |
|---|---:|---:|
| Detektor-Abdeckung direkt / Governor | 100 / 100 % | 100 / 100 % |
| längste Lücke direkt / Governor | 182–247 / 254–272 ms | 226–238 / 232–255 ms |
| mittlere AoI direkt / Governor | 470–482 / 488–491 ms | 474–478 / 476–483 ms |
| verspätet / stale im Governor | 0 / 0 | 0 / 0 |
| Sprachmodell | 20,8 ± 0,8 Token/s allein | 18,1–18,4 ± 1,7–1,9 Token/s |
| SoC-Temperatur | — | 35 → 46–47 °C je Lauf |

Der Detektor merkt das Sprachmodell nicht; das Sprachmodell verliert ein
Achtel seines Durchsatzes und streut doppelt so stark. Der Governor hatte
nichts zu entscheiden — kein Auftrag überzog seinen Plan. Ein `llama-bench`
mit 30 Durchgängen deckt rund drei Viertel eines `gate-m3`-Laufs ab, nicht
den ganzen. Über zwei Minuten drosselt das Telefon nicht; über längere Zeit
ist das nicht gemessen.


## Was das heißt

1. **Die Logik ist portabel, der Vorteil nicht.** Derselbe Governor steht
   ohne eine geänderte Zeile vor einem zweiten Backend auf einer zweiten
   GPU, und `vig profile` misst dagegen. Den großen Faktor aus Gate M3 gibt
   es dort aber nur, wo ein ungesteuertes Backend unter Konkurrenz
   einbricht. Auf dem Telefon bricht es bis 138 % nicht ein, und darüber
   verliert der Governor selbst.
2. **Die Engstelle entscheidet.** Auf dem Laptop ist sie GPU-Zeit, und dort
   hilft Serialisieren. Auf dem Telefon sind zwei Drittel einer Laufzeit
   Transport und CPU; ein einzelner Slot wirft die Überlappung weg, die das
   Backend direkt nutzt. Mit `slots: 2` und gemessener Interferenz holt der
   Governor die Hälfte davon zurück — über Last liegt er dann bei `pose` vor
   dem Backend, in schwerer Überlast noch immer dahinter, und der geschützte
   Detektor verliert genau den Vorsprung, den ihm der eine Slot gab. Die
   Zahl der Slots ist damit kein Feintuning, sondern die Beschreibung des
   Backends: so viele, wie es nebenläufig rechnet.
3. **Ein Profil muss den Weg messen, den der Governor sieht.** Mit der
   Interpreterzeit statt der Laufzeit über gRPC ist er dreimal schlechter
   als kein Governor. `vig profile` misst richtig; eine Kalibrierung, die
   sich selbst nachführt, müsste genau diese Zahl nachführen.
4. **Ein Nachbar auf der CPU stört den geschützten Strom hier nicht.** Das
   Sprachmodell verliert ein Achtel, der Detektor nichts.

Nicht gemessen: ein anderes Telefon (die Profile gehören diesem Gerät),
Drosselung über mehr als zwei Minuten, drei Slots (so viele Threads hat das
Backend), `vig calibrate` mit echten Bildern (es misst mit Nulltensoren, und
die NMS des Detektors hängt an der Zahl der Kandidaten).
