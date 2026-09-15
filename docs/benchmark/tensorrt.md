# TensorRT: schneller, und der Engpass bleibt (NV-08)

Datum: 2026-09-10 · RTX 3070 Laptop (8 GB), Treiber 580.173.02, Triton 2.70.0,
TensorRT 11.0.0.114 · beide Seiten am selben Tag auf derselben Karte gemessen

Die Frage, die NV-08 als Risiko formuliert: *beseitigt eine Compilerruntime den
Engpass?* Dann waere der Governor fuer diesen Lastfall ueberfluessig, und das
waere ein No-Go-Signal fuer weiteren Ausbau.

**Antwort: nein.** TensorRT senkt die serialisierte Auslastung von 103 % auf
76 % und halbiert damit den Vorsprung des Governors — beseitigt ihn aber nicht.
Bei 76 % verfehlt ein getunter Triton weiterhin jeden zehnten Detektorzyklus.

## Aufbau

Dieselben Gewichte auf beiden Seiten: die ONNX-Dateien des Gate-M3-Aufbaus,
mit `trtexec` in Engines uebersetzt. **fp32, nicht fp16** — TensorRT 11 kennt
kein `--fp16` mehr, Netze sind strongly typed und die Praezision kommt aus dem
Graphen. Das ist fuer diese Frage das Richtige: gemessen wird die Runtime und
nicht nebenbei ein Praezisionswechsel, der die Ausgabe veraendert und damit ein
anderes Modell waere (ADR-0025).

Die Batchdimension von Pose und Tiefe ist im ONNX dynamisch und wurde beim Bau
auf 4 festgenagelt — dieselbe Arbeitsmenge wie im ONNX-Aufbau. Ohne das haette
`trtexec` Batch 1 gewaehlt und der Vergleich haette ein Viertel der Arbeit
gegen die volle gemessen.

Der VLM bleibt ONNX. Eine TensorRT-Uebersetzung eines generativen Modells ist
ein eigenes Vorhaben; der Lastfall bleibt damit realistisch — drei
TensorRT-Stroeme neben einem ONNX-Block.

## Laufzeiten allein

| Strom | ONNX | TensorRT | Gewinn |
|---|---:|---:|---:|
| detector (RF-DETR 512) | 15 639 us | 11 488 us | 26,5 % |
| pose (ResNet-18, Batch 4) | 4 082 us | 3 358 us | 17,7 % |
| depth (ResNet-50, Batch 4) | 9 286 us | 6 220 us | 33,0 % |
| vlm (ONNX auf beiden Seiten) | 97 902 us | 93 842 us | 4,1 % |

Geschuetzte serialisierte Auslastung: **103 % → 76 %**.

## Der A/B-Lauf

30 s je Lauf und Puffertiefe, Baseline mit Rate Limiter und Prioritaeten.

**ONNX, 103 % Auslastung** (zwei Laeufe)

| Strom | Triton | Vigilant | Faktor |
|---|---:|---:|---:|
| detector | 82 / 83 % | 99 % | 24,7x / 23,9x |
| pose | 91 % | 99 % | 10,8x |
| depth | 97 % | 91 % / 99 % | siehe unten |
| vlm | 100 % | 0 / 1 % | ADR-0012 |

**TensorRT, 76 % Auslastung**

| Strom | Triton | Vigilant | Faktor |
|---|---:|---:|---:|
| detector | 90 % | 99 % | 13,3x |
| pose | 93 % | 99 % | 8,4x |
| depth | 99 % | 100 % | besser |
| vlm | 100 % | 0 % | ADR-0012 |

## Was daraus folgt

**Der Vorsprung halbiert sich, weil die Baseline besser wird.** Vigilant liegt
in beiden Faellen bei 99 % Detektorabdeckung; Triton steigt von 82 % auf 90 %.
Der Governor wird nicht schlechter — der Abstand wird kleiner.

**Der Engpass bleibt.** Bei 76 % serialisierter Auslastung verfehlt ein
getunter Triton weiterhin 10 % der Detektorzyklen und 7 % der Posezyklen. Eine
schnellere Runtime verschiebt die Schwelle, an der Steuerung noetig wird; sie
hebt sie nicht auf.

**Fuer den Vertrieb heisst das:** die Aussage muss „unter Konkurrenz" lauten,
und die Konkurrenz muss echt sein. Wer Luft auf der Karte hat, braucht keinen
Governor — das steht so auch im README und wird durch diese Messung bestaetigt,
nicht widerlegt.

## Ein Befund, der nicht reproduzierte

Im ersten ONNX-Lauf lag `depth` mit Vigilant bei 91 % gegen 97 % der Baseline —
also **schlechter**. Der Wiederholungslauf zeigte 99 % gegen 97 %. Der Wert
liegt bei dieser Auslastung im Rauschen; als Regression wird er deshalb nicht
gefuehrt, als Gewinn aber auch nicht.

Die Verbrauchersicht ist an dieser Stelle die stabilere Groesse: dort steht
`depth` in **beiden** Laeufen bei 100 % gegen 99 %, mit einer laengsten Luecke
von 9 bzw. 24 ms gegen 64 bis 106 ms bei der Baseline. Die Lieferfenstermetrik
bestraft den Governor dafuer, veraltete Arbeit zu verwerfen, die der Verbraucher
nicht gebraucht haette — genau der Unterschied, fuer den NV-01 die zweite
Kennzahl eingefuehrt hat.

## Zwei Zahlen, die nicht vergleichbar sind

Der Detektor misst in diesem Aufbau 15 639 us, im
[Variantenaufbau](rfdetr-variants.md) 13 394 us — bei bytegleichen Gewichten.
Unterschiedlich sind die Serverkonfiguration (explizite Dimensionen gegen
Autocomplete, vier gegen fuenf geladene Modelle) und der Rate Limiter
(`execution_count` gegen aus).

Das ist keine Ungenauigkeit, sondern der Grund fuer die Gueltigkeitsdomaene im
Profilmanifest: ein Profil gilt fuer die Umgebung, in der es gemessen wurde,
und Zahlen aus zwei Umgebungen gehoeren nicht in eine Tabelle. Deshalb sind
oben **beide** Seiten am selben Tag im selben Aufbau gemessen.

## Was das nicht ist

Kein TensorRT-Direct-Pfad. Der Governor spricht weiterhin OIP mit Triton und
merkt von der Runtime nichts — ausser im Profilmanifest, wo
`runtime.platform` jetzt `tensorrt_plan` statt `onnxruntime_onnx` traegt und
damit verhindert, dass Profile beider Welten vermischt werden. Ein eigener
Executor ohne Triton ist NV-09 und nicht gebaut.

## Rohdaten

`InferenceQoS-runtime/messungen/gate-m3-trt-run/` — beide A/B-Protokolle, der
Wiederholungslauf und die gemessenen Konfigurationen samt Profilmanifesten.
