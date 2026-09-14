# Validierung von `vig autotune` gegen die bekannten Aufbauten

Datum: 14.09.2026. Geprueft wird nicht, ob der Governor etwas bringt, sondern
ob **`vig autotune` auf unseren eigenen, bereits von Hand vermessenen
Aufbauten dasselbe herausbekommt wie wir**. Wo die Zahlen abweichen, ist das
ein Befund ueber `autotune` und nicht ueber die Maschine.

Rohdaten:
`InferenceQoS-runtime/autotune-laptop-2026-09-14/` und
`InferenceQoS-runtime/autotune-pixel2-2026-09-14/` (der Lauf ohne Vorlauf
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

### 1. Die Fremdlasterkennung ist auf ARM falsch geeicht (offen)

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

### 5. Der Bericht wechselt mitten im Dokument die Sprache (offen)

`qualification.md` ist durchgehend englisch — Ueberschriften, Prosa,
Schritttabelle — und dann steht der Urteilsblock auf Deutsch da, zweimal:
einmal als Schlagzeile, einmal unter „Is the governor worth it here?".
`autotune` uebernimmt das Urteil woertlich von `vig-fit`, und `vig-fit`
schreibt deutsch. Das ist eine bewusste Entscheidung gewesen (woertlich
zitieren statt uebersetzen) und die falsche: Dies ist das Dokument, das ein
Interessent liest, um eine Anschaffung zu begruenden. Ein Urteil, das er
nicht lesen kann, ist kein Urteil.

### 6. Die Belegungsstufe misst auf diesem Backend die falsche Groesse (offen)

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
