# Validierung von `vig autotune` gegen die bekannten Aufbauten

Datum: 14.09.2026. Geprueft wird nicht, ob der Governor etwas bringt, sondern
ob **`vig autotune` auf unseren eigenen, bereits von Hand vermessenen
Aufbauten dasselbe herausbekommt wie wir**. Wo die Zahlen abweichen, ist das
ein Befund ueber `autotune` und nicht ueber die Maschine.

Rohdaten:
`InferenceQoS-runtime/messungen/autotune-laptop-2026-09-14/` und
`InferenceQoS-runtime/messungen/autotune-pixel2-2026-09-14/` (der Lauf ohne Vorlauf
darin unter `ohne-vorlauf/`).

## Was hier ausdruecklich nicht steht

**Die Abdeckungszahlen aus Gate M3 — Detektor 85 → 100 %, Pose 91–92 → 100 %,
Tiefe 97–98 → 100 % — sind nicht nachgemessen worden.** Sie stammen aus
`gate-m3`, einem anderen Werkzeug mit einem anderen Lauf. `autotune` misst
Profile, Nebenlaeufigkeit und gerichtete Interferenz und faellt ein Urteil
ueber die Konfiguration; es erzeugt keine Abdeckungsreihe. Ein
`gate-m3`-Lauf haette `gate-m3` bestaetigt, nicht `autotune`. Wer diesen
Bericht liest, darf daraus **nicht** schliessen, dass `autotune` 85 → 100 %
reproduziert hat.

## Laptop: RTX 3070, Triton

RTX 3070 Laptop (8 GB), Treiber 580.178.04, CC 8.6, Triton 2.70.0,
`examples/gate_m3/vig.yaml`, 200 Messungen je Stufe, `taskset -c 8-15`, unter
`measure-pending` und exklusiver `quiet.lock`. Systemlast 1,06 — der Lauf
wurde korrekt **nicht** als verschmutzt gefuehrt. Karte gedrosselt auf 1710
von 2100 MHz, Grund `SwPowerCap`.

**Dieser Lauf ist aelter als der Vorlauf-Fix und aelter als die Korrektur am
Manifest.** Er zeigt das Werkzeug im Zustand von 17:48 und wurde nicht
wiederholt; die gespeicherten Artefakte tragen deshalb noch die
Rust-Darstellung im Geraetefeld (siehe Befund 6).

### Das Ergebnis: keine Qualifikation

**1 von 4 Messreihen verwertbar.** Drei wurden verworfen, weil der SM-Takt
mitten in der Reihe wanderte:

```
Messreihe verworfen: Hardwarezustand geaendert: GPU 0 clock_sm_mhz: ~1500 -> ~1800
Messreihe verworfen: Hardwarezustand geaendert: GPU 0 clock_sm_mhz: ~1900 -> ~1800
Messreihe verworfen: Hardwarezustand geaendert: GPU 0 clock_sm_mhz: ~1700 -> ~1800
```

Jede dieser Reihen hatte **200 Freigaben, 200 Abschluesse, null
Fehlschlaege**: die Daten sahen tadellos aus und wurden trotzdem weggeworfen,
weil die Bedingungen sich waehrend der Messung verschoben haben. Der Lauf
endet mit `No qualification: 3 of 4 measurement series were discarded`, und
der Bericht sagt in klaren Worten, dass die Groessen, die diese Reihen
ergeben haetten, **nicht gesetzt** sind.

Das ist das gewuenschte Verhalten, und es ist zugleich das staerkste Argument
dafuer, dass **diese Maschine ein schlechter Qualifikationswirt ist**. Der
Takt laeuft nicht einmal kalt hoch und bleibt dann stehen, er wandert in
beide Richtungen unter einem Leistungslimit. Beide Haelften gehoeren
zusammen: das Werkzeug verhaelt sich richtig, und der Aufbau taugt nicht.

### Profile: nur eine Zahl ist ueberhaupt vergleichbar

| Modell | `examples/gate_m3/vig.yaml` | nach dem Lauf | gemessen? |
|---|---:|---:|---|
| pose | 4 082 us | **3 835 us** (−6,0 %) | **ja**, die eine verwertbare Reihe |
| depth | 9 286 us | 9 286 us | nein — Reihe verworfen, Wert steht |
| detector | 15 639 us | 15 639 us | nein — Reihe verworfen, Wert steht |
| vlm | 97 902 us | 97 902 us | nein — Reihe verworfen, Wert steht |

**Drei dieser vier Zeilen sind keine Uebereinstimmung.** Sie sind der
unveraenderte Eingabewert: eine verworfene Reihe ueberschreibt nichts. Wer
die Tabelle als „drei von vier passen" liest, liest sie falsch. Vergleichbar
ist allein `pose`, und das liegt 6 % unter dem dokumentierten Wert.

Interferenz wurde auf dem Laptop **gar nicht** gemessen: `slots: 1` laesst
weder Belegungsstufe noch Paarung zu. Die Interferenzpruefung dieser
Validierung stammt vollstaendig vom Telefon.

### `doctor`: die dokumentierte Erwartung trifft zu

Gegen die **Eingangskonfiguration**, direkt aufgerufen:

```
FAIL PROTECTED_WORKLOAD_UNSCHEDULABLE:
     geschuetzte Auslastung 103 %, ueber 1 Slot(s) nicht tragbar.
RESULT NOT_READY        (Exitcode 1)
```

**`autotune` meldet dagegen `READY_WITH_WARNINGS`.** Das ist kein
Widerspruch, sondern eine andere Frage: `autotune` prueft die Konfiguration,
die es **erzeugt** hat, nicht die Eingabe. Die Rechnung dahinter, mit
konservativer Laufzeit `p99 x 1,10` ueber der Vertragsperiode:

| Strom | Eingabe | erzeugte Konfiguration |
|---|---:|---:|
| detector (33 ms) | 19 596 us → 65,3 % | unveraendert 65,3 % |
| pose (33 ms) | 5 690 us → 19,0 % | **4 359 us → 14,5 %** |
| depth (66 ms) | 11 453 us → 19,1 % | unveraendert 19,1 % |
| **Summe** | **103 %** → `NOT_READY` | **98 %** → `READY_WITH_WARNINGS` |

Die eine verwertbare Reihe — `pose` — faellt niedriger aus als das
dokumentierte Profil und drueckt die Summe unter die Grenze. Beide Urteile
sind fuer ihre jeweilige Datei richtig. Wer den Bericht liest, muss wissen,
auf welche Datei er sich bezieht, sonst sieht es aus, als widerspraeche
`autotune` unserer eigenen Messung.

