#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Vigilant e.K.
# SPDX-License-Identifier: BUSL-1.1
"""Side-by-side demo renderer: "NVIDIA Triton alone" vs. "with Vigilant".

One clip is shown twice. Each side replays a timeline that was RECORDED on the
GPU (the two arms back to back, same frames, same load). A displayed frame at
clip time t only shows results whose done_ms <= t. Detector boxes are drawn at
their stored coordinates on the CURRENT video frame, so a slow arm shows boxes
lagging behind moving objects. Nothing is smoothed, interpolated or hidden.

Dependencies: python3 standard library + Pillow, and ffmpeg/ffprobe on PATH.
ffmpeg decodes the clip to rgb24 through a pipe, Pillow composes each frame,
a second ffmpeg encodes H.264 (libx264, yuv420p, BT.709).

Usage
-----
    python3 tools/demo/render.py --video clip.mp4 \
        --left direct.jsonl --right governed.jsonl --out demo.mp4 \
        --title "City driving, Kraków" --attribution "Video: ... · RF-DETR ..."

    python3 tools/demo/render.py --selftest /tmp/demo-selftest

Timeline JSONL schema (one file per arm)
----------------------------------------
Line 1 is a header object:

    {"type":"header","arm":"direct"|"governed","label":"NVIDIA Triton alone",
     "source_fps":30,"frames":1200,
     "detector":{"model":"rfdetr_small","period_ms":100,"max_age_ms":200,
                 "input_size":512},
     "vlm":{"model":"smolvlm","prompt":"...","max_tokens":16},
     "gpu":"NVIDIA GeForce RTX 3070 Laptop GPU",
     "recorded_at":"ISO8601","commit":"abc1234"}

  Required: type=="header", arm in {"direct","governed"}, source_fps > 0,
  detector.period_ms > 0, detector.max_age_ms > 0. Everything else is optional
  (label defaults to "NVIDIA Triton alone" / "with Vigilant" by arm).

Then one object per finished (or refused/failed) request, any order:

  detector:
    {"type":"detector","frame":123,"capture_ms":4100.0,"send_ms":4101.2,
     "done_ms":4135.9,"status":"ok","variant":"small",
     "boxes":[[cls_id,score,x0,y0,x1,y1],...]}

    Box coordinates are normalized 0..1 relative to the source frame. cls_id is
    the COCO 91-index (1 person, 2 bicycle, 3 car, 4 motorcycle, 6 bus,
    8 truck, 10 traffic light, 13 stop sign; others get a generic label).
    status is "ok", "refused:superseded", "refused:stale",
    "refused:infeasible" or "error:<text>"; non-ok entries carry no boxes.
    "variant" may be absent.

  vlm:
    {"type":"vlm","frame":90,"capture_ms":3000.0,"send_ms":3000.5,
     "done_ms":3180.2,"status":"ok","text":"A person is crossing the street."}

  Times are milliseconds since the start of the clip's frame 0 on the
  recording clock (capture_ms of frame k is k*1000/source_fps). If capture_ms
  is missing but frame is present, capture_ms = frame*1000/source_fps.
  done_ms is required. Unknown fields and unknown types are ignored, malformed
  lines are skipped with a warning.

Rendering rules (as implemented)
--------------------------------
* Output frame k shows clip time t = start_ms + k*1000/fps.
* Current detection = among status=="ok" results with done_ms <= t, the one
  with the newest capture_ms (a later-arriving older result never replaces a
  newer one). age = t - capture_ms. Boxes with score < 0.5 are hidden.
* Badge: green if age <= max_age/2, amber if <= max_age, red above.
  BLIND if age > max_age or no result yet: video dimmed to 45 %, red frame,
  banner "BLIND — last detection N ms old". Exception: during the first
  max_age_ms of the clip (t < max_age_ms) an arm without any result is shown
  as "waiting for first detection" instead, identically for both arms.
* Evaluation starts at E = max(start_ms, max_age_ms) (same grace for both).
  "fresh cycles" = share of sampling instants E, E+period, E+2*period, ... <= t
  at which the current detection had age <= max_age.
  "longest blind gap" = longest continuous interval in [E, t] with age >
  max_age (or no result).
  "language model answers" = ok vlm results with start_ms <= done_ms <= t.
* The final 2 s show a summary card per side with the same three numbers,
  evaluated at the last rendered frame.
"""
from __future__ import annotations

import argparse
import bisect
import json
import math
import multiprocessing
import os
import random
import shutil
import subprocess
import sys
import time

from PIL import Image, ImageDraw, ImageFont

# --------------------------------------------------------------------------
# Style
# --------------------------------------------------------------------------
FONT_DIR = "/usr/share/fonts/truetype/dejavu"
BG = (11, 15, 20)            # #0b0f14
CARD = (18, 24, 32)
CARD_EDGE = (32, 42, 54)
DIVIDER = (30, 38, 48)
TEXT = (236, 240, 244)
SUBTLE = (160, 170, 182)
FAINT = (104, 114, 126)
ACCENT = (74, 158, 255)      # #4a9eff  Vigilant side
RED = (255, 77, 77)          # #ff4d4d  BLIND
AMBER = (255, 176, 32)       # #ffb020
GREEN = (61, 220, 132)       # #3ddc84
DARK_TEXT = (8, 11, 15)

# Box colors by class family; deliberately not red/amber/green (status colors).
FAMILY_COLOR = {"people": (255, 122, 198), "vehicles": (64, 208, 224),
                "other": (200, 182, 255)}
PEOPLE = {1}
VEHICLES = {2, 3, 4, 5, 6, 7, 8, 9}
COCO_NAMES = {1: "person", 2: "bicycle", 3: "car", 4: "motorcycle",
              5: "airplane", 6: "bus", 7: "train", 8: "truck", 9: "boat",
              10: "traffic light", 11: "fire hydrant", 13: "stop sign",
              14: "parking meter", 15: "bench", 16: "bird", 17: "cat",
              18: "dog", 27: "backpack", 28: "umbrella", 31: "handbag"}
MIN_SCORE = 0.5
TYPEWRITER_MS = 400.0
# The caption card changes at most this often, so fast answers stay readable.
CAPTION_HOLD_MS = 1500.0
SUMMARY_MS = 2000.0
DIM_FACTOR = 0.45
DEFAULT_LABEL = {"direct": "NVIDIA Triton alone", "governed": "with Vigilant"}
HONEST_LINE = ("Recorded back to back on one GPU with the same frames and the "
               "same load. Boxes are drawn where the detector saw them.")


