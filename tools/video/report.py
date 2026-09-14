# SPDX-FileCopyrightText: 2026 Vigilant e.K.
# SPDX-License-Identifier: BUSL-1.1
"""Die Messprotokolle lesen — und ausschliesslich die Woerter uebersetzen.

Unsere Werkzeuge geben deutsch aus, das Video ist englisch. Der bequeme Weg
waere, die Tabelle fuer das Bild neu zu tippen. Dann stuende dort eine Zahl,
die niemand mehr gegen eine Datei pruefen kann, und sie wuerde still veralten
— genau das, was dieses Projekt sonst niemandem durchgehen laesst.

Also der Umweg: Die Zahlen werden aus der echten Datei **geparst**, ersetzt
werden nur die Woerter, und zwar nach den Regeln, die hier offen stehen. Nach
der Ersetzung prueft `assert_english`, ob ein deutsches Wort uebrig blieb; wenn
ja, bricht der Bau ab. Ein geaendertes Protokoll faellt damit auf, statt als
halbdeutsches Bild durchzurutschen.

Was das *nicht* ist: eine Uebersetzung des Werkzeugs. Wer die Ausgabe selbst
englisch will, braucht eine Sprachumschaltung in `vig` — die gibt es nicht, und
dieses Modul tut nicht so, als gaebe es sie.
"""
from __future__ import annotations

import re
from pathlib import Path

_DATE = re.compile(r"(20\d{2}-\d{2}-\d{2})")


def source_label(path: Path, date: str = "") -> str:
    """Dateiname und Messdatum — die Quellenangabe, die ins Bild gehoert.

    Steht das Datum im Pfad (`measure-morgen-2026-09-12`), wird es von dort
    gelesen. Verzeichnisse wie `gate-m3-r03/` tragen keines; dann muss der
    Aufrufer es aus dem zugehoerigen Bericht mitgeben. Erfunden wird keines:
    ohne beides sagt das Bild, dass das Datum fehlt.
    """
    match = _DATE.search(str(path))
    stamp = date or (match.group(1) if match else "date not in path")
    # Mit Verzeichnis, nicht nur Dateiname: im Baum liegen mehrere `gate-r1.txt`,
    # und eine Quellenangabe, die zwei Dateien meinen kann, ist keine.
    return f"{path.parent.name}/{path.name} · {stamp}"


# ------------------------------------------------------------------ gate-m3 --

def gate_rows(path: Path) -> list[list[str]]:
    """Die Stromtabelle aus dem Gate-M3-Protokoll.

    Sechs Spalten: Name, Abdeckung Triton, Abdeckung Vigilant, Antwortalter
    Triton, Antwortalter Vigilant, Urteil. Passt die Form nicht, ist das ein
    Abbruch und keine Warnung: ein Bild aus einer halb erkannten Tabelle waere
    schlimmer als gar keines.
    """
    lines = path.read_text(encoding="utf-8").splitlines()
    head = next((i for i, line in enumerate(lines)
                 if line.strip().startswith("Strom")), None)
    if head is None:
        raise SystemExit(f"{path}: Kopfzeile 'Strom' nicht gefunden — "
                         "hat sich das Protokollformat geaendert?")
    rows = []
    for line in lines[head + 2:]:
        if not line.strip():
            break
        cells = [cell.strip() for cell in line.split("|")]
        if len(cells) != 6:
            raise SystemExit(f"{path}: Zeile mit {len(cells)} statt 6 Spalten: "
                             f"{line.strip()!r}")
        cells[5] = {"besser": "better"}.get(cells[5], cells[5])
        rows.append(cells)
    if not rows:
        raise SystemExit(f"{path}: Tabelle ohne Zeilen.")
    return rows


def gate_setup(path: Path) -> str:
    """Die Randbedingungen des Laufs, englisch, mit den Zahlen aus der Datei."""
    text = path.read_text(encoding="utf-8")
    load = re.search(r"Auslastung\s+(\d+)\s*%", text)
    seconds = re.search(r"Messdauer\s+(\d+)\s*s", text)
    if not (load and seconds):
        raise SystemExit(f"{path}: Auslastung oder Messdauer nicht gefunden.")
    setup = (f"protected work needs {load.group(1)} % of one GPU slot  ·  "
             f"{seconds.group(1)} s per run")
    # Der Datenpfad wird nur behauptet, wenn das Protokoll ihn nennt. Eine
    # Fassung, die "shared memory" unbedingt anhaengte, haette ihn auch ueber
    # Laeufe geschrieben, die ihn nicht protokollieren.
    if "Shared Memory" in text:
        setup += "  ·  tensors passed through shared memory"
    return setup


