# Gate-M3 auf dem neuen Treiber (R04)

Datum: 11.09.2026. Maschine: RTX 3070 Laptop (8 GB), Kerneltreiber
**580.178.04**, Triton 2.70.0 (26.06-py3, CUDA 13.3 im Forward-Compatibility-
Modus), ONNX-Modelle, System Shared Memory. Rohprotokolle:
`InferenceQoS-runtime/messungen/gate-m3-r04-driver178/`.

## Warum neu gemessen wurde

Mit dem Neustart vom 11.09. kam ein neuer Treiber: 580.173.02 → 580.178.04.
Jede bis dahin veröffentlichte Zahl ist unter dem alten entstanden, und die
Support-Matrix nannte nur ihn als qualifiziert. Ein Treiberwechsel ändert
Kernelauswahl, Taktverhalten und Energieverwaltung — ob die Aussage hält, ist
eine Messung und keine Annahme.

## Was sich außer dem Treiber unterscheidet

Zwei Dinge, und beide stehen hier, weil sie die Vergleichbarkeit mit R03
betreffen:

- **Kein Hardwarewächter im Benchmark.** R03 lief noch mit einem Wächter je
  Actor; seit `dd83086` startet ihn nur `vig serve`. Dieser Lauf hatte also
  keinen `nvidia-smi`-Unterprozess nebenher. Ab dem nächsten Lauf ruft
  `gate-m3` den Wächter wieder auf, wie `vig serve` es tut.
- **Etwas höherer Takt, dieselbe Grenze.** Unter 580.173.02 lief die Karte
  leistungsbegrenzt bei 1830 von 2100 MHz. Unter 580.178.04 zeigt der
  Taktmitschnitt der Messphase vom selben Tag (alle 5 s) unter Last
  1920 MHz bei rund 104 W, mit aktivem Software-Leistungslimit
  (`SwPowerCap`, `0x4`). Eine Stichprobe während Lauf 1 hatte 1905 MHz ohne
  Drosselgrund gezeigt — ein einzelner Moment, keine Charakterisierung. Die
  Karte läuft weiter am Leistungslimit, nur etwas höher getaktet; das
  erklärt, warum die Absolutwerte leicht anders liegen dürfen.

## Ergebnis: die Kernzahlen halten

Lieferfensterabdeckung, drei Läufe, 30 s je Seite und Puffertiefe:

| Strom | Triton | Vigilant | Faktor weniger unabgedeckt | R03 (580.173.02) |
|---|---|---|---|---|
| detector | 85 / 84 / 84 % | 99 / 99 / 99 % | **21,3 / 22,1 / 22,1x** | 20,1 / 22,4 / 22,4x |
| pose | 91 / 91 / 91 % | 99 / 99 / 99 % | **10,1 / 11,9 / 10,5x** | 10,5 / 12,0 / 10,6x |
| depth | 97 / 98 / 97 % | 99 / 100 / 99 % | 2,6x / besser / 5,0x | −1,7 / −3,7 / +1,1x |
| vlm | 100 / 100 / 100 % | 0 / 0 / 0 % | — | — |

Verbrauchersicht, längste Versorgungslücke:

| Strom | Triton | Vigilant |
|---|---|---|
| detector | 121 / 114 / 116 ms | 15 / 14 / 16 ms |
| pose | 91 / 53 / 88 ms | 18 / 18 / 19 ms |
| depth | 83 / 58 / 81 ms | 26 / 25 / 24 ms |

**Detektor und Pose liegen innerhalb der Spannweite von R03.** Der
Treiberwechsel ändert an der Aussage nichts.

**Die Tiefe ist diesmal positiv, und das ist kein Befund.** R03 hat gezeigt,
dass dieser Strom um null streut; drei positive Läufe unter einem anderen
Taktzustand widerlegen das nicht. Wir führen die Tiefe weiter nicht als
Gewinn.

**Die Lückenzahlen gelten nur zusammen.** Die Pose auf Tritonseite springt
zwischen zwei Läufen derselben Konfiguration von 53 auf 91 ms — dieselbe
Streuung, die R03 beschrieben hat. Die Abdeckung, die über hunderte Perioden
mittelt, tut das nicht.

## Was daraus folgt

580.178.04 ist für Gate M3 auf dieser Maschine qualifiziert. **Nicht**
wiederholt wurden auf dem neuen Treiber: Lastrampe, Dauerlauf, TensorRT,
NV-16. Deren Zahlen gelten für 580.173.02 und stehen dort mit diesem Treiber.

## Reproduzieren

```bash
# Umgebung: ../../deploy/triton/README.md
taskset -c 8-15 target/release/vig doctor -c examples/gate_m3/vig.yaml
taskset -c 8-15 target/release/gate-m3 examples/gate_m3/vig.yaml
```