### `vig-fit`: ein Urteil fuer uns

```
URTEIL Ab 90 % Last verfehlt der direkte Weg 996 ‰ der Takte des
  geschuetzten Stroms, der Governor 0 ‰.
  Der Preis steht daneben: die nachrangigen Stroeme verlieren direkt
  505 ‰, unter dem Governor 1000 ‰.
```

Der Preis steht im selben Satz wie der Gewinn. Das Werkzeug lief 82 s.

## Telefon: Pixel 2, TFLite-Backend

Geraet FA82P1A01294, `vig` statisch fuer `aarch64-unknown-linux-musl`,
`taskset f0` (Gold-Kerne), TFLite-Server mit GPU-Delegate, 200 Messungen je
Stufe, `slots: 2`. Bezug: [android-gpu.md](android-gpu.md), Kalibrierung vom
11.09., 21:18–21:28, gleiche Kernbindung und ebenfalls ohne `nvidia-smi`.

Zwei Laeufe: **ohne Vorlauf** (der Stand vor dem Fix) und **mit Vorlauf**.
Beide kalt gestartet — 34 °C `back_therm` gegen 33 °C im dokumentierten Lauf,
Endtemperatur 43 °C in beiden Faellen.

### Soloprofile

| Modell | dokumentiert | ohne Vorlauf | mit Vorlauf |
|---|---:|---:|---:|
| pose | 89 815 us | 80 858 us (−10,0 %) | **89 173 us (−0,7 %)** |
| depth | 199 642 us | 194 078 us (−2,8 %) | 206 206 us (+3,3 %) |
| detector | 222 435 us | 152 083 us (−31,6 %) | 158 474 us (**−28,8 %**) |

### Belegungsstufe (ein zweiter Slot mit demselben Modell)

| Modell | dokumentiert | ohne Vorlauf | mit Vorlauf |
|---|---:|---:|---:|
| pose | 0,90x | 0,54x | **0,90x** |
| depth | 2,11x | 1,79x | 2,05x |
| detector | 1,82x | 1,94x | 1,87x |

**Alle drei ruecken mit dem Vorlauf an die dokumentierten Werte heran, `pose`
trifft sie exakt.** Der Vorlauf tut also, was er soll — ausser beim Detektor
allein.

### Gerichtete Interferenz

Verhaeltnis und Aufschlag stehen getrennt: ein Verhaeltnis, das passt,
waehrend die Grundlinie um ein Drittel wandert, ist **nicht** dasselbe
Ergebnis.

| Zelle | dokumentiert | ohne Vorlauf | mit Vorlauf |
|---|---|---|---|
| depth neben detector | 1,25x / +51 155 | 1,26x / +52 112 | **1,24x / +51 147** |
| depth neben pose | 1,25x / +50 063 | 1,23x / +45 625 | 1,15x / +32 411 |
| detector neben depth | 1,52x / +116 347 | 2,42x / +216 799 | 2,18x / +187 875 |
| detector neben pose | 0,65x / +0 | 0,97x / +0 | 0,96x / +0 |
| pose neben depth | 2,64x / +147 650 | 2,88x / +152 354 | 2,70x / +151 893 |
| pose neben detector | 0,67x / +0 | 0,68x / +0 | 0,64x / +0 |

`depth neben detector` liegt mit Vorlauf **acht Mikrosekunden** neben dem
dokumentierten Aufschlag. Beide Laeufe klemmen negative Interferenz auf `+0`
(ADR-0026), wie der dokumentierte auch.

### Was das an der eingefrorenen Konfiguration aendert

| | dokumentiert | beide autotune-Laeufe |
|---|---|---|
| `no_corun` | `[pose, depth]` | `[detector, depth]` **und** `[pose, depth]` |
| `interference`-Tabelle | 2 Eintraege | **leer** |

Die Heuristik serialisiert ab der doppelten Laufzeit. `detector neben depth`
liegt dokumentiert bei 1,52x, gemessen bei 2,42x bzw. 2,18x — die zu niedrige
Detektor-Grundlinie schiebt die Zelle ueber die Schwelle. Damit wandert das
Paar von „Interferenzeintrag" zu „serialisieren", und weil danach beide
`depth`-Paare serialisiert sind, bleibt fuer die Interferenztabelle nichts
uebrig. **Eine falsche Grundlinie wird zu einer anderen Konfiguration**, nicht
nur zu einer anderen Zahl. Der Vorlauf verkleinert den Fehler, kehrt ihn aber
nicht um.

### Was uebereinstimmt

- **12 von 12 Messreihen verwertbar** in beiden Laeufen — und aus demselben
  Grund wie im dokumentierten: ohne `nvidia-smi` gibt es keinen
  Hardwarezustand, an dem eine Reihe scheitern koennte.
- `slots: 2` uebernommen, `pipelining_depth: 0`.
- `vig doctor` laeuft auf ARM durch: `READY_WITH_WARNINGS`, geschuetzte
  serialisierte Auslastung 29 bzw. 30 %, Backend und alle drei Modelle
  erreichbar.

## Befunde ueber `autotune` selbst

Getrennt von den Messtabellen: Wer entscheiden will, ob er dem Werkzeug
traut, muss an einer Stelle sehen, was es falsch macht.

### 1. Die Fremdlasterkennung ist auf ARM falsch geeicht (behoben am 15.09.)

**Nachtrag 15.09.:** Die Ursache war tiefer als die Eichung. `loadavg` ist die
Laenge der Warteschlange, nicht fremde Arbeit: Das Pixel 2 stand im Leerlauf
bei **3,49** und verbrauchte dabei **0,04 Kerne** (fuenf Sekunden
`/proc/stat`, nichts lief). Keine Schwelle haette das richtig getrennt.
`autotune` misst jetzt die fremde CPU-Zeit aus `/proc/stat` abzueglich der
eigenen, vor und nach dem Schritt, Grenze ein ganzer Kern; der Laptop lag zur
selben Zeit mit Terminal und Editor bei 0,68. Eine nicht beobachtbare Last gilt
nicht mehr als ruhig. Der urspruengliche Befund steht unveraendert darunter.

`autotune` vergleicht `/proc/loadavg` gegen eine feste Schwelle von 1,5. Auf
dem Telefon laufen Backend und Messklient auf demselben Geraet; die gemeldete
Last von 4,50 vor und 4,66 nach der Messung ist **die Arbeit der Messung
selbst**. Beide Telefonlaeufe wurden deshalb als „measured under foreign
load" gefuehrt und haben nicht qualifiziert — aus einem Grund, der nicht
stimmt.

