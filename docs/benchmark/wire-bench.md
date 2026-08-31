# Wire-Benchmark: bringt der Governor etwas?

Stand: 2026-08-31 · Release-Build · **kein Vergleich gegen Triton**

> Das Backend ist ein Modell mit begrenzter Ausfuehrungskapazitaet, kein
> Inferenzserver. Gemessen wird OneTimer gegen den **direkten** Weg zu
> demselben Backend. Gate M3 gegen einen getunten Triton auf echter Hardware
> steht weiterhin aus.

## Aufbau

Derselbe Workload laeuft zweimal durch denselben Stack. Gleich gehalten werden:

* **Die Frames.** Ankunftszeitpunkte, Perioden, Frame-Nummern.
* **Die Laufzeiten.** Die Dauer eines Frames haengt an `(seed, stream, frame)`
  und nicht am Fortschritt eines Zufallsstroms. Derselbe Frame kostet in beiden
  Laeufen dasselbe, gleichgueltig in welcher Reihenfolge er ausgefuehrt wird.
* **Die Kapazitaet.** Dieselbe Slotzahl, dasselbe Semaphor.
* **Der Client.** Derselbe Treiber, dieselbe Puffertiefe.
* **Der Transport.** Dieselben HTTP/2-Fenster und Nachrichtengrenzen.

Der Client sendet **unabhaengig vom Systemzustand** weiter — ein Sensor wird
nicht langsamer, weil das System ueberlastet ist. Ein Treiber, der auf die
Antwort wartet, bevor er den naechsten Frame schickt, wuerde das Problem
wegdefinieren, um das es geht.

Beide Seiten werden mit den Puffertiefen 1, 4 und 16 gefahren; je Strom zaehlt
das jeweils beste Ergebnis. Nur eine Seite ihre beste Tiefe waehlen zu lassen
waere ein verstecktes Handicap: bei geringer Last ist eine Tiefe von 1 eine
Selbstdrosselung, die von sich aus optimal ist (Spec 19.1).

## Erfolgsmetrik

Abdeckung nach ADR-0005: Anteil der Perioden, in denen ein Ergebnis geliefert
wurde, dessen Alter unter `max_age` lag. Eine hohe Abdeckung bei wenigen
Inferenzen ist besser als eine niedrige bei vielen — und eine Politik, die
alles verwirft, faellt sofort auf.

## Ergebnisse

Messdauer 15 s je Lauf und Puffertiefe, Kerne 8-15 reserviert.

### A — geschuetzte Vertraege ueberzeichnet

133 % geschuetzte Auslastung auf einem Slot. `doctor` meldet dafuer
`PROTECTED_WORKLOAD_UNSCHEDULABLE`; gemessen wird trotzdem, um zu zeigen, was
in dieser Lage passiert.

| Strom | Abdeckung ohne | mit | AoI p95 ohne | mit | Faktor |
|---|---:|---:|---:|---:|---:|
| detector | 45 % | **99 %** | 216 ms | **15 ms** | 108x |
| pose | 46 % | **99 %** | 209 ms | **25 ms** | 107x |
| depth | 52 % | **99 %** | 225 ms | **32 ms** | 95x |
| vlm | 100 % | 3 % | 301 ms | 202 ms | — |

Backend: 854 Inferenzen ohne, **1112 mit** Governor. Der Governor fuehrt also
mehr aus und liefert dabei frischere Ergebnisse — kein Tausch von Durchsatz
gegen Latenz, sondern weniger Arbeit an bereits wertlosen Daten.

Der VLM verhungert. Bei dieser Konfiguration ist das die richtige Antwort und
`doctor` sagt es vorher; siehe ADR-0012.

### B — tragfaehige Vertraege, ein Slot

61 % geschuetzte Auslastung.

| Strom | Abdeckung ohne | mit | AoI p95 ohne | mit |
|---|---:|---:|---:|---:|
| detector | 87 % | **100 %** | 36 ms | **23 ms** |
| pose | 91 % | **100 %** | 175 ms | **25 ms** |
| depth | 97 % | **100 %** | 189 ms | **40 ms** |
| vlm | 100 % | 14 % | 309 ms | 3562 ms |

Der wichtigste Befund des ganzen Benchmarks steht in dieser Zeile: **auch bei
61 % Auslastung startet der VLM fast nie.** Die Blockade ist eine Eigenschaft
der Laufzeitverhaeltnisse, nicht der Last — ein 200-ms-Job gefaehrdet auf einem
nicht unterbrechbaren Slot immer die naechste 50-ms-Ankunft. Daraus entstand
ADR-0012 und die zugehoerige `doctor`-Warnung.

### C — dieselbe Last, zwei Slots, kein Co-Run-Verbot

30 % geschuetzte Auslastung, reichlich Reserve.

| Strom | Abdeckung ohne | mit | AoI p95 ohne | mit |
|---|---:|---:|---:|---:|
| detector | 100 % | 98 % | 15 ms | 14 ms |
| pose | 100 % | 100 % | 22 ms | 50 ms |
| depth | 100 % | 100 % | 30 ms | 50 ms |
| vlm | 100 % | 100 % | 321 ms | 348 ms |

**Ohne Konkurrenz bringt der Governor nichts** — und das ist die ehrliche
Aussage, nicht eine Verlegenheit. Alle 533 Requests wurden weitergereicht, kein
einziger verworfen. Die hoehere AoI von `pose` und `depth` ist kein Fehler,
sondern die konfigurierte Absicht: sie sind `high`, der Detektor ist
`protected` und geht vor.

Die 98 % beim Detektor sind kein verlorener Request — es wurde keiner
verworfen. Ein Ergebnis, das knapp in das naechste 50-ms-Fenster rutscht,
laesst das vorige leer. Die Abdeckungsmetrik ist an dieser Stelle
randempfindlich; bei viel Reserve und kurzen Perioden ist sie das falsche
Werkzeug, weil sie nichts mehr zu unterscheiden hat.

## Was der Benchmark ueber den Code gesagt hat

Drei Fehler wurden hier gefunden, nicht durch Tests:

1. `SafetyMargin::NONE` war als `1/1` dargestellt. `as_percent()` lieferte 1
   statt 100 — einen Wert, den `from_percent` ablehnt. Der Margenregler wich
   still auf den Default aus und plante mit 110 % statt 100 %.
2. Der Slot-Belegungsgrad wurde **nach** dem eigenen Dispatch erfasst, geplant
   wird aber mit dem Wert davor. Jede Estimator-Zelle war um eins verschoben;
   der Schaetzer waere vorhanden, aber wirkungslos gewesen.
3. `dispatched_late` zaehlte Planungsversuche statt Dispatches — 2057 bei 924
   weitergereichten Requests.

Keiner der drei erzeugte eine Fehlermeldung. Alle drei waren nur an Verhalten
unter Dauerlast zu erkennen.

## Reproduzieren

```bash
cargo build --release -p onetimer-bench
taskset -c 8-15 target/release/wire-bench          # alle Szenarien
taskset -c 8-15 target/release/wire-bench C        # nur ein Szenario
```

`taskset` ist kein Detail: ein Latenzbenchmark neben einem laufenden Build
misst den Build. Ein erster Lauf dieses Benchmarks entstand versehentlich
parallel zu einer Kompilierung und zeigte Zahlen, die sich auf isolierten
Kernen nicht bestaetigten.
