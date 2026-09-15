# Der Pilot mit Präemption: was sie bringt, was sie kostet, und was nicht geht

Maschine: RTX 3070 Laptop (8 GB), Treiber 580.178.04, Triton 2.70.0 (26.06),
vLLM-Image 26.06-vllm-python-py3, Messung auf `taskset -c 8-15`. Stand
`1dea01e`, eingefrorene Binaries mit Hash im Manifest. 12:51–14:46.

**Die Frage.** Der [erste vollständige Pilotlauf](messkette-2026-09-12.md)
lieferte unter Vigilant 1–2 statt 30 Lageberichte je Minute: ein langer, nicht
unterbrechbarer Auftrag kommt zwischen kurzen geschützten Ankünften nie dran
(ADR-0012). Dagegen gibt es die präemptierbare Lane (ADR-0035). Bekommt der
Berichtspfad damit Fortschritt, ohne dass der Alarmpfad dafür bezahlt?

**Die Matrix ist bewusst kleiner** als die Voreinstellung des Piloten: vier
Lastpunkte, drei Wiederholungen, eine Puffertiefe, 45 s je Zelle statt 60 s
über zwei Puffertiefen. Das sind rund 30 min je Konfiguration; drei
Konfigurationen samt Containerstarts passen so in zwei Stunden. Rohdaten:
`InferenceQoS-runtime/messungen/measure-pilot-praemption-2026-09-12/` und
`.../measure-pilot-lane-2026-09-12/`.

**Gültigkeit: keine Messzelle ist verschmutzt.** Der Wächter markierte im
ersten Lauf 6 von 74 Proben, im zweiten 1 von 122 — aber alle sechs liegen in
den Startblöcken (`up-plain`, `up-xsched`), und sie zeigen die eigenen
Backends beim Hochfahren (`triton_python_b`, `VLLM::EngineCor`) sowie einmal
die Speicherkompaktierung des Kernels (`kcompactd0`). Die Allowlist des
Wächters kannte diese Prozessnamen noch nicht; fremde Last war es nicht.

## Befund 1: Das Sprachmodell lädt unter dem XSched-Shim nicht

Der geplante Aufbau — beide Backends unter XSched, Detektor mit hoher, das
Sprachmodell mit niedriger Priorität — kam nie zustande. Der Detektor startet
unter dem Shim einwandfrei und registriert seine XQueues beim `xserver`
(Priorität 1, im Protokoll nachlesbar). Das vLLM-Backend meldet dagegen
`error: creating server: Internal - failed to load all models`.

Die Gegenprobe trennt den Shim von jeder anderen Erklärung: dasselbe Image,
dasselbe Modell, **allein auf der Karte**, ohne Detektor und ohne `xserver`.

| | mit Shim | ohne Shim |
|---|---|---|
| Server wird bereit | **nein** | ja |
| Protokoll endet bei | `gpu_worker.py: Using V2 Model Runner` | 113 Zeilen bis „bereit" |

Der Shim greift dabei nachweislich (`[XSCHED INFO] using global scheduler`
erscheint auch im Engine-Subprozess), aber die vLLM-Engine kommt bis zum
GPU-Worker und dann nicht weiter — ohne Traceback, ohne Signal. Das ist ein
anderes Bild als der bekannte SIGSEGV durch zwei `libcuda` im Prozess
([Ursache](../spikes/nv15-xsched.md)); die Ursache im vLLM-Image ist offen.

**Ohne unterbrechbares Backend gibt es keine Präemption des Berichtspfads.**
Was sich messen ließ, ist die Frage dahinter — und sie ist die wichtigere:
Was passiert, wenn der Governor mit einer Lane plant, die das Backend nicht
einlöst? Genau davor warnt ADR-0035 mit `source: declared`.

## Die drei gemessenen Konfigurationen

| Kurzname | Detektor | Sprachmodell | Governor |
|---|---|---|---|
| `plain` | ohne Shim | ohne Shim | ohne Lane |
| `shim-det` | **unter XSched** (Priorität 1) | ohne Shim | ohne Lane |
| `lane-erklaert` | unter XSched | ohne Shim | **mit Lane**, R = 4 ms angegeben |

## Befund 2: Der Shim kostet am geschützten Pfad 17–20 %

Jeder Pilotlauf misst zu Beginn die Laufzeit des Detektors ohne jede
Konkurrenz. Derselbe Test, dieselbe Karte, nur der Shim unterscheidet sich:

| | ohne Shim | mit Shim |
|---|---:|---:|
| Detektor p50 | 27 676 µs | 32 504 µs |
| Detektor p95 | 28 348 µs | 33 509 µs |
| Detektor p99 | 28 685 µs | 34 430 µs |
| 2400 Referenzframes | 78 s | 90 s |

