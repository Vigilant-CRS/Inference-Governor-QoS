#!/usr/bin/env bash
# Beschafft die Modelle des Reproduktionspakets und legt Triton-Modell-
# repositories an.
#
#   tools/repro/fetch-models.sh [--with-llm] [zielverzeichnis]
#
# Vorgabe: ./repro-triton neben dem Arbeitsverzeichnis. Mit `--with-llm`
# entsteht daneben `<ziel>-llm` mit dem Sprachmodell. Die Gewichte gehoeren
# **nicht** in die Versionsverwaltung — weder fremde noch eigene
# (deploy/triton/README.md). Im Repository stehen nur dieses Skript und die
# Digests.
#
# Warum diese Modelle
# -------------------
# Die veroeffentlichten Gate-M3-Zahlen stammen von Modellen, die niemand
# ausserhalb hat: ein internes Detektionsmodell und ResNet-Stellvertreter mit
# passenden Formen und Laufzeiten. Fuer die Scheduling-Aussage ist das
# zulaessig, zum Nachfahren taugt es nicht. Hier stehen deshalb nur Modelle,
# die jeder unter Apache-2.0 laden kann:
#
#   rtdetr_r18  RT-DETR R18  (PekingU/rtdetr_r18vd)     83 MB  kleine Variante
#   rtdetr_r50  RT-DETR R50  (PekingU/rtdetr_r50vd)    175 MB  grosse Variante
#   qwen        Qwen3-0.6B   (Qwen/Qwen3-0.6B)         1,5 GB  nur --with-llm
#
# Die Lizenz stammt jeweils vom Basismodell: die ONNX-Konvertierungen auf
# HuggingFace tragen kein eigenes Lizenzfeld.
#
# Beide Detektoren sind fuer die Variantenwahl tauglich, und das ist geprueft
# und nicht angenommen: dieselbe I/O-Signatur (`pixel_values` → `logits`,
# `pred_boxes`, mit tools/onnx-signature.py gelesen), dieselbe Vorverarbeitung
# (beide `preprocessor_config.json` sind in den wirksamen Feldern gleich:
# `do_normalize: false`, Skalierung 1/255, bilinearer Resize auf 640x640,
# kein Padding) und dieselben 80 COCO-Klassen in derselben Reihenfolge (beide
# `config.json` feldweise verglichen). Genau das braucht die Variantenwahl:
# zwei echte Modelle, gleiche Bedeutung, verschiedene Groesse und Laufzeit.
#
# Nicht dabei und warum: YOLOv8 (AGPL, und die ONNX-Fassungen antworten ohne
# Anmeldung mit HTTP 401), YOLOX (die Releasedateien liegen hinter einer
# Weiterleitung, die sich schlecht pruefen laesst).
#
# Das Sprachmodell liegt in einem **eigenen** Repository, weil es ein anderes
# Triton-Image braucht: das vLLM-Backend und das onnxruntime-Backend lassen
# sich nicht in einen Prozess legen (docs/benchmark/nv16-prefill.md).
set -euo pipefail

MIT_LLM=0
ZIEL=""
for arg in "$@"; do
  case "$arg" in
    --with-llm) MIT_LLM=1 ;;
    -*) echo "Unbekannte Option: $arg" >&2; exit 2 ;;
    *) ZIEL=$arg ;;
  esac
done
ZIEL=${ZIEL:-./repro-triton}
ZIEL_LLM="$ZIEL-llm"
mkdir -p "$ZIEL"

# name|quelle|sha256|lizenz|basismodell
MODELLE=(
  "rtdetr_r18|https://huggingface.co/onnx-community/rtdetr_r18vd/resolve/main/onnx/model.onnx|11843b02455cc24009aed24d4c40db721b1093be5ccd6bbe7b9c441abb1d0558|Apache-2.0|PekingU/rtdetr_r18vd"
  "rtdetr_r50|https://huggingface.co/onnx-community/rtdetr_r50vd/resolve/main/onnx/model.onnx|b1a6aa26c56b7838b02c2b5fa66d312deee1295095ea5e85f5679a6f41eee855|Apache-2.0|PekingU/rtdetr_r50vd"
)

# Die Dateien des Sprachmodells. Digest je Datei, nicht nur fuer die Gewichte:
# ein vertauschter Tokenizer faellt sonst nirgends auf.
QWEN_BASIS="https://huggingface.co/Qwen/Qwen3-0.6B/resolve/main"
QWEN_DATEIEN=(
  "model.safetensors|f47f71177f32bcd101b7573ec9171e6a57f4f4d31148d38e382306f42996874b"
  "config.json|660db3b73d788119c04535e48cf9be5f55bc3100841a718637ae695b442f27dd"
  "generation_config.json|2325da0f15bb848e018c5ae071b7943332e9f871d6b60e2ed22ca97d4cb993d2"
  "merges.txt|8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5"
  "tokenizer.json|aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4"
  "tokenizer_config.json|d5d09f07b48c3086c508b30d1c9114bd1189145b74e982a265350c923acd8101"
  "vocab.json|ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910"
  "LICENSE|832dd9e00a68dd83b3c3fb9f5588dad7dcf337a0db50f7d9483f310cd292e92e"
)

