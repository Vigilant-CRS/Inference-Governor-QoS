#!/usr/bin/env bash
# Vig-Edge-Pilot: Daten fuer den Referenzpiloten vorbereiten.
#
#   tools/pilot/prepare-data.sh [ZIELVERZEICHNIS]
#
# Quelle: MOT16 (MOTChallenge), Lizenz CC BY-NC-SA 3.0. Nur fuer den
# internen Test; weder Bilder noch Annotationen gehoeren ins Repository.
#
# Je Sequenz entstehen drei Dateien:
#   <seq>.rgb      alle Frames hintereinander, 512x512 RGB24, roh
#   <seq>.gt.csv   frame,id,x1,y1,x2,y2,visibility — normiert auf [0,1],
#                  nur Fussgaenger (Klasse 1), die MOT als relevant markiert
#   <seq>.meta     fps frames breite hoehe
#
# Die Frames werden gestreckt, nicht beschnitten: RF-DETR skaliert beim
# Training ebenso quadratisch, und normierte Koordinaten bleiben dabei gleich.
set -euo pipefail

OUT=${1:-${VIG_PILOT_DIR:-pilot}}
SIZE=${SIZE:-512}
# Drei feste Ueberwachungskameras und eine bewegte: stationaere Kameras
# plus eine mobile Einheit.
SEQS=${SEQS:-"MOT16-02 MOT16-04 MOT16-09 MOT16-11"}
URL=https://motchallenge.net/data/MOT16.zip
SHA256=${SHA256:-}

mkdir -p "$OUT"
ZIP="$OUT/MOT16.zip"
if [ ! -f "$ZIP" ]; then
  curl -sSL --fail -o "$ZIP" "$URL"
fi
if [ -n "$SHA256" ]; then
  echo "$SHA256  $ZIP" | sha256sum -c -
fi

for seq in $SEQS; do
  if [ -f "$OUT/$seq.rgb" ] && [ -f "$OUT/$seq.gt.csv" ] && [ -f "$OUT/$seq.meta" ]; then
    echo "$seq: schon vorbereitet"
    continue
  fi
  python3 - "$ZIP" "$OUT" "$seq" <<'PY'
import sys, zipfile, os
zip_path, out, seq = sys.argv[1:4]
z = zipfile.ZipFile(zip_path)
prefix = f"train/{seq}/"
info = dict(l.split("=", 1) for l in z.read(prefix + "seqinfo.ini").decode().splitlines() if "=" in l)
fps, frames = int(info["frameRate"]), int(info["seqLength"])
width, height = int(info["imWidth"]), int(info["imHeight"])
img_dir = os.path.join(out, seq, "img1")
os.makedirs(img_dir, exist_ok=True)
for name in z.namelist():
    if name.startswith(prefix + "img1/") and name.endswith(".jpg"):
        target = os.path.join(img_dir, os.path.basename(name))
        if not os.path.exists(target):
            with open(target, "wb") as f:
                f.write(z.read(name))
rows = []
for line in z.read(prefix + "gt/gt.txt").decode().splitlines():
    # frame, id, left, top, width, height, consider, class, visibility
    f, tid, left, top, w, h, consider, cls, vis = line.split(",")[:9]
    if int(consider) != 1 or int(cls) != 1:
        continue
    x1, y1 = float(left) - 1.0, float(top) - 1.0
    x2, y2 = x1 + float(w), y1 + float(h)
    clamp = lambda v: min(max(v, 0.0), 1.0)
    rows.append((int(f), int(tid), clamp(x1 / width), clamp(y1 / height),
                 clamp(x2 / width), clamp(y2 / height), float(vis)))
rows.sort()
with open(os.path.join(out, f"{seq}.gt.csv"), "w") as g:
    g.write("frame,id,x1,y1,x2,y2,visibility\n")
    for r in rows:
        g.write("%d,%d,%.6f,%.6f,%.6f,%.6f,%.3f\n" % r)
with open(os.path.join(out, f"{seq}.meta"), "w") as m:
    m.write(f"{fps} {frames} {width} {height}\n")
print(f"{seq}: {frames} Frames, {fps} fps, {width}x{height}, {len(rows)} GT-Boxen")
PY
  read -r fps _ _ _ < "$OUT/$seq.meta"
  ffmpeg -loglevel error -y -framerate "$fps" -i "$OUT/$seq/img1/%06d.jpg" \
    -vf "scale=${SIZE}:${SIZE}:flags=bilinear" -pix_fmt rgb24 -f rawvideo "$OUT/$seq.rgb"
  rm -rf "${OUT:?}/$seq"
  echo "$seq: $(stat -c %s "$OUT/$seq.rgb") Bytes Rohframes"
done
