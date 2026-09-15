# SPDX-FileCopyrightText: 2026 Vigilant e.K.
# SPDX-License-Identifier: BUSL-1.1
"""Was die Videos sagen, als Daten — Text, Quelle, Bild.

Jede Zahl, die gesprochen oder gezeigt wird, traegt hier ihren Beleg als
`source`. Das ist keine Zierde: Ein Video altert schneller als ein Repository,
und die einzige Verteidigung dagegen ist, dass jede Behauptung auf eine Datei
zeigt, die sich aendert, wenn die Messung sich aendert.

Der Sprechertext steht vollstaendig hier und nicht im Rendercode, damit eine
Aenderung am Wortlaut keine Aenderung am Bild erzwingt — und damit der
Untertitel aus derselben Quelle kommt wie die Stimme.

**Zwei Schnitte.** `SCENES` ist das Erklaervideo (YouTube, Projektseite),
`PROMO_SCENES` der Werbe-Cut (~75 s). Beide beginnen beim Markt — Roboter und
Fahrzeuge buendeln ihre Software auf zentralen Rechnern, viele Modelle teilen
sich einen Chip — und enden bei `vig autotune`. Die Grenzen (wann es nichts
bringt, was es kostet, was nicht belegt ist) stehen vollstaendig in README,
STATUS und den Berichten. Ein Urteil wird nie ohne seine Last gezeigt.
"""
from __future__ import annotations

from dataclasses import dataclass, field

#: Die Plattformen der Geraeteszene. Name, Chip und Backend stehen hier mit
#: ihrem Beleg (`docs/benchmark/android-gpu.md`, `validierung-autotune.md`);
#: was autotune dort gemessen und eingestellt hat, liest das Bild aus der
#: jeweiligen `qualification.json` des genannten Laufs.
DEVICES = [
    {"name": "Laptop GPU", "chip": "NVIDIA GeForce RTX 3070",
     "backend": "NVIDIA Triton 2.70", "run": "autotune-laptop-2026-09-15f"},
    {"name": "Pixel 2", "chip": "Snapdragon 835 · Adreno 540",
     "backend": "TensorFlow Lite, GPU delegate", "run": "autotune-pixel2-2026-09-15-usecase-fenster"},
    {"name": "Pixel 5", "chip": "Snapdragon 765G · Adreno 620",
     "backend": "TensorFlow Lite, GPU delegate", "run": "autotune-pixel5-2026-09-15-usecase-fenster"},
]

#: Aufgezeichnete Demo-Clips (tools/demo/render.py): links der direkte Weg,
#: rechts der Governor, dieselben Frames nacheinander auf derselben GPU. Ein
#: Eintrag braucht nur den Laufordner (relativ zu InferenceQoS-runtime/), den
#: Clip und seine zwei Zeitachsen; alle Zahlen in Bild und Sprechertext rechnet
#: `report.demo_facts` daraus. `subject` ist das Wort fuer die Maschine im
#: Sprechertext, `attribution` die Namensnennung des Materials (CC BY verlangt
#: sie; sie steht in der Szene und in der YouTube-Beschreibung).
#:
#: Der Clip wird referenziert, nicht kopiert: wird er neu gerendert, zieht das
#: Video beim naechsten Bau nach. Weitere Clips (Lieferroboter, Humanoid) mit
#: derselben Ordnerstruktur sind ein weiterer Eintrag und eine Szene mit
#: `data={"demo": <Schluessel>}`.
DEMOS = {
    "krakow-cams4": {
        "run": "demo/runs/krakow-cams4-1825",
        "clip": "demo.mp4",
        "left": "direct.jsonl",
        "right": "governed.jsonl",
        #: Ab welcher Sekunde der Zeitachsen der Clip gerendert wurde
        #: (render.py --start-s). Der Clip zeigt "0.0 s" im ersten Bild.
        "render_start_s": 0.0,
        "subject": "vehicle",
        "report": "docs/benchmark/demo-2026-09-15.md",
        "attribution": ("Video: 'City Driving 4K: Kraków Poland 2024' by Relaxing Roads 4K, "
                        "CC BY 3.0, via Wikimedia Commons. RF-DETR (Apache-2.0), "
                        "SmolVLM (Apache-2.0)."),
    },
    "sidewalk-cams4": {
        "run": "demo/runs/sidewalk-cams4-2033",
        "clip": "demo.mp4",
        "left": "direct.jsonl",
        "right": "governed.jsonl",
        "render_start_s": 0.0,
        "subject": "sidewalk robot",
        "report": "docs/benchmark/demo-2026-09-15.md",
        "attribution": ("Video: 'Walking in EDINBURGH – Scotland (UK)' by POPtravel, "
                        "CC BY 3.0, via Wikimedia Commons. RF-DETR (Apache-2.0), "
                        "SmolVLM (Apache-2.0)."),
    },
    "humanoid-cams4": {
        "run": "demo/runs/humanoid-cams4-2039",
        "clip": "demo.mp4",
        "left": "direct.jsonl",
        "right": "governed.jsonl",
        "render_start_s": 0.0,
        "subject": "humanoid robot",
        "report": "docs/benchmark/demo-2026-09-15.md",
        "attribution": ("TUM RGB-D benchmark (fr3/walking_halfsphere), Computer Vision "
                        "Group, TU Munich, CC BY 4.0. RF-DETR (Apache-2.0), "
                        "SmolVLM (Apache-2.0)."),
    },
}

