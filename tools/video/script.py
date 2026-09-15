# SPDX-FileCopyrightText: 2026 Vigilant e.K.
# SPDX-License-Identifier: BUSL-1.1
"""Was das Video sagt, als Daten — Text, Quelle, Bild.

Jede Zahl, die gesprochen oder gezeigt wird, traegt hier ihren Beleg als
`source`. Das ist keine Zierde: Ein Video altert schneller als ein Repository,
und die einzige Verteidigung dagegen ist, dass jede Behauptung auf eine Datei
zeigt, die sich aendert, wenn die Messung sich aendert.

Der Sprechertext steht vollstaendig hier und nicht im Rendercode, damit eine
Aenderung am Wortlaut keine Aenderung am Bild erzwingt — und damit der
Untertitel aus derselben Quelle kommt wie die Stimme.

**Fassung vom 15.09.2026.** Das Video sagt, was das System kann: das Problem,
die vier Entscheidungen, die Messung, die Plattformen, `vig autotune`. Die
Grenzen — wann es nichts bringt, was es kostet, was nicht belegt ist — stehen
weiter vollstaendig in README, STATUS und den Berichten; ein Drei-Minuten-Film
ist nicht der Ort, sie alle zu erzaehlen. Was das Video zeigt, bleibt belegt:
jede Zahl kommt weiterhin aus einer Datei.
"""
from __future__ import annotations

from dataclasses import dataclass, field

#: Die Plattformen der Geraeteszene. Name, Chip und Backend stehen hier mit
#: ihrem Beleg (`docs/benchmark/android-gpu.md`, `validierung-autotune.md`);
#: Serienzahl und Urteil liest das Bild aus der jeweiligen
#: `qualification.json` des genannten Laufs.
DEVICES = [
    {"name": "Laptop GPU", "chip": "NVIDIA GeForce RTX 3070",
     "backend": "NVIDIA Triton 2.70", "run": "autotune-laptop-2026-09-15f"},
    {"name": "Pixel 2", "chip": "Snapdragon 835 · Adreno 540",
     "backend": "TensorFlow Lite, GPU delegate", "run": "autotune-pixel2-2026-09-15"},
    {"name": "Pixel 5", "chip": "Snapdragon 765G · Adreno 620",
     "backend": "TensorFlow Lite, GPU delegate", "run": "autotune-pixel5-2026-09-15"},
]

#: Wo die Zahlen herkommen. Der Schluessel steht in `Scene.source`.
SOURCES = {
    "gate-m3": "docs/benchmark/messkette-2026-09-12.md",
    "run": "InferenceQoS-runtime/gate-m3-r03/gate-r1.txt",
    "doctor": "InferenceQoS-runtime/measure-nachlauf-2026-09-11e/pre4-doctor.txt",
    # Der Lauf mit dem ausgelieferten Stand vom 15.09. (`contaminated: false`,
    # englisches Urteil, Verweigerung vorn).
    "autotune": "InferenceQoS-runtime/autotune-laptop-2026-09-15f/qualification.json",
    "devices": ", ".join(f"InferenceQoS-runtime/{d['run']}/qualification.json"
                         for d in DEVICES),
}

#: Messdaten fuer Protokolle, deren Pfad kein Datum traegt. Sie stehen im
#: zugehoerigen Bericht — `gate-m3-r03.md` nennt den 10.09.2026 — und nicht
#: in der Dateizeit, die ein Kopiervorgang verstellt.
SOURCE_DATES = {
    "run": "2026-09-10",
}


@dataclass
class Scene:
    """Eine Szene: was gesprochen wird, was zu sehen ist, wie lange."""

    key: str
    #: Der Sprechertext. Leer heisst: stumme Szene mit fester Dauer.
    narration: str
    #: Welcher Renderer das Bild macht (siehe render.py).
    visual: str
    #: Belegschluessel aus SOURCES, wenn die Szene eine Zahl zeigt.
    source: str = ""
    #: Kapiteltitel unter dem Video, in der Sprache des Zuschauers.
    chapter: str = ""
    #: Sekunden Stille nach der Szene, damit nichts hetzt.
    pause: float = 0.6
    #: Nur fuer stumme Szenen: feste Dauer.
    hold: float = 0.0
    #: Freie Parameter fuer den Renderer.
    data: dict = field(default_factory=dict)
    #: Gehoert die Szene in den 45-Sekunden-Schnitt?
    short_cut: bool = False


#: Die Kameraperiode, die Detektorlaufzeit und der unteilbare Hintergrundblock,
#: mit denen die Animation rechnet. Es sind die Groessen aus dem README-Beispiel
#: und aus der Gate-M3-Konfiguration, nicht erfundene runde Zahlen.
PERIOD_MS = 33
DETECTOR_MS = 15
BACKGROUND_MS = 95

