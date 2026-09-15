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

import json
import math
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


# -------------------------------------------------------------- vig autotune --

#: Was die vier Schritte tun, in einem Satz. Das steht hier, weil
#: `qualification.json` nur den Namen des Schritts nennt. Ein unbekannter
#: Schritt bricht ab statt durchzurutschen: Ein Bild, das einen Schritt
#: stillschweigend weglaesst, behauptet einen kuerzeren Lauf als den, der
#: stattgefunden hat.
_AUTOTUNE_STEPS = {
    "discover": "read the models from the backend",
    "measure": "measure runtimes, concurrency, interference",
    "tune": "tune the governor settings for this load",
    "fit": "is the governor worth it on this load?",
    "check": "check the resulting configuration",
}


def autotune_lines(path: Path) -> list[str]:
    """Die Zeilen eines echten `vig autotune`-Laufs, englisch.

    Gelesen wird `qualification.json` und nicht `qualification.md`: Das Urteil
    steht im Bericht heute auf Deutsch, die strukturierten Felder daneben sind
    englisch. Das Bild haengt damit nicht daran, wann der deutsche Satz
    repariert wird — und es zeigt weiterhin, was gemessen wurde, statt dessen,
    was jemand abgetippt hat.

    Gezeigt wird ausdruecklich auch, was der Lauf *nicht* behauptet: wie viele
    Messreihen verworfen wurden und ob er eine Freigabe verweigert. Das ist
    kein Schoenheitsfehler, den man wegschneidet, sondern der Grund, dem
    Werkzeug ueberhaupt zu glauben.
    """
    data = json.loads(path.read_text(encoding="utf-8"))

    lines = []
    for step in data.get("steps", []):
        name = step.get("step", "")
        if name not in _AUTOTUNE_STEPS:
            raise SystemExit(
                f"{path}: unbekannter Schritt {name!r} — hat sich das "
                f"Ausgabeformat von `vig autotune` geaendert? Ergaenze "
                f"_AUTOTUNE_STEPS in tools/video/report.py.")
        lines.append(f"{name:<9}{_AUTOTUNE_STEPS[name]:<44}"
                     f"{step.get('outcome', '?'):<7}{step.get('seconds', 0):>3} s")

    series = data.get("series", {})
    qualified, discarded = series.get("qualified"), series.get("discarded")
    if qualified is None or discarded is None:
        raise SystemExit(f"{path}: series.qualified/series.discarded fehlen — "
                         "ohne sie verschweigt das Bild, wie viel verworfen wurde.")
    lines.append(f"SERIES   {qualified} of {qualified + discarded} usable, "
                 f"{discarded} discarded")

    if "release" not in data:
        raise SystemExit(f"{path}: kein Feld 'release'. Das Bild soll gerade "
                         "zeigen, ob eine Freigabe verweigert wurde.")
    reasons = "; ".join(data.get("release_reasons", []))
    lines.append(f"RESULT   release {data['release']}"
                 + (f" — {reasons}" if reasons else ""))

    fit = fit_numbers(data.get("fit_verdict") or "", path)
    if fit:
        lines.append(f"FIT      protected missed: direct {fit[0]} ‰ → governor {fit[1]} ‰")
        lines.append(f"         background missed: direct {fit[2]} ‰ → governor {fit[3]} ‰")

    if data.get("doctor"):
        lines.append(f"DOCTOR   {data['doctor']}")

    return [assert_english(line, path) for line in lines]


#: Die vier Promillezahlen im Urteil von `vig-fit`, in dieser Reihenfolge:
#: geschuetzter Strom direkt / Governor, nachrangige Stroeme direkt / Governor.
#: Aeltere Laeufe schreiben das Urteil deutsch, neuere englisch; gelesen werden
#: nur die Zahlen, nie der Satz. Stehen nicht genau vier da, bricht der Bau ab:
#: eine halb gelesene Zeile waere eine Zahl ohne Beleg.
_PERMILLE = re.compile(r"(\d+)\s*‰")


