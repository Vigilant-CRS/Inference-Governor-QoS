# ADR-0041: Der Look-ahead schuetzt die Versorgung, nicht nur die Deadline

**Status:** Akzeptiert, opt-in · 2026-09-12
**Betrifft:** `core/feasibility` (`ExpectedArrival`, `guard_protected`),
`core/scheduler` (`build_forecast`, `set_protect_supply`), `config/schema`
(`backend.protect_supply`), `gateway/actor`; Spec 10.7, ADR-0005, NV-01;
ADR-0036, ADR-0038
**Ausloeser:** Die Rampe vom 12.09. mit gelernter Marge
([Messung](../benchmark/messkette-2026-09-11.md))

## Kontext

Die Kalibrierung nach ADR-0038 macht den Plan genauer. Auf der GPU kostet das
den geschuetzten Strom. Rampe vom 12.09., 125 % Last, je drei Laeufe ohne
Fremdlast (`measure-morgen-2026-09-12`), unabgedeckte Detektorperioden:

| Plan | Detektor |
|---|---:|
| korrektes Profil, feste Marge 110 % | 0 ‰ |
| korrektes Profil, gelernte Marge | 205 ‰ |
| Profil x0,7, gelernte Marge | 221 ‰ |
| Profil x2, gelernte Marge | 479 ‰ |

Verletzt wurde dabei keine einzige Deadline.

Der Grund steht schon in ADR-0038: je kleiner der Plan, desto weniger Vetos,
desto mehr Hintergrundarbeit laeuft, desto mehr Versorgungsluecken entstehen.
Die feste Marge hat den geschuetzten Strom also nicht durch Planung geschuetzt,
sondern durch Pessimismus — ein Schutz, den niemand eingestellt hat und der
verschwindet, sobald der Plan stimmt.

**Der Look-ahead prueft die falsche Groesse.** Er fragt je erwarteter Ankunft:
Haelt dieser Frame mit dem Kandidaten noch seine Deadline? Das Produkt
verspricht aber etwas anderes (ADR-0005, NV-01): dass der Verbraucher zu jedem
Zeitpunkt ein Ergebnis unter `max_age` vorfindet. Ein Vertrag mit
`deadline = 1,5 T` und `max_age = 2 T` laesst genau die Luecke dazwischen: das
Ergebnis des vorigen Frames laeuft `T` nach dieser Aufnahme ab, die Deadline
dieses Frames erst nach `1,5 T`.

Dieselbe Verwechslung hatte die Variantenwahl, dort gemessen als 143 ‰ bei
90 % und 66 bis 84 ‰ bei 110 bis 125 % Last, und dort seit `53b919a` behoben:
gewaehlt wird die beste Variante, die **vor dem Ablauf des vorigen Ergebnisses**
fertig wird. Die Messung vom 12.09. nimmt das ab: `frontier` verfehlt auf
jeder Laststufe 0 ‰, und bis 75 % bleibt die grosse Variante die Wahl — die
Regel kostet dort also keine Qualitaet. Der Guard kannte diese Frist noch
nicht.

## Entscheidung

**Eine erwartete Ankunft traegt zwei Fristen, und der Guard prueft beide
einzeln:**

```text
deadline   Aufnahme + D                    wie bisher (ADR-0036)
supply     letzte brauchbare Aufnahme + max_age
```

`supply` ist der Ablauf des letzten Ergebnisses, das der Scheduler ohnehin
mitfuehrt (`usable_until`, ADR-0005). Vetoiert wird, wenn der Kandidat **eine**
der beiden Fristen reisst, die ohne ihn gehalten haette. Beide zu einem
Minimum zu verschmelzen waere falsch: ist die Versorgung ohnehin verloren,
schuetzt der Guard weiter die Deadline.

**Voreinstellung aus.** `backend.protect_supply: true` schaltet ihn ein. Ohne
die Zeile ist das Verhalten bitgleich; ein Vertrag ohne `max_age`, ein Strom
ohne je ein brauchbares Ergebnis und ein bereits abgelaufenes Ergebnis
erzeugen keine Versorgungsfrist.

## Was die Simulation zeigt

Ein geschuetzter Strom (33 ms Takt, 22 ms Plan, `deadline = 1,5 T`,
`max_age = 2 T`) neben einem Hintergrundstrom alle 60 ms, ohne Marge, 20 s,
Seed 1 (`vig-sim/tests/supply_guard.rs`, Tabelle mit `--ignored`):

| Hintergrundauftrag | Verbraucher ohne/mit | Hintergrund versorgt ohne/mit | Vetos ohne/mit |
|---:|---:|---:|---:|
| 10 ms | 0 / 0 ‰ | 333 / 333 | 0 / 9 |
| 15 ms | 0 / 0 ‰ | 333 / 333 | 0 / 202 |
| 20 ms | 0 / 0 ‰ | 333 / 304 | 0 / 1 144 |
| **25 ms** | **13 / 0 ‰** | **333 / 0** | 2 / 7 980 |
| 30 ms | 0 / 0 ‰ | 331 / 0 | 142 / 7 980 |

Drei Dinge stehen darin, und alle drei gehoeren in die Entscheidung:

1. **Der Schutz wirkt genau in dem Fenster, in dem er gebraucht wird.** Nur
   bei 25 ms reisst die Versorgung, ohne dass eine Deadline faellt — und
   genau dort schliesst der Guard die Luecke (13 → 0 ‰). Unterhalb passt die
   Arbeit ohnehin, oberhalb vetoiert schon die Deadline.
2. **Der Preis ist hart.** Bei 25 ms bekommt der Hintergrund gar nichts mehr
   (333 → 0 versorgte Fenster, Vetos 2 → 7 980): jeder Auftrag, der laenger
   dauert als der Abstand zwischen zwei geschuetzten Ankuenften, passt in
   keine Luecke mehr. Das ist dieselbe Aushungerung, die ADR-0012 fuer den
   unteilbaren 90-ms-Block beschreibt — hier durch eine engere Frist erzeugt.
3. **Er zahlt auch, wo nichts zu retten war.** Bei 20 ms ist der Verbraucher
   ohne Schutz lueckenlos, und der Schutz kostet trotzdem 9 % der
   Hintergrundfenster. Der Guard rechnet mit der Prognose, nicht mit dem
   Ausgang.

**Muss der Preis gedeckelt werden?** Der Governor kennt dafuer bereits eine
Groesse: `minimum_background_progress_pct` (Spec 19.6, mit
`consumer_period_ms` und `observation_window`). Sie ist heute vom
Versorgungsschutz unabhaengig — ein eingeschalteter Schutz kann sie
verletzen, ohne es zu merken. Das ist bewusst **nicht** Teil dieser
Entscheidung: erst soll die GPU zeigen, wie gross der Verlust auf einer
echten Last ist. Ist er so gross wie in der Simulation, gehoert der Schutz
unter den Mindestfortschritt gestellt (der Guard laesst dann wieder Arbeit
zu, sobald der Hintergrund unter seine Zusage faellt) — und erst diese
Kombination taugt als Voreinstellung. Die kooperative Zerlegung (ADR-0014)
ist der andere Hebel: zerlegte Auftraege passen wieder in die Luecken.

## Konsequenzen

- **Der Verbraucher wird geschuetzt, nicht der Request.** Im Simulator
  (`vig-sim/tests/supply_guard.rs`) verschwinden die Luecken, die jede
  Deadline halten.
- **Es kostet Hintergrundfortschritt.** Jeder zusaetzliche Schutz ist
  zusaetzliches absichtliches Idle (Spec 10.7). Der Test misst das mit:
  weniger versorgte Hintergrundfenster, mehr `deferred_for_protected`. Wer
  den Hintergrund braucht, laesst den Schalter aus oder vergroessert
  `max_age` — die Versorgungsfrist ist eine Vertragsgroesse, keine Policy.
- **Die Schranke aus NV-23 bleibt unberuehrt.** Sie gilt fuer die
  Voreinstellung, und die ist bitgleich. Eingeschaltet kann der Guard nur
  mehr zurueckhalten, also das Alter eines geschuetzten Frames nur senken;
  gemessen ist das nicht, und die Gittersuche laeuft weiter gegen die
  Voreinstellung.
- **Konservativ in der teuren Richtung.** Die Frist kommt aus dem *letzten*
  gelieferten Ergebnis. Laeuft gerade ein Frame, der vor der erwarteten
  Ankunft fertig wird, waere die echte Frist spaeter — der Guard vetoiert
  dann mehr als noetig. Das ist die sichere Richtung, aber es ist ein
  bekannter Pessimismus.

## Abnahme

Die Messung, die den Schalter zur Voreinstellung machen koennte: die Rampe
bei 110 und 125 % Last mit gelernter Marge, einmal mit und einmal ohne
`protect_supply`, je drei Laeufe. Erwartet wird, dass der Detektor mit Schutz
wieder bei den 0–12 ‰ der festen Marge liegt, und in derselben Tabelle, was
die uebrigen Stroeme und die Hintergrundarbeit dafuer zahlen. Ohne diese Zahl
bleibt es bei opt-in.

## Auf der GPU gemessen, 12.09.2026

RTX 3070 Laptop, Rampe, je drei Laeufe, ohne Fremdlast
([Abnahme](../benchmark/abnahme-2026-09-12.md)). Detektor unabgedeckt /
alle Stroeme:

| Last | ohne Schutz | Schutz, feste Marge | Schutz + gelernte Marge | Schutz + Pipelining |
|---|---|---|---|---|
| 100 % | 0 / 188 ‰ | — | — | 0 / 4 ‰ |
| 110 % | 16 / 172 ‰ | 0 / 998 ‰ | 12 / 99 ‰ | 0 / 998 ‰ |
| 125 % | 21 / 510 ‰ | 2 / 445 ‰ | 45 / 508 ‰ | 3 / 705 ‰ |

Die Simulation hat den Preis richtig vorhergesagt: allein nimmt der Guard
dem Hintergrund bei 110 % praktisch alles. Erst zusammen mit der
Kalibrierung (ADR-0038) ist er in beiden Spalten besser als der Zustand
ohne beides. Damit ist die offene Frage dieses ADR nicht mehr theoretisch:
**ohne Deckelung durch `minimum_background_progress_pct` ist der Schalter
nicht empfehlenswert**, ausser zusammen mit der Kalibrierung oder in einem
Aufbau, in dem der Hintergrund ohnehin keine Chance hat (Gate M3: dort
aendern sich die Zahlen nicht).
