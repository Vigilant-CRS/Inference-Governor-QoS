#!/usr/bin/env bash
# Laedt, was vig-tflite-server auf dem Telefon braucht, nach $RUNTIME —
# nie ins Repository (ADR-0039):
#
#   - TFLite 2.16.1 (Kern und GPU-Delegate) aus den AARs von Maven Central,
#     Apache-2.0. 2.17.0 ist dort nur noch ein Verweis auf LiteRT ohne AAR.
#   - drei oeffentliche Modelle mit freier Lizenz, analog zu Gate M3:
#       detector  EfficientDet-Lite0, MediaPipe Object Detector, Apache-2.0
#       pose      Pose Landmarker lite (Landmarkenmodell), MediaPipe, Apache-2.0
#       depth     MiDaS v2.1 small, MIT (isl-org/MiDaS)
#
# Jede Datei wird gegen einen festen SHA-256 geprueft: ein Modell, das sich
# unter derselben Adresse aendert, ist ein anderes Messobjekt.
#
#   tools/android/fetch-assets.sh
#   RUNTIME=/pfad tools/android/fetch-assets.sh
set -euo pipefail

RUNTIME=${RUNTIME:-/run/media/dd/USB_4028/Projekte/InferenceQoS-runtime}
LIBS=$RUNTIME/modelle/android-tflite
MODELS=$RUNTIME/modelle/android-models
mkdir -p "$LIBS" "$MODELS"

fetch() { # Ziel URL SHA-256
  local target=$1 url=$2 sum=$3
  if [ ! -f "$target" ] || ! echo "$sum  $target" | sha256sum -c --status; then
    curl -sfL --max-time 300 -o "$target.part" "$url"
    mv "$target.part" "$target"
  fi
  echo "$sum  $target" | sha256sum -c --quiet || { echo "Pruefsumme falsch: $target" >&2; exit 1; }
}

MAVEN=https://repo1.maven.org/maven2/org/tensorflow
fetch "$LIBS/tensorflow-lite-2.16.1.aar" "$MAVEN/tensorflow-lite/2.16.1/tensorflow-lite-2.16.1.aar" \
  34b065817e294e7dd3569504e8b9454938ceae453b6a459da54d5f6e45d3c94e
fetch "$LIBS/tensorflow-lite-gpu-2.16.1.aar" "$MAVEN/tensorflow-lite-gpu/2.16.1/tensorflow-lite-gpu-2.16.1.aar" \
  4440e44d295e0e13964d4fe9d9e8c3d2c3f870b92fcb7b945e45036acdfcd459
unzip -o -q -j "$LIBS/tensorflow-lite-2.16.1.aar" jni/arm64-v8a/libtensorflowlite_jni.so -d "$LIBS/arm64"
unzip -o -q -j "$LIBS/tensorflow-lite-gpu-2.16.1.aar" jni/arm64-v8a/libtensorflowlite_gpu_jni.so -d "$LIBS/arm64"

fetch "$MODELS/efficientdet_lite0.tflite" \
  https://storage.googleapis.com/mediapipe-models/object_detector/efficientdet_lite0/float32/1/efficientdet_lite0.tflite \
  40338edf5ec70d43e318b0a716a84d4564cd1802759a7a07170c7e43796dbf58
# Derselbe Detektor mit NMS im Graphen (TFLite_Detection_PostProcess, laeuft
# auf der CPU): 25 Boxen statt 19 206 Anker in der Antwort.
fetch "$MODELS/effdet_lite0_nms.tar.gz" \
  https://www.kaggle.com/api/v1/models/tensorflow/efficientdet/tfLite/lite0-detection-metadata/1/download \
  0799da1decd268958a9eb8341aab08eb99dee27b0ffdf9924e10ce428a97e854
tar -xzf "$MODELS/effdet_lite0_nms.tar.gz" -C "$MODELS" 1.tflite
mv -f "$MODELS/1.tflite" "$MODELS/efficientdet_lite0_nms.tflite"
echo "2e04c53bfeac0ac2a30c057c7e2a777594ce39baaac35a92f74fb1e8c4fc4e0b  $MODELS/efficientdet_lite0_nms.tflite" |
  sha256sum -c --quiet
fetch "$MODELS/midas_v21_small.tflite" \
  https://github.com/isl-org/MiDaS/releases/download/v2_1/model_opt.tflite \
  93d871071edff1218973ce25ee27ce95ccd20450c70a55e1b89efa3f5a772cdd
# Pose: das Landmarkenmodell aus dem MediaPipe Pose Landmarker (lite). Nicht
# MoveNet: dort laufen nur 97 von 297 Knoten auf dem GPU-Delegate, der Rest
# auf der CPU — das Modell waere kein GPU-Last-Stellvertreter.
fetch "$MODELS/pose_landmarker_lite.task" \
  https://storage.googleapis.com/mediapipe-models/pose_landmarker/pose_landmarker_lite/float16/1/pose_landmarker_lite.task \
  59929e1d1ee95287735ddd833b19cf4ac46d29bc7afddbbf6753c459690d574a
unzip -o -q "$MODELS/pose_landmarker_lite.task" pose_landmarks_detector.tflite -d "$MODELS"
echo "ad6cfd3c903eb31a4ee788b809e45ecf9fa69923b69b9f3f2d9ae616ff433e58  $MODELS/pose_landmarks_detector.tflite" |
  sha256sum -c --quiet

ls -la "$LIBS/arm64" "$MODELS"
