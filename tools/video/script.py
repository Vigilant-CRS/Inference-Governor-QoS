# SPDX-FileCopyrightText: 2026 Vigilant e.K.
# SPDX-License-Identifier: BUSL-1.1
"""Was das Video sagt, als Daten — Text, Quelle, Bild.

Jede Zahl, die gesprochen oder gezeigt wird, traegt hier ihren Beleg als
`source`. Das ist keine Zierde: Ein Video altert schneller als ein Repository,
und die einzige Verteidigung dagegen ist, dass jede Behauptung auf eine Datei
zeigt, die sich aendert, wenn die Messung sich aendert.

Der Sprechertext steht vollstaendig hier und nicht im Rendercode, damit eine
Aenderung am Wortlaut keine Aenderung am Bild erzwingt — und damit der
Untertitel aus derselben Quelle kommt wie die Stimme. Zwei Fassungen desselben
Satzes waeren zwei Fassungen, die auseinanderlaufen koennen.
"""
from __future__ import annotations

from dataclasses import dataclass, field

#: Wo die Zahlen herkommen. Der Schluessel steht in `Scene.source`.
SOURCES = {
    "gate-m3": "docs/benchmark/messkette-2026-09-12.md",
    "wp26": "docs/benchmark/wp26.md",
    "android": "docs/benchmark/android-gpu.md",
    "ramp": "docs/benchmark/load-ramp.md",
    "run": "InferenceQoS-runtime/measure-morgen-2026-09-12/a-gate-r1.txt",
    "doctor": "InferenceQoS-runtime/measure-nachlauf-2026-09-11e/pre4-doctor.txt",
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
    #: Kapiteltitel unter dem Video. Steht hier und nicht im Baucode, weil ein
    #: Kapitel dasselbe verspricht wie die Szene — und `scene.key` ("stale",
    #: "price") ist unsere Sprache, nicht die des Zuschauers.
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
        narration="One GPU. Several models. And only the newest frame is worth anything.",
        visual="title",
        chapter="One GPU, several models",
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
        key="stale",
        narration=(
            "An inference server schedules requests. It does not know that frame "
            "four is worthless the moment frame five exists. So it computes it "
            "anyway: carefully, correctly, and too late to be used."
        ),
        visual="stale",
        chapter="Why the inference server cannot fix it",
        pause=0.7,
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
        key="measured",
        narration=(
            "A real run on one laptop GPU, against a tuned Triton with priorities "
            "and the same shared memory path. Coverage, the share of control "
            "cycles where a fresh result was there, goes from eighty-five percent "
            "to one hundred for the detector. Three runs, all of them in the "
            "repository."
        ),
        visual="terminal_run",
        chapter="Measured against a tuned Triton",
        source="run",
        pause=0.8,
        short_cut=True,
    ),
    Scene(
        key="price",
        narration=(
            "And here is what it costs, because leaving this out would make the "
            "rest worthless. In that same run the background language model gets "
            "nothing. A ninety-five millisecond block does not fit next to a "
            "thirty-three millisecond period, with or without us. For models that "
            "can be split, the trade becomes visible instead. In a separate run, "
            "two completed generations become forty, and the detector gives up "
            "seven points of coverage."
        ),
        visual="price",
        chapter="What it costs",
        source="wp26",
        pause=0.8,
    ),
    Scene(
        key="limits",
        narration=(
            "Three cases where we would tell you not to use it. Below saturation, "
            "your server is fine. A single stream: fifty lines in your own client "
            "do most of it. And where the bottleneck is transport and CPU rather "
            "than GPU time, the backend overlaps better than we serialise. We "
            "measured that on a phone."
        ),
        visual="limits",
        chapter="When not to use it",
        source="android",
        pause=0.7,
    ),
    Scene(
        key="try",
        narration=(
            "Point it at the server you already run. It checks your configuration "
            "first and says what will not work before you start. Free for "
            "evaluation, and for up to three devices in production."
        ),
        visual="terminal_doctor",
        chapter="Trying it on the server you already run",
        source="doctor",
        pause=0.7,
    ),
    Scene(
        key="close",
        narration=(
            "Vigilant Inference Governor. The numbers, the method, and the runs "
            "that failed are all in the repository."
        ),
        visual="close",
        chapter="Where the numbers live",
        pause=1.2,
    ),
]

#: Nur fuer den Kurzschnitt: eine stumme Schlusskarte.
#:
#: Fuer LinkedIn bleiben rund 45 Sekunden, und in die passen Problem und
#: Messung — aber nicht der gesprochene Satz ueber den Preis. Ihn wegzulassen
#: waere genau die Werbung, die dieses Skript verbietet. Also steht er dort
#: als Text. Ein kurzer Schnitt darf kuerzer sein, nicht unehrlicher.
SHORT_TAIL = Scene(
    key="short-tail",
    narration="",
    visual="short_tail",
    hold=5.5,
    pause=0.0,
)

#: Kapitelmarken und Beschreibung entstehen aus denselben Szenen; siehe
#: make_video.py. Hier stehen nur die Texte, die kein Szenentext sind.
YOUTUBE_TITLE = "Vigilant Inference Governor — keeping the newest camera frame alive on a shared GPU"

YOUTUBE_TAGS = [
    "edge ai", "inference", "gpu scheduling", "robotics", "triton inference server",
    "nvidia jetson", "real time", "computer vision", "ros 2", "latency",
    "age of information", "machine learning infrastructure",
]