def fit_numbers(verdict: str, path: Path) -> tuple[str, str, str, str] | None:
    """Die Kennzahlen des Urteils, oder None, wenn `fit` kein Urteil hat."""
    if not verdict.strip():
        return None
    found = _PERMILLE.findall(verdict)
    if len(found) != 4:
        raise SystemExit(f"{path}: fit_verdict traegt {len(found)} Promillezahlen "
                         "statt vier — Format von vig-fit geaendert?")
    return found[0], found[1], found[2], found[3]


# ------------------------------------------------------------ Geraeteszene --

_LOAD = re.compile(r"(?:From|Ab) (\d+) %")
_NOT_WORTH = ("not worth it", "lohnt sich der")


def device_summary(path: Path) -> dict:
    """Was `vig autotune` auf einem Geraet gemessen und eingestellt hat.

    Gelesen werden `series` aus `qualification.json` und die Struktur der
    eingefrorenen `measured.yaml` daneben: Slots, serialisierte Paare,
    Interferenzeintraege, Modelle. **Kein Urteil**: „lohnt sich" haengt an der
    Last, die man dem Werkzeug gibt, nicht am Geraet — ein Urteil ohne seine
    Last waere in einem Geraetebild eine falsche Aussage. Fehlt ein Feld,
    bricht der Bau ab.
    """
    import yaml
    data = json.loads(path.read_text(encoding="utf-8"))
    series = data.get("series") or {}
    qualified, discarded = series.get("qualified"), series.get("discarded")
    if qualified is None or discarded is None:
        raise SystemExit(f"{path}: series.qualified/discarded fehlen")
    config_path = path.parent / "measured.yaml"
    if not config_path.is_file():
        raise SystemExit(f"{config_path}: fehlt — ohne Konfiguration kein Geraetebild")
    config = yaml.safe_load(config_path.read_text(encoding="utf-8"))
    backend = config.get("backend") or {}
    models = config.get("models") or {}
    def count(n: int, one: str, many: str) -> str:
        return f"{n} {one if n == 1 else many}"
    slots = int(backend.get("slots", 1))
    pairs = len(backend.get("no_corun") or [])
    entries = len(backend.get("interference") or [])
    lines = [
        f"{count(len(models), 'model', 'models')} measured, "
        f"{qualified} of {qualified + discarded} series usable",
        f"{count(slots, 'slot', 'slots')} · {count(pairs, 'serialised pair', 'serialised pairs')}",
        count(entries, "interference entry", "interference entries"),
    ]
    return {"lines": [assert_english(line, path) for line in lines]}


# -------------------------------------------------------------- Demo-Clips --

_ONES = ("zero one two three four five six seven eight nine ten eleven twelve "
         "thirteen fourteen fifteen sixteen seventeen eighteen nineteen").split()
_TENS = "_ _ twenty thirty forty fifty sixty seventy eighty ninety".split()


def number_words(n: int) -> str:
    """Eine ganze Zahl (0..9999) so, wie der Sprecher sie sagen soll.

    Der Sprechertext der uebrigen Szenen schreibt Zahlen aus ("ninety-nine",
    "thirty-three"), damit die Stimme sie nicht nach eigenem Ermessen liest;
    hier geschieht dasselbe, nur aus der Datei statt von Hand.
    """
    if not 0 <= n <= 9999:
        raise SystemExit(f"number_words: {n} ausserhalb 0..9999")
    if n < 20:
        return _ONES[n]
    if n < 100:
        tens, ones = divmod(n, 10)
        return _TENS[tens] + (f"-{_ONES[ones]}" if ones else "")
    if n < 1000:
        hundreds, rest = divmod(n, 100)
        return f"{_ONES[hundreds]} hundred" + (f" and {number_words(rest)}" if rest else "")
    thousands, rest = divmod(n, 1000)
    joiner = " and " if 0 < rest < 100 else " "
    return f"{_ONES[thousands]} thousand" + (f"{joiner}{number_words(rest)}" if rest else "")


