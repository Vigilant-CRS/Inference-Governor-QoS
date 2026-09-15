# SPDX-FileCopyrightText: 2026 Vigilant e.K.
# SPDX-License-Identifier: BUSL-1.1
"""Baut das Demo-Video: Stimme, Bilder, Schnitt, Untertitel, Kurzfassung.

    taskset -c 0-7 nice -n 10 python3 tools/video/make_video.py

Der Ablauf ist bewusst einstufig und wiederholbar: Aus `script.py` entstehen
die Sprachdateien, deren gemessene Laenge die Szenendauer bestimmt — nicht
umgekehrt. Ein Bild wird also nie "auf Zuruf" laenger, sondern genau so lang,
wie der Satz dauert, der dazu gesprochen wird. Untertitel und Kapitelmarken
fallen aus derselben Rechnung.

Grosse Artefakte landen ausserhalb des Repositorys (`--out`), weil ein
Repository kein Videospeicher ist. Im Repository steht nur, wie man sie
wiederherstellt.
"""
from __future__ import annotations

import argparse
import json
import multiprocessing
import os
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import render  # noqa: E402
import script  # noqa: E402

FPS = 30
PIPER = Path.home() / ".local/bin/piper"
VOICE = Path.home() / ".local/opt/piper/en_US-lessac-high.onnx"
#: Tempo und Atem — und die beiden sind nicht dasselbe.
#:
#: 1.15 streckte jede einzelne Silbe. Das Ergebnis war nicht ruhig, sondern
#: zaeh: gedehnte Vokale klingen nach Muehe, nicht nach Bedacht. Mit 1.0
#: spricht das Modell in seinem eigenen Takt, und die Ruhe wandert dorthin,
#: wo sie hingehoert — in die Pause zwischen zwei Saetzen. Das ist der
#: "breath between beats" aus dem Nachbarprojekt: nicht langsamer reden,
#: sondern seltener.
LENGTH_SCALE = "1.0"
SENTENCE_SILENCE = "0.5"