def warn(msg: str) -> None:
    print(f"render.py: warning: {msg}", file=sys.stderr)


# --------------------------------------------------------------------------
# Timelines
# --------------------------------------------------------------------------
class HeaderError(ValueError):
    pass


def _num(v) -> float | None:
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return None
    v = float(v)
    return v if math.isfinite(v) else None


def validate_header(h: dict, path: str) -> dict:
    def fail(why):
        raise HeaderError(f"{path}: invalid header: {why}")
    if not isinstance(h, dict) or h.get("type") != "header":
        fail('first line must be an object with "type":"header"')
    if h.get("arm") not in ("direct", "governed"):
        fail('"arm" must be "direct" or "governed"')
    if not (_num(h.get("source_fps")) or 0) > 0:
        fail('"source_fps" must be a positive number')
    det = h.get("detector")
    if not isinstance(det, dict):
        fail('"detector" object missing')
    for key in ("period_ms", "max_age_ms"):
        if not (_num(det.get(key)) or 0) > 0:
            fail(f'"detector.{key}" must be a positive number')
    if not isinstance(h.get("vlm", {}), dict):
        fail('"vlm" must be an object')
    return h


class Timeline:
    """One arm: parsed header plus ok results in arrival order."""

    def __init__(self, path: str | None, side: str):
        self.path = path
        self.side = side
        self.header: dict | None = None
        self.det: list[dict] = []      # ok detector results
        self.vlm: list[dict] = []      # ok vlm results
        self.det_seen = 0              # detector entries of any status
        self.vlm_seen = 0
        self.not_ok: dict[str, int] = {}
        self.skipped = 0
        self.no_data = True
        if not path or not os.path.isfile(path):
            if path:
                warn(f"{side} timeline {path!r} not found, rendering 'no data'")
            return
        with open(path, encoding="utf-8") as fh:
            lines = [ln for ln in fh if ln.strip()]
        if not lines:
            warn(f"{side} timeline {path!r} is empty, rendering 'no data'")
            return
        try:
            header = json.loads(lines[0])
        except json.JSONDecodeError as exc:
            raise HeaderError(f"{path}: header line is not JSON: {exc}") from exc
        self.header = validate_header(header, path)
        fps = float(header["source_fps"])
        for n, line in enumerate(lines[1:], start=2):
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                self.skipped += 1
                continue
            if not isinstance(e, dict) or e.get("type") not in ("detector", "vlm"):
                continue
            cap = _num(e.get("capture_ms"))
            if cap is None and _num(e.get("frame")) is not None:
                cap = _num(e.get("frame")) * 1000.0 / fps
            done = _num(e.get("done_ms"))
            status = e.get("status")
            if cap is None or done is None or not isinstance(status, str):
                self.skipped += 1
                continue
            if e["type"] == "detector":
                self.det_seen += 1
            else:
                self.vlm_seen += 1
            if status != "ok":
                key = f'{e["type"]} {status.split(":", 1)[0] if status.startswith("error") else status}'
                self.not_ok[key] = self.not_ok.get(key, 0) + 1
                continue
            if e["type"] == "detector":
                boxes = []
                for b in e.get("boxes") or []:
                    if (isinstance(b, (list, tuple)) and len(b) >= 6
                            and all(_num(v) is not None for v in b[:6])):
                        boxes.append((int(b[0]), float(b[1]), *map(float, b[2:6])))
                self.det.append({"capture": cap, "done": done, "boxes": boxes})
            else:
                text = e.get("text")
                if not isinstance(text, str):
                    text = ""
                self.vlm.append({"capture": cap, "done": done,
                                 "text": " ".join(text.split())})
        if self.skipped:
            warn(f"{path}: skipped {self.skipped} malformed line(s)")
        self.det.sort(key=lambda r: (r["done"], r["capture"]))
        self.vlm.sort(key=lambda r: (r["done"], r["capture"]))
        self.no_data = self.det_seen == 0 and self.vlm_seen == 0

    # header accessors with defaults -------------------------------------
    def hget(self, *keys, default=None):
        node = self.header or {}
        for k in keys:
            if not isinstance(node, dict) or k not in node:
                return default
            node = node[k]
        return node

    @property
    def arm(self) -> str:
        return self.hget("arm", default="direct" if self.side == "left" else "governed")

    @property
    def label(self) -> str:
        lab = self.hget("label")
        return lab if isinstance(lab, str) and lab.strip() else DEFAULT_LABEL[self.arm]


def newest_capture_index(results: list[dict]):
    """Change points of 'newest capture that has arrived' over time."""
    times, picks = [], []
    best = None
    for r in results:                       # sorted by (done, capture)
        if best is None or r["capture"] > best["capture"]:
            best = r
            if times and times[-1] == r["done"]:
                picks[-1] = r
            else:
                times.append(r["done"])
                picks.append(r)
    return times, picks