SCENES = [
    Scene(
        key="title",
        narration=(
            "Your GPU is working flat out, and part of that work is on frames "
            "your robot has already thrown away."
        ),
        visual="title",
        chapter="Stop computing the past",
        pause=0.8,
    ),
    Scene(
        key="problem",
        narration=(
            "A robot camera gives you a frame every thirty-three milliseconds. "
            "Your detector needs fifteen. That fits, until something else wants "
            "the same GPU. Now the detector waits behind a block it cannot "
            "interrupt, and by the time its answer arrives, the robot has "
            "already moved."
        ),
        visual="timeline_fifo",
        chapter="What goes wrong at 33 milliseconds",
        pause=0.7,
        short_cut=True,
    ),
    Scene(
        key="governor",
        narration=(
            "Vigilant sits in front of your inference server and asks one "
            "question before every dispatch: will this result still be useful "
            "when it is finished? Work that cannot be is dropped before it costs "
            "anything. Same protocol, same models. In your client, only the "
            "address changes."
        ),
        visual="timeline_governor",
        chapter="What the governor does",
        pause=0.7,
    ),
    Scene(
        key="capabilities",
        narration=(
            "Four decisions, all made before a request reaches the GPU. A newer "
            "frame replaces an older one that is still waiting. Work that would "
            "finish too late is never started. A long background job waits when "
            "protected work is due. And when time runs short, a smaller model "
            "variant takes over."
        ),
        visual="capabilities",
        chapter="Four decisions before the GPU",
        pause=0.7,
    ),
    Scene(
        key="usecases",
        narration=(
            "It is built for machines where several models share one "
            "accelerator. A humanoid robot, whose vision must not wait behind "
            "its planner. Or driver assistance, with several cameras and a "
            "slower scene analysis on the same chip."
        ),
        visual="usecases",
        chapter="Built for robots and vehicles",
        pause=0.7,
    ),
    Scene(
        key="measured",
        narration=(
            "Measured against a tuned Triton on the same GPU, with a real "
            "detector and a background job that cannot be interrupted: the "
            "detector answers in time in ninety-nine percent of control cycles "
            "instead of eighty-five. Twenty times fewer missed cycles."
        ),
        visual="terminal_run",
        chapter="Measured against a tuned Triton",
        source="run",
        pause=0.8,
        short_cut=True,
    ),
    Scene(
        key="devices",
        narration=(
            "And it is not tied to one machine. The same governor runs in front "
            "of Triton on an NVIDIA GPU, and in front of TensorFlow Lite on the "
            "Adreno GPUs of two Android devices. On each of them, vig autotune "
            "measured the hardware and gave its own answer."
        ),
        visual="devices",
        chapter="Tested on edge hardware",
        source="devices",
        pause=0.8,
    ),
    Scene(
        key="autotune",
        narration=(
            "You do not have to take our numbers. One command, vig autotune, "
            "measures your own machine: runtimes, concurrency, interference. It "
            "freezes a configuration and answers the question that matters: is "
            "the governor worth it here? On this laptop, above ninety percent "
            "load, the protected stream goes from missing almost every cycle to "
            "missing none. And where you do not need it, it tells you that too."
        ),
        visual="autotune",
        chapter="vig autotune: is it worth it on your machine?",
        source="autotune",
        pause=0.8,
    ),
    Scene(
        key="try",
        narration=(
            "Point it at the inference server you already run. Same protocol, "
            "same models, and your client changes one line. Free for evaluation, "
            "and for up to three devices in production."
        ),
        visual="terminal_doctor",
        chapter="Try it on the server you already run",
        source="doctor",
        pause=0.7,
    ),
    Scene(
        key="close",
        narration=(
            "Faster GPUs compute the past faster. Vigilant stops computing it. "
            "Run vig autotune on the machine you already have, and let the "
            "numbers decide."
        ),
        visual="close",
        chapter="Stop computing the past",
        pause=1.2,
    ),
]

#: Nur fuer den Kurzschnitt: eine stumme Schlusskarte mit dem naechsten Schritt.
SHORT_TAIL = Scene(
    key="short-tail",
    narration="",
    visual="short_tail",
    hold=5.5,
    pause=0.0,
)

#: Kapitelmarken und Beschreibung entstehen aus denselben Szenen; siehe
#: make_video.py. Hier stehen nur die Texte, die kein Szenentext sind.
YOUTUBE_TITLE = ("Stop Computing the Past — Inference QoS for Edge Robotics "
                 "| Vigilant Inference Governor")

YOUTUBE_TAGS = [
    "edge ai", "inference", "gpu scheduling", "robotics", "triton inference server",
    "tensorflow lite", "nvidia jetson", "real time", "computer vision", "ros 2",
    "latency", "age of information", "humanoid robot", "adas",
    "machine learning infrastructure",
]
