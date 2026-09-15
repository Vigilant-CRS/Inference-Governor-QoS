# RF-DETR-Varianten: gemessen, und warum Variantenwahl hier keinen Betriebspunkt hat

Datum: 2026-09-10 · RTX 3070 Laptop (8 GB), Treiber 580.173.02, Triton 2.70.0,
onnxruntime-Backend, exklusiv, ein Slot, Rate Limiter aus · 200 Messungen je
Modell, 20 Aufwaermlaeufe, Nulltensoren

Fuenf Detektionsmodelle aus einem echten Anwenderprojekt, in einem eigenen
Modellrepository. Aufbau, Herkunft und Klassenreihenfolgen:
[`examples/rfdetr_variants/`](../../examples/rfdetr_variants/).

## Die Zahlen

| Modell | Auflaesung | Klassen | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| `rfdetr_nano_512` | 512 | 7 | 13 338 us | 13 8xx us | 14 4xx us |
| `rfdetr_512` | 512 | 9 | 13 394 us | 14 0xx us | 14 5xx us |
| `rfdetr_768` | 768 | 9 | 33 333 us | 34 5xx us | 35 4xx us |
| `rfdetr_28cls_768` | 768 | 28 | 33 596 us | 34 9xx us | 35 5xx us |
| `rfdetr_23cls_768` | 768 | 23 | 33 707 us | 34 9xx us | 35 7xx us |

Zwei unabhaengige Laeufe, dieselbe Reihenfolge: alle p50 innerhalb von 1,2 %,
vier von fuenf innerhalb von 0,5 %. Die Wiederholbarkeitsschwelle aus der
[Hardwarequalifikation](../hardware-qualification.md) (10 % auf p95) ist damit
deutlich unterboten.

## Der Befund

**Die Auflaesung bestimmt die Laufzeit, das Modell fast nicht.**

* 512 px gegen 768 px: Faktor 2,5 bei 2,25-facher Pixelzahl. Nahezu linear in
  den Pixeln.
* Bei **gleicher** Auflaesung liegen alle Modelle innerhalb von 1,1 %:
  13 338 gegen 13 394 us bei 512 px, 33 333 bis 33 707 us bei 768 px. Zwischen
  7 und 28 Klassen liegt kein messbarer Laufzeitunterschied — der Backbone
  entscheidet, nicht der Kopf.

Damit hat die Variantenwahl auf dieser Modellfamilie **keinen
Betriebspunkt**, und zwar aus zwei unabhaengigen Gruenden:

1. **Fachlich nicht austauschbar.** Die Klassenmengen unterscheiden sich
   (9 / 23 / 28), und wo sie gleich sind, unterscheidet sich die
   Eingabeauflaesung. `vig doctor` nennt beides beim Namen; Belege in
   [`examples/rfdetr_variants/README.md`](../../examples/rfdetr_variants/README.md).
2. **Und selbst wenn sie austauschbar waeren, waere nichts zu gewinnen.**
   Zwischen `rfdetr_768` und den beiden Varianten mit 23 und 28 Klassen liegen 1,1 % Laufzeit. Eine
   Degradation, die ein Prozent bringt, ist keine.

Der einzige echte Hebel — 512 gegen 768 px, 20 ms Unterschied — ist genau der,
den der Eingabevertrag verbietet: eine andere Aufloesung ist eine andere
Vorverarbeitung, kein Ersatz. Wer ihn nutzen will, muss den Client umstellen,
und dann ist es eine Entscheidung des Betreibers und keine des Governors.

**Das ist ein negatives Ergebnis, und es bleibt stehen.** Es sagt nicht, dass
Variantenwahl nicht funktioniert; es sagt, dass diese fuenf Modelle sie nicht
belegen koennen. Fuer einen Nachweis braucht es eine Variantenreihe, die
dieselbe Semantik bei derselben Aufloesung und deutlich verschiedener Laufzeit
liefert — etwa dasselbe Modell in FP16 gegen INT8, oder eine bewusst
destillierte Version. Beides liegt hier nicht vor.

## Nebenbefund: dieselbe GPU, zwei Zahlen

`rfdetr_512` ist bytegleich mit dem Detektor aus dem
[Gate-M3-Lauf](gate-m3.md) — gleicher SHA-256. Dort wurde p50 = 14 916 us
gemessen, hier 13 394 us, also 10 % schneller.

Das ist kein Widerspruch, sondern der Grund fuer das Profilmanifest (NV-03).
Die Gate-M3-Zahl entstand auf einer Karte unter `SwPowerCap` bei 1830 von
2100 MHz und waehrend eines belasteten Laufs; diese hier auf einer leeren
Karte. Dieselben Gewichte, derselbe Server, zwei Betriebszustaende, zwei
Zahlen. Das Manifest in `measured.yaml` haelt beide Zustaende fest, statt sie
zu einer Zahl zu verruehren.

## Was dabei kaputtging und reparaert wurde

Der erste Kalibrierlauf hat einen Fehler im Artefakt-Digest aufgedeckt.
Digestiert wurden alle Dateien des Modellverzeichnisses ausser `config.pbtxt`
— also auch die `PROVENANCE.txt`, die vorher daneben gelegt wurde. Der Digest
verschob sich, weil jemand eine Notiz bearbeitet hatte. Ein Artefaktdigest,
den ein Kommentar bewegt, ist keiner. Digestiert werden jetzt nur die
Versionsverzeichnisse; die Korrektur steht in
[ADR-0019](../adr/0019-profile-identity-beyond-a-metadata-hash.md).

## Rohdaten

`measured.yaml` samt vollstaendigen Profilmanifesten liegt neben den
Messdaten ausserhalb dieses Repositories
(`InferenceQoS-runtime/messungen/variants-2026-09-10/`). Jedes Profil traegt
Artefakt-Digest, Serverversion, Geraet, Treiber, Aufteilung und
Gueltigkeitsdomaene; nachvollziehbar mit

```bash
vig doctor -c measured.yaml
```