#: Wo die Zahlen herkommen. Der Schluessel steht in `Scene.source`.
SOURCES = {
    "gate-m3": "docs/benchmark/messkette-2026-09-12.md",
    "run": "InferenceQoS-runtime/messungen/gate-m3-r03/gate-r1.txt",
    "doctor": "InferenceQoS-runtime/messungen/measure-nachlauf-2026-09-11e/pre4-doctor.txt",
    "autotune": "InferenceQoS-runtime/messungen/autotune-laptop-2026-09-15f/qualification.json",
    # Der Tuning-Lauf mit Fenstern in Takten (mindestens 200 je Punkt), auf dem
    # Anwendungsfall Lieferroboter (docs/use-cases.md). Der fruehere Lauf
    # `…-tuned-saturated` mass mit 10-s-Fenstern, also 24 bis 27 Takten.
    "tuned": "InferenceQoS-runtime/messungen/autotune-pixel2-2026-09-15-usecase-fenster/qualification.json",
    "devices": ", ".join(f"InferenceQoS-runtime/messungen/{d['run']}/qualification.json"
                         for d in DEVICES),
}
for _key, _demo in DEMOS.items():
    SOURCES[f"demo-{_key}"] = ", ".join(
        f"InferenceQoS-runtime/{_demo['run']}/{_demo[part]}"
        for part in ("clip", "left", "right")) + f" (report: {_demo['report']})"

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
    #: False: die Szene setzt das vorige Kapitel fort. YouTube verlangt je
    #: Kapitel mindestens zehn Sekunden; die kurzen Demo-Szenen fuer
    #: Lieferroboter und Humanoid gehoeren deshalb zum Kapitel davor.
    chapter_break: bool = True
    #: Sekunden Stille nach der Szene, damit nichts hetzt.
    pause: float = 0.6
    #: Nur fuer stumme Szenen: feste Dauer.
    hold: float = 0.0
    #: Freie Parameter fuer den Renderer.
    data: dict = field(default_factory=dict)


#: Die Kameraperiode, die Detektorlaufzeit und der unteilbare Hintergrundblock,
#: mit denen die Animation rechnet. Es sind die Groessen aus dem README-Beispiel
#: und aus der Gate-M3-Konfiguration, nicht erfundene runde Zahlen.
PERIOD_MS = 33
DETECTOR_MS = 15
BACKGROUND_MS = 95

# ------------------------------------------------------------ Erklaervideo --

