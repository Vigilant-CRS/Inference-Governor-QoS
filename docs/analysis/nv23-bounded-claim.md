# NV-23: Eine begrenzte Aussage über die Versorgung eines geschützten Stroms

Stand: 11.09.2026, nach [ADR-0036](../adr/0036-the-look-ahead-counts-from-the-capture.md)
· Paket NV-23 ([Roadmap](../roadmap/2026-09-09-vnext/05-arbeitspakete.md))
· Code: `crates/vig-sim/src/bounded.rs` · Tests: `crates/vig-sim/tests/nv23_bounded.rs`

Die erste Fassung dieser Analyse fand drei Schwächen des Look-ahead. Sie sind
behoben; die Aussage unten ist die für den korrigierten Scheduler. Was sich
geändert hat, steht am Ende unter „Die drei Befunde".

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
| `e_n` | die Prognose des Schedulers für die Ankunft von Frame `n`: `e_n = a_{n−1} + T` |
| `ĉ_n` | die Prognose für seine Aufnahme: `ĉ_n = c_{n−1} + T = e_n − δ` |
| `J_E` | die Jitterhülle des Vertrags (`release_jitter_ms`), falls eine erklärt ist |
| `W` | wie weit die Frist einer überfälligen Ankunft mitwandert: `max(δ, 2J_E)` |
| `f_n` | Fertigstellung von Frame `n` |

Drei Regeln des Schedulers tragen die Herleitung. Sie stehen hier, weil die
Schranke an ihnen hängt und nicht an einer Idealisierung:

- **Vorrang.** Wartet ein geschützter Frame, startet keine Hintergrundarbeit
  (`best_candidate`, Schlüssel beginnt mit der Kritikalität).
- **Prognose.** Bei jeder Ankunft setzt der Scheduler die nächste Ankunft auf
  `Ankunft + T` und die nächste Aufnahme auf `Aufnahme + T` (`on_arrival`).
  Die Prognose enthält die nächste Ankunft jedes bewachten Stroms, gleich wie
  weit sie weg ist.