def spoken_times(n: int) -> str:
    return {0: "not once", 1: "once", 2: "twice"}.get(n, f"{number_words(n)} times")


#: Sprechbare Brueche einer Sekunde. Getroffen wird nur, was innerhalb von 12 %
#: liegt; sonst sagt der Sprecher die Millisekunden. 336 ms sind "a third of a
#: second", 250 ms "a quarter", 420 ms bleiben "four hundred and twenty".
_FRACTIONS = ((250.0, "a quarter of a second"), (1000.0 / 3, "a third of a second"),
              (500.0, "half a second"), (1000.0, "a full second"))


def spoken_ms(ms: float) -> str:
    for value, words in _FRACTIONS:
        if abs(ms - value) <= 0.12 * value:
            return words
    return f"{number_words(int(round(ms, -1)))} milliseconds"


def percent_text(share: float) -> str:
    """Anteil fuer Bild und Beschreibung, eine Nachkommastelle wo noetig.

    Der Demo-Renderer zeigt ganze Prozent, also "0 %" fuer 0,17 % und "100 %"
    fuer 99,8 %. Hier steht die genauere Zahl: "0 %" behauptet, es habe
    keinen einzigen frischen Takt gegeben, und "100 %" behauptet, es habe
    keinen verpassten gegeben. Deshalb wird nie auf 100 hoch- und nie auf 0
    heruntergerundet.
    """
    if share >= 1.0:
        return "100 %"
    pct = round(share * 100, 1)
    if pct >= 100.0:
        pct = math.floor(share * 1000) / 10
    if pct <= 0.0:
        return "0 %" if share <= 0 else "< 0.1 %"
    return f"{pct:.1f} %" if pct % 1 else f"{pct:.0f} %"


def spoken_percent(share: float) -> str:
    """Derselbe Anteil wie `percent_text`, zum Sprechen: "ninety-nine point eight percent"."""
    text = percent_text(share).replace(" %", "")
    if text.startswith("<"):
        return "less than a tenth of a percent"
    whole, _, tenth = text.partition(".")
    words = number_words(int(whole)) + (f" point {number_words(int(tenth))}" if tenth else "")
    return f"{words} percent"


