# SPDX-FileCopyrightText: 2026 Vigilant e.K.
# SPDX-License-Identifier: BUSL-1.1
"""Die Bilder. Eine Handschrift, kein Themensystem.

Dunkler Grund, eine Akzentfarbe, serifenlose Systemschrift, die Wortmarke
unten rechts — dieselbe Sparsamkeit wie in den Videos des Nachbarprojekts.
Was hier *nicht* passiert: Zahlen in Bilder tippen. Die Tabellen kommen aus
den Berichten, die Terminalbilder aus echten Messprotokollen, die Animation
aus `sim.py`. Wer eine Zahl aendern will, aendert die Messung.
"""
from __future__ import annotations

import os
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

import report
import sim

W, H = 1920, 1080

BG = "#0d1117"
PANEL = "#11151c"
BAR = "#1b2029"
TEXT = "#e6edf3"
DIM = "#7c8797"
FAINT = "#39414d"
ACCENT = "#4c90cc"
OK = "#56d364"
BAD = "#f0654f"

FONT_DIR = "/usr/share/fonts/truetype/dejavu"
SANS = os.path.join(FONT_DIR, "DejaVuSans.ttf")
SANS_BOLD = os.path.join(FONT_DIR, "DejaVuSans-Bold.ttf")
MONO = os.path.join(FONT_DIR, "DejaVuSansMono.ttf")

_FONTS: dict = {}


def font(path: str, size: int) -> ImageFont.FreeTypeFont:
    key = (path, size)
    if key not in _FONTS:
        _FONTS[key] = ImageFont.truetype(path, size)
    return _FONTS[key]


def base() -> Image.Image:
    """Leeres Bild mit Wortmarke."""
    image = Image.new("RGB", (W, H), BG)
    draw = ImageDraw.Draw(image)
    draw.text((W - 60, H - 52), "VIGILANT", font=font(SANS_BOLD, 22), fill=DIM,
              anchor="rs")
    return image


def wrap(draw: ImageDraw.ImageDraw, text: str, fnt, width: int) -> list[str]:
    lines, line = [], ""
    for word in text.split():
        probe = (line + " " + word).strip()
        if draw.textlength(probe, font=fnt) <= width:
            line = probe
        else:
            lines.append(line)
            line = word
    if line:
        lines.append(line)
    return lines


def headline(draw: ImageDraw.ImageDraw, text: str, y: int = 120, color: str = TEXT,
             size: int = 62) -> int:
    fnt = font(SANS_BOLD, size)
    for line in wrap(draw, text, fnt, W - 320):
        draw.text((160, y), line, font=fnt, fill=color)
        y += int(size * 1.25)
    return y


def kicker(draw: ImageDraw.ImageDraw, text: str, y: int = 74) -> None:
    draw.text((160, y), text.upper(), font=font(SANS_BOLD, 22), fill=ACCENT)


def footnote(draw: ImageDraw.ImageDraw, text: str) -> None:
    draw.text((160, H - 62), text, font=font(SANS, 22), fill=DIM)


# ---------------------------------------------------------------- Szenenbilder

def title(_scene, progress: float) -> Image.Image:
    image = base()
    draw = ImageDraw.Draw(image)
    fade = min(1.0, progress * 3)
    shade = tuple(int(c * fade) for c in (0xE6, 0xED, 0xF3))
    draw.text((160, 420), "Vigilant Inference Governor", font=font(SANS_BOLD, 84),
              fill=shade)
    draw.text((160, 540), "One GPU. Several models. Only the newest frame counts.",
              font=font(SANS, 40), fill=DIM)
    draw.line([(160, 640), (160 + int(520 * min(1.0, progress * 1.6)), 640)],
              fill=ACCENT, width=5)
    return image


def _axis(draw: ImageDraw.ImageDraw, horizon: float) -> tuple[int, int, float]:
    left, right = 200, W - 200
    scale = (right - left) / horizon
    draw.line([(left, 880), (right, 880)], fill=FAINT, width=2)
    for ms in range(0, int(horizon) + 1, 100):
        x = left + int(ms * scale)
        draw.line([(x, 875), (x, 890)], fill=FAINT, width=2)
        draw.text((x, 902), f"{ms} ms", font=font(SANS, 20), fill=DIM, anchor="ma")
    return left, right, scale


