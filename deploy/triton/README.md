# Triton-Referenzumgebung für Gate M3

Diese Anleitung baut die Umgebung, gegen die OneTimer gemessen wird. Sie
beschreibt **nicht**, wie OneTimer ausgeliefert wird — das NVIDIA-Image wird
nicht redistribuiert, sondern von der offiziellen Registry bezogen (Spec 20.3,
6.4).

## Voraussetzungen

GPU-Zugriff im Container. Auf Ubuntu:

```bash
curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey \
  | sudo gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
  | sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' \
  | sudo tee /etc/apt/sources.list.d/nvidia-container-toolkit.list
sudo apt-get update && sudo apt-get install -y nvidia-container-toolkit
```

Die Befehle **einzeln** ausführen. In einer `&&`-Kette hinter einer
sudo-Passwortabfrage bricht sie stillschweigend nach dem ersten Glied ab —
das ist beim Aufsetzen dieser Umgebung zweimal passiert.

Prüfen:

```bash
docker run --rm --device nvidia.com/gpu=all ubuntu:24.04 nvidia-smi -L
```

`--device nvidia.com/gpu=all` und nicht `--gpus all`: Docker ≥ 29 löst
`--gpus` über CDI auf und rät dabei den Hersteller falsch
(`failed to discover GPU vendor from CDI`). Der explizite CDI-Gerätename
funktioniert ohne weitere Konfiguration, sobald `nvidia-cdi-refresh` die
Spezifikation unter `/var/run/cdi/nvidia.yaml` erzeugt hat.

## Modellrepository

Das Repository liegt **außerhalb** dieses Git-Repos. Modellgewichte gehören
nicht in die Versionsverwaltung — weder fremde noch eigene.

```
$MODELS/
  rfdetr/1/model.onnx          # das eigentliche Detektionsmodell
  detector_small/1/model.onnx  # schnellere Variante
  pose_main/1/model.onnx
  depth_main/1/model.onnx
  vlm_main/1/model.onnx        # langer, nicht unterbrechbarer Block
```

Jedes Verzeichnis braucht eine `config.pbtxt`. Die wesentlichen Punkte:

```protobuf
# Kein dynamisches Batching. Die Warteschlange gehört vor den Governor, nicht
# dahinter (ADR-0002) - sonst entsteht eine zweite, unsichtbare Queue, die
# OneTimers Entscheidungen neu ordnet.
max_batch_size: 0
instance_group [ { count: 1, kind: KIND_GPU } ]
```

Ein fester Batch in `input.dims` erzeugt eine reproduzierbare
Ausführungsdauer. Der Ausgabename unterscheidet sich zwischen Modellfamilien
(`resnetv17_dense0_fwd` bei ResNet-50, `resnetv15_dense0_fwd` bei ResNet-18);
die Ausgabeform muss dynamisch bleiben, sonst lehnt Triton die Konfiguration
als Widerspruch zum Modellgraphen ab.

## Starten

```bash
docker run -d --name vig-triton --device nvidia.com/gpu=all \
  -p 8000:8000 -p 8001:8001 -p 8002:8002 \
  -v "$MODELS:/models:ro" --ipc=host \
  nvcr.io/nvidia/tritonserver:26.06-py3 \
  tritonserver --model-repository=/models --allow-client-shm=true
```

`--allow-client-shm=true` ist ab Triton 26 Pflicht. Ohne das Flag lehnt der
Server die Registrierung mit
`Client shared memory is disabled` ab — und der Vergleich fiele
stillschweigend auf den Copy-Pfad zurück, der den Transport statt das
Scheduling misst.

`--ipc=host` ist nicht optional. Der Vergleich läuft über System Shared
Memory, weil der gRPC-Copy-Pfad sonst den Transport statt das Scheduling misst
(ADR-0003) — und ein Container hat standardmäßig sein **eigenes** `/dev/shm`.
Ohne `--ipc=host` meldet Triton
`Unable to open shared memory region`, obwohl die Region auf dem Host
existiert. `--shm-size` hilft dabei nicht: es vergrößert nur den isolierten
Bereich des Containers, statt ihn zu teilen.

## Ablauf

```bash
vig doctor  -c examples/gate_m3/vig.yaml   # Konfiguration und Backend
vig profile -c examples/gate_m3/vig.yaml   # echte Laufzeitprofile
gate-m3          examples/gate_m3/vig.yaml      # der Vergleich
```

Den Vergleich auf reservierten Kernen fahren (`taskset -c 8-15`) und **nicht**
neben einem laufenden Build: ein Latenzbenchmark neben einer Kompilierung misst
die Kompilierung.

## Warum diese Baseline fair ist

Triton läuft mit denselben Modellen, derselben Instance-Group-Konfiguration,
demselben Transport und derselben Shared-Memory-Anbindung. Dynamisches
Batching ist auf beiden Seiten aus — bei periodischer Einzelbildlast macht ein
Batcher die Baseline nicht schneller, sondern nur träger.

Beide Seiten werden zudem mit mehreren Client-Puffertiefen gefahren; je Strom
zählt das jeweils bessere Ergebnis. Nur eine Seite ihre beste Tiefe wählen zu
lassen wäre ein verstecktes Handicap (Spec 19.1).

## Laufzeitdaten liegen nicht im Repository

Modelle und Container-Images gehören nicht in die Versionsverwaltung —
Gewichte sind gitignored (Spec §20.3), NVIDIA-Images dürfen nicht
weiterverteilt werden (§6.4). Auf der Entwicklungsmaschine liegen sie neben
dem Repository:

```text
InferenceQoS-runtime/
  onetimer-vision/     Triton-Modellrepository (RF-DETR, Pose, Tiefe, VLM)
  onetimer-llm/        Qwen3-0.6B für WP26
  images/              docker save der beiden Triton-Images
```

Der Pfad ist bewusst **nicht** unter `~/.cache`. Auf der Entwicklungsmaschine
liegt das Systemlaufwerk bei 96 % Belegung, und ein Modellrepository plus
zwei NVIDIA-Images sind 60 GB — die gehören auf dasselbe Laufwerk wie das
Projekt, nicht auf die Systemplatte.

### Image aus dem Archiv wiederherstellen

```bash
docker load -i .../InferenceQoS-runtime/images/triton-vision-26.06.tar
docker load -i .../InferenceQoS-runtime/images/triton-vllm-26.06.tar
```

Das spart den Download von 35 GB. Läuft ein Benchmark nicht mehr, weil das
Image fehlt, ist das der erste Griff.

### Warum das Build-Verzeichnis dazugehört

`CARGO_TARGET_DIR` gehört nicht auf ein anderes Laufwerk gesetzt. Der
Cargo-Standard ist `<workspace>/target` und damit von sich aus dort, wo das
Projekt liegt; ein Override auf `~/.cache` verlegt 6,6 GB Build-Artefakte
still auf die Systemplatte.