def _demo_renderer():
    """tools/demo/render.py laden — unter anderem Namen.

    Beide Module heissen `render`; ein normaler Import holte je nach
    sys.path das falsche. Die Zahlen werden mit genau dem Code gerechnet,
    der auch die Zusammenfassungskarte im Clip zeichnet.
    """
    import importlib.util
    path = Path(__file__).resolve().parent.parent / "demo" / "render.py"
    spec = importlib.util.spec_from_file_location("demo_render", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def demo_facts(clip: Path, left: Path, right: Path, render_start_s: float = 0.0) -> dict:
    """Die Kennzahlen eines Demo-Clips, aus seinen beiden Zeitachsen.

    Gerechnet wie die Zusammenfassung von `tools/demo/render.py`: Fenster vom
    Renderstart bis zum letzten gerenderten Bild, "frisch" = zum Abtastzeitpunkt
    (alle `period_ms`) war das neueste angekommene Ergebnis hoechstens
    `max_age_ms` alt, Sprachmodell-Antworten = ok-Antworten mit
    `start <= done_ms <= Ende`. Antworten, die erst nach dem letzten Bild
    ankamen, zaehlen nicht — der Clip zeigt sie ja auch nicht.

    Dazu das mittlere Alter eines Detektorergebnisses bei Ankunft (Median ueber
    alle ok-Ergebnisse im Fenster) und die Anzahl der Kameras aus dem Kopf
    (`detector` plus `aux_cameras`). Fehlt etwas, bricht der Bau ab.
    """
    d = _demo_renderer()
    for path in (clip, left, right):
        if not path.is_file():
            raise SystemExit(f"{path}: fehlt — Demo-Szene ohne Clip oder Zeitachse")
    duration, fps = d.probe(str(clip))
    nframes = max(1, int(round(duration * fps)))
    start_ms = render_start_s * 1000.0
    end_ms = start_ms + (nframes - 1) * 1000.0 / fps

    arms = []
    for side, path in (("left", left), ("right", right)):
        tl = d.Timeline(str(path), side)
        if tl.header is None or tl.no_data:
            raise SystemExit(f"{path}: Zeitachse ohne Kopf oder ohne Ergebnisse")
        max_age = float(tl.hget("detector", "max_age_ms"))
        period = float(tl.hget("detector", "period_ms"))
        arm = d.ArmState(tl, max_age, period, start_ms, end_ms)
        share = arm.fresh_share(end_ms)
        if share is None:
            raise SystemExit(f"{path}: Clip kuerzer als max_age_ms, kein Takt bewertet")
        ages = sorted(r["done"] - r["capture"] for r in tl.det
                      if start_ms <= r["capture"] <= end_ms)
        if not ages:
            raise SystemExit(f"{path}: kein Detektorergebnis im Clipfenster")
        arms.append({"tl": tl, "share": share, "answers": arm.vlm_answers(end_ms),
                     "age_p50": ages[len(ages) // 2] if len(ages) % 2 else
                     (ages[len(ages) // 2 - 1] + ages[len(ages) // 2]) / 2,
                     "max_age": max_age, "period": period})
    (lt, rt) = (arms[0]["tl"], arms[1]["tl"])
    if lt.arm != "direct" or rt.arm != "governed":
        raise SystemExit(f"{left}/{right}: links muss der direkte, rechts der "
                         f"geregelte Arm stehen (gefunden {lt.arm}/{rt.arm})")
    if arms[0]["max_age"] != arms[1]["max_age"] or arms[0]["period"] != arms[1]["period"]:
        raise SystemExit(f"{left}/{right}: die Arme haben verschiedene Vertraege")

    stream = "protected"
    with open(right, encoding="utf-8") as fh:
        for line in fh:
            if '"type":"detector"' in line.replace(" ", ""):
                stream = json.loads(line).get("stream") or stream
                break
    cameras = 1 + len(rt.hget("aux_cameras", default=[]) or [])
    gpu = str(rt.hget("gpu", default="GPU"))
    clip_s = (end_ms - start_ms + 1000.0 / fps) / 1000.0
    return {
        "cameras": cameras,
        "Cameras": number_words(cameras).capitalize(),
        "cameras_word": number_words(cameras),
        "gpu": gpu,
        "gpu_kind": "laptop GPU" if "laptop" in gpu.lower() else "GPU",
        "stream": stream,
        "left_label": lt.label, "right_label": rt.label,
        "left_fresh": arms[0]["share"], "right_fresh": arms[1]["share"],
        "left_fresh_text": percent_text(arms[0]["share"]),
        "right_fresh_text": percent_text(arms[1]["share"]),
        "right_fresh_spoken": ("every control cycle" if arms[1]["share"] >= 1.0 else
                               f"{spoken_percent(arms[1]['share'])} of control cycles"),
        "left_pct_spoken": spoken_percent(arms[0]["share"]),
        "right_pct_spoken": spoken_percent(arms[1]["share"]),
        "left_age_ms": arms[0]["age_p50"], "right_age_ms": arms[1]["age_p50"],
        "left_age_spoken": spoken_ms(arms[0]["age_p50"]),
        "left_answers": arms[0]["answers"], "right_answers": arms[1]["answers"],
        "left_answers_spoken": spoken_times(arms[0]["answers"]),
        "right_answers_spoken": spoken_times(arms[1]["answers"]),
        "max_age_ms": arms[0]["max_age"], "period_ms": arms[0]["period"],
        "clip_s": clip_s,
        "clip_text": f"{clip_s:.1f}".rstrip("0").rstrip(".") + " s",
        "clip_spoken": f"{number_words(int(round(clip_s)))} seconds",
        "clip_duration_s": duration,
    }


def assert_demo_claims(facts: dict, where: str) -> None:
    """Was der Sprechertext der Demo-Szene behauptet, muss der Lauf hergeben.

    Der Text sagt "Triton alone ... effectively blind" und "with Vigilant the
    front camera stays fresh". Ein neuer Lauf, in dem das nicht mehr stimmt
    (weil die GPU nicht mehr ueberlastet ist, oder weil der Governor verliert),
    bricht hier ab, statt die alten Saetze ueber neue Zahlen zu legen.
    """
    if not facts["left_fresh"] < 0.05:
        raise SystemExit(f"{where}: direkter Arm {facts['left_fresh_text']} frisch — "
                         "der Text nennt ihn blind. Szene fuer diesen Lauf umschreiben.")
    if not facts["right_fresh"] >= 0.95:
        raise SystemExit(f"{where}: Governor-Arm nur {facts['right_fresh_text']} frisch — "
                         "der Text nennt die Kamera frisch. Szene umschreiben.")


# ------------------------------------------------------------ Tuning-Szene --

def tuning_summary(path: Path) -> dict:
    """Der Tuning-Schritt eines `vig autotune`-Laufs, fuer das Bild.

    Gelesen wird `tuning` aus `qualification.json`: ungetunt gegen getunt und
    jede probierte Einstellung mit ihrer Entscheidung. Fehlt der Abschnitt,
    bricht der Bau ab — ein Tuning-Bild ohne Tuning-Lauf waere eine Behauptung.
    """
    data = json.loads(path.read_text(encoding="utf-8"))
    tuning = data.get("tuning")
    if not tuning:
        raise SystemExit(f"{path}: kein Abschnitt 'tuning' — Lauf ohne tune-Schritt?")
    def objective(value):
        if not value:
            return None
        return (value["protected_worst_permille"], value["background_mean_permille"])
    untuned, tuned = objective(tuning.get("untuned")), objective(tuning.get("tuned"))
    if untuned is None or tuned is None:
        raise SystemExit(f"{path}: tuning.untuned/tuned fehlen")
    # Zeile 0 ist der Ausgangspunkt: ohne ihn sieht niemand, wogegen die
    # probierten Einstellungen verglichen wurden.
    rows = [("untuned (as measured)", f"{untuned[0]} ‰", f"{untuned[1]} ‰", "baseline")]
    for candidate in tuning.get("candidates", []):
        o = objective(candidate.get("objective"))
        rows.append((assert_english(candidate.get("change") or "untuned", path),
                     "—" if o is None else f"{o[0]} ‰",
                     "—" if o is None else f"{o[1]} ‰",
                     candidate.get("decision", "?")))
    confirmation = tuning.get("confirmation") or {}
    pairs = []
    for pair in confirmation.get("pairs", []):
        u, t = objective(pair.get("untuned")), objective(pair.get("tuned"))
        pairs.append((pair.get("pair"), u, t, bool(pair.get("held"))))
    # Der Vergleich mit dem direkten Weg (vig-fit, beide Arme): nur die Zahlen,
    # nie der Satz. Ohne Urteil bleibt die Zeile weg.
    fit = None
    verdict = data.get("fit_verdict") or ""
    if verdict.strip() and not any(p in verdict for p in _NOT_WORTH):
        load = _LOAD.search(verdict)
        numbers = fit_numbers(verdict, path)
        if load and numbers:
            fit = (load.group(1), *numbers)
    return {"untuned": untuned, "tuned": tuned, "improved": bool(tuning.get("improved")),
            "applied": bool(tuning.get("applied")), "rows": rows, "pairs": pairs,
            "fit": fit, "series": data.get("series") or {}}

