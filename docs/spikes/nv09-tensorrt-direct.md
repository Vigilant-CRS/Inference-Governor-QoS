# NV-09: der direkte TensorRT-Pfad spart 3 von 5 ms — und der grösste Teil davon ist Transport

Datum: 10.09.2026. Maschine: RTX 3070 Laptop, Treiber 580.173.02.
TensorRT 11.0.0.114 aus dem Triton-Image 26.06-py3, dieselben Engines wie im
Gate-M3-Vergleich. Probe: [nv09-tensorrt-direct-probe.cpp](nv09-tensorrt-direct-probe.cpp).

Ein Durchstich zur Entscheidung, kein Executor. Was hier steht, ist gemessen.

## Korrektur einer früheren Aussage

In `STATUS.md` stand, NV-09 sei blockiert, weil die TensorRT-Header fehlen —
„auch im Triton-Image nicht enthalten". Das war falsch. Sie liegen unter
`/usr/include/x86_64-linux-gnu/NvInfer.h`, die Bibliotheken unter
`/usr/lib/x86_64-linux-gnu/libnvinfer.so.11`, und die CUDA-Header im Image
unter `/usr/local/cuda-13.3/targets/x86_64-linux/include`. Geprüft wurde
vorher nur `/usr/include/NvInfer.h`.

Der Durchstich baut und läuft im Container mit:

```
g++ -O2 -std=c++17 probe.cpp -o probe \
  -I/usr/include/x86_64-linux-gnu \
  -I/usr/local/cuda-13.3/targets/x86_64-linux/include \
  -L/usr/local/cuda-13.3/targets/x86_64-linux/lib -lnvinfer -lcudart
```

Feste Shapes aus der Engine, ein Puffer je Tensor, ein Auftrag je Aufruf,
`cudaEvent` mit `cudaEventBlockingSync` statt Hostuhr — die Bausteine, die der
Liefergegenstand von NV-09 nennt.

## Was der direkte Pfad liefert

300 Läufe je Engine, nach 30 Aufwärmläufen:

| Engine | p50 | p95 | p99 |
|---|---:|---:|---:|
| pose_main (72 MB) | 1795 us | 2002 us | 2063 us |
| depth_main (131 MB) | 3644 us | 3936 us | 4020 us |
| rfdetr (108 MB) | 8377 us | 9211 us | 10158 us |

## Wo die Zeit bleibt

`pose_main`, dieselbe Engine, dieselbe Karte, aufgeschlüsselt über Tritons
eigene Statistik:

| Ebene | Zeit | Aufschlag |
|---|---:|---:|
| direkt, TensorRT im Prozess | 1795 us | — |
| Tritons `compute_infer` | 1901 us | +106 us (6 %) |
| Tritons `queue` + Ein-/Ausgabekopien | +734 us | |
| Triton serverseitig gesamt (`success`) | 2719 us | +924 us (51 %) |
| über gRPC gemessen (`vig profile`) | 5050 us | +2331 us (181 %) |

Drei Aussagen, und die zweite ist die wichtigste:

**Tritons TensorRT-Backend ist nicht das Problem.** Es kostet gegenüber dem
direkten Aufruf 106 us, also sechs Prozent. Ein eigener Executor würde hier
fast nichts gewinnen.

**Der grösste Einzelposten ist der Transport.** 2331 us von 5050 us — 46 % —
liegen zwischen Tritons Serverantwort und dem, was der Client misst: gRPC,
Protobuf, Kopien über den Socket. Der Messpfad von `vig profile` benutzt
**kein** Shared Memory; `vig-bench` tut es (ADR-0003), und der Gate-M3-Lauf
misst deshalb etwas anderes als diese Zeile.

**Tritons eigene Kopien kosten 734 us.** Ein-, Ausgabe und Queue. Auch davon
sollte der Shared-Memory-Pfad einen Teil sparen.

## Die fehlende Messung, nachgeholt

Der erste Vergleich mass `vig profile`, und dessen Messpfad benutzt **kein**
Shared Memory. Für die Frage „lohnt sich ein eigener Executor" ist das der
falsche Vergleich: den Transport hat dieses Projekt mit ADR-0003 längst
adressiert.

Deshalb dieselbe Engine noch einmal über System Shared Memory, mit
`vig-bench --bin shm-latency`, 300 Läufe je Modell:

| Modell | direkt | über Shm (p50) | Tritons `success` | Tritons `compute_infer` |
|---|---:|---:|---:|---:|
| pose_main | 1795 us | **2330 us** | 2442 us | 1667 us |
| depth_main | 3644 us | **4141 us** | 3867 us | 3233 us |
| rfdetr | 8377 us | **9103 us** | 8757 us | 7806 us |

**Shared Memory räumt den Transport ab.** Bei `pose_main` fällt die vom
Client gemessene Latenz von 5050 auf 2330 us — und liegt damit **unter**
Tritons eigenem `success`-Mittel von 2442 us. Die 2331 us, die vorher wie
Overhead aussahen, waren die Nutzlast auf dem Draht.

Was bleibt, ist der Abstand zum direkten Aufruf:

| Modell | Abstand direkt -> Shm | Anteil |
|---|---:|---:|
| pose_main | 535 us | 23 % |
| depth_main | 497 us | 12 % |
| rfdetr | 726 us | 8 % |

Und dieser Abstand ist erklärt: Tritons Ein- und Ausgabekopien
(`compute_input` + `compute_output`) kosten bei `pose_main` 667 us, bei
`depth_main` 552 us, bei `rfdetr` 818 us. Das ist der ganze Rest.

## Was das für NV-09 heisst

Ein eigener TensorRT-Executor gewinnt für diese Modelle **500 bis 730
Mikrosekunden je Inferenz** — 8 bis 23 %, und der Anteil sinkt mit der
Modellgrösse. Er gewinnt sie fast vollständig aus Tritons Ein- und
Ausgabekopien, nicht aus dem Backend: Tritons TensorRT-Anbindung kostet
gegenüber dem direkten Aufruf sechs Prozent.

Er kostet:

- eine C-FFI und damit eine Entscheidung über `unsafe_code = "forbid"`,
- Sanitizer- und GPU-Werkzeugtests als Liefergegenstand,
- einen zweiten Pfad für Abbruch, Eventfehler und Prozessende, der genauso
  belastbar sein muss wie der bestehende,
- und den Verzicht auf alles, was Triton mitbringt: Modellverwaltung,
  Statistik für den Abschlussabgleich (NV-00), Ratenbegrenzung.

**Für den Engpass dieses Projekts ändert er nichts.** Der Gate-M3-Vergleich
scheitert nicht an 500 us je Inferenz, sondern daran, dass ein 90-ms-Block die
Karte belegt. 500 us davon zu sparen verschiebt keine einzige Deadline.

Wo 500 us zählen: bei einem Strom mit sehr kurzer Periode, wo sie einen
messbaren Anteil der Laufzeit ausmachen — bei `pose_main` sind es 23 %. Das
ist ein benannter Anwendungsfall wert, aber keine allgemeine Priorität.

## Was nicht geprüft wurde

Alles, was die Abnahme von NV-09 sonst noch verlangt: Outputprüfung gegen eine
Referenz, Buffer-Reuse erst nach letzter Nutzung, Clientabbruch, Eventfehler,
Prozessende, ein Triton-only-Build ohne CUDA SDK. Der Durchstich beantwortet
**eine** Frage — lohnt sich der Weg — und beantwortet sie mit „für 8 bis 23 %
je Inferenz, und nicht für den Engpass, den die Messungen zeigen".