# ---------------------------------------------------------------- vig doctor --

#: Ganze Zeilen, nicht einzelne Vokabeln. Ein Muster je Zeilenart heisst: Die
#: Zahlen kommen aus der Datei (Rueckverweise), die Woerter aus dieser Tabelle,
#: und eine unbekannte Zeilenart faellt sofort auf.
_RULES: list[tuple[re.Pattern, str]] = [
    (re.compile(r"^OK   Konfigurationsschema gueltig$"),
     "OK   configuration schema valid"),
    (re.compile(r"^FAIL (\S+): geschuetzte Auslastung (\d+) %, ueber (\d+) "
                r"Slot\(s\) nicht tragbar\. Best-Effort-Arbeit kommt damit "
                r"strukturell nie zum Zug\.$"),
     r"FAIL \1: protected work needs \2 % of \3 slot(s). "
     r"Background work would never get a turn."),
    (re.compile(r"^OK   Backend erreichbar unter (\S+)$"),
     r"OK   backend reachable at \1"),
    (re.compile(r"^OK   (\S+) -> (\S+) bereit$"),
     r"OK   \1 -> \2 ready"),
    (re.compile(r"^OK   (\S+): Shared Memory verfuegbar — Tensoren werden als "
                r"Referenz durchgereicht$"),
     r"OK   \1: shared memory available — tensors passed by reference"),
    (re.compile(r"^OK   (\S+): triton (\S+)$"), r"OK   \1: triton \2"),
    (re.compile(r"^RESULT (\S+)$"), r"RESULT \1"),
]

#: Woerter, die nach der Ersetzung nichts mehr zu suchen haben. Die Liste ist
#: absichtlich grob: lieber ein falscher Alarm, den jemand liest, als ein
#: deutsches Wort in einem englischen Video.
_GERMAN = ("gueltig", "geschuetzt", "Auslastung", "tragbar", "erreichbar",
           "bereit", "verfuegbar", "durchgereicht", "Tensoren", "Abdeckung",
           "Strom", "Antwortalter", "Faktor", "besser", "Messdauer",
           "Puffertiefe", "laeuft", "praemptierbar", "Restblockierung",
           "Profil", "Fingerabdruck", "pruefbar", "nicht", "ueber", "werden",
           "Treiber", "unbekannt", "gemessen")


def assert_english(line: str, path: Path) -> str:
    for word in _GERMAN:
        if word.lower() in line.lower():
            raise SystemExit(
                f"{path}: nach der Uebersetzung steht noch '{word}' in:\n"
                f"  {line}\n"
                f"Ergaenze eine Regel in tools/video/report.py statt das Bild "
                f"halbdeutsch zu rendern.")
    return line


def _translate(line: str, path: Path) -> str:
    for pattern, replacement in _RULES:
        if pattern.match(line):
            return assert_english(pattern.sub(replacement, line), path)
    raise SystemExit(f"{path}: keine Uebersetzungsregel fuer:\n  {line}\n"
                     f"Siehe _RULES in tools/video/report.py.")


#: Welche Zeilen das Bild zeigt. Ausgewaehlt, nicht erfunden: der eine FAIL,
#: die Erreichbarkeit beider Server, die vier Modelle, der Shared-Memory-Pfad
#: und das Gesamturteil. Die Warnungen ueber fehlende Profil-Fingerabdruecke
#: bleiben draussen, weil sie vier Mal dasselbe sagen — nicht, weil sie
#: unangenehm waeren; der FAIL steht ja an zweiter Stelle.
_DOCTOR_KEEP = ("Konfigurationsschema", "PROTECTED_WORKLOAD_UNSCHEDULABLE",
                "Backend erreichbar", "bereit", "Shared Memory", "RESULT ")


def doctor_lines(path: Path) -> list[str]:
    """Die Zeilen des echten `vig doctor`-Laufs, englisch."""
    picked = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.rstrip()
        if any(marker in line for marker in _DOCTOR_KEEP):
            picked.append(_translate(line, path))
    if not any(line.startswith("FAIL") for line in picked):
        raise SystemExit(f"{path}: kein FAIL gefunden — das Bild behauptet "
                         "aber, der Lauf sei nicht startbereit gewesen.")
    return picked
