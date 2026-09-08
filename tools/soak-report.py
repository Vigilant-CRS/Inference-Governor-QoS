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


def response_age_ms(row):
    """Das p95-Antwortalter einer CSV-Zeile.

    Die Spalte hiess frueher ``aoi_p95_ms``. Sie misst das Alter der
    ausgelieferten Antworten, nicht das Informationsalter ueber die Zeit, und
    heisst deshalb jetzt ``response_age_p95_ms``. Beide Namen werden gelesen:
    die vorhandenen Achtstundenmessungen tragen noch den alten Kopf, und ein
    Auswertungswerkzeug, das alte Messreihen nicht mehr oeffnet, entwertet
    genau die Daten, wegen derer es existiert.
    """
    for key in ("response_age_p95_ms", "aoi_p95_ms"):
        if key in row:
            return row[key]
    raise KeyError("weder response_age_p95_ms noch aoi_p95_ms in der Zeile")


def summarise(rows, label):
    by_stream = defaultdict(list)
    for r in rows:
        by_stream[r["stream"]].append(r)
    print(f"\n  {label}")
    print("  Strom     | unabgedeckt ‰ | Antwortalter p95 ms | geliefert | abgewiesen")
    print("  ----------|---------------|---------------------|-----------|-----------")
    for name, rs in sorted(by_stream.items()):
        unc = sorted(int(r["uncovered_permille"]) for r in rs)
        aoi = sorted(int(response_age_ms(r)) for r in rs)
        med = lambda v: v[len(v) // 2] if v else 0
        print(
            f"  {name:<9} | {med(unc):>13} | {med(aoi):>19} | "
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
    pat = re.compile(r'vig_margin_percent\{model="(\d+)"\} (\d+)')
    first, last = {}, {}
    seen = defaultdict(list)
    try:
        with open(path) as f:
            for line in f:
                m = pat.match(line)
                if m:
                    model, value = m.group(1), int(m.group(2))
                    first.setdefault(model, value)
                    last[model] = value
                    seen[model].append(value)
    except FileNotFoundError:
        return
    print("\n  Sicherheitsmargen (Modell: Start -> Ende, Hoechstwert, Ausschlaege)")
    for model in sorted(last, key=int):
        if len(set(seen[model])) == 1 and int(model) > 2:
            continue  # unbenutzter Slot eines aelteren Laufs
        a, b = first[model], last[model]
        series = seen[model]
        peak = max(series)
        # Nur Start und Ende zu vergleichen verschweigt genau das Interessante:
        # der Regler zieht nach einer Unterprognose schnell hoch und faellt
        # langsam zurueck. Wer nur die Endpunkte liest, sieht eine Excursion
        # nicht, die dazwischen lag.
        excursions = sum(1 for v in series if v > a)
        print(f"  Modell {model}: {a} % -> {b} %, Hoechstwert {peak} %, "
              f"{excursions} von {len(series)} Ablesungen erhoeht")
        if b > a:
            print("  BEFUND Die Marge endet ueber ihrem Startwert. Der Regler ist "
                  "nicht\n         zurueckgekommen — die Prognose verschaetzt sich "
                  "wiederholt (ADR-0013).")
        elif peak > a:
            print("         Der Regler hat angezogen und ist zurueckgekommen. "
                  "Genau so\n         ist er gedacht (ADR-0013).")


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