class ArmState:
    """Precomputed, stateless per-time queries for one arm."""

    def __init__(self, tl: Timeline, max_age: float, period: float,
                 start_ms: float, end_ms: float):
        self.tl = tl
        self.max_age = max_age
        self.period = period
        self.start_ms = start_ms
        self.eval_start = max(start_ms, max_age)
        self.det_t, self.det_r = newest_capture_index(tl.det)
        self.vlm_t, self.vlm_r = newest_capture_index(tl.vlm)
        self.vlm_done = [r["done"] for r in tl.vlm]
        self._build_blind_intervals()
        self._build_cycles(end_ms)

    def detection_at(self, t: float):
        i = bisect.bisect_right(self.det_t, t) - 1
        return self.det_r[i] if i >= 0 else None

    def vlm_at(self, t: float):
        i = bisect.bisect_right(self.vlm_t, t) - 1
        return self.vlm_r[i] if i >= 0 else None

    def vlm_answers(self, t: float) -> int:
        return max(0, bisect.bisect_right(self.vlm_done, t)
                   - bisect.bisect_left(self.vlm_done, self.start_ms))

    def _build_blind_intervals(self):
        raw = []
        if not self.det_t:
            raw.append((-math.inf, math.inf))
        else:
            raw.append((-math.inf, self.det_t[0]))
            for i, r in enumerate(self.det_r):
                seg_end = self.det_t[i + 1] if i + 1 < len(self.det_t) else math.inf
                stale_at = max(self.det_t[i], r["capture"] + self.max_age)
                if stale_at < seg_end:
                    raw.append((stale_at, seg_end))
        merged = []
        for a, b in raw:
            a = max(a, self.eval_start)
            if b <= a:
                continue
            if merged and a <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(merged[-1][1], b))
            else:
                merged.append((a, b))
        self.blind = merged
        self.blind_starts = [a for a, _ in merged]
        self.blind_prefmax = []
        m = 0.0
        for a, b in merged:
            m = max(m, b - a)
            self.blind_prefmax.append(m)

    def longest_blind(self, t: float) -> float:
        j = bisect.bisect_left(self.blind_starts, t)   # intervals starting before t
        if j == 0:
            return 0.0
        a, b = self.blind[j - 1]
        cur = min(b, t) - a
        return max(cur, self.blind_prefmax[j - 2] if j >= 2 else 0.0)

    def _build_cycles(self, end_ms: float):
        self.cycle_prefix = [0]
        s = self.eval_start
        while s <= end_ms + 1e-6:
            r = self.detection_at(s)
            fresh = r is not None and s - r["capture"] <= self.max_age
            self.cycle_prefix.append(self.cycle_prefix[-1] + (1 if fresh else 0))
            s += self.period

    def fresh_share(self, t: float):
        if t < self.eval_start:
            return None
        n = min(int((t - self.eval_start) // self.period) + 1, len(self.cycle_prefix) - 1)
        return self.cycle_prefix[n] / n if n > 0 else None


# --------------------------------------------------------------------------
# Drawing helpers
# --------------------------------------------------------------------------
class Fonts:
    def __init__(self, s):
        cache = {}

        def f(name, size):
            key = (name, max(8, round(size * s)))
            if key not in cache:
                cache[key] = ImageFont.truetype(os.path.join(FONT_DIR, name), key[1])
            return cache[key]
        R, B, I = "DejaVuSans.ttf", "DejaVuSans-Bold.ttf", "DejaVuSans-Oblique.ttf"
        self.title = f(B, 32)
        self.info = f(R, 17)
        self.clock = f(R, 20)
        self.side = f(B, 27)
        self.badge = f(B, 18)
        self.box = f(B, 14)
        self.counter = f(R, 18)
        self.counter_b = f(B, 18)
        self.card_head = f(B, 15)
        self.card_age = f(R, 17)
        self.prompt = f(I, 16)
        self.caption = f(R, 30)
        self.caption_wait = f(I, 26)
        self.blind = f(B, 60)
        self.blind_sub = f(R, 26)
        self.footer = f(R, 19)
        self.attrib = f(R, 15)
        self.sum_head = f(B, 20)
        self.sum_value = f(B, 58)
        self.sum_label = f(R, 19)


def fit_font(font_path: str, text: str, size: int, max_w: float):
    """Largest font <= size whose rendering of text fits max_w."""
    while size > 9:
        font = ImageFont.truetype(font_path, size)
        if font.getlength(text) <= max_w:
            return font
        size -= 1
    return ImageFont.truetype(font_path, size)


def ellipsize(font, text: str, max_w: float) -> str:
    if font.getlength(text) <= max_w:
        return text
    while text and font.getlength(text + "…") > max_w:
        text = text[:-1]
    return text.rstrip() + "…"


def rounded_mask(size, radius) -> Image.Image:
    m = Image.new("L", size, 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, size[0] - 1, size[1] - 1], radius, fill=255)
    return m


def age_color(age: float, max_age: float):
    if age <= max_age / 2:
        return GREEN
    if age <= max_age:
        return AMBER
    return RED


# --------------------------------------------------------------------------
# Renderer
# --------------------------------------------------------------------------
class Renderer:
    def __init__(self, cfg: dict):
        self.cfg = cfg
        W, H = cfg["width"], cfg["height"]
        self.W, self.H = W, H
        s = min(W / 1920, H / 1080)
        self.s = s
        ox, oy = (W - 1920 * s) / 2, (H - 1080 * s) / 2
        self.X = lambda v: int(round(ox + v * s))    # layout coords (1920x1080 design)
        self.Y = lambda v: int(round(oy + v * s))
        self.S = lambda v: max(1, int(round(v * s)))
        self.f = Fonts(s)

        # video geometry: 928x522 (16:9) per side, even dimensions
        self.VW = self.S(928) // 2 * 2
        self.VH = self.VW * 9 // 16 // 2 * 2
        self.vx = [self.X(20), self.X(972)]
        self.vy = self.Y(140)
        self.video_mask = rounded_mask((self.VW, self.VH), self.S(10))
        self.dim_lut = [int(i * DIM_FACTOR) for i in range(256)] * 3
        self.summary_lut = [int(i * 0.28) for i in range(256)] * 3
        self.shade_lut = [int(i * 0.35) for i in range(256)] * 3
        self._masks: dict = {}
        self._wrap_cache: dict = {}

        self.fps = cfg["fps"]
        self.start_ms = cfg["start_s"] * 1000.0
        self.nframes = cfg["nframes"]
        self.end_ms = self.start_ms + (self.nframes - 1) * 1000.0 / self.fps

        self.tl = [Timeline(cfg["left"], "left"), Timeline(cfg["right"], "right")]
        ref = next((t for t in self.tl if t.header), None)
        self.max_age = float(cfg["max_age_ms"] or
                             (ref.hget("detector", "max_age_ms") if ref else 200))
        self.period = float(ref.hget("detector", "period_ms") if ref else 100)
        self.arms = [ArmState(t, self.max_age, self.period, self.start_ms, self.end_ms)
                     for t in self.tl]
        self.bg = self._static_layer()

    # -- static layer ------------------------------------------------------
    def _static_layer(self) -> Image.Image:
        X, Y, S, f, cfg = self.X, self.Y, self.S, self.f, self.cfg
        img = Image.new("RGB", (self.W, self.H), BG)
        d = ImageDraw.Draw(img)
        ref = next((t for t in self.tl if t.header), None)

        # header bar
        d.text((X(20), Y(36)), cfg["title"], font=f.title, fill=TEXT, anchor="lm")
        if ref:
            det_model = ref.hget("detector", "model", default="detector")
            vlm_model = ref.hget("vlm", "model", default="language model")
            gpu = ref.hget("gpu", default="one GPU")
            info = (f"{det_model} every {self.period:g} ms, blind when older than "
                    f"{self.max_age:g} ms  ·  {vlm_model} alongside, best effort  ·  {gpu}")
            d.text((X(20), Y(68)), ellipsize(f.info, info, X(1600) - X(20)),
                   font=f.info, fill=SUBTLE, anchor="lm")
        d.line([(X(20), Y(92)), (X(1900), Y(92))], fill=DIVIDER, width=S(1))

        for i, tl in enumerate(self.tl):
            vx = self.vx[i]
            color = ACCENT if tl.arm == "governed" else TEXT
            d.text((vx + S(2), Y(117)), ellipsize(f.side, tl.label, S(560)),
                   font=f.side, fill=color, anchor="lm")
            # video placeholder edge + caption card
            d.rounded_rectangle([vx - S(1), self.vy - S(1), vx + self.VW, self.vy + self.VH],
                                S(11), outline=CARD_EDGE, width=S(1))
            cy0, cy1 = Y(710), Y(930)
            d.rounded_rectangle([vx, cy0, vx + self.VW - 1, cy1], S(12),
                                fill=CARD, outline=CARD_EDGE, width=S(1))
            vlm_model = tl.hget("vlm", "model", default=None) or (
                ref.hget("vlm", "model", default="") if ref else "")
            head = "LANGUAGE MODEL" + (f"  ·  {vlm_model}" if vlm_model else "")
            d.text((vx + S(22), cy0 + S(26)), head, font=f.card_head, fill=FAINT, anchor="lm")
            prompt = tl.hget("vlm", "prompt", default=None) or (
                ref.hget("vlm", "prompt", default=None) if ref else None)
            if isinstance(prompt, str) and prompt.strip():
                p = ellipsize(f.prompt, f"asked: “{' '.join(prompt.split())}”", self.VW - S(44))
                d.text((vx + S(22), cy0 + S(54)), p, font=f.prompt, fill=FAINT, anchor="lm")

        # footer
        d.line([(X(20), Y(956)), (X(1900), Y(956))], fill=DIVIDER, width=S(1))
        legend = [("people", FAMILY_COLOR["people"]), ("vehicles", FAMILY_COLOR["vehicles"]),
                  ("other", FAMILY_COLOR["other"])]
        lx = X(1900)
        items = []
        for name, col in reversed(legend):
            w = f.footer.getlength(name)
            lx -= w
            items.append((lx, name, col))
            lx -= S(22) + S(26)
        honest_w = lx - X(20) - S(30)
        d.text((X(20), Y(988)), ellipsize(f.footer, HONEST_LINE, honest_w),
               font=f.footer, fill=SUBTLE, anchor="lm")
        for x, name, col in items:
            d.rounded_rectangle([x - S(22), Y(988) - S(7), x - S(8), Y(988) + S(7)],
                                S(3), outline=col, width=S(2))
            d.text((x, Y(988)), name, font=f.footer, fill=SUBTLE, anchor="lm")
        if cfg["attribution"]:
            af = fit_font(os.path.join(FONT_DIR, "DejaVuSans.ttf"), cfg["attribution"],
                          S(15), X(1900) - X(20))
            d.text((X(20), Y(1022)), ellipsize(af, cfg["attribution"], X(1900) - X(20)),
                   font=af, fill=FAINT, anchor="lm")
        return img

    # -- helpers -----------------------------------------------------------
    def mask(self, w, h, r):
        key = (w, h, r)
        if key not in self._masks:
            self._masks[key] = rounded_mask((w, h), r)
        return self._masks[key]

    def shade(self, img, box, radius, lut=None):
        """Darken a rounded region of img in place (a cheap translucent panel)."""
        x0, y0, x1, y1 = box
        region = img.crop(box).point(lut or self.shade_lut)
        img.paste(region, (x0, y0), self.mask(x1 - x0, y1 - y0, radius))

    def pill(self, img, d, x_right, cy, text, font, color, dot=True):
        S = self.S
        tw = font.getlength(text)
        h = S(34)
        w = int(tw + S(26) + (S(20) if dot else 0))
        x0 = x_right - w
        d.rounded_rectangle([x0, cy - h // 2, x_right, cy + h // 2], h // 2, fill=CARD,
                            outline=CARD_EDGE, width=S(1))
        tx = x0 + S(13)
        if dot:
            r = S(5)
            d.ellipse([tx, cy - r, tx + 2 * r, cy + r], fill=color)
            tx += S(20)
        d.text((tx, cy), text, font=font, fill=color, anchor="lm")

    def wrap(self, text, font, max_w, max_lines):
        key = (text, id(font), max_w, max_lines)
        if key in self._wrap_cache:
            return self._wrap_cache[key]
        lines, cur = [], ""
        words = text.split(" ")
        for i, word in enumerate(words):
            cand = f"{cur} {word}".strip()
            if font.getlength(cand) <= max_w or not cur:
                cur = cand
                continue
            lines.append(cur)
            cur = word
            if len(lines) == max_lines:
                cur = ""
                lines[-1] = ellipsize(font, lines[-1] + " " + " ".join(words[i:]), max_w)
                break
        if cur:
            lines.append(ellipsize(font, cur, max_w))
        if len(self._wrap_cache) > 512:
            self._wrap_cache.clear()
        self._wrap_cache[key] = lines
        return lines

    # -- per frame ---------------------------------------------------------
    def compose(self, k: int, frame: bytes) -> bytes:
        t = self.start_ms + k * 1000.0 / self.fps
        summary = (self.nframes >= 4 * self.fps and
                   k >= self.nframes - round(SUMMARY_MS / 1000.0 * self.fps))
        img = self.bg.copy()
        d = ImageDraw.Draw(img)
        video = Image.frombuffer("RGB", (self.VW, self.VH), frame, "raw", "RGB", 0, 1)
        for i in range(2):
            self.draw_side(img, d, i, video, t, summary)
        d.text((self.X(1900), self.Y(36)), f"{t / 1000.0:5.1f} s", font=self.f.clock,
               fill=FAINT, anchor="rm")
        return img.tobytes()

    def draw_side(self, img, d, i, video, t, summary):
        S, f = self.S, self.f
        arm, tl = self.arms[i], self.tl[i]
        vx, vy, VW, VH = self.vx[i], self.vy, self.VW, self.VH
        badge_cy = self.Y(117)
        right = vx + VW

        det = None if tl.no_data else arm.detection_at(t)
        age = t - det["capture"] if det else None
        if tl.no_data:
            state = "nodata"
        elif det is None:
            state = "blind" if t >= self.max_age else "waiting"
        else:
            state = "blind" if age > self.max_age else "live"

        # video (dimmed when blind / no data / summary)
        if summary:
            panel = video.point(self.summary_lut)
        elif state in ("blind", "nodata"):
            panel = video.point(self.dim_lut)
        else:
            panel = video
        img.paste(panel, (vx, vy), self.video_mask)

        # badge
        if state == "nodata":
            self.pill(img, d, right, badge_cy, "no data", f.badge, FAINT)
        elif state == "waiting":
            self.pill(img, d, right, badge_cy, "waiting for first detection", f.badge, SUBTLE)
        elif det is None:
            self.pill(img, d, right, badge_cy, "no detection yet", f.badge, RED)
        else:
            self.pill(img, d, right, badge_cy, f"detection {age:.0f} ms old", f.badge,
                      age_color(age, self.max_age))

        banner = None
        if summary:
            self.draw_summary(img, d, i, vx, vy)
        elif state == "blind":
            d.rounded_rectangle([vx - S(3), vy - S(3), right + S(2), vy + VH + S(2)],
                                S(12), outline=RED, width=S(4))
            sub = (f"last detection {age:.0f} ms old" if det else "no detection yet")
            banner = self.banner(img, d, vx, vy, "BLIND", sub, RED)
        elif state == "nodata":
            banner = self.banner(img, d, vx, vy, "NO DATA", "timeline missing or empty", SUBTLE)

        # detections at their stored coordinates (this is where lag shows);
        # drawn last so a stale "ghost" box is never hidden behind the banner
        if det and not summary:
            self.draw_boxes(img, d, det["boxes"], vx, vy, VW, VH, avoid=banner)

        # counters freeze on the final numbers while the summary is shown
        self.draw_counters(d, i, self.end_ms if summary else t, vx)
        self.draw_caption(img, d, i, t, vx)

    def draw_boxes(self, img, d, boxes, vx, vy, VW, VH, avoid=None):
        S, font = self.S, self.f.box
        lab_h = S(20)

        def hits(x0, y0, x1, y1):
            return (avoid is not None and x0 < avoid[2] and x1 > avoid[0]
                    and y0 < avoid[3] and y1 > avoid[1])
        for cls, score, x0, y0, x1, y1 in boxes:
            if score < MIN_SCORE:
                continue
            fam = "people" if cls in PEOPLE else "vehicles" if cls in VEHICLES else "other"
            col = FAMILY_COLOR[fam]
            bx0 = vx + max(0.0, min(1.0, min(x0, x1))) * VW
            bx1 = vx + max(0.0, min(1.0, max(x0, x1))) * VW
            by0 = vy + max(0.0, min(1.0, min(y0, y1))) * VH
            by1 = vy + max(0.0, min(1.0, max(y0, y1))) * VH
            if bx1 - bx0 < 2 or by1 - by0 < 2:
                continue
            r = int(min(S(5), (bx1 - bx0) / 2, (by1 - by0) / 2))
            # dark halo first, so thin lines stay readable on bright footage
            d.rounded_rectangle([bx0 - 1, by0 - 1, bx1 + 1, by1 + 1], r + 1,
                                outline=(0, 0, 0), width=S(4))
            d.rounded_rectangle([bx0, by0, bx1, by1], r, outline=col, width=S(2))
            label = f"{COCO_NAMES.get(cls, 'object')} {score:.2f}"
            lw = font.getlength(label) + S(12)
            lx = min(bx0, vx + VW - lw - S(2))
            # label above the box; inside the top edge if no room or it would
            # cover the banner; above the banner as a last resort
            ly = by0 - lab_h - S(2)
            if ly < vy + S(2) or hits(lx, ly, lx + lw, ly + lab_h):
                ly = by0 + S(3)
            if hits(lx, ly, lx + lw, ly + lab_h):
                ly = avoid[1] - lab_h - S(4)
            d.rounded_rectangle([lx, ly, lx + lw, ly + lab_h], S(4), fill=col)
            d.text((lx + S(6), ly + lab_h / 2), label, font=font, fill=DARK_TEXT, anchor="lm")

    def banner(self, img, d, vx, vy, big, sub, color):
        """One-row banner in the bottom strip of the video (mostly road surface).

        Returns its rectangle so box labels can move out of the way; the boxes
        themselves are drawn afterwards, on top, and are never hidden.
        """
        S, f = self.S, self.f
        gap = S(24)
        bw, sw = f.blind.getlength(big), f.blind_sub.getlength(sub)
        w = int(bw + gap + sw + S(64))
        h = S(84)
        cx, cy = vx + self.VW // 2, vy + self.VH - S(66)
        box = (cx - w // 2, cy - h // 2, cx - w // 2 + w, cy - h // 2 + h)
        self.shade(img, box, S(14))
        d.rounded_rectangle(box, S(14), outline=color, width=S(2))
        x = box[0] + S(32)
        d.text((x, cy + S(2)), big, font=f.blind, fill=color, anchor="lm")
        d.text((x + bw + gap, cy + S(4)), sub, font=f.blind_sub, fill=TEXT, anchor="lm")
        return box

    def stats(self, i, t):
        arm, tl = self.arms[i], self.tl[i]
        if tl.no_data:
            return None, None, None
        return arm.fresh_share(t), arm.longest_blind(t), arm.vlm_answers(t)

    def draw_counters(self, d, i, t, vx):
        S, f = self.S, self.f
        share, gap, answers = self.stats(i, t)
        segs = [("fresh cycles ", f.counter, FAINT),
                ("—" if share is None else f"{share * 100:.0f} %", f.counter_b, TEXT),
                ("      longest blind gap ", f.counter, FAINT),
                ("—" if gap is None else f"{gap:.0f} ms", f.counter_b, TEXT),
                ("      language model answers ", f.counter, FAINT),
                ("—" if answers is None else f"{answers}", f.counter_b, TEXT)]
        x, cy = vx + S(4), self.Y(686)
        for text, font, col in segs:
            d.text((x, cy), text, font=font, fill=col, anchor="lm")
            x += font.getlength(text)

    def draw_caption(self, img, d, i, t, vx):
        S, f = self.S, self.f
        arm, tl = self.arms[i], self.tl[i]
        cy0 = self.Y(710)
        right = vx + self.VW - S(22)
        # Answers can arrive every ~120 ms; restarting the typewriter each time
        # leaves one letter on screen. The card therefore changes at most every
        # CAPTION_HOLD_MS: it shows the newest answer that had arrived by the
        # last hold boundary. The age line still uses the real capture time.
        t_hold = self.start_ms + ((t - self.start_ms) // CAPTION_HOLD_MS) * CAPTION_HOLD_MS
        r = None if tl.no_data else arm.vlm_at(t_hold)
        text_x, first_cy, line_h = vx + S(22), cy0 + S(102), S(41)
        if tl.no_data:
            d.text((text_x, first_cy), "no data", font=f.caption_wait, fill=FAINT, anchor="lm")
            return
        if r is None:
            d.text((text_x, first_cy), "waiting for the language model",
                   font=f.caption_wait, fill=FAINT, anchor="lm")
            return
        age_s = (t - r["capture"]) / 1000.0
        d.text((right, cy0 + S(26)), f"describes a frame {age_s:.1f} s old",
               font=f.card_age, fill=SUBTLE, anchor="rm")
        lines = self.wrap(r["text"] or "(empty answer)", f.caption, self.VW - S(44), 3)
        reveal = t - max(r["done"], t_hold)
        if reveal < TYPEWRITER_MS:
            budget = int(sum(len(ln) for ln in lines) * max(0.0, reveal) / TYPEWRITER_MS)
        else:
            budget = 1 << 30
        for n, line in enumerate(lines):
            if budget <= 0:
                break
            d.text((text_x, first_cy + n * line_h), line[:budget], font=f.caption,
                   fill=TEXT, anchor="lm")
            budget -= len(line)

    def draw_summary(self, img, d, i, vx, vy):
        S, f, tl = self.S, self.f, self.tl[i]
        share, gap, answers = self.stats(i, self.end_ms)
        cx, cy = vx + self.VW // 2, vy + self.VH // 2
        dur = (self.end_ms - self.start_ms + 1000.0 / self.fps) / 1000.0
        color = ACCENT if tl.arm == "governed" else TEXT
        d.rounded_rectangle([vx + S(36), vy + S(70), vx + self.VW - S(36), vy + self.VH - S(70)],
                            S(16), fill=CARD, outline=CARD_EDGE, width=S(1))
        head = tl.label
        tail = f"   ·   whole clip, {dur:.1f} s"
        hw = f.sum_head.getlength(head) + f.sum_label.getlength(tail)
        hx = cx - hw / 2
        d.text((hx, cy - S(128)), head, font=f.sum_head, fill=color, anchor="lm")
        d.text((hx + f.sum_head.getlength(head), cy - S(128)), tail, font=f.sum_label,
               fill=SUBTLE, anchor="lm")
        d.line([(vx + S(80), cy - S(96)), (vx + self.VW - S(80), cy - S(96))],
               fill=CARD_EDGE, width=S(1))
        cols = [("—" if share is None else f"{share * 100:.0f} %", "fresh cycles"),
                ("—" if gap is None else f"{gap:.0f} ms", "longest blind gap"),
                ("—" if answers is None else str(answers), "language model answers")]
        inner_x, inner_w = vx + S(36), self.VW - S(72)
        col_w = inner_w / 3
        for n, (value, label) in enumerate(cols):
            x = inner_x + col_w * n + col_w / 2
            d.text((x, cy - S(18)), value, font=f.sum_value, fill=TEXT, anchor="mm")
            d.text((x, cy + S(44)), label, font=f.sum_label, fill=SUBTLE, anchor="mm")
        foot = (f"detector sampled every {self.period:g} ms  ·  "
                f"fresh = at most {self.max_age:g} ms old")
        d.text((cx, cy + S(124)), foot, font=f.sum_label, fill=FAINT, anchor="mm")


# --------------------------------------------------------------------------
# ffmpeg plumbing
# --------------------------------------------------------------------------
def need(tool):
    if not shutil.which(tool):
        sys.exit(f"render.py: {tool} not found on PATH")


def probe(video: str) -> tuple[float, float]:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
         "stream=r_frame_rate:format=duration", "-of", "json", video],
        check=True, capture_output=True, text=True).stdout
    info = json.loads(out)
    num, den = info["streams"][0]["r_frame_rate"].split("/")
    return float(info["format"]["duration"]), float(num) / float(den)


def read_exact(stream, n: int) -> bytes:
    """Read n bytes from an unbuffered pipe (short only at EOF)."""
    chunks, got = [], 0
    while got < n:
        chunk = stream.read(n - got)
        if not chunk:
            break
        chunks.append(chunk)
        got += len(chunk)
    return b"".join(chunks)


_RENDERER: Renderer | None = None


def _work(item):
    k, frame = item
    return _RENDERER.compose(k, frame)


def render(cfg: dict) -> dict:
    global _RENDERER
    need("ffmpeg")
    need("ffprobe")
    duration, src_fps = probe(cfg["video"])
    fps = cfg["fps"]
    avail = max(0.0, duration - cfg["start_s"])
    dur = min(avail, cfg["duration_s"]) if cfg["duration_s"] else avail
    cfg["nframes"] = max(1, int(round(dur * fps)))
    _RENDERER = r = Renderer(cfg)
    for tl in r.tl:
        hfps = tl.hget("source_fps")
        if hfps and abs(float(hfps) - src_fps) > 0.01:
            warn(f"{tl.path}: source_fps {hfps} differs from the clip's {src_fps:g} fps")
    ages = {tl.hget("detector", "max_age_ms") for tl in r.tl if tl.header}
    if len(ages) > 1 and not cfg["max_age_ms"]:
        warn(f"headers disagree on max_age_ms {sorted(ages)}; using {r.max_age:g}")
    gpus = {tl.hget("gpu") for tl in r.tl if tl.header}
    if len(gpus) > 1:
        warn(f"headers name different GPUs: {sorted(map(str, gpus))}")

    VW, VH = r.VW, r.VH
    dec = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-nostdin", "-ss", f"{cfg['start_s']:.3f}", "-i", cfg["video"],
         "-t", f"{dur:.3f}", "-an",
         "-vf", f"fps={fps},scale={VW}:{VH}:flags=lanczos",
         "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
        stdout=subprocess.PIPE, bufsize=0)
    enc = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24",
         "-s", f"{r.W}x{r.H}", "-r", str(fps), "-i", "-",
         "-vf", "scale=out_color_matrix=bt709:out_range=tv",
         "-c:v", "libx264", "-preset", cfg["preset"], "-crf", "18", "-pix_fmt", "yuv420p",
         "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709",
         "-movflags", "+faststart", cfg["out"]],
        stdin=subprocess.PIPE)

    fsize = VW * VH * 3
    frames_done = 0
    last = None
    t0 = time.time()
    next_report = t0 + 2.0

    def frames():
        nonlocal last
        for k in range(cfg["nframes"]):
            buf = read_exact(dec.stdout, fsize)
            if len(buf) == fsize:
                last = buf
            elif last is None:
                raise SystemExit("render.py: could not decode any frame from the clip")
            yield k, last        # pad with the last frame if the decoder ends early

    jobs = max(1, cfg["jobs"])
    pool = None
    if jobs > 1:
        try:
            pool = multiprocessing.get_context("fork").Pool(jobs)
        except ValueError:
            pool = None
    try:
        it = frames()
        while True:
            batch = []
            for item in it:
                batch.append(item)
                if len(batch) >= jobs * 4:
                    break
            if not batch:
                break
            outs = pool.map(_work, batch) if pool else [_work(b) for b in batch]
            for out in outs:
                enc.stdin.write(out)
            frames_done += len(batch)
            now = time.time()
            if now >= next_report:
                next_report = now + 2.0
                print(f"  {frames_done}/{cfg['nframes']} frames, "
                      f"{frames_done / (now - t0):.1f} fps", file=sys.stderr)
    finally:
        if pool:
            pool.close()
            pool.join()
        dec.stdout.close()
        dec.wait()
        enc.stdin.close()
        rc = enc.wait()
    if rc != 0:
        sys.exit(f"render.py: encoder failed with exit code {rc}")
    elapsed = time.time() - t0
    stats = {"frames": frames_done, "seconds": elapsed, "fps": frames_done / elapsed}
    for i, tl in enumerate(r.tl):
        share, gap, answers = r.stats(i, r.end_ms)
        print(f"  {tl.side:5s} {tl.label!r}: fresh cycles "
              f"{'-' if share is None else f'{share * 100:.1f} %'}, longest blind gap "
              f"{'-' if gap is None else f'{gap:.0f} ms'}, VLM answers {answers}, "
              f"not ok {tl.not_ok or '{}'}", file=sys.stderr)
    print(f"wrote {cfg['out']}: {frames_done} frames in {elapsed:.1f} s "
          f"({stats['fps']:.1f} fps)", file=sys.stderr)
    return stats