SCENES = [
    Scene(
        key="trend",
        narration=(
            "Robots and cars are moving to central computers. Vision, planning "
            "and language models now share one chip, and they all want it at "
            "the same time."
        ),
        visual="trend",
        chapter="Many models, one chip",
        pause=0.8,
    ),
    Scene(
        key="problem",
        narration=(
            "A robot camera gives you a frame every thirty-three milliseconds. "
            "Your detector needs fifteen. That fits, until something else wants "
            "the same chip. Now the detector waits behind a block it cannot "
            "interrupt, and by the time its answer arrives, the robot has "
            "already moved."
        ),
        visual="timeline_fifo",
        chapter="What goes wrong at 33 milliseconds",
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
        key="capabilities",
        narration=(
            "Four decisions, all made before a request reaches the chip. A newer "
            "frame replaces an older one that is still waiting. Work that would "
            "finish too late is never started. A long background job waits when "
            "protected work is due. And when time runs short, a smaller model "
            "variant takes over."
        ),
        visual="capabilities",
        chapter="Four decisions before the chip",
        pause=0.7,
    ),
    Scene(
        key="usecases",
        narration=(
            "It is built for exactly that kind of machine. A humanoid robot, "
            "whose vision must not wait behind its planner. Or a vehicle's "
            "central computer, running several cameras and a slower scene "
            "analysis on the same chip."
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
    ),
    # Die Demo-Szenen: Fahrzeug, Lieferroboter, Humanoid. Sie ersetzen
    # `measured` nicht: dort steht die Zahl gegen einen *getunten* Triton mit
    # einem unteilbaren Block, hier ein anderer, ueberlasteter Aufbau (vier
    # Kameras plus Sprachmodell) — zu sehen statt zu lesen. Die Felder in
    # geschweiften Klammern fuellt `report.demo_facts` aus den Zeitachsen des
    # jeweiligen Clips; `report.assert_demo_claims` bricht ab, wenn ein neuer
    # Lauf "blind" oder "fresh" nicht mehr hergibt. Der letzte Satz der Folge
    # bleibt: ohne Ueberlastung hilft der Governor nicht
    # (docs/benchmark/demo-2026-09-15.md).
    Scene(
        key="demo",
        narration=(
            "Here is what that looks like. {Cameras} cameras and a language model "
            "share one {gpu_kind}: more work than the chip can do. Triton alone "
            "computes every frame in arrival order. Its detections arrive "
            "{left_age_spoken} late, so the {subject} is effectively blind. With "
            "Vigilant, the {stream} camera stays fresh in {right_fresh_spoken}. The "
            "price is visible too: the language model answered {right_answers_spoken}. "
            "Without the governor, {left_answers_spoken} in {clip_spoken}."
        ),
        visual="demo_clip",
        chapter="On camera: {cameras_word} cameras, one GPU",
        source="demo-krakow-cams4",
        pause=0.6,
        # align_end: der Ausschnitt endet mit dem Clip, damit die
        # Zusammenfassungskarte der letzten zwei Sekunden unter dem Satz ueber
        # den Preis steht. start_s ist dann der frueheste Anfang.
        data={"demo": "krakow-cams4", "start_s": 6.0, "align_end": True,
              "caption": "Recorded on one {gpu_kind} · same frames, back to back"},
    ),
    Scene(
        key="demo-sidewalk",
        narration=(
            "On a {subject}, with {cameras_word} cameras as well: the {stream} camera "
            "is fresh in {left_pct_spoken} of cycles with Triton alone, and "
            "{right_pct_spoken} with Vigilant."
        ),
        visual="demo_clip",
        source="demo-sidewalk-cams4",
        chapter_break=False,
        pause=0.5,
        data={"demo": "sidewalk-cams4", "start_s": 14.0,
              "caption": "Recorded on one {gpu_kind} · same frames, back to back"},
    ),
    Scene(
        key="demo-humanoid",
        narration=(
            "And the {stream} camera of a {subject}: {left_pct_spoken} with Triton "
            "alone, {right_pct_spoken} with Vigilant. With room to spare on the chip, "
            "Triton alone keeps up, and the governor does not help."
        ),
        visual="demo_clip",
        source="demo-humanoid-cams4",
        chapter_break=False,
        pause=0.8,
        data={"demo": "humanoid-cams4", "start_s": 14.0,
              "caption": "Recorded on one {gpu_kind} · same frames, back to back"},
    ),
    Scene(
        key="devices",
        narration=(
            "And it is not tied to one machine. The same governor runs in front "
            "of Triton on an NVIDIA GPU, and in front of TensorFlow Lite on the "
            "Adreno GPUs of two Android devices. On each of them, vig autotune "
            "measured the hardware and set the governor up for it."
        ),
        visual="devices",
        chapter="Tested on edge hardware",
        source="devices",
        pause=0.8,
    ),
    Scene(
        key="autotune",
        narration=(
            "You do not have to take our numbers, or our settings. One command, "
            "vig autotune, measures your models on your machine, tries the "
            "governor's settings against your contracts, and only keeps what "
            "holds up when it runs again. On a Pixel 2 carrying a delivery "
            "robot's load, the governor cut the detector's missed cycles by two "
            "thirds, paid for by the lower-priority streams."
        ),
        visual="tuning",
        chapter="vig autotune: tuned for your machine",
        source="tuned",
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
            "Faster chips compute the past faster. Vigilant stops computing it. "
            "Run vig autotune on the machine you already have, and let it tune "
            "the governor for your load."
        ),
        visual="close",
        chapter="Stop computing the past",
        pause=1.2,
    ),
]

