# NV-23: Eine begrenzte Aussage über die Versorgung eines geschützten Stroms

Stand: 11.09.2026 · Paket NV-23 ([Roadmap](../roadmap/2026-09-09-vnext/05-arbeitspakete.md))
· Code: `crates/vig-sim/src/bounded.rs` · Tests: `crates/vig-sim/tests/nv23_bounded.rs`

Kein universeller Echtzeitbeweis. Eine Teilfrage mit festen Annahmen, eine
Schranke, ihre Herleitung — und eine Suche, die den **echten** Scheduler gegen
diese Schranke fährt. Dazu für jede Annahme ein Gegenbeispiel, sobald sie
fällt: sonst wäre nicht zu unterscheiden, ob eine Annahme gebraucht wird oder
nur gut aussieht.

## Die Frage

Ein geschützter Kamerastrom teilt sich eine nicht unterbrechbare
Ausführungseinheit mit Hintergrundarbeit. Der Governor hält Hintergrundarbeit
zurück, wenn eine geschützte Ankunft erwartet wird (Look-ahead, Spec 10.7).
**Wie alt ist ein geschütztes Ergebnis höchstens, wenn es ausgeliefert wird —
und wie lange ist der Verbraucher höchstens ohne brauchbares Ergebnis?**

## Das Modell

| Symbol | Bedeutung |
|---|---|
| `T` | Periode des geschützten Stroms, zugleich seine Vertragsperiode |
| `c_n` | Aufnahme des Frames `n`: `c_n = nT + j_n` mit `|j_n| ≤ J` |
| `δ` | Transportzeit, konstant: Ankunft `a_n = c_n + δ` |
| `D`, `A` | Deadline und Höchstalter, beide ab Aufnahme |
| `Ĉ` | geplante Laufzeit des geschützten Stroms: `p99 × Marge` |
| `e_n` | die Prognose des Schedulers für Frame `n`: `e_n = a_{n−1} + T` |
| `H` | Look-ahead-Horizont, Voreinstellung 100 ms |
| `f_n` | Fertigstellung von Frame `n` |

Drei Regeln des Schedulers tragen die Herleitung. Sie stehen hier, weil die
Schranke an ihnen hängt und nicht an einer Idealisierung:

- **Vorrang.** Wartet ein geschützter Frame, startet keine Hintergrundarbeit
  (`best_candidate`, Schlüssel beginnt mit der Kritikalität).
- **Prognose.** Bei jeder Ankunft setzt der Scheduler die nächste auf
  `Ankunft + T` (`on_arrival`) — ab der **Ankunft**, nicht ab der Aufnahme.