# --------------------------------------------------------------------------
# Selftest: synthetic clip + timelines
# --------------------------------------------------------------------------
ST_SECONDS, ST_FPS, ST_W, ST_H = 8, 30, 1280, 720
# objects: (cls_id, score, w, h, x(t), y(t), ffmpeg color, ffmpeg x expr, ffmpeg y expr)
ST_OBJECTS = [
    (1, 0.91, 90, 210, lambda t: 560 + 430 * math.sin(2 * math.pi * t / 4), lambda t: 400,
     "0xe76f51", "560+430*sin(2*PI*t/4)", "400"),
    (3, 0.88, 280, 150, lambda t: 500 - 380 * math.sin(2 * math.pi * t / 6), lambda t: 150,
     "0x2a9d8f", "500-380*sin(2*PI*t/6)", "150"),
    (10, 0.77, 44, 110, lambda t: 1160, lambda t: 70 + 20 * math.sin(2 * math.pi * t),
     "0xe9c46a", "1160", "70+20*sin(2*PI*t)"),
    (44, 0.31, 60, 60, lambda t: 200, lambda t: 620, "0x8d99ae", "200", "620"),  # hidden (<0.5)
]


def st_boxes(t_s: float, rng: random.Random):
    out = []
    for cls, score, w, h, fx, fy, *_ in ST_OBJECTS:
        x, y = fx(t_s) + rng.uniform(-3, 3), fy(t_s) + rng.uniform(-3, 3)
        out.append([cls, round(score + rng.uniform(-0.03, 0.03), 3),
                    round(x / ST_W, 4), round(y / ST_H, 4),
                    round((x + w) / ST_W, 4), round((y + h) / ST_H, 4)])
    return out


