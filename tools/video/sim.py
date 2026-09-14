# SPDX-FileCopyrightText: 2026 Vigilant e.K.
# SPDX-License-Identifier: BUSL-1.1
"""Die Animation wird gerechnet, nicht gezeichnet.

`tools/render_terminal.py` im Nachbarprojekt hat eine Regel, die hier
uebernommen wird: Ein Bild, das eine Behauptung illustriert, wird aus der
Sache erzeugt, die es behauptet — sonst driftet es davon weg, ohne dass ein
Test es merkt. Fuer eine Terminalausgabe heisst das: den Befehl ausfuehren.
Fuer die Animation heisst es: die beiden Politiken wirklich durchrechnen.

Deshalb steht hier ein winziger, deterministischer Simulator mit denselben
drei Groessen, die im Video gesprochen werden — Kameraperiode, Detektorlaufzeit
und ein unteilbarer Hintergrundblock. Was das Bild zeigt, ist sein Ergebnis.
Insbesondere: Dass der Hintergrundblock unter dem Governor gar nicht erst
startet, ist keine gezeichnete Behauptung, sondern faellt aus der Regel heraus
— und es ist dieselbe Aussage, die Gate M3 misst (VLM 0 % Abdeckung).

Der Simulator ist bewusst nicht der Scheduler des Produkts. Er bildet die eine
Entscheidung ab, um die es im Video geht, und nichts sonst.
"""
from __future__ import annotations

from dataclasses import dataclass


@dataclass
class Block:
    """Ein Stueck Rechenzeit auf der GPU."""

    start: float
    end: float
    kind: str          # "detector" | "background"
    frame: int | None  # Nummer des Kamerabildes, wenn es eines ist
    late: bool = False  # fertig, als das Ergebnis schon veraltet war


@dataclass
class Drop:
    """Ein Bild, das nie gerechnet wurde, und warum."""

    at: float
    frame: int
    reason: str  # "superseded" | "too late"


@dataclass
class Held:
    """Hintergrundarbeit, die der Governor zurueckgehalten hat."""

    at: float
    until: float


@dataclass
class Trace:
    arrivals: list[tuple[float, int]]
    blocks: list[Block]
    drops: list[Drop]
    held: list[Held]
    horizon: float


def simulate(policy: str, *, period: float, detector: float, background: float,
             horizon: float = 400.0, background_at: float = 36.0) -> Trace:
    """Rechnet eine der beiden Politiken durch.

    `fifo` ist der Inferenzserver ohne Wissen ueber Frische: Alles, was
    ankommt, wird der Reihe nach gerechnet. `governor` verwirft ein Bild,
    sobald ein neueres existiert, und startet Hintergrundarbeit nur, wenn sie
    vor der naechsten geschuetzten Ankunft fertig waere.
    """
    arrivals = [(i * period, i + 1) for i in range(int(horizon // period) + 1)]
    blocks: list[Block] = []
    drops: list[Drop] = []
    held: list[Held] = []

    queue: list[tuple[float, int]] = []
    background_pending = True
    now = 0.0
    index = 0

    while now < horizon:
        # Alles einsammeln, was bis jetzt angekommen ist.
        while index < len(arrivals) and arrivals[index][0] <= now:
            queue.append(arrivals[index])
            index += 1

        if policy == "governor" and len(queue) > 1:
            # Nur das neueste Bild ist noch etwas wert (ADR-0005).
            for at, frame in queue[:-1]:
                drops.append(Drop(at=now, frame=frame, reason="superseded"))
            queue = queue[-1:]

        if queue:
            arrival_time, frame = queue.pop(0)
            end = now + detector
            # Veraltet, wenn bei Fertigstellung schon ein neueres Bild da ist.
            stale = any(a <= end for a, f in arrivals if f > frame)
            blocks.append(Block(now, end, "detector", frame, late=stale and policy == "fifo"))
            now = end
            continue

        if background_pending and now >= background_at:
            next_arrival = next((a for a, _ in arrivals if a > now), horizon)
            if policy == "governor" and now + background > next_arrival:
                # Wuerde die naechste geschuetzte Ankunft blockieren.
                held.append(Held(at=now, until=next_arrival))
                now = next_arrival
                continue
            blocks.append(Block(now, now + background, "background", None))
            now += background
            background_pending = False
            continue

        # Nichts zu tun: zur naechsten Ankunft springen.
        nxt = next((a for a, _ in arrivals if a > now), horizon)
        now = min(nxt, horizon)

    return Trace(arrivals=arrivals, blocks=blocks, drops=drops, held=held,
                 horizon=horizon)


def worst_wait(trace: Trace, period: float) -> float:
    """Laengste Zeit, in der kein frisches Detektorergebnis vorlag.

    Die Zahl steht im Bild und muss deshalb aus derselben Rechnung kommen wie
    die Balken daneben.
    """
    ready = [b.end for b in trace.blocks if b.kind == "detector" and not b.late]
    if not ready:
        return trace.horizon
    gaps = [b - a for a, b in zip(ready, ready[1:])]
    return max(gaps + [ready[0]])