**XSched ist auf dieser Karte nicht gratis, und bezahlt wird dort, wo es
wehtut.** In den bisherigen Zahlen stand nur der Nutzen: In
[Gate M3](messkette-2026-09-11.md#xsched-und-präemption-nv-15) hielten unter
XSched alle Ströme 100 %. Der Preis war dort nicht sichtbar, weil die
Abdeckung schon am Anschlag lag; hier, wo die Aufgabenmetrik zählt, ist er
es. Beide Arme erben ihn: Die Alarmzeit steigt auch beim Backend direkt.

## Befund 3: Die angegebene Lane bringt dem Bericht viel und kostet den Alarm

Lageberichte je Minute unter Vigilant, Median aus drei Wiederholungen:

| Lastpunkt | `plain` | `shim-det` | `lane-erklaert` |
|---|---:|---:|---:|
| A (halbe Last) | 30 | 30 | 30 |
| B | 2 | 10 | **30** |
| C | 1 | 1 | 1 |
| D | 1 | 1 | 1 |

Bei Punkt B tut die Lane genau das, was sie verspricht: Der Berichtspfad
läuft voll durch, weil der Governor ihn nicht mehr zurückhält. Bei C und D
ändert sie nichts — dort ist die Karte so voll, dass auch eine Lane nichts
mehr freimacht.

Und der Preis, gemessen am Alarmpfad (Vigilant, p95 in ms, und Abdeckung):

| Lastpunkt | `plain` | `shim-det` | `lane-erklaert` |
|---|---|---|---|
| A | 1295 ms, 995 ‰ | 1359 ms, 994 ‰ | 1337 ms, **882 ‰** |
| B | 1318 ms, 963 ‰ | 1700 ms, 906 ‰ | 1498 ms, **827 ‰** |
| C | 1294 ms, 632 ‰ | 1778 ms, 645 ‰ | 1373 ms, 646 ‰ |
| D | 1419 ms, 429 ‰ | 1728 ms, 428 ‰ | 1747 ms, 431 ‰ |

**Der Bericht verzögert den Alarm jetzt messbar.** Das Kriterium P5 — der
Berichtspfad darf die Alarmzeit um höchstens 10 % verschlechtern — hält in
`plain` und `shim-det` überall und **verfehlt bei D unter der Lane**: 1747 ms
mit Bericht gegen 1359 ms ohne, also +28 %. Die Abdeckung bei A und B fällt
mit Bericht von 994–997 ‰ auf 882–827 ‰.

Das ist der erwartete Schaden einer Zusage ohne Deckung: Der Governor plant
die geschützte Arbeit mit 4 ms Restblockierung, weil er das Backend für
unterbrechbar hält. Es ist keines. Die Lane ist damit kein Feature, das man
„auf Verdacht" einschaltet — `vig doctor` warnt bei `source: declared` zu
Recht, und diese Messung sagt, was die Warnung wert ist.

## Befund 4: P1 braucht einen Vergleichsarm, der die Aufgabe noch erfüllt

Alle drei Läufe zeigen dieselbe Lücke im Entwurf des Kriteriums P1 („Vigilants
Alarmzeit ist mindestens 30 % kürzer als die des Backends direkt"). Bei C und
D liefert der Vergleichsarm gar keine Versorgung mehr:

| Lastpunkt | Backend direkt: Abdeckung | längste Lücke | Trefferquote |
|---|---:|---:|---:|
| C | 0 ‰ | 45 000 ms | 0 ‰ |
| D | 0 ‰ | 45 000 ms | 0 ‰ |

Seine Alarmzeit ist dann die Zeit bis zu einem zufälligen Treffer und kein
Maßstab. An ihr gemessen verfehlt Vigilant die 30-Prozent-Schwelle (C:
1373 gegen 1885 ms, 27 % kürzer) — als einziger Arm, der die Aufgabe
überhaupt erfüllt. Der Vorschlag steht in
[edge-pilot.md](../pilot/edge-pilot.md#offen-p1-braucht-einen-vergleichsarm-der-die-aufgabe-noch-erfüllt);
er ist **bewusst nicht umgesetzt**, weil eine nachträglich passend gelegte
Schwelle genau der Fehler wäre, den die Trennung in zwei Gruppen behebt.

## Die Urteile

| Lauf | Gruppe Planung | Gruppe Anwendung | Exitcode |
|---|---|---|---|
| `plain` | **bestanden** (13 Kriterien) | 0 verfehlt, 4 nicht anwendbar | 0 |
| `shim-det` | verfehlt: P1 bei C und D | 0 verfehlt, 4 nicht anwendbar | 1 |
| `lane-erklaert` | verfehlt: P1 bei C und D, **P5 bei D** | 0 verfehlt, 4 nicht anwendbar | 1 |

Die vier nicht anwendbaren Kriterien sind jedes Mal dieselben: A1 und A2 auf
C und D. Die Referenz ohne Konkurrenz liegt selbst bei 347–352 ms statt der
geforderten 300 ms und findet nur 213 von 1000 annotierten Objekten. Genau
dafür ist die Trennung da — vorher hätte derselbe Befund jeden Lauf als
„verfehlt" ausgewiesen, obwohl er nichts über die Planung sagt.

## Was daraus folgt

1. **Präemption für den Berichtspfad ist auf diesem Stapel nicht verfügbar.**
   Das vLLM-Backend läuft nicht unter dem Shim. Solange das so ist, ist die
   Lane für den Piloten kein Werkzeug, sondern ein Risiko.
2. **XSched kostet am geschützten Pfad 17–20 % Laufzeit.** Wer es einschaltet,
   sollte wissen, wofür er das zahlt.
3. **Eine angegebene Lane ohne unterbrechbares Backend schadet messbar:** Der
   Bericht läuft (30 statt 2 je Minute bei B), und der Alarm zahlt mit +28 %
   p95 bei D und 12–14 Punkten Abdeckung bei A und B.
4. **Das Kriterium P1 gehört überarbeitet,** bevor der nächste Pilotlauf ein
   Urteil tragen soll.

## Was offen bleibt

- **Warum vLLM unter dem Shim nicht lädt.** Die Engine bricht im GPU-Worker
  ab, ohne Meldung. Ein Ansatz wäre, den Shim nur im Triton-Prozess und nicht
  in den vLLM-Subprozessen zu setzen.
- **R ist weiter geschätzt, nicht gemessen.** `vig calibrate` verwarf am
  11.09. jede Reihe wegen wandernden Takts.
- **Ein Backend, das wirklich unterbrechbar ist.** Erst damit lässt sich die
  Frage des Auftrags — Fortschritt für den Bericht ohne Preis für den Alarm —
  positiv beantworten statt negativ.