**Auf dem Laptop trat der Fehler nicht auf**: dort lief die Messung bei
Systemlast 1,06 und wurde korrekt nicht als verschmutzt gefuehrt. Die
Schwelle trifft richtig, solange die Maschine wirklich ruhig ist, und falsch,
sobald die eigene Messung die Last erzeugt. Auf einem Geraet, auf dem Backend
und Klient zusammenliegen, ist das der Normalfall.

Die Schwelle ist **nicht** nachtraeglich passend gedreht worden — das waere
genau der Fehler, den dieses Werkzeug verhindern soll.

### 2. Der fehlende Vorlauf — behoben, mit begrenzter Reichweite

Die Soloprofile wurden einmal gemessen, ohne die Karte vorher auf einen
stehenden Takt zu bringen. Der Warmlauf im Messkern verwirft zwanzig Aufrufe
je Reihe: genug gegen einen kalten Cache, nichts gegen einen kalten Takt.

**Behoben:** Vor der ersten Reihe faehrt der Kalibrator die Karte unter
Dauerlast warm und wartet, bis der Takt steht; wo kein Taktmesser existiert,
waermt er 20 s und sagt ausdruecklich, dass er nichts belegen kann
(„Vorlauf: 20 s gefahren, aber nicht belegt"). Ein Vorlauf ohne Nachweis ist
besser als eine kalte erste Reihe, wird aber nicht als Nachweis ausgegeben.

**Belegt ist die Wirkung auf dem Telefon** (Tabellen oben): `pose` allein von
−10,0 % auf −0,7 %, alle drei Belegungsverhaeltnisse naeher an den
dokumentierten, `pose` unter Belegung exakt.

**Auf dem Laptop ist der Vorlauf nicht geprueft** — der Lauf ist aelter als
der Fix. Und es spricht einiges dagegen, dass er dort hilft: die drei
Verwerfungen zeigen den Takt in **beide** Richtungen wandern (1500 → 1800,
1900 → 1800, 1700 → 1800) unter `SwPowerCap` bei 1710 von 2100 MHz. Wo ein
Leistungslimit den Takt dauernd verschiebt, gibt es keinen stehenden Zustand,
in den man sich hineinwaermen koennte. Das ist eine Schlussfolgerung aus dem
Protokoll, keine Messung; ein Vorlauf-Lauf auf dem Laptop steht aus.

### 3. Die zu niedrige Detektor-Grundlinie bleibt unerklaert (offen)

Der Vorlauf bewegt sie um 4 % (152 083 → 158 474 us), die Luecke zum
dokumentierten Wert bleibt bei 28,8 %. Das ist ein **zweiter Mechanismus**,
nicht der Takt. Was auffaellt: `detector` ist das einzige Modell mit einem
Operator auf der CPU (die NMS-Nachbearbeitung, 266 von 267 Knoten auf der
GPU). Was davon die Ursache ist, sagt diese Messung nicht; sie wird hier
festgehalten und nicht weiter verfolgt.

### 4. Die eingefrorene Konfiguration unterscheidet gemessen und uebernommen nicht (Soloprofile offen, Interferenztabelle behoben)

Wird eine Reihe verworfen, bleibt der vorhandene Wert stehen — richtig so,
geschaetzt wird nichts. Er steht danach aber mit demselben `samples:` und
derselben `source:` da wie ein frisch gemessener. Auf dem Laptop gingen drei
von vier Profilen unveraendert aus der Eingabe in die erzeugte Datei, ohne
Kennzeichnung. Der Bericht sagt „1 von 4 Reihen verwertbar"; die Datei, die
in Betrieb geht, sagt es nicht.

**Fuer die Soloprofile gilt das unveraendert.** Fuer die
**Interferenztabelle** war derselbe Befund schaerfer und ist behoben: Dort
leerte `apply()` die gerichtete Tabelle bedingungslos und schrieb nur die
Paare zurueck, die dieser Lauf messen konnte. Ein frueher gemessener
Aufschlag verschwand damit **ersatzlos** — schlimmer als ein veralteter Wert,
denn eine fehlende Zeile ist von „gemessen und unkritisch" nicht zu
unterscheiden. Entfernt wird jetzt nur, was dieser Lauf neu setzt, was er
serialisiert und was er messen wollte und verwerfen musste; die verworfenen
Paare nennt der Lauf ausdruecklich beim Namen. Beim Reparieren fiel
ausserdem auf, dass ein Lauf mit `slots: 1` die Tabelle ebenfalls leerte,
obwohl er gar keine Paare misst.

Die Kennzeichnung der Soloprofile bleibt damit der offene Rest — sie braucht
ein Feld im Schema, das zwischen „in diesem Lauf gemessen" und „aus der
Eingabe uebernommen" unterscheidet.

### 5. Der Bericht wechselt mitten im Dokument die Sprache (behoben am 15.09.)

**Nachtrag 15.09.:** `vig-fit` schreibt dieselbe Feststellung jetzt zusaetzlich
als `verdict_en` in sein JSON, aus einer gemeinsamen, sprachfreien Feststellung
und damit mit denselben Zahlen; `autotune` uebernimmt die englische Fassung. Die
Terminalausgabe von `vig-fit` bleibt deutsch wie alle Werkzeuge dieses
Projekts. Aeltere Laeufe tragen weiter den deutschen Satz.

`qualification.md` ist durchgehend englisch — Ueberschriften, Prosa,
Schritttabelle — und dann steht der Urteilsblock auf Deutsch da, zweimal:
einmal als Schlagzeile, einmal unter „Is the governor worth it here?".
`autotune` uebernimmt das Urteil woertlich von `vig-fit`, und `vig-fit`
schreibt deutsch. Das ist eine bewusste Entscheidung gewesen (woertlich
zitieren statt uebersetzen) und die falsche: Dies ist das Dokument, das ein
Interessent liest, um eine Anschaffung zu begruenden. Ein Urteil, das er
nicht lesen kann, ist kein Urteil.

### 6. Die Belegungsstufe misst auf diesem Backend die falsche Groesse (behoben am 15.09.)

**Nachtrag 15.09.:** Das war der eigentliche Grund, warum `autotune` nicht
dasselbe ergab wie die von Hand getunte Konfiguration. `under_load` gilt im
Plan fuer **jeden** belegten Nachbarn, nicht nur fuer dasselbe Modell: Der
Governor haette den Detektor neben `pose` mit 297 ms geplant, gemessen sind
dort 0,96x von 158 ms. Eine Stufe ab `(belegt + 1) × 90 %` der Solozeit gilt
jetzt als Warteschlange und wird nicht geschrieben (Detektor 1,87x und Tiefe
2,05x fallen darunter, `pose` mit 0,90x nicht); keine Stufe liegt mehr unter
der Solozeit; eine verworfene Stufe laesst die naechste nicht nachruecken. Der
Nachweis auf dem Telefon steht unten unter „Vergleichslauf".

`autotune` uebernimmt von `vig calibrate` die Stufe „1 weiterer Slot belegt",
die den zweiten Slot mit **demselben** Modell belegt. Auf dem TFLite-Server
wartet dieser Auftrag im Modellthread, statt nebenher zu laufen — das ist
eine Warteschlange, keine Nebenlaeufigkeit. `android-gpu.md` haelt das fest
und entfernt die Stufe in den Messkonfigurationen von Hand. Die Stufe lief,
erzeugte Zahlen (2,05x / 1,87x / 0,90x), und diese Zahlen bedeuten auf einem
Backend mit einem Thread je Modell **keine Belegung**. `autotune` sagt das
nicht.

### 7. Das Manifest zeigte Rust-Innereien — behoben

Im Laptoplauf stand `Observed(Sample { value: "NVIDIA GeForce RTX 3070 Laptop
GPU", source: NvidiaSmi, observed_at_ms: 1789401260308 })` im Geraetefeld.
Jetzt stehen dort der Wert und daneben Quelle und Zeitpunkt als eigene
Felder; wo nichts beobachtbar ist, steht `not observable`. Die gespeicherten
Laptop-Artefakte tragen noch die alte Form, weil der Lauf aelter ist als der
Fix.

### 8. Takt nicht beobachtbar — so gewollt

Ohne `nvidia-smi` meldet der Bericht einen eigenen Abschnitt „Clock not
observable" mit dem Wortlaut des Fehlers und dem Satz, dass nichts an seine
Stelle gesetzt wurde. Kein Abbruch, keine stillschweigend andere Groesse.

### 9. Die Dauerzusage haelt

„Planned steps (18 minutes, estimated generously)" — innerhalb der zugesagten
halben Stunde, auf dem schwaechsten Geraet, das wir haben, ohne dass auf
`--quick` ausgewichen werden musste.

## Nachvalidierung am 15.09.2026, nach den vier Reparaturen

Die Befunde 1 bis 4 wurden am 14.09. um 22:41 behoben (`456fe07`). Geprueft war
das zu diesem Zeitpunkt ausschliesslich durch Unit- und Funktionstests — 16 fuer
`autotune`, Gate gruen mit 57 Suiten und 845 Tests. **Ein Lauf gegen echte
Hardware fand danach zunaechst nicht statt**; die Nacht war durch den Dauerlauf
belegt, der die Messsperre acht Stunden exklusiv hielt. Tests, die den Schritten
gefaelschte Ergebnisse unterschieben, zeigen, dass die Ehrlichkeitsregeln
greifen — nicht, dass der Befehl auf der Karte durchlaeuft.

Am 15.09. ist er zweimal gelaufen, gegen `examples/gate_m3/vig.yaml`.

**Erster Lauf (09:17, `autotune-laptop-2026-09-15/`): verschmutzt — durch die
Auswertung des Dauerlaufs, die daneben lief.** `measure` und `fit` stehen auf
`contaminated`, Grund „system load 2.08 before and 2.22 after". Der
Fremdlastwaechter hat also die eigene Nebenarbeit erwischt; das ist sein Zweck,
und der Lauf taugt damit als Funktionsnachweis, nicht als Messung.

Aus ihm stammt der beste Beleg fuer Befund 1: `state.json` traegt jetzt
`config`, `doctor`, das vollstaendige `fit_verdict`, die Serienzahlen, **alle
vier Schritte mit Ausgang, Grund und Notizen** — und den Fingerabdruck
`127.0.0.1:8001|6779674cb1213087`. Vorher stand dort `{"done":[…]}`. Und `done`
enthielt nur `["discover","check"]`: Die verschmutzten Schritte gelten **nicht**
als erledigt, eine Fortsetzung wuerde sie wiederholen.

**Zweiter Lauf (09:24, `autotune-laptop-2026-09-15b/`): sauber.**

| | |
|---|---|
| Exitcode | 0 |
| Schritte | alle vier `done` |
| `contaminated` | `false` |
| `complete` | `true` |
| `release` | `refused` — „3 of 4 measurement series were discarded" |
| `doctor` | `READY_WITH_WARNINGS` |

Damit ist belegt, was vorher nur behauptet war: Der reparierte Befehl laeuft auf
echter Hardware vollstaendig durch, schreibt alle Artefakte und verweigert die
Freigabe mit genau einem nachvollziehbaren Grund.

**Eine Ungereimtheit, die offen bleibt.** Derselbe Aufbau lieferte einmal
`NOT_READY` und einmal `READY_WITH_WARNINGS`. Naheliegend ist, dass das
`measured.yaml` des verschmutzten Laufs unter Fremdlast entstand und `doctor`
damit andere Zahlen vorfand — **nachgewiesen ist das nicht**, und bis dahin ist
es eine Vermutung und keine Erklaerung.

**Nicht vergleichbar mit dem 14.09.** `examples/gate_m3/vig.yaml` hat
`slots: 1`; der Lauf meldet „4 models, 1 slots, 200 samples — 4 cells in all"
und misst deshalb **keine Paare**. Die Validierung vom Vortag lief mit zwei
Slots und 16 Zellen. Die Zahlen der beiden Tage stehen also nebeneinander, nicht
gegeneinander.

**Was der Lauf ueber die Reparatur von Befund 3 zeigt:** Die Ansage nennt jetzt
die Matrix, auf der sie beruht, und sagt dazu, dass sie die Hardware nicht
kennt — „The estimate scales with that matrix, not with your hardware."

## Vergleichslauf am 15.09.2026, mit dem reparierten Stand

Stand `99f66c7` (dazu `rustls` 0.23.45, ohne Einfluss auf eine Messung ueber
Loopback). Gleiche Eingabe, gleiche Kernbindung, gleiche Probenzahl wie am
14.09. Die Tabellen hat `InferenceQoS-runtime/skripte/compare-measured.py` aus den
beiden eingefrorenen Dateien erzeugt, nicht aus diesem Text.

### Pixel 2: `autotune` ergibt jetzt die Struktur der Handkonfiguration

Lauf `InferenceQoS-runtime/messungen/autotune-pixel2-2026-09-15/`, `taskset f0`, 200
Proben, `vig-slots2-base.yaml`. Kalt gestartet bei 32 °C, Ende 39 °C. Die
`loadavg` stand vor dem Lauf bei **3,46** — und der Lauf ist trotzdem
**nicht** verschmutzt, weil jetzt fremde CPU-Zeit gemessen wird (Befund 1).

| | |
|---|---|
| Schritte | alle vier `done` |
| Messreihen | **12 von 12** verwertbar |
| `contaminated` | false |
| Freigabe | **not issued** — der beste Ausgang, den das Werkzeug kennt |
| `vig-fit` | „Up to 125 % offered load the direct path loses nothing either. On this machine, with these models and contracts, the governor is not worth it" |
| `doctor` | `READY_WITH_WARNINGS` |

**Gegen die von Hand getunte Konfiguration** (`examples/android_gpu/vig-slots2.yaml`, 11.09.):

| Modell | von Hand p50 us | autotune p50 us | Abweichung | von Hand Laststufe | autotune Laststufe |
|---|---:|---:|---:|---|---|
| depth | 199 642 | 193 969 | −2,8 % | keine | keine |
| detector | 222 435 | 220 582 | −0,8 % | keine | 273 106 |
| pose | 89 815 | 92 918 | +3,5 % | keine | 92 918 |

| `no_corun` | von Hand | autotune |
|---|---|---|
| [depth, pose] | ja | ja |

| Interferenz (Opfer ← Nachbar) | von Hand added_us | autotune added_us |
|---|---:|---:|
| depth ← detector | 51 155 | 47 865 (−6,4 %) |
| detector ← depth | 116 347 | 121 562 (+4,5 %) |

**Gegen den `autotune`-Lauf vom 14.09.:**

| Modell | 14.09. p50 us | 15.09. p50 us | Abweichung | 14.09. Laststufe | 15.09. Laststufe |
|---|---:|---:|---:|---|---|
| depth | 206 206 | 193 969 | −5,9 % | 423 622 | keine |
| detector | 158 474 | 220 582 | +39,2 % | 297 155 | 273 106 |
| pose | 89 173 | 92 918 | +4,2 % | 80 850 | 92 918 |

| `no_corun` | 14.09. | 15.09. |
|---|---|---|
| [depth, detector] | ja | **nein** |
| [depth, pose] | ja | ja |

Was das heisst:

- **Dieselbe Struktur.** Dasselbe serialisierte Paar, dieselben zwei
  Interferenzeintraege, Aufschlaege innerhalb von 6,4 %, Soloprofile innerhalb
  von 3,5 %. Am 14.09. waren es zwei serialisierte Paare und eine leere
  Tabelle.
- **Die Detektor-Grundlinie trifft** (−0,8 % statt −28,8 %). Damit liegt
  `detector neben depth` bei 1,55x statt 2,18x, unter der Schwelle, und das
  Paar wird wieder ein Interferenzeintrag statt `no_corun`. **Warum** die
  Grundlinie am 14.09. so niedrig lag (Befund 3), belegt dieser Lauf nicht; er
  zeigt nur, dass sie es heute nicht ist.
- **Die Warteschlangen-Erkennung greift** (Befund 6): Tiefe neben sich selbst
  2,10x, keine Laststufe geschrieben.
- **Eine Abweichung bleibt.** Der Detektor neben sich selbst lag diesmal bei
  **1,23x** (dokumentiert 1,82x, am 14.09. 1,87x). Das liegt unter der
  Warteschlangen-Grenze, die Stufe ist geschrieben, die Handkonfiguration hat
  keine. Der Detektor ist das einzige Modell mit einem Operator auf der CPU
  (NMS); welcher der beiden Werte typisch ist, sagt ein einzelner Lauf nicht.
  Die Richtung ist die vorsichtige: Mit belegtem Nachbarn plant der Governor
  den Detektor mit 273 statt 221 ms, nicht kuerzer. `pose` steht auf der
  Solozeit, weil die gemessenen 0,86x geklemmt werden.
- **Das Urteil passt zur Messung auf dem Telefon.** `android-gpu.md` fand bis
  zu geplanten 138 % keinen Einbruch des Backends; `vig-fit` sagt fuer 90 bis
  125 % dasselbe in einem Satz: hier lohnt sich der Governor nicht.

### Pixel 2, zweiter Lauf: wie gut wiederholt sich das?

Lauf `InferenceQoS-runtime/messungen/autotune-pixel2-2026-09-15b/`, 13:21–13:33, gleiches
Geraet, gleiche Eingabe, Binaries auf dem Stand `2c1f3d0` (mit der
gemeinsamen CPU-Messung). Wieder **vollstaendig und unverschmutzt**: 12 von 12
Reihen, fremde Rechenzeit 0,02 Kerne vor und 0,06 nach `vig-fit`, Freigabe
„not issued", dasselbe Urteil.

| Modell | Lauf 1 (12:00) p50 us | Lauf 2 (13:21) p50 us | Abweichung | Lauf 1 Laststufe | Lauf 2 Laststufe |
|---|---:|---:|---:|---|---|
| depth | 193 969 | 195 410 | +0,7 % | keine | keine |
| detector | 220 582 | 220 927 | +0,2 % | 273 106 | 242 826 |
| pose | 92 918 | 89 967 | −3,2 % | 92 918 | 89 967 |

| Interferenz (Opfer ← Nachbar) | Lauf 1 added_us | Lauf 2 added_us | von Hand |
|---|---:|---:|---:|
| depth ← detector | 47 865 | 59 123 (+23,5 %) | 51 155 |
| detector ← depth | 121 562 | 122 610 (+0,9 %) | 116 347 |

`no_corun` in beiden Laeufen und in der Handkonfiguration: [depth, pose]
(`pose` neben `depth` 2,65x, von Hand 2,64x).

Was das heisst:

- **Die Struktur wiederholt sich exakt:** dasselbe serialisierte Paar,
  dieselben zwei Interferenzeintraege, die Tiefe wieder als Warteschlange
  erkannt (2,13x), dasselbe Urteil.
- **Die Soloprofile wiederholen sich innerhalb von 3,2 %** und liegen im
  zweiten Lauf innerhalb von 2,1 % der Handmessung.
- **Am meisten streut der kleinere Aufschlag** (`depth ← detector`, 47 865 →
  59 123 us). Die Handmessung liegt zwischen beiden Laeufen. Wer mit dieser
  Zahl plant, sollte mehrere Laeufe haben — ein einzelner autotune-Lauf ist
  eine Messung, kein Mittelwert.
- **Die Laststufe des Detektors** liegt diesmal bei 1,09x (242 826 us) statt
  1,23x, beide unter der Warteschlangen-Grenze; die Handkonfiguration hat keine.

## Tuning auf Hardware, 15.09.2026 nachmittags

Seit ADR-0045 hat `vig autotune` den Schritt `tune`: Es probiert
Pipelining, Versorgungsschutz, gelernte Marge und Sicherheitsmarge in einem
Durchgang gegen die Vertraege und behaelt nur, was die geschuetzten Stroeme
ueber die Rauschschwelle hinaus besser versorgt. Drei Laeufe, alle vollstaendig
und unverschmutzt.

### Laptop, Gate-M3-Last: die Handkonfiguration ist schon die beste

`messungen/autotune-laptop-2026-09-15-tuned/` (14:16, erste Zielgroesse) und
`messungen/autotune-laptop-2026-09-15-tuned-mean/` (14:29, Mittelwert der
nachrangigen Stroeme, Stand `08cf0ff`). Beide: sechs Fassungen, **keine
behalten**. Der zweite Lauf:

| # | Einstellung | geschuetzt, schlechtester ‰ | nachrangig, Mittel ‰ | Entscheidung |
|---:|---|---:|---:|---|
| 0 | ungetunt (wie gemessen) | 3 | 333 | Ausgangspunkt |
| 1 | `pipelining_depth → 0` | 0 | 333 | im Rauschen |
| 2 | `protect_supply → true` | 0 | 333 | im Rauschen |
| 3 | `margin_learning → an` | 0 | 333 | im Rauschen |
| 4 | `safety_margin_percent → 125` | 2 | 333 | im Rauschen |
| 5 | `safety_margin_percent → 100` | 0 | 333 | im Rauschen |

- **Die geschuetzten Stroeme liegen in jeder Fassung bei 0 bis 3 ‰.** Auf
  dieser Last gibt es dort nichts zu gewinnen, und die Schwelle von 5 ‰
  verhindert, dass ein Zufallsunterschied als Einstellung festgeschrieben wird.
- **Die 333 ‰ sind `pose` und `depth` bei 0 ‰ und der VLM-Block bei 1000 ‰.**
  Der unteilbare 95-ms-Block passt neben einer 33-ms-Periode nie (ADR-0012).
  In der ersten Zielgroesse, dem *schlechtesten* nachrangigen Strom, stand er
  in jeder Fassung als 1000 ‰ und haette jede Verbesserung anderer Stroeme
  verdeckt; deshalb zaehlt seit `08cf0ff` der Mittelwert.
- **Das ist eine Bestaetigung, kein Leerlauf.** `examples/gate_m3/vig.yaml`
  ist die von Hand abgestimmte Konfiguration (unter anderem
  `pipelining_depth: 1`). autotune findet keine bessere — und schreibt deshalb
  keine andere.

### Pixel 2, Overload-Vertraege: ungesaettigt, nichts zu tunen

`messungen/autotune-pixel2-2026-09-15-tuned/`, Eingabe
`vig-slots2-overload.yaml`. 12 von 12 Reihen. Die Datei plant **138 % auf
einem Slot**; autotune misst aber `slots: 2`, also rund 69 % je Slot, und
`vig-fit` faehrt bis 125 % davon. Das Telefon ist damit nicht gesaettigt:
Der direkte Weg verliert bis 125 % nichts, und alle sechs Fassungen liegen bei
0 ‰ geschuetzt und 0 bis 15 ‰ nachrangig — im Rauschen.

**Die Lehre fuer Werkzeug und Anwender:** Tuning braucht eine Last, auf der
etwas zu retten ist. Ohne Saettigung ist „die gemessene Konfiguration ist schon
die beste" die richtige Antwort, und sie sagt nichts ueber das Geraet.

### Pixel 2, Heavy-Vertraege: ein Gewinn, der nicht haelt

`messungen/autotune-pixel2-2026-09-15-tuned-heavy/`, Eingabe
`vig-slots2-heavy.yaml` (277 % auf einem Slot, rund 138 % je Slot), Stand
`08cf0ff`. 12 von 12 Reihen, unverschmutzt (fremde Rechenzeit 0,02 / 0,17
Kerne), Geraet 36 → 41 °C.

Das Tuning **behielt zwei Einstellungen**: Versorgungsschutz an, dann
Sicherheitsmarge 100.

| # | Einstellung | geschuetzt, schlechtester ‰ | nachrangig, Mittel ‰ | Entscheidung |
|---:|---|---:|---:|---|
| 0 | ungetunt | 220 | 820 | Ausgangspunkt |
| 1 | `pipelining_depth → 1` | 220 | 817 | im Rauschen |
| 2 | `protect_supply → true` | 75 | 783 | **behalten** |
| 3 | `margin_learning → an` | 440 | 837 | schlechter als ungetunt |
| 4 | `safety_margin_percent → 125` | 260 | 849 | schlechter als ungetunt |
| 5 | `safety_margin_percent → 100` | 50 | 730 | **behalten** |

**Der Bestaetigungslauf widerspricht.** `fit` misst danach genau diese
getunte Konfiguration noch einmal, gegen den direkten Weg:

| geschuetzter Strom (Detektor), getunte Konfiguration | im Tuning, 10 s | in `fit`, 10 s |
|---|---:|---:|
| 100 % Last | 50 ‰ | 25 ‰ |
| 110 % Last | 22 ‰ | 90 ‰ |
| 125 % Last | 40 ‰ | **440 ‰** |

Dieselbe Konfiguration, einmal 40 und einmal 440 ‰: Auf einem gesaettigten,
sich erwaermenden Telefon streut ein einzelnes 10-Sekunden-Fenster weit mehr,
als die behaltene Einstellung bewirkt. **Der Gewinn 220 → 50 ‰ ist deshalb
nicht belegt** und wird nirgends als Ergebnis genannt.

Dazu kommt: `vig doctor` meldet fuer diese Vertraege **NOT_READY** —
geschuetzte Auslastung 126 % ueber zwei Slots. Keine Einstellung des Governors
kann eine Zusage tragen, die die Hardware nicht traegt; das Tuning haette gar
nicht erst suchen duerfen.

**Zwei Fehler im Werkzeug, beide in Arbeit:** (1) eine Machbarkeitspruefung vor
dem Tuning — bei NOT_READY wird nicht getunt, mit Begruendung; (2) ein
Bestaetigungsschritt — ungetunt und getunt werden abwechselnd erneut gemessen,
und uebernommen wird nur, was in jedem Paar haelt.

Fuer einen belastbaren Tuning-Nachweis auf dem Telefon braucht es eine Last,
die **gesaettigt, aber planbar** ist: `vig-slots2-saturated.yaml`. Ein erster
Entwurf lockerte nur den Detektor (250 → 350 ms) und blieb bei 123 % NOT_READY
— die geschuetzte Auslastung zaehlt auch `pose` und `depth` (Klasse `high`).
Die Datei nimmt deshalb alle drei Perioden von Overload mal 0,74 (Detektor
370 ms, `pose` 185 ms, `depth` 740 ms): `vig doctor` offline **95 %,
READY_WITH_WARNINGS**. `vig-fit` faehrt davon 100 bis 125 % und saettigt die
zwei Slots.

### Pixel 2, gesaettigt und planbar: der Governor gewinnt, das Tuning haelt dicht

`messungen/autotune-pixel2-2026-09-15-tuned-saturated/`, Eingabe
`vig-slots2-saturated.yaml` (95 % geschuetzt, READY_WITH_WARNINGS), Binary mit
Machbarkeitspruefung und Bestaetigung. 15:09–15:28, 35 → 39 °C.

| | |
|---|---|
| Schritte | alle fuenf `done` (measure 591 s, tune 381 s, fit 90 s) |
| Messreihen | 12 von 12 verwertbar |
| `contaminated` | false |
| Freigabe | **not issued** |
| `doctor` | READY_WITH_WARNINGS |

**Der Governor gegen den direkten Weg** (`fit`, beide Arme): ab 90 % Last
verfehlt der direkte Weg **208 ‰** der Takte des geschuetzten Stroms, der
Governor **0 ‰**. Der Preis steht daneben: Die nachrangigen Stroeme verlieren
direkt 270 ‰, unter dem Governor 1000 ‰. Das ist der Gegenbefund zu den beiden
ungesaettigten Telefonlaeufen oben: Ob der Governor auf einem Telefon etwas
bringt, haengt an der Last, nicht am Geraet.

**Das Tuning.** Die Suche fand eine Einstellung, die besser aussah:

| # | Einstellung | geschuetzt, schlechtester ‰ | nachrangig, Mittel ‰ | Entscheidung |
|---:|---|---:|---:|---|
| 0 | ungetunt | 0 | 515 | Ausgangspunkt |
| 1 | `pipelining_depth → 1` | 0 | 482 | im Rauschen |
| 2 | `protect_supply → true` | 30 | 515 | schlechter als ungetunt |
| 3 | `margin_learning → an` | 0 | 508 | im Rauschen |
| 4 | `safety_margin_percent → 125` | 37 | 488 | schlechter als ungetunt |
| 5 | `safety_margin_percent → 100` | 0 | 376 | behalten |

**Die Bestaetigung hat sie verworfen**, abwechselnd gemessen:

| Paar | ungetunt geschuetzt / nachrangig ‰ | getunt geschuetzt / nachrangig ‰ | haelt |
|---:|---:|---:|---|
| 1 | 34 / 489 | 37 / 485 | nein — geschuetzt schlechter |
| 2 | 0 / 508 | 0 / 469 | nein — im Rauschen |

`measured.yaml` blieb ungetunt. Das ist der Fall, fuer den die Bestaetigung
gebaut wurde: Auf dem Heavy-Lauf haette dieselbe Art Suchgewinn eine
Einstellung festgeschrieben, die im naechsten Lauf nicht mehr galt. Hier sagt
das Werkzeug stattdessen, dass sie nicht haelt, und laesst die Konfiguration,
wie sie ist.

**Was damit belegt ist:** `vig autotune` laeuft auf dem Telefon mit allen fuenf
Schritten durch, erkennt eine unerfuellbare Last (Heavy) und schreibt nichts
auf Grund von Rauschen fest. **Was nicht belegt ist:** ein bestaetigter
Tuning-Gewinn. Auf keiner der gemessenen Lasten hat eine andere Einstellung als
die gemessene zweimal hintereinander gehalten.

**Nachtraeglich: zu kurze Fenster.** Alle Zahlen dieses Abschnitts stammen aus
10-s-Fenstern. Bei 370 ms Detektorperiode sind das 27 Takte je Punkt (am
90-%-Punkt 24): 37 ‰ sind **ein** Takt, die 208 ‰ von `fit` fuenf. Die Suche
hat also null gegen einen verfehlten Takt verglichen. Seitdem bemessen `tune`,
`fit` und `vig-fit` das Fenster in Takten (ADR-0045, Nachtrag „Messfenster in
Takten"); die Nachmessung steht unten.

### Laptop: die Verweigerung steht vorn

Lauf `InferenceQoS-runtime/messungen/autotune-laptop-2026-09-15f/`, gleiche Bedingungen
wie `15e`. **2 von 4** Reihen verworfen (`SwPowerCap` wechselte waehrend der
Reihe), `contaminated: false`, Freigabe verweigert. Der Bericht beginnt jetzt
mit „This run did not qualify this machine: 2 of 4 measurement series were
discarded" statt mit dem Urteil; das Urteil steht englisch in seinem
Abschnitt: geschuetzter Strom ab 90 % direkt 996 ‰, Governor 0 ‰, nachrangige
Stroeme 559 ‰ → 1000 ‰.

### Pixel 5: ein zweites Geraet, dieselbe Struktur

Zweites Geraet ohne Handkonfiguration (Snapdragon 765G, Adreno 620),
dieselben Modelle, dieselbe Eingabe, dieselben Binaries (`2c1f3d0`),
`taskset c0` (die beiden A76-Kerne). Lauf
`InferenceQoS-runtime/messungen/autotune-pixel5-2026-09-15/`, 13:01–13:13, 33 °C zu
Beginn, 35 °C am Ende.

Ein erster Versuch um 12:02 brach ab, weil das Geraet mitten in der
Paarmessung vom USB verschwand; seine Rohdaten liegen unter
`…-abgebrochen/` und gehen in nichts hier ein.

| | |
|---|---|
| Schritte | alle vier `done` |
| Messreihen | **12 von 12** verwertbar |
| `contaminated` | false — fremde Rechenzeit 0,02 Kerne vor, 0,03 nach `vig-fit` |
| Freigabe | **not issued** |
| `vig-fit` | „Up to 125 % offered load the direct path loses nothing either … the governor is not worth it" |
| `doctor` | `READY_WITH_WARNINGS` |

**Gegen den Pixel-2-Lauf desselben Stands** (verschiedene Geraete, also keine
Uebereinstimmung der Zahlen erwartet — verglichen wird, ob `autotune` dieselbe
Art Konfiguration ableitet):

| Modell | Pixel 2 p50 us | Pixel 5 p50 us | Abweichung | Pixel 2 Laststufe | Pixel 5 Laststufe |
|---|---:|---:|---:|---|---|
| depth | 193 969 | 215 138 | +10,9 % | keine | keine |
| detector | 220 582 | 264 316 | +19,8 % | 273 106 | 432 192 |
| pose | 92 918 | 93 801 | +1,0 % | 92 918 | 106 054 |

| `no_corun` | Pixel 2 | Pixel 5 |
|---|---|---|
| [depth, pose] | ja | ja |

| Interferenz (Opfer ← Nachbar) | Pixel 2 added_us | Pixel 5 added_us |
|---|---:|---:|
| depth ← detector | 47 865 | 15 325 |
| detector ← depth | 121 562 | 194 195 |

Was das heisst:

- **Dieselbe Struktur auf einem anderen SoC.** Dasselbe serialisierte Paar
  (`pose` leidet 2,42x unter `depth`, auf dem Pixel 2 dokumentiert 2,64x), dieselben
  zwei Interferenzeintraege, dasselbe Urteil. Die Zahlen darin gehoeren dem
  Geraet: Das Pixel 5 ist hier **langsamer** als das Pixel 2 — die Adreno 620
  ist eine Mittelklasse-GPU, die Adreno 540 war eine Flaggschiff-GPU.
- **Die Warteschlangen-Grenze hat eine Kante, und sie ist zweimal getroffen
  worden.** Die Tiefe lag neben sich selbst bei **1,82x** (im abgebrochenen
  Versuch 1,83x), die Grenze ist 1,80x. Beide Male als Warteschlange erkannt;
  ein etwas schnellerer Lauf haette eine Laststufe geschrieben. Das ist ein
  Grund, die Schwelle nicht als belegt zu betrachten, sondern als Heuristik mit
  genau diesem Messpunkt daneben.
- **Der Detektor schreibt auf beiden Geraeten eine Laststufe** (1,23x und
  1,63x), die die Handkonfiguration des Pixel 2 nicht hat. Auf beiden in der
  vorsichtigen Richtung.
- **Wiederholbarkeit auf demselben Geraet:** Tiefe und Detektor liegen im
  abgebrochenen Versuch und in diesem Lauf innerhalb von 1 % (215 181 /
  215 138 und 266 803 / 264 316 us), `pose` weicht um 4,4 % ab (89 826 /
  93 801 us). Das ist ein Hinweis, kein Beleg: der erste Versuch ist
  unvollstaendig.

### Nebenbefund: `vig-fit` hatte denselben Fehler

Waehrend des Telefonlaufs meldete `vig-fit` „Die Systemlast liegt bei 4,6 …
auf einer unruhigen Maschine". Es las ebenfalls `loadavg`. Beide Werkzeuge
messen jetzt dieselbe Groesse aus `vig_platform::cpu`; das JSON von `vig-fit`
traegt `foreign_cores_centi_before` und `_after` statt `loadavg_before` und
`_after`.

## Nachmessung mit Fenstern in Takten, 15.09.2026 abends

Anwendungsfall „Lieferroboter mit Telefon-SoC" (docs/use-cases.md), Eingabe
`vig-slots2-saturated.yaml`, beide Telefone gleichzeitig, Binaries aus dem
Arbeitsstand, der als `90f517c` committet wurde. `tune` misst 74 s je Punkt
(200 Takte), `fit` 83 s je Arm und Punkt (201 Takte am 90-%-Punkt). Laeufe
`messungen/autotune-pixel2-2026-09-15-usecase-fenster/` und
`…-pixel5-…/`, 16:35–17:35, alle fuenf Schritte `done`, Exitcode 0, fremde
Rechenzeit 0,04 bis 0,08 Kerne.

**`fit`, Detektor (geschuetzt), verfehlte Takte direkt / Governor:**

| Last | Pixel 2 | Pixel 5 |
|---:|---:|---:|
| 90 % | **293 / 99 ‰** | **497 / 208 ‰** |
| 100 % | 4 / 4 ‰ | 4 / 4 ‰ |
| 110 % | 4 / 0 ‰ | 4 / 4 ‰ |
| 125 % | 3 / 3 ‰ | 3 / **32 ‰** |

**Der Preis (nachrangig, direkt / Governor):** `pose` bei 90 % 272 / 363 ‰
(Pixel 2) und 222 / 413 ‰ (Pixel 5); `depth` bei 125 % 0 / 328 ‰ und
0 / 571 ‰.

**`tune`:** Auf keinem Telefon wurde etwas behalten. Pixel 2: ungetunt 5 ‰
geschuetzt, alle Fassungen 5 bis 12 ‰. Pixel 5: ungetunt 28 ‰, bester Versuch
`protect_supply → true` mit 16 ‰ — die gezaehlte Schwelle verlangte 35 ‰
Vorsprung, also keine Bestaetigung.

Was daraus folgt:

- **Die alte Zahl ist ersetzt.** „208 → 0 ‰" aus dem 10-s-Fenster (fuenf von
  24 Takten) wird nirgends mehr genannt; mit 201 Takten sind es 293 → 99 ‰ auf
  dem Pixel 2.
- **Der direkte Weg verliert fast nur am 90-%-Punkt.** Ab 100 % liegen beide
  Arme beim Detektor bei hoechstens 4 ‰. Das war schon im 10-s-Lauf so (208 ‰
  bei 90 %, 37 ‰ bei 100 %, 0 ‰ bei 110 %) und ist **nicht erklaert**.
  Verdacht: die Phasenlage der drei Perioden gegen die rund 220 ms Rechenzeit
  des Detektors. Bevor diese Zahl als „der Governor bringt auf Telefonen X"
  verallgemeinert wird, gehoert der Punkt zwischen 85 und 100 % feiner
  abgetastet.
- **Ein Punkt gegen uns:** Pixel 5 bei 125 %, Governor 32 ‰ gegen direkt 3 ‰.
  `vig-fit` nennt im Urteilssatz nur den ersten Punkt, an dem der direkte Weg
  verliert; dieser Punkt steht nur in der Tabelle. Offen: das Urteil sollte
  auch einen spaeteren Punkt nennen, an dem der Governor schlechter ist.
- **Tuning:** Mit Fenstern, die einzelne Takte zaehlen, gab es auf diesem
  Anwendungsfall nichts zu gewinnen. Ein bestaetigter Tuning-Gewinn liegt
  weiterhin auf keiner Last vor.