# Laedt eine Datei und prueft ihren Digest. Bricht bei Abweichung ab, statt
# weiterzumessen: entweder hat die Quelle eine neue Fassung veroeffentlicht,
# oder die Datei ist unterwegs kaputt gegangen. Beides ist ein Grund, nicht
# weiterzumachen — eine Messung gegen unbekannte Gewichte ist keine Messung.
hole() { # ziel quelle digest name
  local datei=$1 quelle=$2 digest=$3 name=$4 ist
  if [ -f "$datei" ] && [ "$(sha256sum "$datei" | cut -d' ' -f1)" = "$digest" ]; then
    echo "  $name: schon da, Digest stimmt"
    return 0
  fi
  echo "  $name: laedt"
  curl -sSL -o "$datei.teil" "$quelle"
  ist=$(sha256sum "$datei.teil" | cut -d' ' -f1)
  if [ "$ist" != "$digest" ]; then
    rm -f "$datei.teil"
    echo "FEHLER $name: Digest weicht ab." >&2
    echo "  erwartet $digest" >&2
    echo "  bekommen $ist" >&2
    exit 1
  fi
  mv "$datei.teil" "$datei"
}

konfiguration() { # name
  cat <<KONFIG
name: "$1"
platform: "onnxruntime_onnx"
# Feste **Eingabeform**, obwohl das Modell dynamische traegt: die
# Messwerkzeuge lesen die Eingabeform aus den Metadaten und ersetzen nur die
# Batchdimension (gate-m3.rs). Aus vier dynamischen Achsen kaeme keine
# Tensorgroesse zustande.
#
# Die **Ausgaben** behalten dagegen ihre dynamische Batchachse. Sie ebenfalls
# festzunageln laesst Triton das Modell gar nicht erst laden:
#   "the model expects 3 dimensions (shape [-1,300,80]) but the model
#    configuration specifies 3 dimensions (shape [1,300,80])"
# Das ist der erste Stolperstein beim Nachbauen, und er kostet einen
# vollstaendig fehlgeschlagenen Serverstart.
#
# Kein dynamisches Batching: die Warteschlange gehoert vor den Governor
# (ADR-0002).
max_batch_size: 0
input [
  { name: "pixel_values" data_type: TYPE_FP32 dims: [ 1, 3, 640, 640 ] }
]
output [
  { name: "logits" data_type: TYPE_FP32 dims: [ -1, 300, 80 ] },
  { name: "pred_boxes" data_type: TYPE_FP32 dims: [ -1, 300, 4 ] }
]
instance_group [
  {
    count: 1
    kind: KIND_GPU
    # Tritons eigener Rate Limiter mit modelluebergreifender Prioritaet: die
    # Vergleichsseite bekommt das Werkzeug, das Triton fuer dieses Problem
    # anbietet (Spec 19.1). Der Governor tritt nicht gegen einen Strohmann an.
    rate_limiter { priority: 1 }
  }
]
KONFIG
}

herkunft="$ZIEL/MODELS.md"
{
  echo "# Modelle dieses Repositorys"
  echo
  echo "Erzeugt von \`tools/repro/fetch-models.sh\` am $(date -u '+%Y-%m-%d %H:%M UTC')."
  echo "Die Gewichte stammen aus den unten genannten Quellen und liegen"
  echo "ausserhalb der Versionsverwaltung."
  echo
  echo "| Modell | Basismodell | Lizenz | SHA-256 | Quelle |"
  echo "|---|---|---|---|---|"
} > "$herkunft"

echo "Detektoren nach $ZIEL"
for eintrag in "${MODELLE[@]}"; do
  IFS='|' read -r name quelle digest lizenz basis <<<"$eintrag"
  mkdir -p "$ZIEL/$name/1"
  hole "$ZIEL/$name/1/model.onnx" "$quelle" "$digest" "$name"
  konfiguration "$name" > "$ZIEL/$name/config.pbtxt"
  echo "| \`$name\` | $basis | $lizenz | \`${digest:0:16}…\` | $quelle |" >> "$herkunft"
done

if [ "$MIT_LLM" = 1 ]; then
  echo "Sprachmodell nach $ZIEL_LLM"
  gewichte="$ZIEL_LLM/qwen/1/weights"
  mkdir -p "$gewichte"
  for eintrag in "${QWEN_DATEIEN[@]}"; do
    IFS='|' read -r datei digest <<<"$eintrag"
    hole "$gewichte/$datei" "$QWEN_BASIS/$datei" "$digest" "qwen/$datei"
  done

  # Der Speicheranteil ist bewusst klein: das Sprachmodell ist hier die
  # **nachrangige** Last neben dem Detektor und teilt sich die Karte mit ihm.
  # Ein vLLM, das sich 90 % des Speichers nimmt, laesst dem Detektor keinen.
  cat > "$ZIEL_LLM/qwen/1/model.json" <<'MODELJSON'
{
  "model": "/models/qwen/1/weights",
  "gpu_memory_utilization": 0.3,
  "max_model_len": 2048,
  "enforce_eager": false,
  "enable_prefix_caching": true
}
MODELJSON

  cat > "$ZIEL_LLM/qwen/config.pbtxt" <<'QWENCFG'
name: "qwen"
backend: "vllm"
# Das generative Modell als nachrangige Last — der Fall aus Spec 1.3 und 15.1,
# den ADR-0012 als ungeloest markiert hat.
#
# `model_transaction_policy` wird bewusst NICHT gesetzt: das vLLM-Backend legt
# sie in seiner Auto-Konfiguration selbst fest und lehnt eine abweichende
# Vorgabe ab.
max_batch_size: 0
instance_group [ { count: 1, kind: KIND_MODEL } ]
QWENCFG

  cp "$gewichte/LICENSE" "$ZIEL_LLM/qwen/LICENSE"
  echo "| \`qwen\` | Qwen/Qwen3-0.6B | Apache-2.0 | \`f47f71177f32bcd1…\` | $QWEN_BASIS |" >> "$herkunft"
fi

echo
echo "Modellrepository: $ZIEL"
[ "$MIT_LLM" = 1 ] && echo "Sprachmodell:     $ZIEL_LLM"
echo "Herkunft und Lizenzen: $herkunft"
echo
echo "Weiter mit:"
echo "  tools/repro/run.sh $ZIEL"