# ------------------------------------------------------------- Werbe-Cut --

#: Rund 75 Sekunden: Markt, Problem, Loesung, Beleg, autotune, Schluss. Eigene,
#: kuerzere Saetze — kein Zusammenschnitt aus dem Erklaervideo, weil ein
#: Werbe-Cut einen anderen Takt braucht als eine Erklaerung.
PROMO_SCENES = [
    Scene(
        key="trend",
        narration=(
            "Robots and cars are moving to one central computer. Vision, "
            "planning and language models now share the same chip."
        ),
        visual="trend",
        pause=0.6,
    ),
    Scene(
        key="problem",
        narration=(
            "And when they share it, the camera waits. By the time the detector "
            "answers, the robot has already moved."
        ),
        visual="timeline_fifo",
        pause=0.5,
    ),
    Scene(
        key="governor",
        narration=(
            "Vigilant decides before every dispatch what is still worth "
            "computing. Stale frames are dropped, protected work goes first, and "
            "a smaller model steps in when time runs short."
        ),
        visual="timeline_governor",
        pause=0.5,
    ),
    Scene(
        key="capabilities",
        narration=(
            "Four decisions, before the chip: drop what is stale, refuse what "
            "would be late, protect what matters, and fit the time you have."
        ),
        visual="capabilities",
        pause=0.6,
    ),
    Scene(
        key="measured",
        narration=(
            "Against a tuned Triton on the same GPU: ninety-nine percent of "
            "control cycles answered in time, instead of eighty-five."
        ),
        visual="terminal_run",
        source="run",
        pause=0.6,
    ),
    Scene(
        key="demo",
        narration=(
            "{Cameras} cameras. One GPU. Triton alone goes blind. Vigilant keeps the "
            "{stream} camera fresh."
        ),
        visual="demo_clip",
        source="demo-krakow-cams4",
        pause=0.5,
        # Ab 14 s: links steht laengst BLIND, rechts laeuft der Detektor mit.
        data={"demo": "krakow-cams4", "start_s": 14.0,
              "caption": "Recorded on one {gpu_kind} · same frames, back to back"},
    ),
    Scene(
        key="devices",
        narration=(
            "Tested on an NVIDIA GPU, and on the Adreno GPUs of two Android "
            "phones."
        ),
        visual="devices",
        source="devices",
        pause=0.8,
    ),
    Scene(
        key="autotune",
        narration=(
            "And vig autotune tunes it for your machine, in one command, and only "
            "keeps what holds up."
        ),
        visual="tuning",
        source="tuned",
        pause=0.6,
    ),
    Scene(
        key="close",
        narration="Stop computing the past. Vigilant Inference Governor.",
        visual="close",
        pause=1.4,
    ),
]

#: Kapitelmarken und Beschreibung entstehen aus denselben Szenen; siehe
#: make_video.py. Hier stehen nur die Texte, die kein Szenentext sind.
YOUTUBE_TITLE = ("Stop Computing the Past — Inference QoS for Robots and Vehicles "
                 "| Vigilant Inference Governor")
PROMO_TITLE = "Many Models, One Chip: Stop Computing the Past | Vigilant Inference Governor"

YOUTUBE_TAGS = [
    "edge ai", "central compute", "software defined vehicle", "humanoid robot",
    "inference", "gpu scheduling", "robotics", "triton inference server",
    "tensorflow lite", "nvidia jetson", "real time", "computer vision", "ros 2",
    "latency", "adas", "machine learning infrastructure",
]
