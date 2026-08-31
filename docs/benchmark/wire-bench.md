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

*(Die Tabellen werden vom Lauf erzeugt; siehe unten zum Reproduzieren.)*

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