def _timeline(policy: str, progress: float, caption: str, verdict: str,
              verdict_color: str) -> Image.Image:
    trace = sim.simulate(policy, period=sim_period(), detector=sim_detector(),
                         background=sim_background())
    image = base()
    draw = ImageDraw.Draw(image)
    kicker(draw, "the same workload, two policies")
    headline(draw, caption, y=118, size=54)
    draw.text((160, 262),
              f"camera every {sim_period():.0f} ms  ·  detector {sim_detector():.0f} ms"
              f"  ·  one {sim_background():.0f} ms block that cannot be interrupted",
              font=font(SANS, 30), fill=DIM)

    left, _right, scale = _axis(draw, trace.horizon)
    now = progress * trace.horizon

    draw.text((60, 470), "camera", font=font(SANS, 24), fill=DIM, anchor="ls")
    draw.text((60, 610), "GPU", font=font(SANS, 24), fill=DIM, anchor="ls")

    for at, number in trace.arrivals:
        if at > now:
            continue
        x = left + int(at * scale)
        draw.rectangle([x, 430, x + 14, 478], fill=ACCENT)
        if number % 3 == 1 and x < W - 260:
            draw.text((x + 7, 412), str(number), font=font(SANS, 20), fill=DIM,
                      anchor="ms")

    for block in trace.blocks:
        if block.start > now:
            continue
        x0 = left + int(block.start * scale)
        x1 = left + int(min(block.end, now) * scale)
        if block.kind == "detector":
            fill = BAD if block.late else OK
            draw.rectangle([x0, 560, max(x1, x0 + 3), 620], fill=fill)
        else:
            draw.rectangle([x0, 560, max(x1, x0 + 3), 620], fill=FAINT)
            if x1 - x0 > 120:
                draw.text(((x0 + x1) // 2, 590), "background block, 95 ms",
                          font=font(SANS, 22), fill=DIM, anchor="mm")

    for hold in trace.held:
        if hold.at > now:
            continue
        x0 = left + int(hold.at * scale)
        x1 = left + int(min(hold.until, now) * scale)
        for x in range(x0, max(x1, x0 + 2), 12):
            draw.line([(x, 560), (x, 620)], fill=FAINT, width=3)

    cursor = left + int(now * scale)
    draw.line([(cursor, 400), (cursor, 890)], fill=TEXT, width=2)

    late = len([b for b in trace.blocks if b.kind == "detector" and b.late
                and b.start <= now])
    waited = sim.worst_wait(trace, sim_period())
    legend = [
        ("results already worthless when finished", BAD if late else DIM, str(late)),
        ("longest stretch without a fresh result", TEXT, f"{waited:.0f} ms"),
    ]
    y = 690
    for label, color, value in legend:
        draw.text((200, y), label, font=font(SANS, 28), fill=DIM)
        # Bewusst links vom Zeitcursor: rechts am Bildrand kollidiert der Wert
        # gegen Ende der Animation mit der Zeitlinie.
        draw.text((1180, y), value, font=font(SANS_BOLD, 28), fill=color,
                  anchor="ra")
        y += 46

    if progress > 0.55:
        draw.text((200, 800), verdict, font=font(SANS_BOLD, 32), fill=verdict_color)
    return image


def sim_period() -> float:
    import script
    return script.PERIOD_MS


def sim_detector() -> float:
    import script
    return script.DETECTOR_MS


def sim_background() -> float:
    import script
    return script.BACKGROUND_MS


def timeline_fifo(_scene, progress: float) -> Image.Image:
    return _timeline("fifo", progress,
                     "First come, first served",
                     "The camera kept sending. The answers stopped being useful.",
                     BAD)


def timeline_governor(_scene, progress: float) -> Image.Image:
    return _timeline("governor", progress,
                     "Freshness first",
                     "Nothing stale was computed — and the block never started.",
                     OK)


def stale(_scene, progress: float) -> Image.Image:
    """Warum ein Server das nicht loesen kann: er kennt den Wert nicht."""
    trace = sim.simulate("fifo", period=sim_period(), detector=sim_detector(),
                         background=sim_background())
    late = [b for b in trace.blocks if b.kind == "detector" and b.late]
    image = base()
    draw = ImageDraw.Draw(image)
    kicker(draw, "why the server cannot fix this")
    headline(draw, "A request is not a promise that anyone still wants the answer.",
             y=118, size=52)

    y = 360
    shown = int(min(len(late), 1 + progress * len(late)))
    for block in late[:shown]:
        newer = [f for a, f in trace.arrivals if a <= block.end and f > (block.frame or 0)]
        draw.text((200, y), f"frame {block.frame}", font=font(SANS_BOLD, 34), fill=TEXT)
        draw.text((420, y), f"computed until {block.end:.0f} ms", font=font(SANS, 32),
                  fill=DIM)
        draw.text((900, y), f"{len(newer)} newer frame(s) had already arrived",
                  font=font(SANS, 32), fill=BAD)
        y += 62

    if progress > 0.5:
        draw.text((200, 700),
                  "Computed correctly. Delivered too late to be used.",
                  font=font(SANS_BOLD, 36), fill=TEXT)
    footnote(draw, "derived from the same simulation as the timeline above")
    return image


def _panel(draw: ImageDraw.ImageDraw, top: int, height: int, title_text: str) -> None:
    """Der Fensterrahmen, in dem eine echte Ausgabe steht."""
    left = 130
    draw.rectangle([left, top, W - left, top + height], fill=PANEL)
    draw.rectangle([left, top, W - left, top + 44], fill=BAR)
    for index, colour in enumerate(("#ff5f57", "#febc2e", "#28c840")):
        draw.ellipse([left + 20 + index * 24, top + 16, left + 32 + index * 24,
                      top + 28], fill=colour)
    draw.text((left + 110, top + 22), title_text, font=font(SANS, 20), fill=DIM,
              anchor="lm")


def _source(draw: ImageDraw.ImageDraw, path: Path, y: int, date: str = "") -> None:
    """Quellenangabe im Bild: welche Datei, welches Datum, was uebersetzt wurde.

    Ohne diese Zeile waere die englische Tabelle eine Behauptung. Mit ihr kann
    jeder die Zahl in der genannten Datei nachschlagen — und sieht zugleich,
    dass an ihr nichts uebersetzt wurde ausser der Ueberschrift darueber.
    """
    draw.text((130, y),
              f"source: {report.source_label(path, date)}  ·  numbers read from "
              f"that file, only the German headings translated",
              font=font(SANS, 22), fill=DIM)


def terminal_run(scene, progress: float) -> Image.Image:
    """Die Stromtabelle des echten Laufs, englisch beschriftet.

    Auch die Zusammenfassung unten holt ihre zwei Zahlen aus der Tabelle und
    nicht aus dem Quelltext: Wer den Lauf austauscht, bekommt eine andere
    Zeile oder einen Abbruch, aber nie eine veraltete Behauptung.
    """
    path = Path(scene.data["path"])
    date = scene.data.get("date", "")
    rows = report.gate_rows(path)
    image = base()
    draw = ImageDraw.Draw(image)
    kicker(draw, "measured, not claimed")
    headline(draw, "A real detector against a block that cannot be interrupted.",
             y=112, size=46)

    top = 228
    # 214 ist kein Geschmackswert: die erste Zeile beginnt 204 Pixel unter der
    # Panelkante, jede weitere 54 tiefer, und die letzte braucht ihre eigene
    # Zeilenhoehe plus Rand. Mit 150 fiel die vierte Zeile aus dem Panel
    # heraus und in die Erklaerzeile darunter.
    height = 214 + 54 * len(rows)
    _panel(draw, top, height, f"gate-m3  ·  {report.source_label(path, date)}")
    draw.text((190, top + 68), report.gate_setup(path), font=font(SANS, 23),
              fill=DIM)

    group_y = top + 116
    draw.text((800, group_y), "answers that arrived in time", font=font(SANS, 23),
              fill=DIM, anchor="ma")
    draw.text((1340, group_y), "age of the newest answer, p95",
              font=font(SANS, 23), fill=DIM, anchor="ma")
    sub_y = group_y + 32
    for x, label in ((700, "Triton"), (900, "Vigilant"),
                     (1240, "Triton"), (1440, "Vigilant")):
        draw.text((x, sub_y), label, font=font(SANS_BOLD, 23), fill=DIM, anchor="ra")
    draw.text((1680, sub_y), "fewer missed", font=font(SANS_BOLD, 23), fill=DIM,
              anchor="ra")
    draw.line([(190, sub_y + 36), (1680, sub_y + 36)], fill=FAINT, width=2)

    shown = int(min(len(rows), 1 + progress * (len(rows) + 1)))
    y = sub_y + 56
    for name, triton_cov, gov_cov, triton_age, gov_age, verdict in rows[:shown]:
        lead = name == "detector"
        colour = ACCENT if lead else TEXT
        fnt = font(SANS_BOLD, 30) if lead else font(SANS, 30)
        draw.text((190, y), name, font=fnt, fill=colour)
        for x, value in ((700, triton_cov), (900, gov_cov),
                         (1240, triton_age), (1440, gov_age)):
            draw.text((x, y), value, font=fnt, fill=colour, anchor="ra")
        # Ein negativer Faktor ist ein Verlust und wird auch so gefaerbt —
        # `depth` steht in diesem Lauf bei -1.7x, und das bleibt rot.
        draw.text((1680, y), verdict, font=fnt, anchor="ra",
                  fill=BAD if verdict.startswith("-") else OK)
        y += 54

    below = top + height + 28
    draw.text((130, below),
              "\"in time\" = the control loop looked every 33 ms and found an "
              "answer younger than its limit",
              font=font(SANS, 25), fill=DIM)
    _source(draw, path, below + 40, date)
    if progress > 0.6:
        # Auch der Faktor kommt aus der Zeile darueber und nicht aus dem
        # Quelltext: gesprochene Zahl und gezeigte Zahl koennen so nicht
        # auseinanderlaufen.
        detector = next(r for r in rows if r[0] == "detector")
        draw.text((130, below + 100),
                  f"detector: {detector[1]} → {detector[2]} of control cycles "
                  f"answered in time  ·  {detector[5]} fewer missed",
                  font=font(SANS_BOLD, 38), fill=OK)
    return image


def terminal_doctor(scene, progress: float) -> Image.Image:
    path = Path(scene.data["path"])
    lines = report.doctor_lines(path)
    image = base()
    draw = ImageDraw.Draw(image)
    kicker(draw, "try it on your own machine")
    headline(draw, "It says what will not work — before you start.", y=112, size=46)

    mono = font(MONO, 24)
    line_h = 34
    top, left = 228, 130
    # Eine echte Ausgabe ist manchmal breiter als das Bild. Abschneiden mit
    # sichtbarer Ellipse, wie `render_terminal.py` es tut — eine Zeile, die
    # rechts aus dem Bild laeuft, behauptet, es stehe dort nichts mehr.
    advance = draw.textlength("M", font=mono) or 14.4
    budget = int((W - 2 * left - 60) / advance)
    lines = [line if len(line) <= budget else line[:budget - 2].rstrip() + " …"
             for line in lines]
    height = 52 + line_h * len(lines) + 26
    _panel(draw, top, height,
           f"vig doctor  ·  {report.source_label(path, scene.data.get('date', ''))}")

    shown = int(min(len(lines), 2 + progress * (len(lines) + 4)))
    y = top + 62
    for line in lines[:shown]:
        colour = TEXT
        if line.startswith("OK"):
            colour = OK
        elif line.startswith("FAIL") or line.startswith("RESULT"):
            colour = BAD
        elif line.startswith("WARN"):
            colour = "#d0a215"
        draw.text((left + 26, y), line, font=mono, fill=colour)
        y += line_h

    below = top + height + 26
    draw.text((left, below),
              "It refuses to pretend: an overloaded configuration is reported, "
              "not smoothed over.",
              font=font(SANS, 25), fill=DIM)
    _source(draw, path, below + 38, scene.data.get("date", ""))
    if progress > 0.55:
        # Nicht tiefer als hier: ab rund 930 liegt das Band des eingebrannten
        # Untertitels. Eine Fassung mit 856/958 verdeckte die Lizenzzeile
        # vollstaendig — in der Fassung ohne Untertitel sah man das nicht.
        commands = ["vig doctor -c vig.yaml", "vig profile", "vig serve"]
        x = 160
        for command in commands:
            width = int(draw.textlength(command, font=font(MONO, 30))) + 56
            draw.rectangle([x, 790, x + width, 852], outline=ACCENT, width=2)
            draw.text((x + 28, 821), command, font=font(MONO, 30), fill=TEXT,
                      anchor="lm")
            x += width + 34
        draw.text((160, 892),
                  "BUSL-1.1 — free for evaluation, and for up to three devices in production",
                  font=font(SANS, 26), fill=DIM)
    return image


def price(_scene, progress: float) -> Image.Image:
    image = base()
    draw = ImageDraw.Draw(image)
    kicker(draw, "and what it costs")
    headline(draw, "The background block pays for it.", y=118, size=54)

    draw.text((200, 300),
              "Same run, the background block (ResNet-50, 95 ms, not interruptible):",
              font=font(SANS, 32), fill=DIM)
    draw.text((200, 356), "0 % answered in time", font=font(SANS_BOLD, 56), fill=BAD)
    draw.text((200, 436),
              "A 95 ms block does not fit next to a 33 ms period — with or without us.",
              font=font(SANS, 28), fill=DIM)

    if progress > 0.4:
        draw.line([(200, 520), (W - 200, 520)], fill=FAINT, width=2)
        draw.text((200, 556), "For models that can be split, the trade becomes visible:",
                  font=font(SANS, 30), fill=TEXT)
        rows = [
            ("", "detector, answers in time", "background progress"),
            ("no decomposition", "98 %", "2 generations"),
            ("cooperative quanta", "91 %", "40 generations"),
        ]
        y = 626
        for label, left_value, right_value in rows:
            head = not label
            fnt = font(SANS, 26) if head else font(SANS_BOLD, 32)
            colour = DIM if head else TEXT
            draw.text((220, y), label, font=font(SANS_BOLD, 32), fill=TEXT)
            draw.text((1050, y), left_value, font=fnt, fill=colour, anchor="ma")
            accent = ACCENT if right_value.startswith("40") else colour
            draw.text((1500, y), right_value, font=fnt, fill=accent, anchor="ma")
            y += 58
    # Die Fussnote nennt die Einheit, nicht einen Faktor. Der Bericht gibt zwei
    # Verhaeltnisse her — 2 auf 40 Generierungen, aber 606 auf 9.300 Zeichen —
    # und "zwanzigmal mehr Fortschritt" haette sich stillschweigend das
    # guenstigere ausgesucht. Zahlen, die im Bericht stehen, altern ehrlich.
    footnote(draw, "docs/benchmark/wp26.md — 2 to 40 generations, "
                   "detector 98 % to 91 %, a different run from the one above")
    return image


def limits(_scene, progress: float) -> Image.Image:
    image = base()
    draw = ImageDraw.Draw(image)
    kicker(draw, "when not to use it")
    headline(draw, "Three cases where we would tell you no.", y=118, size=54)

    cards = [
        ("A GPU with room left",
         "Your server already delivers. Come back when it queues."),
        ("A single stream",
         "Keep the newest frame in your own client. Fifty lines get most of it."),
        ("Transport-bound",
         "Where CPU and transport dominate, the backend overlaps better than we serialise."),
    ]
    width, gap, pad = 520, 40, 36
    shown = int(min(len(cards), 1 + progress * 4))
    x = 160
    body_font = font(SANS, 26)
    for head, body in cards[:shown]:
        draw.rectangle([x, 340, x + width, 700], fill=PANEL)
        draw.line([(x, 340), (x + width, 340)], fill=ACCENT, width=4)
        draw.text((x + pad, 396), head, font=font(SANS_BOLD, 36), fill=TEXT)
        y = 466
        # Umbruch an der Kartenbreite statt von Hand gesetzter Zeilen: ein
        # laengerer Satz haengt sonst rechts aus der Karte heraus.
        for line in wrap(draw, body, body_font, width - 2 * pad):
            draw.text((x + pad, y), line, font=body_font, fill=DIM)
            y += 40
        x += width + gap
    footnote(draw, "the phone measurement: docs/benchmark/android-gpu.md")
    return image


def usecases(_scene, progress: float) -> Image.Image:
    """Zwei Einsatzbilder — und was wir ausdruecklich nicht behaupten.

    Die Abgrenzung steht im selben Bild wie die Bilder selbst und nicht im
    Kleingedruckten. Wer das hier einem Sicherheitsverantwortlichen zeigt,
    soll die Grenze sehen, bevor er fragen muss.
    """
    image = base()
    draw = ImageDraw.Draw(image)
    kicker(draw, "where this belongs")
    headline(draw, "Two pictures — and what we are not claiming.", y=118, size=54)

    cards = [
        ("Humanoid robot",
         "Vision in the control loop. A local language model plans the next "
         "move on the same GPU. The governor keeps the thinking from blocking "
         "the seeing."),
        ("Driver assistance, pre-development",
         "Several cameras, object detection at a fixed rate, and a slower "
         "scene analysis beside it on the same accelerator."),
    ]
    width, gap, pad = 780, 60, 40
    shown = int(min(len(cards), 1 + progress * 3))
    x = 160
    head_font = font(SANS_BOLD, 34)
    body_font = font(SANS, 28)
    for head, body in cards[:shown]:
        draw.rectangle([x, 300, x + width, 668], fill=PANEL)
        draw.line([(x, 300), (x + width, 300)], fill=ACCENT, width=4)
        y = 352
        # Auch die Ueberschrift wird umgebrochen: "Driver assistance,
        # pre-development" passt bei 34 Punkt nicht in eine Kartenzeile.
        for line in wrap(draw, head, head_font, width - 2 * pad):
            draw.text((x + pad, y), line, font=head_font, fill=TEXT)
            y += 46
        y += 18
        for line in wrap(draw, body, body_font, width - 2 * pad):
            draw.text((x + pad, y), line, font=body_font, fill=DIM)
            y += 42
        x += width + gap

    if progress > 0.5:
        draw.text((160, 740), "Plausible pictures, not customer deployments.",
                  font=font(SANS_BOLD, 32), fill=TEXT)
        draw.text((160, 796),
                  "We claim nothing about certification or hard real time. "
                  "We are one component —",
                  font=font(SANS, 28), fill=DIM)
        draw.text((160, 838),
                  "the safety argument stays with the manufacturer.",
                  font=font(SANS, 28), fill=DIM)
    return image


def close(_scene, progress: float) -> Image.Image:
    image = base()
    draw = ImageDraw.Draw(image)
    draw.text((160, 400), "Vigilant Inference Governor", font=font(SANS_BOLD, 72),
              fill=TEXT)
    draw.line([(160, 510), (160 + int(520 * min(1.0, progress * 2)), 510)],
              fill=ACCENT, width=5)
    lines = [
        ("github.com/Vigilant-CRS/Inference-Governor-QoS", TEXT),
        ("Same protocol, same models — in your client, only the address changes.", DIM),
        ("Vigilant e.K., Stuttgart · vigilant-crs.de · info@vigilant-crs.de", DIM),
    ]
    y = 570
    for text, colour in lines:
        draw.text((160, y), text, font=font(SANS, 34 if colour == TEXT else 28),
                  fill=colour)
        y += 62
    return image


def short_tail(_scene, progress: float) -> Image.Image:
    """Die Schlusskarte des Kurzschnitts: der Preis, ohne gesprochenen Satz."""
    image = base()
    draw = ImageDraw.Draw(image)
    kicker(draw, "the other half")
    headline(draw, "And the background pays for it.", y=150, size=58)
    draw.text((160, 360),
              "In the same run, the background block never ran.",
              font=font(SANS, 34), fill=DIM)
    draw.text((160, 416),
              "A 95 ms block does not fit next to a 33 ms period — with or without us.",
              font=font(SANS, 34), fill=DIM)
    draw.line([(160, 520), (160 + int(520 * min(1.0, progress * 2)), 520)],
              fill=ACCENT, width=5)
    draw.text((160, 580), "The measurements, the method, and the reports:",
              font=font(SANS, 32), fill=DIM)
    draw.text((160, 644), "github.com/Vigilant-CRS/Inference-Governor-QoS",
              font=font(SANS_BOLD, 40), fill=TEXT)
    return image


def autotune(scene, progress: float) -> Image.Image:
    """Der Lauf, der die Maschine des Anwenders vermisst — mitsamt dem, was er ablehnt.

    Die Zeilen kommen aus `qualification.json` eines echten Laufs. Dass dort
    heute "release refused" steht, wird nicht weggeschnitten: Ein Werkzeug,
    das eine Freigabe verweigert, weil es drei von vier Messreihen verworfen
    hat, ist genau deshalb glaubwuerdig. Faellt spaeter ein sauberer Lauf an,
    zeigt dasselbe Bild dessen Zahlen — es liest die Datei, es merkt sie sich
    nicht.
    """
    path = Path(scene.data["path"])
    lines = report.autotune_lines(path)
    image = base()
    draw = ImageDraw.Draw(image)
    kicker(draw, "measure your own machine")
    headline(draw, "It measures your machine — and says what it will not claim.",
             y=112, size=46)

    mono = font(MONO, 24)
    line_h = 34
    top, left = 228, 130
    advance = draw.textlength("M", font=mono) or 14.4
    budget = int((W - 2 * left - 60) / advance)
    lines = [line if len(line) <= budget else line[:budget - 2].rstrip() + " …"
             for line in lines]
    height = 52 + line_h * len(lines) + 26
    _panel(draw, top, height,
           f"vig autotune  ·  {report.source_label(path, scene.data.get('date', ''))}")

    shown = int(min(len(lines), 2 + progress * (len(lines) + 4)))
    y = top + 62
    for line in lines[:shown]:
        colour = TEXT
        if line.startswith("RESULT"):
            colour = BAD if "refused" in line else OK
        elif line.startswith("SERIES"):
            colour = "#d0a215" if " 0 discarded" not in line else OK
        elif line.startswith("DOCTOR"):
            colour = OK
        draw.text((left + 26, y), line, font=mono, fill=colour)
        y += line_h

    below = top + height + 26
    draw.text((left, below),
              "It refuses to certify on data it threw away — and tells you how "
              "much it threw away.",
              font=font(SANS, 25), fill=DIM)
    _source(draw, path, below + 38, scene.data.get("date", ""))
    return image


RENDERERS = {
    "title": title,
    "short_tail": short_tail,
    "timeline_fifo": timeline_fifo,
    "stale": stale,
    "timeline_governor": timeline_governor,
    "usecases": usecases,
    "terminal_run": terminal_run,
    "price": price,
    "limits": limits,
    "autotune": autotune,
    "terminal_doctor": terminal_doctor,
    "close": close,
}

#: Szenen, deren Bild sich bewegt. Alles andere wird einmal gerendert und
#: stehen gelassen — das spart Platz und Zeit, ohne dass man es sieht.
ANIMATED = {"title", "timeline_fifo", "stale", "timeline_governor", "usecases",
            "terminal_run", "price", "limits", "autotune", "terminal_doctor",
            "close", "short_tail"}


def render(scene, progress: float) -> Image.Image:
    return RENDERERS[scene.visual](scene, progress)


def thumbnail(path: Path) -> None:
    """1280x720, lesbar in der Vorschaugroesse einer Empfehlungsliste."""
    image = Image.new("RGB", (1280, 720), BG)
    draw = ImageDraw.Draw(image)
    draw.text((80, 120), "The newest frame", font=font(SANS_BOLD, 82), fill=TEXT)
    draw.text((80, 216), "is the only one worth", font=font(SANS_BOLD, 82), fill=TEXT)
    draw.text((80, 312), "computing.", font=font(SANS_BOLD, 82), fill=ACCENT)
    draw.line([(80, 440), (520, 440)], fill=ACCENT, width=6)
    # 99, nicht 100. Der Film sagt "to ninety-nine", und der Lauf, auf den die
    # Messszene zeigt (gate-m3-r03), weist 99/99/99 % aus. Die 100 % stehen in
    # einer anderen Messkette; sie hier zu zeigen hiesse, das Vorschaubild die
    # guenstigere Zahl aus einem Lauf nehmen zu lassen, den das Video gar nicht
    # belegt. Dieses Bild ist ausserdem das einzige ohne Quellenangabe und das
    # erste, was jemand sieht - gerade hier darf nichts stehen, was der Film
    # nicht haelt.
    draw.text((80, 486), "answers in time   85 %  →  99 %", font=font(SANS_BOLD, 44),
              fill=OK)
    draw.text((80, 556), "one GPU · several models · measured, with the price shown",
              font=font(SANS, 30), fill=DIM)
    draw.text((1200, 660), "VIGILANT", font=font(SANS_BOLD, 30), fill=DIM, anchor="rs")
    image.save(path)