def selftest(outdir: str, jobs: int, preset: str) -> None:
    need("ffmpeg")
    os.makedirs(outdir, exist_ok=True)
    clip = os.path.join(outdir, "selftest-clip.mp4")
    # moving colored rectangles on a gridded background
    graph = [f"color=c=0x1b2430:s={ST_W}x{ST_H}:r={ST_FPS}:d={ST_SECONDS},"
             f"drawgrid=w=80:h=80:t=1:c=0x2c3a4a[bg0]"]
    prev = "bg0"
    for n, (_, _, w, h, _, _, color, xe, ye) in enumerate(ST_OBJECTS):
        graph.append(f"color=c={color}:s={w}x{h}:r={ST_FPS}:d={ST_SECONDS}[o{n}]")
        graph.append(f"[{prev}][o{n}]overlay=x='{xe}':y='{ye}':shortest=1[bg{n + 1}]")
        prev = f"bg{n + 1}"
    font = os.path.join(FONT_DIR, "DejaVuSans.ttf")
    graph.append(f"[{prev}]drawtext=fontfile={font}:text='frame %{{n}}':x=24:y=680:"
                 f"fontsize=24:fontcolor=white@0.6[out]")
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-filter_complex", ";".join(graph),
                    "-map", "[out]", "-c:v", "libx264", "-preset", "veryfast", "-crf", "20",
                    "-pix_fmt", "yuv420p", clip], check=True)

    frames = ST_SECONDS * ST_FPS
    base_header = {"type": "header", "source_fps": ST_FPS, "frames": frames,
                   "detector": {"model": "rfdetr_small", "period_ms": 100, "max_age_ms": 200,
                                "input_size": 512},
                   "vlm": {"model": "smolvlm", "prompt": "Describe what is ahead in one sentence.",
                           "max_tokens": 16},
                   "gpu": "synthetic (selftest)", "recorded_at": "2026-09-15T00:00:00Z",
                   "commit": "selftest"}
    captions = ["An orange figure walks to the right across the grid.",
                "A teal car drives left above a small grey square.",
                "A yellow traffic light hangs in the top right corner.",
                "The orange figure turns around and heads back to the left while the teal "
                "car keeps moving across the upper part of the frame, which is a very long "
                "answer that must be truncated cleanly."]

    def write(path, header, entries):
        entries.sort(key=lambda e: e["done_ms"])
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(json.dumps(header) + "\n")
            for n, e in enumerate(entries):
                fh.write(json.dumps(e) + "\n")
                if n == 5:
                    fh.write("{this line is not json\n")   # robustness check

    # right arm: governed, detector fresh (~35 ms), VLM slower but steady
    rng = random.Random(7)
    right = []
    for k in range(0, frames, 3):
        cap = k * 1000.0 / ST_FPS
        done = cap + 1.0 + rng.uniform(26, 44)
        right.append({"type": "detector", "frame": k, "capture_ms": cap, "send_ms": cap + 1.0,
                      "done_ms": round(done, 1), "status": "ok", "variant": "small",
                      "boxes": st_boxes(cap / 1000.0, rng), "queue_depth": 0})
    for n, k in enumerate(range(0, frames, 30)):
        cap = k * 1000.0 / ST_FPS
        if n == 3:
            right.append({"type": "vlm", "frame": k, "capture_ms": cap, "send_ms": cap + 0.5,
                          "done_ms": cap + 2.0, "status": "refused:stale"})
            continue
        right.append({"type": "vlm", "frame": k, "capture_ms": cap, "send_ms": cap + 0.5,
                      "done_ms": round(cap + rng.uniform(650, 900), 1), "status": "ok",
                      "text": captions[n % len(captions)]})
    write(os.path.join(outdir, "right.jsonl"), {**base_header, "arm": "governed",
                                                "label": "with Vigilant"}, right)

    # left arm: direct, laggy detector (120-260 ms) with a stall 3.3-5.2 s
    rng = random.Random(11)
    left = []
    for k in range(0, frames, 3):
        cap = k * 1000.0 / ST_FPS
        if 3300 <= cap < 5000:
            if k % 9 == 0:
                left.append({"type": "detector", "frame": k, "capture_ms": cap,
                             "send_ms": cap + 1.0, "done_ms": cap + 1500.0,
                             "status": "error:deadline exceeded"})
                continue
            done = 5200.0 + (cap - 3300) * 0.2 + rng.uniform(0, 30)   # queue drains late
        else:
            done = cap + rng.uniform(120, 260)
        left.append({"type": "detector", "frame": k, "capture_ms": cap, "send_ms": cap + 1.0,
                     "done_ms": round(done, 1), "status": "ok",
                     "boxes": st_boxes(cap / 1000.0, rng)})
    for n, k in enumerate(range(0, frames, 30)):
        cap = k * 1000.0 / ST_FPS
        left.append({"type": "vlm", "frame": k, "capture_ms": cap, "send_ms": cap + 0.5,
                     "done_ms": round(cap + rng.uniform(380, 520), 1), "status": "ok",
                     "text": captions[(n + 1) % len(captions)]})
    write(os.path.join(outdir, "left.jsonl"), {**base_header, "arm": "direct",
                                               "label": "NVIDIA Triton alone"}, left)

    out = os.path.join(outdir, "selftest.mp4")
    cfg = dict(video=clip, left=os.path.join(outdir, "left.jsonl"),
               right=os.path.join(outdir, "right.jsonl"), out=out, width=1920, height=1080,
               title="Selftest, synthetic clip",
               attribution="Synthetic test pattern generated by ffmpeg · no models were run · "
                           "timelines are invented to exercise lag, BLIND and captions",
               max_age_ms=None, start_s=0.0, duration_s=None, fps=30, jobs=jobs, preset=preset)
    render(cfg)
    for ts in ("1.0", "4.4", "5.6", "7.5"):
        png = os.path.join(outdir, f"frame-{ts}s.png")
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-ss", ts, "-i", out, "-frames:v", "1",
                        png], check=True)
        print(f"  frame {png}", file=sys.stderr)


