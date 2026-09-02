# Protokollkompatibel — jetzt belegt statt behauptet

Stand: 2026-09-02

## Die unbelegte Behauptung

Im README stand von Anfang an, OneTimer sei ein **protokollkompatibler**
Governor: es spricht das Open Inference Protocol (KServe v2), einen offenen
Standard, den neben NVIDIA Triton auch OpenVINO Model Server, MLServer,
TorchServe und KServe sprechen.

Gemessen wurde bis hierher ausschließlich gegen Triton. In einer
Wettbewerbsmatrix ist eine unbelegte Behauptung nichts wert — Spec §3.5
verlangt an dieser Stelle ausdrücklich Ehrlichkeit.

## Der Versuch

Derselbe Governor, dieselben ONNX-Modelle, zwei verschiedene Server. Das
Modellverzeichnis konnte unverändert bleiben: Triton und OVMS erwarten beide
`<name>/1/model.onnx`.

```bash
docker run -d --name onetimer-ovms -p 9100:9000 \
  -v <modelle>:/models:ro -v <konfig>:/config.json:ro \
  openvino/model_server:latest --config_path /config.json --port 9000

onetimer calibrate --config vorlage.yaml --out ovms.yaml
target/release/oip-check ovms.yaml 10
```

## Ergebnis

| | Triton 2.70.0 (GPU) | OpenVINO Model Server 2026.3 (CPU) |
|---|---|---|
| Erkannt als | `triton 2.70.0` | `OpenVINO Model Server 2026.3.1` |
| Shared Memory | ja — Referenzpfad | **nein — Kopierpfad** |
| Ströme | 3 | 2 |
| Gesendet | 758 | 150 |
| Geliefert | 737 | **150** |
| Abgewiesen | 21 | **0** |

**Der Governor regelt beide Server ohne eine einzige Codeänderung.** Nötig war
nur, den Konfigurationsschlüssel `backend.type` für `oip` und `kserve` zu
öffnen; der Adapter selbst enthielt nie etwas Triton-Eigenes außer einem
Strukturnamen.

## Was der Server über sich sagt, wird gelesen statt geraten

Triton meldet in seinen Servermetadaten dreizehn Erweiterungen, darunter
`system_shared_memory`. **OVMS meldet gar keine.** Genau darauf muss ein
Governor vorbereitet sein, und seit dieser Messung ist er es:

```text
$ onetimer doctor --config ovms.yaml
OK   127.0.0.1:9100: OpenVINO Model Server 2026.3.1.3a28d490b
WARN 127.0.0.1:9100: kein Shared Memory. Grosse Tensoren laufen ueber den Kopierpfad;
     gemessen kostet ein 6,2-MB-Bild dort +11,7 ms statt +160 us (Faktor 73).
```

Die Zahl in der Warnung stammt aus [`data-plane.md`](data-plane.md). Ein
Betreiber, der diesen Unterschied erst im Betrieb bemerkt, hat die falsche
Hardware gekauft — deshalb steht er in der Prüfung und nicht im Kleingedruckten.

## Die Kalibrierung fällt auf anderer Hardware anders aus

Dasselbe Modellpaar, zweimal gemessen:

| Paar | auf der RTX 3070 | auf der CPU (OVMS) |
|---|---:|---:|
| Tiefe neben Pose | 1,10x–1,20x → **erlaubt** | **2,66x → `no_corun`** |

Auf der GPU dürfen sich Pose und Tiefe einen Slot teilen, auf der CPU nicht:
dort sättigen zwei Inferenzen die Kerne, und Nebenläufigkeit bringt keinen
Durchsatz mehr. Das hätte niemand geraten — es wurde gemessen, und genau dafür
gibt es [`onetimer calibrate`](../adr/0018-calibrate-hardware-not-requirements.md).

## Grenzen

- **Zwei Modelle, ein zweiter Server.** MLServer, TorchServe und KServe sind
  weiterhin ungeprüft. Die Behauptung lautet ab jetzt „gegen zwei Server
  belegt", nicht „gegen alle".
- **Keine Leistungsaussage.** OVMS lief auf der CPU; die Laufzeiten sind nicht
  mit denen der GPU vergleichbar und sollen es nicht sein. Geprüft wurde der
  Regelweg, nicht der Durchsatz.
- **Kein Shared-Memory-Pfad auf OVMS.** Der schnelle Datenpfad bleibt Servern
  vorbehalten, die die Erweiterung melden. Das ist keine Einschränkung von
  OneTimer, sondern eine des Servers — und sie wird jetzt benannt.

## Reproduzieren

```bash
target/release/oip-check <konfiguration.yaml> [sekunden]
```

Startet den Governor vor dem in der Konfiguration genannten Backend, schickt
Verkehr durch und zählt. Endet mit Rückgabewert 1, wenn keine einzige Antwort
zurückkam.