- **Veto.** Hintergrundarbeit mit geplanter Laufzeit `b̂` startet zur Zeit `s`
  nur, wenn die erwartete Ankunft mit ihr noch machbar bleibt:
  `max(e, s + b̂) + Ĉ ≤ d`. Die Frist `d` ist die des Vertrags, ab der
  erwarteten Aufnahme: `d = ĉ + D`. Ist die Ankunft überfällig (`s > e`),
  wandert sie mit, höchstens um `W`: `d = ĉ + min(s − e, W) + D`
  (`forecast_deadline`; außerhalb der Annahmen zusätzlich nie früher, als der
  Frame allein fertig wäre, ADR-0036). Der Guard vetoiert nur, wenn die
  Ankunft **ohne** den Kandidaten machbar wäre (`guard_protected`, „nur wenn
  der Kandidat die Ursache ist"). Das ist unten die Annahme A4 — und sie hat ein
  Gegenbeispiel.

## Die Annahmen

| | Annahme | Warum |
|---|---|---|
| A1 | Ein Slot, keine zusätzlichen Kredite (`slots: 1`, `pipelining_depth: 0`) | nicht unterbrechbar, eine Einheit |
| A2 | Genau ein bewachter Strom (`protected`, `LATEST`, Kapazität 1); alles andere unterhalb von `high`, nicht zerlegbar; Vertragsperiode = nominale Periode | ein Strom, eine Prognose |
| A3 | Keine tatsächliche Laufzeit über ihrem Profil-p99 | dann ist der Plan ≥ die Wirklichkeit, und der Margenregler steht auf seinem Boden |
| A4 | `Ĉ + δ + max(0, 2J − W) ≤ D`; mit Hülle `J_E ≥ J` also `Ĉ + δ ≤ D`, ohne Hülle `max(Ĉ + 2J, Ĉ + δ) ≤ D` | sonst gibt der Look-ahead einen verspäteten Frame auf |
| A5 | `Ĉ + 2J ≤ T` | ein Frame ist fertig, bevor die Prognose seines Nachfolgers greift |
| A6 | — | entfällt seit ADR-0036: die nächste Ankunft zählt immer |
| A7 | `D + 2J < T + Ĉ + δ` | kein Frame wartet noch, wenn der nächste eintrifft |
| A8 | Keine Hintergrundarbeit trifft vor der ersten geschützten Ankunft ein | vorher gibt es keine Prognose |
| A9 | `Δ ≤ A` mit `Δ = 2J + D` | kein Frame wird als wertlos verworfen |

Außerhalb des Modells, und damit auch außerhalb der Aussage: Backendfehler,
Abbrüche, Anwendungshinweise, die scharfe Prognose (NV-06), eine gemessene
Interferenztabelle, kooperative Zerlegung, mehrere bewachte Ströme, mehrere
Slots.

## Der Satz

> **Unter A1–A9 wird jeder geschützte Frame ausgeliefert, und für jeden gilt**
>
> `f_n − c_n ≤ Δ = 2J + D`.
>
> **Ist zusätzlich `Δ < A`, ist die längste Versorgungslücke nach der ersten
> Auslieferung höchstens**
>
> `max(0, T + 2J + Δ − A) = max(0, T + D + 4J − A)`.
>
> Insbesondere gibt es keine Lücke, wenn `A ≥ T + D + 4J`. Die erste
> Schranke wird erreicht.

Vor ADR-0036 lautete die Schranke `δ + 2J + D`: der Look-ahead rechnete die
Frist ab der erwarteten Ankunft. Der Jitter bleibt in der Schranke — dass ein
Frame `2J` früher aufgenommen wird, als die Prognose sagt, sieht niemand
voraus. Die Transportzeit sieht der Scheduler an jedem Request.

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
`max(a_n, B_n)` mit `B_n = max(ĉ_n, c_n) + D − Ĉ`.

*Anfang, n = 0.* Nach A8 läuft beim Eintreffen von Frame 0 keine
Hintergrundarbeit und wartet nichts vor ihm: er startet bei `a_0`.

*Schritt.* Frame `n` wartet höchstens auf zweierlei: auf seinen Vorgänger und
auf Hintergrundarbeit, die vor seinem Eintreffen gestartet wurde (Vorrang:
danach startet keine mehr).

1. *Der Vorgänger.* Nach Induktionsannahme und A3 ist
   `f_{n−1} ≤ max(a_{n−1} + Ĉ, B_{n−1} + Ĉ)`. Der erste Term ist
   `c_{n−1} + δ + Ĉ = ĉ_n − T + δ + Ĉ ≤ ĉ_n + D − Ĉ`, weil `2Ĉ + δ ≤ T + D`
   (aus A4 und A5). Für den zweiten gilt `c_{n−1} + D ≤ ĉ_n + D − Ĉ`, weil
   `Ĉ ≤ T`, und `ĉ_{n−1} + D = c_{n−2} + T + D ≤ ĉ_n + D − Ĉ` genau dann,
   wenn `c_{n−1} − c_{n−2} ≥ Ĉ` — aus `c_{n−1} − c_{n−2} ≥ T − 2J` und A5.
2. *Hintergrundarbeit, gestartet bei `s` mit `a_{n−1} ≤ s < a_n`.* Zu diesem
   Zeitpunkt sind `e_n` und `ĉ_n` gesetzt; die Prognose enthält Frame `n`
   immer. Zwei Fälle:
   - *Pünktlich, `s ≤ e_n`.* Die Frist ist `ĉ_n + D = e_n − δ + D`. Ohne den
     Kandidaten wäre Frame `n` machbar, `e_n + Ĉ ≤ e_n − δ + D` nach A4. Also
     greift das Veto, und der Start ist nur erlaubt, wenn
     `s + b̂ + Ĉ ≤ ĉ_n + D`: die Arbeit endet spätestens bei `ĉ_n + D − Ĉ`.
   - *Überfällig, `e_n < s < a_n`.* Dann ist `s − e_n < a_n − e_n ≤ 2J`, und
     die Frist ist `ĉ_n + min(s − e_n, W) + D`. Mit `W ≥ 2J` (Hülle `J_E ≥ J`)
     ist sie `s − δ + D`, und ohne den Kandidaten ist der Frame machbar,
     sobald `Ĉ + δ ≤ D`. Mit kleinerem `W` fehlen dem Guard höchstens
     `2J − W`, und A4 deckt genau das. Erlaubt ist der Start nur, wenn die
     Arbeit bis zur Frist minus `Ĉ` endet — und die Frist liegt nie nach
     `c_n + D`: bei `s − e_n ≤ W` ist sie `s − δ + D < a_n − δ + D`, sonst
     `e_n + W − δ + D ≤ e_n + D < c_n + D`, weil der Frame nach
     `e_n + W ≥ e_n + δ` eintrifft.

   Zählt die Ankunft für den Kandidaten nicht (sie liegt nach seinem Ende),
   endet er vor ihr. Hintergrundarbeit, die **vor** `a_{n−1}` startete, hielt
   Frame `n−1` auf und ist in dessen Fertigstellung enthalten.

Also startet Frame `n` bei `max(a_n, B_n)` oder früher. ∎

**Die Altersschranke.** `f_n ≤ max(a_n + Ĉ, B_n + Ĉ)`. Der erste Term ist
`c_n + δ + Ĉ ≤ c_n + D` (A4), der zweite `max(ĉ_n, c_n) + D`, und
`ĉ_n − c_n = j_{n−1} − j_n ≤ 2J`. Also `f_n − c_n ≤ 2J + D = Δ`.

**Keine Verluste.** Verdrängt wird Frame `n` nur, wenn er beim Eintreffen von
`n+1` noch wartet. Er wartet höchstens bis `B_n ≤ nT + J + D − Ĉ` (beide
Aufnahmen `c_n` und `ĉ_n = c_{n−1} + T` liegen höchstens `J` nach dem
Rasterpunkt `nT`); sein Nachfolger trifft frühestens bei `(n+1)T − J + δ`
ein. A7 schließt das aus. Als wertlos verworfen würde er nur, wenn schon die optimistische
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
geplant mit `b̂ = T + D − 2Ĉ − δ = 45 ms`. Damit ist `s + b̂ + Ĉ = ĉ_5 + D`:
der Guard lässt ihn laufen, gerade noch. Frame 5 wartet bis `ĉ_5 + D − Ĉ`,
läuft `Ĉ`, und ist bei `ĉ_5 + D` fertig — sein Alter ist `2J + D = 37 ms`,
**auf die Mikrosekunde die Schranke**
(`the_bound_is_reached_exactly`). Vor ADR-0036 durfte derselbe Auftrag
46 ms lang sein, und Frame 5 wurde mit 38 ms fertig
(`the_deadline_counts_from_the_capture_not_the_arrival`).

## Jede Annahme wird gebraucht

Für jede Annahme, die fallen kann, ohne dass das Modell sinnlos wird, gibt es
einen Test, der das Gegenbeispiel fährt:

| Annahme fällt | Was dann passiert | Test |
|---|---|---|
| A3 | Hintergrundarbeit überzieht ihren Plan um 5 ms; der geschützte Frame wartet 5 ms länger, als der Look-ahead zugelassen hat | `a_background_job_that_overruns_its_plan_breaks_the_bound` |
| A4 | Ohne Jitterhülle: ein Frame kommt `2J` nach seiner Prognose. Seine Frist wandert nur um `δ` mit; danach ist er „nicht zu retten" — der Guard vetoiert nicht mehr, und ausgerechnet dann startet ein 80-ms-Auftrag. Derselbe Fall **mit** Hülle hält die Schranke | `a_late_frame_the_guard_gave_up_on_waits_for_the_whole_background_job`, `with_an_envelope_a_late_frame_is_not_given_up` |
| A7 | Frame 4 wartet bis genau zur Ankunft von Frame 5 und wird verdrängt | `when_a_frame_can_still_wait_at_the_next_arrival_it_is_superseded` |
| A8 | Vor der ersten Ankunft gibt es keine Prognose; ein 33-ms-Auftrag, der beim Start schon wartet, läuft durch | `background_before_the_first_frame_is_the_start_up_exception` |

A1, A2 und A9 sind Rahmenannahmen: ohne sie ist die Frage eine andere. A6
hatte bis ADR-0036 ein Gegenbeispiel (Periode 120 ms, Horizont 100 ms, ein
115-ms-Auftrag reichte über die nächste Ankunft hinweg); derselbe Fall ist
jetzt geschützt (`an_arrival_beyond_the_old_horizon_is_protected`).

## Abgleich mit der Simulation

`search` fährt jedes Gitter durch den echten `vig_core::Scheduler` — dieselbe
Zulassung, derselbe Look-ahead, derselbe Margenregler wie im Gateway — mit
gesetzten Laufzeiten und Weckrufen genau dann, wenn der Scheduler sie
verlangt. Die Versorgungslücke wird mit derselben Regel gemessen wie im
Benchmark (`CoverageTracker`). Geprüft wird bei jedem Lauf innerhalb der
Annahmen: kein Frame verloren, jedes Alter `≤ Δ`, jede Lücke `≤` Schranke,
und der Scheduler rechnet mit genau dem `Ĉ` der Analyse.

Das Gitter liegt absichtlich an den Rändern: Deadlines genau auf der Grenze
von A4 und bei `T + Ĉ + δ − 2J − 1 µs` (A7), jeder Strom mit Jitter einmal mit
und einmal ohne Jitterhülle, Höchstalter genau bei `Δ`, Laufzeiten genau auf dem
Plan, Hintergrundarbeit, die 1 µs nach, eine halbe Periode nach und 1 µs vor
einer Freigabe eintrifft, und die Jittermuster „abwechselnd" (jeder zweite
Frame `2J` zu früh) und „+J, 0, −J" (der Nachfolger so früh wie möglich).

