# Gate M3 auf der GPU eines Telefons

Datum: 11.09.2026. Gerät: Pixel 2 (Snapdragon 835, Adreno 540, Android 11,
ungerootet, passiv gekühlt). Backend: `vig-tflite-server` (ADR-0039) mit
TFLite 2.16.1 und GPU-Delegate V2 über GLES. Governor, Lastgenerator und
Backend laufen auf dem Telefon; der Laptop schiebt nur Dateien. Rohdaten:
`InferenceQoS-runtime/android-gpu-2026-09-11/`.

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
   Backend direkt nutzt. Richtig wären Slots, die der gemessenen
   Nebenläufigkeit entsprechen — das offene Paket "zweiter Betriebspunkt".
3. **Ein Profil muss den Weg messen, den der Governor sieht.** Mit der
   Interpreterzeit statt der Laufzeit über gRPC ist er dreimal schlechter
   als kein Governor. `vig profile` misst richtig; eine Kalibrierung, die
   sich selbst nachführt, müsste genau diese Zahl nachführen.
4. **Ein Nachbar auf der CPU stört den geschützten Strom hier nicht.** Das
   Sprachmodell verliert ein Achtel, der Detektor nichts.

Nicht gemessen: ein anderes Telefon (die Profile gehören diesem Gerät),
Drosselung über mehr als zwei Minuten, Eingaben mit echten Bildern statt
Nullen (NMS hängt von der Zahl der Kandidaten ab), mehr als ein Slot.
