# Der Entscheidungspfad auf echten ARM-Kernen

Datum: 11.09.2026. Geräte: Pixel 2 (Snapdragon 835, Android 11) und Pixel 5
(Snapdragon 765G, Android 14) über USB; Bezug: RTX-3070-Laptop
(i7-10870H). Werkzeug: `decision-bench` (`crates/vig-sim/src/bin/`),
Ablauf `tools/arm/build-and-run.sh`. Rohdaten:
`InferenceQoS-runtime/messungen/arm-phones-2026-09-11/`.

## Die Frage

Der Kern ist für `aarch64` gebaut und unter Emulation getestet. Emulation
sagt, **dass** er läuft, und nichts darüber, **wie schnell**. Die Frage an
eine Jetson-Klasse-CPU lautet: fällt eine Scheduling-Entscheidung in einem
Bruchteil der Periode, die sie schützt — auch auf schwachen Kernen, auch mit
vielen Modellen?

Handys sind dafür kein Ersatz für eine Gerätequalifikation, aber eine
ehrliche Schranke: ihre großen Kerne sind Cortex-A73- und A76-Klasse, die
eines Jetson Orin A78AE. Wer auf einem A73 schnell genug ist, ist es auf einem
A78AE erst recht; wer es auf einem A53 ist, hat Reserve.

## Aufbau

- **Der echte `vig_core::Scheduler`**, getrieben durch dieselbe
  deterministische Ereignisfolge wie der Simulator (`harness::run_observed`,
  bitgleich zu `run`). Jeder Aufruf von `on_event` wird mit der Wanduhr
  gemessen, getrennt nach Ereignisart: Ankunft (Zulassung, Verdrängung,
  Variantenwahl, Dispatch), Fertigstellung, Takt.
- **Drei Szenarien.** Der Vertragssatz aus `examples/gate_m3/vig.yaml`
  (Detektor mit zwei Varianten, Pose, Tiefe, VLM) auf 103 % skaliert; dazu
  16 und 32 Kamerastroeme mit je zwei Varianten bei 110 %. Über der Sättigung,
  damit Verdrängung, Verwerfen und Abwertung im gemessenen Pfad liegen:
  im Gate-Szenario 254 Verdrängungen und 374 verworfene Aufträge je Lauf.
- **300 s simulierte Zeit** je Lauf, ein verworfener Aufwärmlauf, drei
  Wiederholungen mit verschiedenen Seeds. Berichtet: Median über die
  Wiederholungen, Spannweite des p99 dazu.
- **Statisch gebaut** für `aarch64-unknown-linux-musl` mit `rust-lld`, ohne
  NDK, ohne Root: das Binary läuft aus `/data/local/tmp`. Kerngruppen per
  `taskset` getrennt.
- **Gemessen wird der Kern, nicht das Gerät.** Keine Inferenz, kein Netz, kein
  Datenpfad, kein OIP-Backend auf den Handys.

Jede Einzelmessung enthält die Kosten einer Uhrablesung — 15 ns auf dem
Laptop, 52 ns auf dem Pixel 2, 156–364 ns auf dem Pixel 5. Sie werden
berichtet und nicht abgezogen.

## Ergebnis

p99 der Entscheidungszeit über **alle** Ereignisse, in Mikrosekunden, Median
aus drei Wiederholungen:

| Kerne | Klasse | 4 Modelle (Gate M3) | 16 Modelle | 32 Modelle |
|---|---|---:|---:|---:|
| Laptop, i7-10870H | x86 | 3,0 | 11,4 | 27,0 |
| Pixel 5 Prime, 2,4 GHz | Cortex-A76 | 5,4 | 13,3 | 25,5 |
| Pixel 2 Gold, 2,46 GHz | Cortex-A73 | 10,3 | 30,6 | 59,8 |
| Pixel 2 Silber, 1,9 GHz | Cortex-A53 | 20,1 | 79,1 | 150,3 |
| Pixel 5 Silber, 1,8 GHz | Cortex-A55 | 20,8 | 67,3 | 128,6¹ |

¹ Eine Wiederholung statt drei; danach trennte sich das Pixel 5 vom USB.
Für diese Zelle ist der Wert das p99 der Ankünfte dieser einen Wiederholung.

Die eigentliche Entscheidung, die **Ankunft**, im Gate-Szenario:

| Kerne | p50 | p99 | p99,9 | Spannweite p99 |
|---|---:|---:|---:|---|
| Laptop | 1,4 | 3,3 | 20,7 | 3,0–3,9 |
| Pixel 5 Prime | 3,4 | 5,5 | 49,7 | 5,3–5,7 |
| Pixel 2 Gold | 7,3 | 9,9 | 50,4 | 9,9–10,0 |
| Pixel 2 Silber | 10,9 | 25,7 | 71,6 | 18,1–27,9 |
| Pixel 5 Silber | 11,5 | 18,6 | 70,0 | 18,4–18,9 |