_Ergebnis: siehe unten, „Stand der Suche"._

## Was daraus folgt

**Eine Konfigurationsregel.** Ein geschützter Strom ist mit Hintergrundlast
lückenlos versorgt, wenn `A ≥ T + D + 4J`. Die Gate-M3-Konfiguration
(`T = D = 33`, `A = 66`) liegt genau auf `A = T + D`; die Schranke sagt dort
eine Lücke von höchstens `4J` — Jitter, nichts sonst. Sie gilt für die
Konfiguration aber nicht vollständig: dort sind Pose und Tiefe `high`, also
ebenfalls bewacht (A2).

**Eine zweite: die Jitterhülle gehört in den Vertrag.** Ohne sie verlangt A4
`Ĉ + 2J ≤ D`, mit ihr nur `Ĉ + δ ≤ D` — der Frame muss seine Deadline allein
halten können, mehr nicht. Eine zu kleine Hülle schützt nur einen Teil der
Verspätung; eine zu große hält Hintergrundarbeit länger auf, wenn ein Frame
ausbleibt (höchstens `2J_E − δ` länger als ohne).

### Die drei Befunde

Die erste Fassung dieser Analyse fand drei Schwächen, jede aus der
Herleitung und jede mit Test. Alle drei sind seit ADR-0036 behoben:

1. **Er gab verspätete Frames auf** (A4). Die Frist einer erwarteten Ankunft
   stand bei `e + D` und wanderte nicht mit. Kam der Frame später, hielt der
   Guard ihn für unrettbar und ließ genau dann Arbeit beliebiger Länge
   starten. *Jetzt:* mit Jitterhülle wandert die Frist bis zu `2J_E` mit; die
   Hülle ist für ein bewachtes Modell keine unerfüllte Forderung mehr. Ohne
   Hülle bleibt es beim bisherigen Verhalten, und A4 bleibt nötig.
2. **Seine Deadline begann bei der Ankunft, die des Vertrags bei der
   Aufnahme.** Deshalb stand `δ` in der Schranke. *Jetzt:* die Frist zählt ab
   der erwarteten Aufnahme; die Schranke ist `2J + D`.
3. **Er sah nur 100 ms voraus** (A6). Ein 5-Hz-Strom war gegen
   Hintergrundarbeit über 100 ms nicht geschützt. *Jetzt:* die nächste
   Ankunft jedes bewachten Stroms zählt, gleich wie weit sie weg ist; A6
   entfällt.

**Was die Aussage nicht sagt.** Nichts über mehrere bewachte Ströme (dort
reserviert der Guard kumulativ, und die Schranke hätte einen Summenterm),
nichts über Jitter jenseits der erklärten Hülle (gemessen wird er nicht),
nichts über mehrere Slots, nichts über Laufzeiten über dem Plan (dafür gibt
es den Margenregler mit einem Ziel von einem Prozent, ADR-0034, und damit
eine Wahrscheinlichkeit, keine Schranke), nichts über zerlegbare Aufträge.
Das ist der Umfang, den NV-23 verlangt: eine präzise Teilfrage, keine
Zertifizierung.

## Stand der Suche

| Gitter | Wann | Parametersätze | innerhalb der Annahmen | Gegenbeispiel | nächste Annäherung an Δ |
|---|---|---:|---:|---|---:|
| klein | jeder Testlauf, Debug, ~20 s | 5 832 | 5 832 | keines | 93,7 % |
| groß | `--ignored`, Release, ~5,5 min | 347 544 | 282 852 | keines | **100 %** — erreicht |

Stand nach ADR-0036, gefahren am 11.09. Das Gitter ist gegenüber der ersten
Fassung gewachsen: jeder Strom mit Jitter läuft einmal mit und einmal ohne
Jitterhülle, und die Deadlines liegen auf den neuen Rändern von A4 und A7.
Im kleinen Gitter liegt kein Satz mehr außerhalb der Annahmen — die Periode
als Deadline verletzte vorher A7, mit dem `δ` in A7 nicht mehr. Im großen
Gitter liegen 41 040 Sätze außerhalb von A5 und 23 652 außerhalb von A7.
Perioden bis 120 ms und Hintergrundarbeit bis 140 ms, vorher als „jenseits
des Horizonts" ausgeschlossen, liegen jetzt innerhalb und halten die
Schranke. Die große Suche erreicht sie auch ohne die gezielte Konstruktion.
In jedem Lauf innerhalb der Annahmen rechnete der Scheduler mit genau dem
`Ĉ` der Analyse — geprüft, nicht angenommen.

Die erste Fassung (vor ADR-0036) hatte 3 240 und 176 472 Sätze, ebenfalls
ohne Gegenbeispiel gegen die damalige Schranke `δ + 2J + D`.

Die erste Fassung des Satzes behauptete die Lückenschranke auch bei `Δ = A`.
Die Suche hat dagegen ein Gegenbeispiel gefunden (siehe „Am Rand"); der Satz
ist entsprechend enger gefasst, und das Gitter enthält seitdem `A = Δ`
(nur die Altersschranke wird geprüft) und `A = Δ + 1 µs` (die engste
Lücke).