def run(argv: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(argv, check=True, capture_output=True, text=True, **kwargs)


def duration_of(path: Path) -> float:
    out = run(["ffprobe", "-v", "error", "-show_entries", "format=duration",
               "-of", "default=noprint_wrappers=1:nokey=1", str(path)])
    return float(out.stdout.strip())


def find_runtime(start: Path) -> Path:
    """Das Verzeichnis mit den Messprotokollen finden.

    Es liegt neben dem *Hauptcheckout*, nicht neben einem Arbeitsbaum — wer
    aus einem `git worktree` heraus rendert, faende sonst nichts. Deshalb wird
    nach oben gesucht, statt eine feste Ebene anzunehmen.
    """
    for base in [start, *start.parents]:
        candidate = base.parent / "InferenceQoS-runtime"
        if candidate.is_dir():
            return candidate
    raise SystemExit("InferenceQoS-runtime nicht gefunden; mit --runtime angeben.")


def synthesise(text: str, out: Path) -> float:
    """Ein Satz wird zu einer WAV-Datei; zurueck kommt ihre Laenge."""
    raw = out.with_suffix(".raw.wav")
    proc = subprocess.run(
        [str(PIPER), "--model", str(VOICE), "--length_scale", LENGTH_SCALE,
         "--sentence_silence", SENTENCE_SILENCE, "--output_file", str(raw)],
        input=text, capture_output=True, text=True)
    if proc.returncode != 0 or not raw.exists():
        raise SystemExit(f"piper failed for {out.name}: {proc.stderr[-400:]}")
    # 48 kHz Stereo, damit alle Segmente ohne Umrechnung aneinanderpassen.
    run(["ffmpeg", "-y", "-v", "error", "-i", str(raw), "-ar", "48000",
         "-ac", "2", str(out)])
    raw.unlink()
    return duration_of(out)


def _render_one(job: tuple) -> None:
    key, visual, data, index, progress, path = job
    scene = script.Scene(key=key, narration="", visual=visual, data=data)
    image = render.render(scene, progress)
    image.save(path)


def render_scene(scene, seconds: float, frames_dir: Path, fps: int) -> int:
    frames_dir.mkdir(parents=True, exist_ok=True)
    count = max(1, int(round(seconds * fps)))
    jobs = []
    for index in range(count):
        progress = index / max(1, count - 1)
        jobs.append((scene.key, scene.visual, scene.data, index, progress,
                     frames_dir / f"{index:05d}.png"))
    with multiprocessing.Pool(processes=min(8, os.cpu_count() or 4)) as pool:
        pool.map(_render_one, jobs, chunksize=4)
    return count


def segment(frames_dir: Path, seconds: float, out: Path, fps: int) -> None:
    run(["ffmpeg", "-y", "-v", "error", "-framerate", str(fps),
         "-i", str(frames_dir / "%05d.png"), "-t", f"{seconds:.3f}",
         "-c:v", "libx264", "-preset", "medium", "-crf", "18",
         "-pix_fmt", "yuv420p", "-r", str(fps), str(out)])


def audio_segment(voice: Path | None, seconds: float, out: Path) -> None:
    """Sprache plus Stille bis zur Szenenlaenge — oder reine Stille."""
    if voice is None:
        run(["ffmpeg", "-y", "-v", "error", "-f", "lavfi",
             "-i", "anullsrc=channel_layout=stereo:sample_rate=48000",
             "-t", f"{seconds:.3f}", str(out)])
        return
    run(["ffmpeg", "-y", "-v", "error", "-i", str(voice),
         "-af", f"apad=whole_dur={seconds:.3f}", "-t", f"{seconds:.3f}",
         "-ar", "48000", "-ac", "2", str(out)])


def concat(paths: list[Path], out: Path, work: Path, *, copy: bool = True) -> None:
    listing = work / (out.stem + ".txt")
    listing.write_text("".join(f"file '{p.resolve()}'\n" for p in paths))
    argv = ["ffmpeg", "-y", "-v", "error", "-f", "concat", "-safe", "0",
            "-i", str(listing)]
    argv += ["-c", "copy"] if copy else []
    argv += [str(out)]
    run(argv)


def srt_chunks(text: str, limit: int = 84) -> list[str]:
    """Untertitelzeilen: ein Satz je Zeile, lange Saetze am Komma, nie im Wort.

    Saetze werden bewusst **nicht** zusammengezogen. Zwei Saetze in einer Zeile
    lesen sich im Vorbeifahren schlechter als zwei kurze Zeilen, und der
    Verbinder zwischen ihnen waere entweder ein falsches Komma oder ein Punkt
    zu viel — die erste Fassung erzeugte daraus Zeilen wie
    "Your detector needs fifteen., That fits".
    """
    marked = text.replace(". ", ".|").replace(": ", ": |")
    parts: list[str] = []
    for sentence in (s.strip() for s in marked.split("|")):
        if not sentence:
            continue
        if len(sentence) <= limit:
            parts.append(sentence)
            continue
        # Knapp zu lange Saetze nicht am Komma zerlegen. "One laptop GPU,
        # against a tuned Triton ..." hat 86 Zeichen und zerfiel so in einen
        # Vierzehn-Zeichen-Fetzen plus volle Zeile; der Fetzen stand 0,9
        # Sekunden. Der mittige Umbruch weiter unten teilt solche Saetze in
        # zwei lesbare Haelften.
        if len(sentence) <= int(limit * 1.25):
            parts.append(sentence)
            continue
        current = ""
        for piece in sentence.split(", "):
            candidate = (current + ", " + piece) if current else piece
            if len(candidate) <= limit:
                current = candidate
            else:
                if current:
                    parts.append(current)
                current = piece
        if current:
            parts.append(current)

    # Sehr kurze Saetze mit dem naechsten zusammenlegen. Die Standzeit ist
    # proportional zur Laenge, und "One GPU." allein stuende 0,7 Sekunden —
    # zu kurz zum Lesen.
    merged: list[str] = []
    for part in parts:
        if (merged and len(merged[-1]) < 26
                and len(merged[-1]) + 1 + len(part) <= limit):
            merged[-1] = merged[-1] + " " + part
        else:
            merged.append(part)

    # Ein Satz ohne Komma bleibt sonst in einem Stueck stehen — 139 Zeichen
    # auf einer Zeile liest im Vorbeifahren niemand. Deshalb am Ende hart
    # umbrechen, aber nie mitten im Wort.
    wrapped: list[str] = []
    for part in merged:
        while len(part) > limit:
            # Knapp zu lange Zeilen mittig trennen: stur bei `limit` zu
            # schneiden liess aus 85 Zeichen die Zeilen 84 und "period"
            # werden, und ein einzelnes Wort als Untertitel ist ein Fehler.
            target = len(part) // 2 if len(part) <= 2 * limit else limit
            cut = part.rfind(" ", 0, target + 1)
            if cut <= 0:
                cut = part.find(" ", target)
            if cut <= 0:
                cut = limit
            wrapped.append(part[:cut].rstrip())
            part = part[cut:].lstrip()
        if part:
            wrapped.append(part)
    return wrapped


def timecode(seconds: float) -> str:
    ms = int(round(seconds * 1000))
    h, ms = divmod(ms, 3_600_000)
    m, ms = divmod(ms, 60_000)
    s, ms = divmod(ms, 1000)
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"


def write_srt(entries: list[tuple[float, float, str]], out: Path) -> None:
    blocks = []
    for index, (start, end, text) in enumerate(entries, start=1):
        blocks.append(f"{index}\n{timecode(start)} --> {timecode(end)}\n{text}\n")
    out.write_text("\n".join(blocks), encoding="utf-8")


def attach_sources(scenes, runtime: Path) -> None:
    """Belege an die Szenen haengen, die eine Datei zeigen.

    Die Messprotokolle liegen neben dem Repository, nicht darin.
    """
    for scene in scenes:
        if scene.source in ("run", "doctor", "autotune", "tuned"):
            relative = Path(script.SOURCES[scene.source]).relative_to(runtime.name)
            scene.data["path"] = str(runtime / relative)
            # Manche Protokollverzeichnisse tragen kein Datum im Namen; dann
            # kommt es aus dem Bericht (script.SOURCE_DATES), nie aus der
            # Dateizeit — die verstellt schon ein Kopiervorgang.
            scene.data["date"] = script.SOURCE_DATES.get(scene.source, "")
        if scene.visual == "devices":
            scene.data["paths"] = [str(runtime / "messungen" / d["run"] / "qualification.json")
                                   for d in script.DEVICES]


def build_cut(scenes, stem: str, out: Path, work: Path, fps: int,
              keep_frames: bool) -> dict:
    """Ein Schnitt: Stimme je Szene, Bilder, Ton, Untertitel, eingebrannte Fassung.

    Erklaervideo und Werbe-Cut laufen durch dieselbe Strecke. Zwei Strecken
    waeren zwei Stellen, an denen Untertitelstil, Lautheit oder Tonformat
    auseinanderlaufen koennen.
    """
    work.mkdir(parents=True, exist_ok=True)
    segments, audio_parts, srt_entries, chapters = [], [], [], []
    clock = 0.0
    print(f"\n{stem}")
    print(f"{'scene':<18}{'voice':>8}{'scene':>8}")
    for index, scene in enumerate(scenes):
        voice_path = None
        spoken = 0.0
        if scene.narration:
            voice_path = work / f"{index:02d}-{scene.key}.wav"
            spoken = synthesise(scene.narration, voice_path)
        seconds = max(scene.hold, spoken + scene.pause)

        frames_dir = work / f"frames-{index:02d}-{scene.key}"
        render_scene(scene, seconds, frames_dir, fps)
        video = work / f"{index:02d}-{scene.key}.mp4"
        segment(frames_dir, seconds, video, fps)
        audio = work / f"{index:02d}-{scene.key}-mix.wav"
        audio_segment(voice_path, seconds, audio)
        if not keep_frames:
            shutil.rmtree(frames_dir)

        segments.append(video)
        audio_parts.append(audio)
        chapters.append((clock, scene.chapter or scene.key))
        if scene.narration:
            chunks = srt_chunks(scene.narration)
            total = sum(len(c) for c in chunks) or 1
            cursor = clock
            for chunk in chunks:
                share = spoken * len(chunk) / total
                srt_entries.append((cursor, cursor + share, chunk))
                cursor += share
        clock += seconds
        print(f"{scene.key:<18}{spoken:>7.1f}s{seconds:>7.1f}s")
    print(f"{'total':<18}{'':>8}{clock:>7.1f}s")

    video_only = work / "video.mp4"
    concat(segments, video_only, work)
    audio_raw = work / "audio.wav"
    concat(audio_parts, audio_raw, work, copy=False)
    audio_norm = work / "audio-norm.wav"
    # Lautheit auf Broadcast-Niveau, damit es neben anderen Videos nicht absaeuft.
    # `-ar 48000` fest: loudnorm rechnet intern mit 192 kHz.
    run(["ffmpeg", "-y", "-v", "error", "-i", str(audio_raw),
         "-af", "loudnorm=I=-16:TP=-1.5:LRA=11", "-ar", "48000", "-ac", "2",
         str(audio_norm)])

    final = out / f"{stem}.mp4"
    run(["ffmpeg", "-y", "-v", "error", "-i", str(video_only), "-i", str(audio_norm),
         "-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-shortest", str(final)])

    srt = out / f"{stem}.en.srt"
    write_srt(srt_entries, srt)

    burned = out / f"{stem}-subtitled.mp4"
    # Achtung, diese Zahlen sind keine Pixel: der Untertitelfilter rechnet auf
    # einer eigenen, kleineren Buehne. Am Bild kalibriert — Fontsize 13 ergibt
    # rund 36 Pixel und eine Zeile unter der Zeitachse.
    style = ("FontName=DejaVu Sans,Fontsize=13,PrimaryColour=&H00F3EDE6,"
             "OutlineColour=&H00170D0D,BorderStyle=3,Outline=1,Shadow=0,"
             "MarginV=26")
    run(["ffmpeg", "-y", "-v", "error", "-i", str(final),
         "-vf", f"subtitles={srt}:force_style='{style}'",
         "-c:v", "libx264", "-preset", "medium", "-crf", "18",
         "-pix_fmt", "yuv420p", "-c:a", "copy", str(burned)])
    return {"final": final, "subtitled": burned, "srt": srt,
            "chapters": chapters, "duration": clock}


DESCRIPTION = [
    "Robots and vehicles are moving to central computers, where vision,",
    "planning and language models share one chip. An inference server works",
    "in arrival order — including camera frames that are already stale by the",
    "time they finish. The Vigilant Inference Governor sits in front of your",
    "inference server (NVIDIA Triton, TensorFlow Lite) and decides before every",
    "dispatch whether a result will still be useful when it is done.", "",
    "What it does: it drops frames a newer one has replaced, refuses work",
    "that would finish too late, holds back long background jobs when",
    "protected work is due, and switches to a smaller model variant when",
    "time runs short. It speaks the Open Inference Protocol, so your",
    "client only changes the address.", "",
    "Measured against a tuned Triton on the same GPU, the detector answers",
    "in time in 99 % of control cycles instead of 85 % — twenty times fewer",
    "missed cycles. Tested on an NVIDIA GPU and on the Adreno GPUs of two",
    "Android devices.", "",
    "vig autotune tunes it for your hardware: one command measures your models",
    "on your machine, tries the governor's settings against your contracts and",
    "keeps the configuration that serves your protected streams best.", "",
    "Built for humanoid and mobile robots, vehicle central computers,",
    "perception pipelines and ROS 2 systems: anywhere a late answer is worth",
    "less than no answer.", "",
    "Repository: https://github.com/Vigilant-CRS/Inference-Governor-QoS",
    "Licence: BUSL-1.1 — free for evaluation and for up to three devices in production.",
    "Vigilant e.K., Stuttgart — https://vigilant-crs.de", "",
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default="/run/media/dd/USB_4028/Projekte/"
                        "InferenceQoS-runtime/video",
                        help="Ablage fuer Video, Ton und Bilder")
    parser.add_argument("--fps", type=int, default=FPS)
    parser.add_argument("--keep-frames", action="store_true")
    parser.add_argument("--runtime", default="",
                        help="Verzeichnis mit den Messprotokollen "
                             "(Voreinstellung: neben dem Hauptcheckout gesucht)")
    parser.add_argument("--only", choices=("explainer", "promo"), default="",
                        help="nur einen der beiden Schnitte bauen")
    args = parser.parse_args()

    here = Path(__file__).resolve()
    runtime = Path(args.runtime) if args.runtime else find_runtime(here)
    out = Path(args.out)
    work = out / "work"
    work.mkdir(parents=True, exist_ok=True)

    pending = runtime / "measure-pending"
    if pending.exists():
        raise SystemExit("Es laeuft eine Messung (measure-pending). Spaeter rendern.")

    attach_sources(script.SCENES, runtime)
    attach_sources(script.PROMO_SCENES, runtime)

    summary = {"voice": VOICE.name, "length_scale": LENGTH_SCALE, "fps": args.fps}
    if args.only in ("", "explainer"):
        explainer = build_cut(script.SCENES, "vigilant-inference-governor", out,
                              work / "explainer", args.fps, args.keep_frames)
        render.thumbnail(out / "thumbnail.png")
        lines = [f"# {script.YOUTUBE_TITLE}", "", *DESCRIPTION, "## Chapters", ""]
        for start, key in explainer["chapters"]:
            lines.append(f"{timecode(start)[3:8]} {key}")
        lines += ["", "## Tags", "", ", ".join(script.YOUTUBE_TAGS), ""]
        (out / "youtube.md").write_text("\n".join(lines), encoding="utf-8")
        summary.update({
            "final": str(explainer["final"]), "subtitled": str(explainer["subtitled"]),
            "srt": str(explainer["srt"]), "thumbnail": str(out / "thumbnail.png"),
            "duration_s": round(explainer["duration"], 2),
        })
    if args.only in ("", "promo"):
        promo = build_cut(script.PROMO_SCENES, "vigilant-inference-governor-promo", out,
                          work / "promo", args.fps, args.keep_frames)
        promo_lines = [f"# {script.PROMO_TITLE}", "", *DESCRIPTION[:6], "",
                       "Full explainer and repository: "
                       "https://github.com/Vigilant-CRS/Inference-Governor-QoS", "",
                       "## Tags", "", ", ".join(script.YOUTUBE_TAGS), ""]
        (out / "youtube-promo.md").write_text("\n".join(promo_lines), encoding="utf-8")
        summary.update({
            "promo": str(promo["final"]), "promo_subtitled": str(promo["subtitled"]),
            "promo_srt": str(promo["srt"]), "promo_duration_s": round(promo["duration"], 2),
        })

    (out / "build.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
