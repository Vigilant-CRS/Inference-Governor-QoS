# NV-12: CUDA-Graphs bringen 4 % und kosten den Mehrmodellbetrieb

Datum: 10.09.2026. Maschine: RTX 3070 Laptop (8 GB), Treiber 580.173.02,
Triton 2.70.0 (26.06-py3), TensorRT-Engines aus demselben ONNX wie der
Gate-M3-Lauf.

## Kein eigener Codepfad

CUDA-Graphs sind über Tritons Modellkonfiguration erreichbar:

```
optimization {
  cuda { graphs: true }
}
```

Damit ist NV-12 dasselbe wie NV-08 bei TensorRT — eine **Messfrage**, kein
Arbeitspaket am Governor. Der Governor sieht davon nichts; er misst, was das
Backend liefert.

## Was es bringt

`pose_main` allein in einem Triton-Server, drei Läufe zu je 300 Messungen,
periodisch freigegeben alle 40 ms:

| | Lauf 1 | Lauf 2 | Lauf 3 | Median |
|---|---:|---:|---:|---:|
| ohne Graphs, p50 | 5184 us | 5224 us | 5203 us | **5203 us** |
| mit Graphs, p50 | 5009 us | 5006 us | 5159 us | **5009 us** |
| ohne Graphs, p95 | 5759 us | 5612 us | 5640 us | **5640 us** |
| mit Graphs, p95 | 5462 us | 5485 us | 5563 us | **5485 us** |

**3,7 % weniger p50, 2,7 % weniger p95.** Die Streuung zwischen Läufen
derselben Konfiguration liegt bei rund 1 %, der Unterschied also knapp, aber
über dem Rauschen.

Das ist die erwartete Größenordnung: CUDA-Graphs sparen Kernel-Launch-Overhead,
und der ist bei einem 5-ms-Kernelbündel klein gegenüber der Rechenzeit. Bei
einem Modell aus vielen sehr kurzen Kerneln wäre mehr zu erwarten.

## Was es kostet

Den Mehrmodellbetrieb. Und das ist die Prämisse dieses Produkts.

**Ein** Modell mit `graphs: true` lädt. **Mehrere** nicht:

```
rfdetr:     UNAVAILABLE: Internal: unable to create TensorRT engine:
            IRuntime::deserializeCudaEngine: Error Code 1: Cuda Runtime
            (In syncStreams at resources.cpp:407)
depth_main: UNAVAILABLE: dasselbe
pose_main:  unable to record CUDA graph for pose_main_0_0
            unable to finish CUDA graph: operation failed due to a previous
            error during capture
```

Triton lädt Modelle nebenläufig. Die Graph-Aufnahme des einen macht den
Legacy-Stream für den anderen unbrauchbar, und das trifft auch Modelle, die
**gar keine** Graphs eingeschaltet haben:

```
vlm_main: CUDA failure 906: operation would make the legacy stream depend on
          a capturing blocking stream
          (onnxruntime gpu_data_transfer.cc, cudaStreamSynchronize(nullptr))
```

`vlm_main` läuft auf onnxruntime und hatte `graphs` nach dem ersten Befund
ausdrücklich abgeschaltet. Es scheiterte trotzdem — an der Aufnahme eines
anderen Modells.

Reproduziert und eingegrenzt:

| Aufbau | Ergebnis |
|---|---|
| vier Modelle, alle mit Graphs | zwei Modelle laden nicht |
| drei TensorRT-Modelle, alle mit Graphs | eines lädt nicht |
| ein TensorRT-Modell mit Graphs | lädt |

Der erste Start des Vier-Modell-Aufbaus **gelang**; der zweite nicht. Es ist
ein Wettlauf beim Laden, kein deterministischer Fehler — was ihn für den
Betrieb schlechter macht, nicht besser.

## Das Urteil

Für einen Governor, dessen ganzer Zweck es ist, **mehrere** Modelle auf einer
Karte zu ordnen, sind CUDA-Graphs auf diesem Stack nicht einsetzbar. 4 %
Laufzeit gegen einen nichtdeterministischen Ladefehler ist kein Tausch.

Was das nicht heißt: dass CUDA-Graphs nichts taugen. Auf einem neueren
Treiber (das Image verlangt 610.43, hier läuft 580.173.02), mit
`graph_spec`-Einträgen statt der pauschalen Aufnahme oder mit sequenziellem
Modell-Laden kann das anders aussehen. Gemessen ist der Stack, der hier
steht.

Ein eigener TensorRT-Direktpfad (NV-09) hätte die Kontrolle über die
Aufnahmereihenfolge und damit über genau diesen Fehler. Er braucht die
`unsafe`-Entscheidung, die in [STATUS](../STATUS.md) steht.
