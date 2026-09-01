#!/usr/bin/env python3
"""Wertet einen Dauerlauf aus: erste gegen letzte Stunde.

Ein Dauerlauf beantwortet nicht "funktioniert es", sondern "funktioniert es
noch". Die Auswertung vergleicht deshalb konsequent Anfang und Ende, statt
Durchschnitte ueber den ganzen Lauf zu bilden — ein Mittelwert versteckt
genau die Drift, die gesucht wird.
"""

import csv
import re
import sys
from collections import defaultdict


def read_streams(path):
    with open(path) as f:
        return [r for r in csv.DictReader(f)]


def summarise(rows, label):
    by_stream = defaultdict(list)
    for r in rows:
        by_stream[r["stream"]].append(r)
    print(f"\n  {label}")
    print("  Strom     | unabgedeckt ‰ | AoI p95 ms | geliefert | abgewiesen")
    print("  ----------|---------------|------------|-----------|-----------")
    for name, rs in sorted(by_stream.items()):
        unc = sorted(int(r["uncovered_permille"]) for r in rs)
        aoi = sorted(int(r["aoi_p95_ms"]) for r in rs)
        med = lambda v: v[len(v) // 2] if v else 0
        print(
            f"  {name:<9} | {med(unc):>13} | {med(aoi):>10} | "
            f"{sum(int(r['delivered']) for r in rs):>9} | "
            f"{sum(int(r['rejected']) for r in rs):>10}"
        )


def memory(rows):
    pts = [(int(r["elapsed_s"]), int(r["rss_kb"])) for r in rows]
    if not pts:
        return
    first, last = pts[0], pts[-1]
    seconds = last[0] - first[0]
    growth = last[1] - first[1]
    print(f"\n  Speicher: {first[1]} kB -> {last[1]} kB ({growth:+d} kB "
          f"in {seconds/3600:.1f} h)")
    if seconds < 3600:
        # Die ersten Minuten belegt jeder Allokator Arenen, die er behaelt.
        # Daraus eine Rate je Stunde zu bilden, ergaebe eine Zahl, die nur
        # das Aufwaermen hochrechnet.
        print("  Lauf zu kurz fuer eine Aussage ueber Wachstum.")
        return
    # Der spaete Teil zeigt das Dauerverhalten; der Anfang ist Aufwaermen.
    warm = [p for p in pts if p[0] >= 3600]
    if len(warm) >= 2:
        rate = (warm[-1][1] - warm[0][1]) / max((warm[-1][0] - warm[0][0]) / 3600, 1e-9)
        print(f"  Nach der ersten Stunde: {rate:+.0f} kB/h")
        if rate > 1024:
            print("  BEFUND Wachstum ueber 1 MB/h im eingeschwungenen Zustand.")
            print("         Das ist ein Leck und kein Aufwaermen.")
        else:
            print("  Kein nennenswertes Wachstum im eingeschwungenen Zustand.")


def margins(path):
    """Die Margen aus dem Metrikprotokoll, erste gegen letzte Ablesung."""
    pat = re.compile(r'onetimer_margin_percent\{model="(\d+)"\} (\d+)')
    first, last = {}, {}
    try:
        with open(path) as f:
            for line in f:
                m = pat.match(line)
                if m:
                    model, value = m.group(1), int(m.group(2))
                    first.setdefault(model, value)
                    last[model] = value
    except FileNotFoundError:
        return
    changed = {k: (first[k], last[k]) for k in last if first.get(k) != last[k]}
    print("\n  Sicherheitsmargen (Modell: Start -> Ende)")
    if not changed:
        print("  unveraendert — der Estimator musste nicht nachregeln.")
    else:
        for model, (a, b) in sorted(changed.items()):
            arrow = "gestiegen" if b > a else "gesunken"
            print(f"  Modell {model}: {a} % -> {b} % ({arrow})")
            if b > a:
                print("  BEFUND Eine ueber den Lauf gestiegene Marge heisst, dass sich "
                      "die\n         Prognose wiederholt verschaetzt hat (ADR-0013).")


def main():
    directory = sys.argv[1] if len(sys.argv) > 1 else "."
    rows = read_streams(f"{directory}/streams.csv")
    if not rows:
        print("Keine Daten.")
        return
    total = int(rows[-1]["elapsed_s"])
    print(f"Dauerlauf ueber {total/3600:.1f} h, {len(rows)} Zeilen")

    hour = 3600
    early = [r for r in rows if int(r["elapsed_s"]) < hour]
    late = [r for r in rows if int(r["elapsed_s"]) >= total - hour]
    if not early or total < 2 * hour:
        summarise(rows, "Gesamter Lauf (zu kurz fuer einen Vergleich)")
    else:
        summarise(early, "Erste Stunde")
        summarise(late, "Letzte Stunde")

    burst = [r for r in rows if r["load"] != rows[0]["load"]]
    if burst:
        summarise(burst, "Nur die Lastspitzen")

    memory(rows)
    margins(f"{directory}/metrics.log")

    outages = sum(1 for r in rows if int(r["delivered"]) == 0)
    if outages:
        print(f"\n  BEFUND {outages} Fenster ohne jede Lieferung.")


if __name__ == "__main__":
    main()