- **Veto.** Hintergrundarbeit mit geplanter Laufzeit `b̂` startet zur Zeit `s`
  nur, wenn die erwartete Ankunft mit ihr noch machbar bleibt:
  `max(e, s + b̂) + Ĉ ≤ e + D`. Aber: der Guard vetoiert nur, wenn die Ankunft
  **ohne** den Kandidaten machbar wäre (`guard_protected`, „nur wenn der
  Kandidat die Ursache ist"), und er betrachtet nur Ankünfte innerhalb von
  `H`. Beides ist unten eine Annahme — und beides hat ein Gegenbeispiel.

## Die Annahmen

| | Annahme | Warum |
|---|---|---|
| A1 | Ein Slot, keine zusätzlichen Kredite (`slots: 1`, `pipelining_depth: 0`) | nicht unterbrechbar, eine Einheit |
| A2 | Genau ein bewachter Strom (`protected`, `LATEST`, Kapazität 1); alles andere unterhalb von `high`, nicht zerlegbar; Vertragsperiode = nominale Periode | ein Strom, eine Prognose |
| A3 | Keine tatsächliche Laufzeit über ihrem Profil-p99 | dann ist der Plan ≥ die Wirklichkeit, und der Margenregler steht auf seinem Boden |
| A4 | `Ĉ + 2J ≤ D` | sonst gibt der Look-ahead einen verspäteten Frame auf |
| A5 | `Ĉ + 2J ≤ T` | ein Frame ist fertig, bevor die Prognose seines Nachfolgers greift |
| A6 | `T ≤ H`, oder jede geplante Hintergrundlaufzeit `≤ H` | die nächste Ankunft liegt im Horizont, wann immer es auf sie ankommt |
| A7 | `D + 2J < T + Ĉ` | kein Frame wartet noch, wenn der nächste eintrifft |
| A8 | Keine Hintergrundarbeit trifft vor der ersten geschützten Ankunft ein | vorher gibt es keine Prognose |
| A9 | `Δ ≤ A` mit `Δ = δ + 2J + D` | kein Frame wird als wertlos verworfen |

Außerhalb des Modells, und damit auch außerhalb der Aussage: Backendfehler,
Abbrüche, Anwendungshinweise, die scharfe Prognose (NV-06), eine gemessene
Interferenztabelle, kooperative Zerlegung, mehrere bewachte Ströme, mehrere
Slots.

## Der Satz

> **Unter A1–A9 wird jeder geschützte Frame ausgeliefert, und für jeden gilt**
>
> `f_n − c_n ≤ Δ = δ + 2J + D`.
>
> **Ist zusätzlich `Δ < A`, ist die längste Versorgungslücke nach der ersten
> Auslieferung höchstens**
>
> `max(0, T + 2J + Δ − A) = max(0, T + D + 4J + δ − A)`.
>
> Insbesondere gibt es keine Lücke, wenn `A ≥ T + D + 4J + δ`. Die erste
> Schranke wird erreicht.

**Am Rand.** Die Suche hat einen Fall gefunden, den die erste Fassung dieses
Satzes übersah: `Δ = A`. Dann kann ein Frame mit Alter genau `A`
ausgeliefert werden — gültig, nicht veraltet, denn veraltet ist erst, was
`A` **überschreitet** —, aber er ist keinen einzigen Augenblick brauchbar.
Die Produktregel zählt ihn deshalb als Lücke, und sie hat recht: ein
Ergebnis, das in dem Moment abläuft, in dem es ankommt, versorgt nichts. Auf
einem 20-ms-Strom, dessen Laufzeit genau auf dem Plan liegt, war das Ergebnis
eine Lücke über das ganze Messfenster. Die Altersschranke gilt mit `Δ ≤ A`;
über die Lücke sagt der Satz nur etwas bei `Δ < A`.

## Der Beweis

**Behauptung (Induktion über n).** Frame `n` startet spätestens bei
`max(a_n, e_n + D − Ĉ)`.

*Anfang, n = 0.* Nach A8 läuft beim Eintreffen von Frame 0 keine
Hintergrundarbeit und wartet nichts vor ihm: er startet bei `a_0`.

*Schritt.* Frame `n` wartet höchstens auf zweierlei: auf seinen Vorgänger und
auf Hintergrundarbeit, die vor seinem Eintreffen gestartet wurde (Vorrang:
danach startet keine mehr).

1. *Der Vorgänger.* Nach Induktionsannahme und A3 ist
   `f_{n−1} ≤ max(a_{n−1} + Ĉ, e_{n−1} + D)`. Der erste Term ist
   `e_n − T + Ĉ ≤ e_n + D − Ĉ`, weil `2Ĉ ≤ T + D` (aus A4, A5). Für den
   zweiten gilt `e_{n−1} = a_{n−2} + T ≤ a_{n−1} + 2J = e_n − T + 2J`, also
   `e_{n−1} + D ≤ e_n + D − Ĉ` genau dann, wenn `Ĉ + 2J ≤ T` — A5.
2. *Hintergrundarbeit, gestartet bei `s` mit `a_{n−1} ≤ s < a_n`.* Zu diesem
   Zeitpunkt ist die Prognose `e_n` gesetzt und, falls sie für den Kandidaten
   überhaupt zählt (`e_n < s + b̂`), nach A6 im Horizont. Ohne den Kandidaten
   wäre Frame `n` machbar: `max(e_n, s) + Ĉ ≤ e_n + 2J + Ĉ ≤ e_n + D` nach A4,
   denn `s < a_n ≤ e_n + 2J`. Also greift das Veto, und der Start ist nur
   erlaubt, wenn `s + b̂ + Ĉ ≤ e_n + D`. Mit A3 endet die Arbeit spätestens bei
   `e_n + D − Ĉ`. Zählt `e_n` nicht (`e_n ≥ s + b̂`), endet sie vor `e_n`.
   Hintergrundarbeit, die **vor** `a_{n−1}` startete, hielt Frame `n−1` auf
   und ist in dessen Fertigstellung enthalten.

Also startet Frame `n` bei `max(a_n, e_n + D − Ĉ)` oder früher. ∎

**Die Altersschranke.** `f_n ≤ max(a_n + Ĉ, e_n + D)`, und
`e_n − c_n = δ + j_{n−1} − j_n ≤ δ + 2J`. Mit `Ĉ ≤ D` folgt
`f_n − c_n ≤ δ + 2J + D = Δ`.

**Keine Verluste.** Verdrängt wird Frame `n` nur, wenn er beim Eintreffen von
`n+1` noch wartet. Er wartet höchstens bis `e_n + D − Ĉ`; sein Nachfolger
trifft frühestens bei `c_{n−1} + 2T − 2J + δ = e_n + T − 2J` ein. A7 schließt
das aus. Als wertlos verworfen würde er nur, wenn schon die optimistische
Fertigstellung über `A` läge — sie liegt unter `f_n`, und `f_n − c_n ≤ Δ ≤ A`
(A9).

**Die Lückenschranke.** Frame `n` ist brauchbar von `f_n` bis `c_n + A` —
ein nicht leeres Intervall, weil `f_n − c_n ≤ Δ < A`. Die Lücke bis zum
nächsten ist `f_{n+1} − (c_n + A) ≤ c_{n+1} + Δ − c_n − A ≤ T + 2J + Δ − A`,
oder null. Bei `Δ = A` kann das Intervall leer sein, und dann gilt diese
Rechnung nicht (siehe „Am Rand"). ∎

## Die Schranke ist scharf

`T = D = 33 ms`, `J = 2 ms`, `δ = 1 ms`, `Ĉ = 10 ms`, abwechselnder Jitter:
Frame 4 kommt `+J`, Frame 5 `−J` — also `2J` vor seiner Prognose. Genau als
Frame 4 fertig wird (`s = a_4 + Ĉ`), liegt ein Hintergrundauftrag bereit,
geplant mit `b̂ = T + D − 2Ĉ = 46 ms`. Damit ist `s + b̂ + Ĉ = e_5 + D`: der
Guard lässt ihn laufen, gerade noch. Frame 5 wartet bis `e_5 + D − Ĉ`, läuft
`Ĉ`, und ist bei `e_5 + D` fertig — sein Alter ist `δ + 2J + D = 38 ms`,
**auf die Mikrosekunde die Schranke**
(`the_bound_is_reached_exactly`).

## Jede Annahme wird gebraucht

Für jede Annahme, die fallen kann, ohne dass das Modell sinnlos wird, gibt es
einen Test, der das Gegenbeispiel fährt:

| Annahme fällt | Was dann passiert | Test |
|---|---|---|
| A3 | Hintergrundarbeit überzieht ihren Plan um 5 ms; der geschützte Frame wartet 5 ms länger, als der Look-ahead zugelassen hat | `a_background_job_that_overruns_its_plan_breaks_the_bound` |
| A4 | Ein Frame kommt `2J` nach seiner Prognose. Gerechnet ab der Prognose ist er schon „nicht zu retten" — der Guard vetoiert nicht mehr, und ausgerechnet dann startet ein 80-ms-Auftrag. Alter 89 ms statt ≤ 16 ms | `a_late_frame_the_guard_gave_up_on_waits_for_the_whole_background_job` |
| A6 | Periode 120 ms, Horizont 100 ms: die nächste Ankunft liegt außerhalb des Horizonts, ein 115-ms-Auftrag reicht über sie hinweg | `an_arrival_beyond_the_horizon_is_not_protected` |
| A7 | Frame 4 wartet bis genau zur Ankunft von Frame 5 und wird verdrängt | `when_a_frame_can_still_wait_at_the_next_arrival_it_is_superseded` |
| A8 | Vor der ersten Ankunft gibt es keine Prognose; ein 33-ms-Auftrag, der beim Start schon wartet, läuft durch | `background_before_the_first_frame_is_the_start_up_exception` |

A1, A2 und A9 sind Rahmenannahmen: ohne sie ist die Frage eine andere.

## Abgleich mit der Simulation

`search` fährt jedes Gitter durch den echten `vig_core::Scheduler` — dieselbe
Zulassung, derselbe Look-ahead, derselbe Margenregler wie im Gateway — mit
gesetzten Laufzeiten und Weckrufen genau dann, wenn der Scheduler sie
verlangt. Die Versorgungslücke wird mit derselben Regel gemessen wie im
Benchmark (`CoverageTracker`). Geprüft wird bei jedem Lauf innerhalb der
Annahmen: kein Frame verloren, jedes Alter `≤ Δ`, jede Lücke `≤` Schranke,
und der Scheduler rechnet mit genau dem `Ĉ` der Analyse.

Das Gitter liegt absichtlich an den Rändern: Deadlines bei `Ĉ + 2J` und bei
`T + Ĉ − 2J − 1 µs`, Höchstalter genau bei `Δ`, Laufzeiten genau auf dem
Plan, Hintergrundarbeit, die 1 µs nach, eine halbe Periode nach und 1 µs vor
einer Freigabe eintrifft, und die Jittermuster „abwechselnd" (jeder zweite
Frame `2J` zu früh) und „+J, 0, −J" (der Nachfolger so früh wie möglich).

_Ergebnis: siehe unten, „Stand der Suche"._

## Was daraus folgt

**Eine Konfigurationsregel.** Ein geschützter Strom ist mit Hintergrundlast
lückenlos versorgt, wenn `A ≥ T + D + 4J + δ`. Die Gate-M3-Konfiguration
(`T = D = 33`, `A = 66`) liegt genau auf `A = T + D`; die Schranke sagt dort
eine Lücke von höchstens `4J + δ` — Jitter und Transport, nichts sonst. Sie
gilt für die Konfiguration aber nicht vollständig: dort sind Pose und Tiefe
`high`, also ebenfalls bewacht (A2).

**Drei Befunde über den Look-ahead**, jeder aus der Herleitung und jeder mit
Test:

1. **Er gibt verspätete Frames auf** (A4). Die Machbarkeit einer erwarteten
   Ankunft rechnet er ab der Prognose `e`, mit Deadline `e + D`. Kommt der
   Frame später, hält er ihn für unrettbar und vetoiert nicht mehr — und
   lässt genau in diesem Moment Arbeit beliebiger Länge starten. Der Vertrag
   kennt eine Jitterhülle (`release_jitter_envelope`), die heute beim Start
   als „nicht durchgesetzt" gemeldet wird. Sie wäre die fehlende Größe: mit
   ihr könnte der Guard eine überfällige Ankunft bis `e + J_Hülle` als
   ausstehend behandeln.
2. **Seine Deadline beginnt bei der Ankunft, die des Vertrags bei der
   Aufnahme.** Deshalb steht `δ` in der Schranke: der Guard lässt Arbeit zu,
   bis der geschützte Frame `D` nach seiner **Ankunft** fertig wäre, und das
   ist `δ` nach seiner Deadline. Mit `J` zusammen kann eine Deadline so um
   `δ + 2J` gerissen werden, ohne dass eine einzige Annahme verletzt ist.
3. **Er sieht nur `H` voraus** (A6). Ein geschützter Strom mit einer Periode
   über 100 ms — ein 5-Hz-Sensor — ist gegen Hintergrundarbeit über 100 ms
   nicht geschützt. Die nächste Ankunft eines bewachten Stroms gehört immer
   in die Prognose, gleich wie weit sie weg ist.

**Was die Aussage nicht sagt.** Nichts über mehrere bewachte Ströme (dort
reserviert der Guard kumulativ, und die Schranke hätte einen Summenterm),
nichts über mehrere Slots, nichts über Laufzeiten über dem Plan (dafür gibt
es den Margenregler mit einem Ziel von einem Prozent, ADR-0034, und damit
eine Wahrscheinlichkeit, keine Schranke), nichts über zerlegbare Aufträge.
Das ist der Umfang, den NV-23 verlangt: eine präzise Teilfrage, keine
Zertifizierung.

## Stand der Suche

| Gitter | Wann | Parametersätze | innerhalb der Annahmen | Gegenbeispiel | nächste Annäherung an Δ |
|---|---|---:|---:|---|---:|
| klein | jeder Testlauf, Debug, ~16 s | 3 240 | 3 024 | keines | 93,7 % |
| groß | `--ignored`, Release, ~2,5 min | 176 472 | 130 032 | keines | **100 %** — erreicht |

Die 216 Sätze des kleinen Gitters außerhalb der Annahmen verletzen alle A7:
das Gitter legt die Deadline absichtlich auch auf `T + Ĉ − 2J − 1 µs` und
darüber. Im großen Gitter liegen 15 768 Sätze außerhalb von A5, 17 712
außerhalb von A7 und 12 960 außerhalb von A6 — Perioden bis 120 ms und
Hintergrundarbeit bis 140 ms liegen dort bewusst jenseits des Horizonts. Die
große Suche erreicht die Schranke auch ohne die gezielte Konstruktion. In jedem Lauf innerhalb der Annahmen rechnete der Scheduler mit
genau dem `Ĉ` der Analyse — geprüft, nicht angenommen.

Die erste Fassung des Satzes behauptete die Lückenschranke auch bei `Δ = A`.
Die Suche hat dagegen ein Gegenbeispiel gefunden (siehe „Am Rand"); der Satz
ist entsprechend enger gefasst, und das Gitter enthält seitdem `A = Δ`
(nur die Altersschranke wird geprüft) und `A = Δ + 1 µs` (die engste
Lücke).
