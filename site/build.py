#!/usr/bin/env python3
"""Baut die Projektseite: aus Markdown wird statisches HTML, sonst nichts.

Warum ein eigenes Skript und kein Generator von der Stange: Die Seite soll
dieselben Dokumente zeigen, die im Repository liegen, ohne dass jemand sie
pflegt oder umschreibt. Jede Datei unter `docs/` wird zu einer Unterseite,
jeder Link von `.md` auf `.html` umgebogen, und fertig. Eine einzige
Abhaengigkeit (`markdown`), keine Javascript-Welt, kein Tracker.

    python3 site/build.py [--out _site]

Mermaid-Bloecke bleiben Codebloecke: die Seite laedt grundsaetzlich kein
fremdes Skript, auch keines von einem CDN. Wer das Diagramm sehen will,
sieht es auf GitHub.
"""

from __future__ import annotations

import argparse
import html
import re
import shutil
import sys
from pathlib import Path

import markdown

WURZEL = Path(__file__).resolve().parent.parent
SEITE = WURZEL / "site"

# Was ausser `docs/` noch als Unterseite erscheint. Der Rest des
# Repositorys bleibt auf GitHub; diese hier braucht man beim Lesen.
ZUSAETZLICH = [
    "README.md",
    "CHANGELOG.md",
    "LICENSING.md",
    "IMPRINT.md",
    "SECURITY.md",
    "CONTRIBUTING.md",
    "THIRD_PARTY_NOTICES.md",
    "Vigilant_Inference_Governor_Specification_v1.0.md",
]

# Welche Dokumente es als Unterseite gibt. Alles andere im Repository —
# Quellcode, Beispiele, die Lizenzdatei — wird auf GitHub verlinkt statt
# kopiert. Wird in `main` gefuellt, bevor gerendert wird.
GEBAUT: set[str] = set()

RUBRIKEN = [
    ("docs/benchmark", "Measurements", "Every number we publish, with its method and its discarded runs."),
    ("docs/adr", "Architecture decisions", "Why the system is built the way it is — including the decisions that went against us."),
    ("docs/analysis", "Analysis", "What holds under which assumptions, and where the proof stops."),
    ("docs/reviews", "External reviews", "Findings from outside, with what became of each."),
    ("docs/spikes", "Spikes", "Experiments that answered one question and ended."),
    ("docs/integrations", "Integrations", "ROS 2 and other ways in."),
    ("docs/pilot", "Pilot", "The internal reference case and its acceptance criteria."),
    ("docs/roadmap", "Roadmap", "Where this is going."),
    ("docs/licensing", "Licensing", "What is free, what needs a contract."),
]

KOPF = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{titel}</title>
<meta name="description" content="{beschreibung}">
<link rel="stylesheet" href="{wurzel}style.css">
<link rel="icon" href="{wurzel}assets/logo.png" type="image/png">
</head>
<body>
<header class="kopf">
  <a class="marke" href="{wurzel}index.html">
    <img src="{wurzel}assets/logo.png" alt="" width="28" height="28">
    <span>Vigilant <strong>Inference Governor</strong></span>
  </a>
  <nav>
    <a href="{wurzel}docs/index.html">Documentation</a>
    <a href="{wurzel}docs/benchmark/index.html">Measurements</a>
    <a href="{wurzel}LICENSING.html">Licence</a>
    <a href="https://github.com/Vigilant-CRS/Inference-Governor-QoS">GitHub</a>
  </nav>
</header>
<main class="{klasse}">
"""

FUSS = """</main>
<footer class="fuss">
  <p><strong>Vigilant e.K.</strong>, Königstraße 22, 70173 Stuttgart, Germany ·
     <a href="mailto:info@vigilant-crs.de">info@vigilant-crs.de</a> ·
     <a href="https://vigilant-crs.de">vigilant-crs.de</a></p>
  <p>Source-available under BUSL-1.1 · <a href="{wurzel}IMPRINT.html">Imprint</a> ·
     <a href="{wurzel}LICENSING.html">Licensing</a> ·
     <a href="{wurzel}SECURITY.html">Security</a></p>
  <p class="klein">Every figure on this site links to the report it comes from.
     Numbers without a report do not belong here.</p>