Entscheidungen je Sekunde im Kern (Gate-Szenario): 718 000 (Laptop),
290 000 (Pixel 5 Prime), 150 000 (Pixel 2 Gold), 98 000 (Pixel 2 Silber).

## Was daraus folgt

**Die Entscheidung ist auf jeder gemessenen CPU vernachlässigbar gegen die
Periode.** Selbst der älteste Kern (Pixel 2 Silber, A53-Klasse) braucht im
Gate-Szenario im p99 26 µs für eine Ankunft — 0,08 % einer 33-ms-Periode. Mit
32 Modellen sind es 150 µs, 0,5 %. Für eine Jetson-Klasse-CPU ist das
Pixel 5 Prime (A76) die nächste Schranke: 5 µs mit vier Modellen, 25 µs mit
32.

**Die Kosten wachsen mit der Zahl der Modelle, ungefähr linear.** Laptop 3 →
11 → 27 µs, Pixel 2 Gold 10 → 31 → 60 µs. Das ist die Kandidatensuche über
alle Modelle je Entscheidung. Bei 32 Modellen noch unkritisch, aber es ist
die Stelle, an der ein deutlich größerer Aufbau zuerst Zeit kostet.

**Ein Takt kostet so viel wie eine Entscheidung, und er geht über alle
Verträge.** Mit 32 Modellen sind das im Median 48 µs auf dem Pixel 2 Gold und
66 µs auf dem Pixel 2 Silber. Der Simulator weckt jede Millisekunde; so
gezählt wären das 5 bis 7 % eines Kerns allein für den Takt. **Das ist eine
Obergrenze, keine Betriebszahl:** der Gateway weckt ereignisgesteuert — er
schläft bis zu dem Zeitpunkt, den der Scheduler anfordert (`Action::WakeAt`),
und ohne Anforderung bis zu einer Stunde. Wie oft er im Betrieb tatsächlich
weckt, misst dieser Bench nicht. Wo es darauf ankommt, ist es der erste Punkt,
den eine Messung am laufenden Gateway klären sollte: die Arbeit je Takt wächst
mit der Zahl der Modelle.

**Die Ausreißer kommen vom Betriebssystem, nicht vom Algorithmus.** Die
Maxima reichen bis 7 ms (Pixel 5 Prime, Gate-Szenario), während das p99,9 bei
34 µs liegt und die Spannweite des p99 über drei Wiederholungen eng ist. Das
Muster ist ein Prozess ohne Echtzeitpriorität auf einem Android-Kernel, den
gelegentlich etwas anderes verdrängt. Auf einem Edge-Gerät, das Regelzyklen
schützen soll, gehört der Governor auf einen isolierten Kern oder in eine
Echtzeitklasse; die Handys zeigen, was ohne das passiert.

**Die Kerne sind stabil.** Die Spannweite des p99 über drei Wiederholungen
liegt meist unter 10 %. Die Batterietemperatur des Pixel 2 stieg während der
Läufe von 33,0 auf 35,0 °C; `/sys/class/thermal` ist ohne Root nicht lesbar,
Taktdrosselung ist deshalb nicht ausgeschlossen, bei Laufzeiten von einigen
Sekunden je Lauf aber unwahrscheinlich.

## Was das nicht zeigt

- **Keine Inferenz auf den Handys.** Ihre GPUs sind für diesen Stack nicht
  erreichbar, und es gibt dort kein OIP-Backend. Gemessen ist der Kern, nicht
  ein Edge-Gerät mit Governor davor.
- **Kein Datenpfad, kein Netz, kein Gateway.** Der Weg über gRPC und Shared
  Memory kostet auf dem Laptop rund 160 µs ([data-plane.md](data-plane.md))
  und ist auf ARM nicht gemessen.
- **Keine Jetson-Qualifikation.** Die A76- und A73-Kerne sind eine Schranke
  für die CPU-Kosten, nicht für Speicherbandbreite, Interferenz oder das
  Verhalten der GPU. Die Zeile „Jetson Orin, Xavier" der Support-Matrix
  bleibt ungetestet.
- **Das Spitzen-RSS von rund 62 MB ist das der Messung, nicht des Kerns.** Der
  Simulator plant alle Ankünfte eines Laufs vorab ein und die Messung hält
  jeden Einzelwert; beides fehlt im Betrieb.
- **Der Laptop lief nicht auf ruhiger Maschine:** Systemlast 3,8 durch andere
  Prozesse, obwohl kein Build lief. Seine Zahlen sind ein Bezug, keine
  Qualifikation.

## Reproduzieren

```bash
tools/arm/build-and-run.sh                    # baut, schiebt, misst, holt ab
SKIP_BUILD=1 tools/arm/build-and-run.sh       # mit vorhandenen Binaries
target/release/decision-bench --scenario gate --seconds 300 --repeats 3
```

Kerngruppen und Geräte stehen im Kopf des Skripts (`DEVICES`).
