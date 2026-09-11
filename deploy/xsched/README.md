# XSched unter Triton: GPU-Präemption im Backendprozess

Diese Anleitung richtet [XSched](https://github.com/XpuOS/xsched) so ein,
dass ein hoch priorisierter Triton-Prozess einen niedrig priorisierten auf
derselben GPU verdrängen kann — Level 2, „abgeschickte Queue stilllegen".
Sie beschreibt **eine Backendkonfiguration**, keinen Teil des Governors:
nativer Code gehört in den Backendprozess
([ADR-0033](../../docs/adr/0033-native-code-lives-in-the-backend-process.md)).

**Stand:** funktioniert unter Triton 2.70 (26.06, CUDA 13.3) auf einer RTX 3070
Laptop (sm86). **Nicht gemessen** — die Zahlen folgen, und der Governor plant
noch nicht mit Präemption (siehe unten).

## Das Wichtigste zuerst: zwei `libcuda` in einem Prozess

Wer XSched in einem aktuellen NVIDIA-Container startet, sieht meist einen
SIGSEGV beim ersten `cudaStreamCreate`:

```text
cudaStreamCreate → XStreamCreate → CudaQueueLv3Trap → CudaQueueLv2
  → InstrumentManager → InstrMemAllocator → cuXtraInstrMemBlockAlloc → libcuda: SIGSEGV
```

Das sieht nach „CUDA 13 wird nicht unterstützt" aus und ist es nicht. Die
Ursache:

- Das Triton-Image läuft im **Forward-Compatibility-Modus**: Anwendung und
  XSched benutzen `/usr/local/cuda/compat/lib.real/libcuda.so.1` (610.43).
- Der NVIDIA Container Toolkit (CDI) legt die **Host**-`libcuda` (hier 580)
  zusätzlich nach `/lib/x86_64-linux-gnu`.
- `cuxtra`, die vorkompilierte Bibliothek, mit der XSched Befehlsspeicher
  anlegt, sucht ihre `libcuda` **selbst** — über eingebaute Pfade oder die
  eigene Variable `CUXTRA_CUDA_LIB`, **nicht** über `XSCHED_CUDA_LIB`. Sie
  findet die Host-Bibliothek.

Zwei `libcuda` im selben Prozess, die Export-Tabelle der einen arbeitet auf
dem Kontext der anderen. Nachweis:

```bash
LD_DEBUG=libs <programm> 2>&1 | grep 'calling init: .*libcuda'
```

zeigt zwei Zeilen. Es muss eine sein.

**Abhilfe:** beide Variablen auf dieselbe Bibliothek setzen.

```bash
export XSCHED_CUDA_LIB=/usr/local/cuda/compat/lib.real/libcuda.so.1
export CUXTRA_CUDA_LIB=$XSCHED_CUDA_LIB
```

Auf einem Host ohne Container gibt es nur eine `libcuda`; dort tritt der
Fehler nicht auf. Die Probe „Host-`libcuda` im Container" hilft nicht, solange
`CUXTRA_CUDA_LIB` fehlt — genau daran ist die erste Diagnose gescheitert
([Spike, Nachtrag 2](../../docs/spikes/nv15-xsched.md)).

## Der zweite Fehler: Level 3 auf sm86

Mit einer einzigen `libcuda` scheitert der Trap-Pfad (Level 3), den XSched
für sm70/sm86 fest wählt, mit `invalid device ordinal` in `trap.cpp:20`. Der
[Patch](xsched-sm86-lv2.patch) (neun Zeilen, drei Dateien) macht mit

```bash
export XSCHED_CUDA_LV3_IMPL=LV2
```

die Level-2-Queue für sm70/sm86 wählbar. Level 3 brachte auf sm86 im Spike
ohnehin nichts über Level 2 hinaus. Alternativ läuft `XSCHED_CUDA_LV3_IMPL=TSG`
ohne Patch.

## Einrichten

```bash
# 1. XSched auf dem gepinnten Stand holen, patchen, im Triton-Image bauen
#    (ohne GPU, dauert einige Minuten)
deploy/xsched/build-xsched.sh /pfad/zu/xsched

# 2. Zwei Triton-Prozesse starten: A (geschützte Modelle, Priorität 1) und
#    B (Hintergrundmodell, Priorität 0), dazu xserver mit HPF
XSCHED_DIR=/pfad/zu/xsched MODELS=/pfad/zum/modellrepo \
  deploy/xsched/triton-two-process.sh up xsched LV2

# Dieselben zwei Prozesse ohne XSched, als Vergleich
deploy/xsched/triton-two-process.sh up plain

deploy/xsched/triton-two-process.sh down
```

Ports: A auf gRPC 9201 / HTTP 9200, B auf gRPC 9101 / HTTP 9100. Welche
Modelle A und B laden, steht in den Variablen `MODELS_A` und `MODELS_B`.
Der Governor spricht beide über `backend_endpoint` je Modell an
([Beispiel](../../examples/gate_m3/vig-two-process.yaml)).

## Stolperstellen

| Symptom | Ursache |
|---|---|
| SIGSEGV in `cuXtraInstrMemBlockAlloc` | `CUXTRA_CUDA_LIB` fehlt — zwei `libcuda` im Prozess |
| `invalid device ordinal @ trap.cpp:20` | Level-3-Trap auf sm86; `XSCHED_CUDA_LV3_IMPL=LV2` (Patch) oder `TSG` |
| `out of memory @ mm.cpp:50` beim Start von B | zu wenig GPU-Speicher: A und B **ersetzen** einen bestehenden Triton, sie laufen nicht daneben. Auf 8 GB passen A, B und ein weiterer Triton mit allen Modellen nicht zusammen |
| Shim findet sich selbst statt der echten `libcuda` | `XSCHED_CUDA_LIB` fehlt; `LD_LIBRARY_PATH` zeigt auf den Shim |
| `xserver` sieht keine Clients | Container brauchen `--ipc=host` und `--pid=host` (Shared Memory und Prozesskennungen) |
| Build: `__cxa_call_terminate@CXXABI_1.3.15` beim Linken eines Beispiels | Host-GCC zu neu für CUDA; im Triton-Image bauen oder `nvcc -ccbin g++-13` |

## Was das noch nicht ist

- **Keine Messung.** Eine Funktionsprobe mit XSched's eigenem Beispiel zeigt
  den hoch priorisierten Prozess bei 98–110 ms unter Konkurrenz statt
  182–205 ms ohne XSched (allein 96 ms). Wie sich das auf Gate M3 überträgt,
  misst `gate-m3` mit der Zwei-Prozess-Konfiguration.
- **Planung mit Präemption nur mit eigener Konfiguration.** Mit `slots: 1`
  und `no_corun` hält der Governor das Hintergrundmodell weiter zurück. Damit
  er es laufen lässt, braucht die Konfiguration eine präemptierbare Lane und
  das **gemessene** Restblocking R — nicht als Annahme, denn die XSched-API
  meldet für jede Ebene „Erfolg", auch für die unfertige
  ([ADR-0035](../../docs/adr/0035-preemption-is-a-measured-backend-property.md),
  [Beispiel](../../examples/gate_m3/vig-preemptible.yaml)). `vig calibrate`
  misst R, solange A und B unter XSched laufen.
- **Kein qualifizierter Stack.** Upstream-Stand `f49289f` plus Patch, auf
  einer Karte. Andere Architekturen, andere Treiber: erst prüfen.

XSched steht unter Apache-2.0; der Patch in diesem Verzeichnis ebenfalls.