</footer>
</body>
</html>
"""


def renderer() -> markdown.Markdown:
    return markdown.Markdown(
        extensions=["tables", "fenced_code", "sane_lists", "md_in_html", "toc"],
        output_format="html5",
    )


def titel_aus(text: str, ersatz: str) -> str:
    for zeile in text.splitlines():
        if zeile.startswith("# "):
            return zeile[2:].strip()
    return ersatz


BLOB = "https://github.com/Vigilant-CRS/Inference-Governor-QoS/blob/main/"
TREE = "https://github.com/Vigilant-CRS/Inference-Governor-QoS/tree/main/"

# Links, die schon in der Quelle ins Leere zeigen. Der Build repariert sie
# nicht still, er sammelt sie und meldet sie am Ende.
TOTE: list[tuple[str, str]] = []


def ein_ziel(ziel: str, quelle: Path) -> str:
    """Ein Linkziel fuer die Seite umschreiben.

    Drei Faelle: ein Markdown-Dokument wird zur Unterseite; eine Datei, die es
    im Repository gibt, aber nicht auf der Seite (Quellcode, Beispiele, die
    Lizenz), zeigt auf GitHub; alles andere bleibt, wie es ist.
    """
    if ziel.startswith(("http://", "https://", "mailto:", "#", "data:")):
        return ziel
    pfad, _, anker = ziel.partition("#")
    schwanz = f"#{anker}" if anker else ""
    if not pfad:
        return ziel

    # `datei.rs:288` — in den Reviews die uebliche Schreibweise fuer eine
    # Fundstelle. Auf GitHub ist das eine Zeilenmarke.
    zeile = ""
    treffer = re.fullmatch(r"(.+?):(\d+)", pfad)
    if treffer and not Path(pfad).exists():
        pfad, zeile = treffer.group(1), f"#L{treffer.group(2)}"

    # Erst relativ zum Dokument, dann relativ zur Wurzel: die Reviews
    # schreiben Pfade oft so, wie sie im Repository stehen.
    # Ein Verweis, der aus dem Repository hinausfuehrt (etwa auf die lokale
    # Messablage daneben), ist fuer eine oeffentliche Seite kein Ziel.
    kandidaten = [(quelle.parent / pfad).resolve(), (WURZEL / pfad).resolve()]
    im_repo = next((k for k in kandidaten if k.exists() and k.is_relative_to(WURZEL)), None)

    if im_repo is None:
        TOTE.append((str(quelle.relative_to(WURZEL)), ziel))
        return ziel

    relativ = im_repo.relative_to(WURZEL).as_posix()
    if relativ in GEBAUT:
        name = Path(relativ).name
        seite = relativ[: -len(name)] + "index.html" if name.upper() == "README.MD" else relativ[:-3] + ".html"
        tiefe = len(ziel_fuer(quelle).parts) - 1
        return "../" * tiefe + seite + schwanz
    return (TREE if im_repo.is_dir() else BLOB) + relativ + (zeile or schwanz)


def links_umbiegen(roh: str, quelle: Path) -> str:
    """Markdown-Links und rohe HTML-Verweise gleich behandeln."""
    roh = re.sub(
        r"\[([^\]]*)\]\(([^)\s]+)\)",
        lambda t: f"[{t.group(1)}]({ein_ziel(t.group(2), quelle)})",
        roh,
    )
    return re.sub(
        r'(href|src)="([^"]+)"',
        lambda t: f'{t.group(1)}="{ein_ziel(t.group(2), quelle)}"',
        roh,
    )


def ziel_fuer(quelle: Path) -> Path:
    rel = quelle.relative_to(WURZEL)
    if rel.name.upper() == "README.MD":
        return rel.parent / "index.html"
    return rel.with_suffix(".html")


def schreibe(ziel: Path, inhalt: str, titel: str, beschreibung: str, klasse: str) -> None:
    tiefe = len(ziel.parts) - 1
    wurzel = "../" * tiefe
    ziel_datei = AUSGABE / ziel
    ziel_datei.parent.mkdir(parents=True, exist_ok=True)
    ziel_datei.write_text(
        KOPF.format(
            titel=html.escape(titel),
            beschreibung=html.escape(beschreibung),
            wurzel=wurzel,
            klasse=klasse,
        )
        + inhalt
        + FUSS.format(wurzel=wurzel),
        encoding="utf-8",
    )


def dokumentseite(quelle: Path, md: markdown.Markdown) -> tuple[Path, str]:
    roh = quelle.read_text(encoding="utf-8")
    titel = titel_aus(roh, quelle.stem)
    md.reset()
    inhalt = md.convert(links_umbiegen(roh, quelle))
    ziel = ziel_fuer(quelle)
    schreibe(ziel, f'<article class="dokument">{inhalt}</article>', f"{titel} — Vigilant Inference Governor", titel, "schmal")
    return ziel, titel


def uebersicht(seiten: dict[Path, str]) -> None:
    teile = ['<article class="dokument"><h1>Documentation</h1>']
    teile.append(
        "<p>Everything in the repository, rendered. The measurement reports are "
        "the ones to read first — they contain the runs that failed as well.</p>"
    )
    vergeben: set[Path] = set()
    for praefix, name, untertitel in RUBRIKEN:
        gruppe = sorted(
            (ziel, titel) for ziel, titel in seiten.items() if str(ziel).startswith(praefix + "/")
        )
        if not gruppe:
            continue
        vergeben.update(ziel for ziel, _ in gruppe)
        teile.append(f"<h2>{html.escape(name)}</h2><p>{html.escape(untertitel)}</p><ul>")
        for ziel, titel in gruppe:
            teile.append(f'<li><a href="{"../" * 0}{ziel.relative_to("docs")}">{html.escape(titel)}</a></li>')
        teile.append("</ul>")
    rest = sorted((z, t) for z, t in seiten.items() if z not in vergeben and str(z).startswith("docs/"))
    if rest:
        teile.append("<h2>Everything else</h2><ul>")
        for ziel, titel in rest:
            teile.append(f'<li><a href="{ziel.relative_to("docs")}">{html.escape(titel)}</a></li>')
        teile.append("</ul>")
    oben = sorted((z, t) for z, t in seiten.items() if not str(z).startswith("docs/"))
    if oben:
        teile.append("<h2>Project files</h2><ul>")
        for ziel, titel in oben:
            teile.append(f'<li><a href="../{ziel}">{html.escape(titel)}</a></li>')
        teile.append("</ul>")
    teile.append("</article>")
    schreibe(
        Path("docs/index.html"),
        "".join(teile),
        "Documentation — Vigilant Inference Governor",
        "All reports, decisions and guides of the Vigilant Inference Governor.",
        "schmal",
    )


def main() -> int:
    zerleger = argparse.ArgumentParser(description=__doc__)
    zerleger.add_argument("--out", default="_site", help="Ausgabeverzeichnis (Vorgabe: _site)")
    argumente = zerleger.parse_args()

    global AUSGABE
    AUSGABE = (WURZEL / argumente.out).resolve()
    if AUSGABE.exists():
        shutil.rmtree(AUSGABE)
    AUSGABE.mkdir(parents=True)

    md = renderer()
    seiten: dict[Path, str] = {}

    # Erst wissen, welche Dokumente Unterseiten werden — sonst kann ein Link
    # nicht entscheiden, ob er auf die Seite oder nach GitHub zeigt.
    quellen = sorted(WURZEL.joinpath("docs").rglob("*.md"))
    quellen += [WURZEL / name for name in ZUSAETZLICH if (WURZEL / name).exists()]
    GEBAUT.update(q.relative_to(WURZEL).as_posix() for q in quellen)

    for quelle in quellen:
        ziel, titel = dokumentseite(quelle, md)
        seiten[ziel] = titel

    uebersicht(seiten)

    # Die Startseite ist handgeschrieben; sie erklaert, nicht dokumentiert.
    md.reset()
    start = md.convert(
        links_umbiegen((SEITE / "index.md").read_text(encoding="utf-8"), WURZEL / "index.md")
    )
    schreibe(
        Path("index.html"),
        start,
        "Vigilant Inference Governor — inference QoS for edge robotics",
        "One GPU, several models: decide what is still worth computing before it reaches the server.",
        "start",
    )

    shutil.copy(SEITE / "style.css", AUSGABE / "style.css")
    ziel_assets = AUSGABE / "assets"
    ziel_assets.mkdir(exist_ok=True)
    for datei in (SEITE / "assets").iterdir():
        shutil.copy(datei, ziel_assets / datei.name)
    (AUSGABE / ".nojekyll").write_text("", encoding="utf-8")

    print(f"{len(seiten) + 2} Seiten nach {AUSGABE}")
    if TOTE:
        einmalig = sorted(set(TOTE))
        print(f"\n{len(einmalig)} Verweise zeigen schon in der Quelle ins Leere:")
        for quelle, ziel in einmalig:
            print(f"  {quelle} -> {ziel}")
        print("Sie bleiben unveraendert; reparieren muss sie das Dokument.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