# --------------------------------------------------------------------------
def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--video", help="clip (MP4, e.g. 1280x720 30 fps)")
    ap.add_argument("--left", help="timeline JSONL for the left side (direct arm)")
    ap.add_argument("--right", help="timeline JSONL for the right side (governed arm)")
    ap.add_argument("--out", help="output MP4")
    ap.add_argument("--width", type=int, default=1920)
    ap.add_argument("--height", type=int, default=1080)
    ap.add_argument("--title", default="")
    ap.add_argument("--attribution", default="")
    ap.add_argument("--max-age-ms", type=float, default=None,
                    help="blind threshold (default: detector.max_age_ms from the header)")
    ap.add_argument("--start-s", type=float, default=0.0)
    ap.add_argument("--duration-s", type=float, default=None)
    ap.add_argument("--fps", type=int, default=30, help=argparse.SUPPRESS)
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 1),
                    help="worker processes for frame composition (default: min(8, cpus))")
    ap.add_argument("--preset", default="medium", help="libx264 preset (default: medium)")
    ap.add_argument("--selftest", metavar="DIR",
                    help="generate a synthetic clip + timelines in DIR and render them")
    a = ap.parse_args(argv)
    if a.width % 2 or a.height % 2:
        ap.error("--width and --height must be even")
    if a.selftest:
        selftest(a.selftest, a.jobs, a.preset)
        return
    if not a.video or not a.out:
        ap.error("--video and --out are required (or use --selftest DIR)")
    try:
        render(dict(video=a.video, left=a.left, right=a.right, out=a.out, width=a.width,
                    height=a.height, title=a.title, attribution=a.attribution,
                    max_age_ms=a.max_age_ms, start_s=a.start_s, duration_s=a.duration_s,
                    fps=a.fps, jobs=a.jobs, preset=a.preset))
    except HeaderError as exc:
        sys.exit(f"render.py: {exc}")


if __name__ == "__main__":
    main()
