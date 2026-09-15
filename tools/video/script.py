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
    "run": "InferenceQoS-runtime/gate-m3-r03/gate-r1.txt",
    "doctor": "InferenceQoS-runtime/measure-nachlauf-2026-09-11e/pre4-doctor.txt",
    # Gelesen wird die JSON, nicht der Bericht daneben: dessen Urteil steht
    # heute auf Deutsch, die strukturierten Felder sind englisch. Das Bild
    # haengt damit nicht daran, wann der deutsche Satz repariert wird.
    # Der Lauf mit dem ausgelieferten Stand vom 15.09. (`contaminated: false`,
    # englisches Urteil, Verweigerung vorn). Die Laeufe 15, 15c und 15d sind
    # verschmutzt; 15e ist sauber, aber aelter als die Reparaturen.
    "autotune": "InferenceQoS-runtime/autotune-laptop-2026-09-15f/qualification.json",
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
        narration=(
            "Your GPU is working flat out, and part of that work is on frames "
            "your robot has already thrown away."
        ),
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
        key="usecases",
        narration=(
            "Two places this belongs. A humanoid robot: vision in the control "
            "loop, a language model planning the next move, one GPU for both. "
            "The governor keeps the thinking from blocking the seeing. Or driver "
            "assistance in pre-development: several cameras, detection at a fixed "
            "rate, a slower analysis beside it. Both are plausible pictures, not "
            "customer deployments. We claim nothing about certification or hard "
            "real time — that argument stays with the manufacturer."
        ),
        visual="usecases",
        chapter="Where this belongs: robot and vehicle",
        pause=0.8,
    ),
    # ------------------------------------------------------------------
    # Die austauschbare Szene.
    #
    # Sie traegt die Kernzahl, und die Kernzahl haengt am gezeigten Lauf.
    # Sobald das Reproduktionspaket mit echtem Detektor *und* echtem lokalem
    # Sprachmodell gemessen ist, wird hier getauscht: `SOURCES["run"]` auf das
    # neue Protokoll, `SOURCE_DATES["run"]` auf dessen Datum, und die drei
    # Zahlen im Sprechertext auf die des neuen Laufs. Bild und Fussnote
    # ziehen automatisch nach, weil beide aus der Datei lesen.
    #
    # Bis dahin gilt: Hintergrund ist ein nicht unterbrechbarer Block
    # (ResNet-50 Batch 48, rund 95 ms, siehe docs/benchmark/gate-m3.md), kein
    # Sprachmodell. Der Text sagt deshalb "background job", nicht "LLM".
    # ------------------------------------------------------------------
    Scene(
        key="measured",
        narration=(
            "One laptop GPU, against a tuned Triton with priorities and the same "
            "shared memory path. With a real detector and a background job that "
            "cannot be interrupted, the detector goes from eighty-five percent of "
            "control cycles to ninety-nine. Twenty times fewer missed cycles. "
            "Three runs, and every log is in the repository."
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
            "The price is in the same table. That background block never runs: "
            "ninety-five milliseconds do not fit beside a thirty-three "
            "millisecond period, with us or without us. For models that can be "
            "split, the trade becomes a dial you set. In a separate run, two "
            "completed generations become forty, and the detector moves from "
            "ninety-eight percent to ninety-one."
        ),
        visual="price",
        chapter="What it costs",
        source="wp26",
        pause=0.8,
    ),
    Scene(
        key="limits",
        narration=(
            "Three cases where we would say no. If your GPU is not busy enough "
            "to queue, your server is already fine. A single stream? Fifty lines "
            "in your own client do most of this. And where the bottleneck is "
            "moving data rather than GPU time, the backend overlaps better than "
            "we serialise — we measured that on a phone."
        ),
        visual="limits",
        chapter="When not to use it",
        source="android",
        pause=0.7,
    ),
    # ------------------------------------------------------------------
    # Die Szene, die am laengsten gefehlt hat.
    #
    # Sie stand lange als Konstante daneben und blieb draussen, weil es den
    # Befehl nicht gab und ein Video keine Funktion versprechen darf, die
    # niemand starten kann. Jetzt gibt es ihn.
    #
    # Das Bild zeigt einen echten Lauf, verworfene Messreihen eingeschlossen.
    # Wer nur den gelungenen Teil zeigt, wirbt; wer auch die Verweigerung
    # zeigt, wird geglaubt.
    # ------------------------------------------------------------------
    Scene(
        key="autotune",
        # Die Zahlen im Text stehen im Bild als FIT-Zeile, gelesen aus
        # `fit_verdict` derselben Datei: 996 und 0, 537 und 1000 Promille.
        narration=(
            "You should not have to trust our laptop. One command, vig autotune, "
            "measures your own machine: runtimes, concurrency, interference. "
            "Then it answers the only question that matters: is the governor "
            "worth it here? On ours, above ninety percent load, the protected "
            "stream goes from missing almost every cycle to missing none, and "
            "the background pays for all of it. Two of four series were thrown "
            "away because the card changed its power state, so it refused to "
            "sign off. And if you don't need us, it says that too."
        ),
        visual="autotune",
        chapter="vig autotune: measure your own machine",
        source="autotune",
        pause=0.8,
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
            "Faster GPUs compute the past faster. Vigilant stops computing it. "
            "Run vig autotune on the machine you already have, and let the "
            "numbers decide."
        ),
        visual="close",
        chapter="Where the numbers live",
        pause=1.2,
    ),
]

#: Die Autotune-Szene stand frueher hier als Konstante und hing bewusst nicht
#: in SCENES: Es gab den Befehl nicht, und ein Video darf keine Funktion
#: versprechen, die niemand starten kann. Seit `vig autotune` existiert, steht
#: sie oben zwischen "limits" und "try" — an der Stelle, die dieser Kommentar
#: ihr damals zugewiesen hat. Der Wortlaut ist unveraendert uebernommen.

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
YOUTUBE_TITLE = "Stop computing the past: an inference governor for shared edge GPUs, with vig autotune"

YOUTUBE_TAGS = [
    "edge ai", "inference", "gpu scheduling", "robotics", "triton inference server",
    "nvidia jetson", "real time", "computer vision", "ros 2", "latency",
    "age of information", "machine learning infrastructure",
]
