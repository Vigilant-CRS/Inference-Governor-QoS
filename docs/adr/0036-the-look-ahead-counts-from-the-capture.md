# ADR-0036: Der Look-ahead rechnet ab der Aufnahme und sieht jede naechste Ankunft

**Status:** Akzeptiert · 2026-09-11
**Betrifft:** `core/feasibility` (`guard_protected`), `core/scheduler`
(`build_forecast`, `forecast_deadline`), `core/contract_ext` (`unenforced`),
`cli/doctor`, `gateway/actor`, `sim/bounded`; Spec 10.7, 10.8, L-010;
ADR-0020, ADR-0032
**Ausloeser:** Die Gegenbeispielsuche von NV-23
([Analyse](../analysis/nv23-bounded-claim.md))

## Kontext

NV-23 hat eine Schranke fuer das Alter eines geschuetzten Frames bewiesen und
gegen den echten Scheduler geprueft: `f_n − c_n ≤ δ + 2J + D`. Die Herleitung
hat drei Stellen offengelegt, an denen der Look-ahead schwaecher ist, als er
sein muesste. Jede hat einen Test, der sie als Gegenbeispiel faehrt.

1. **Er gibt verspaetete Frames auf.** Die erwartete Ankunft `e` hatte die
   Frist `e + D`. Kommt der Frame spaeter, rechnet der Guard ab `now` gegen
   eine Frist, die nicht mitwandert; wird sie ohne den Kandidaten
   unerreichbar, vetoiert er nicht mehr — „nur vetoieren, wenn der Kandidat
   die Ursache ist" — und laesst genau dann Arbeit beliebiger Laenge starten.
   Der verspaetete Frame kommt Mikrosekunden spaeter und wartet sie ganz ab.
   Die Annahme A4 (`Ĉ + 2J ≤ D`) war die Bedingung, dass das nie passiert.
2. **Seine Deadline beginnt bei der Ankunft, die des Vertrags bei der
   Aufnahme** (Spec L-010). Der Guard liess Arbeit zu, bis der geschuetzte
   Frame `D` nach seiner Ankunft fertig waere — das ist die Transportzeit
   `δ` nach seiner Deadline. Deshalb stand `δ` in der Schranke.
3. **Er sah fest 100 ms voraus** (Spec 10.8, `DEFAULT_HORIZON`). Ein
   geschuetzter Strom mit einer Periode darueber — ein 5-Hz-Sensor — war
   gegen Arbeit ungeschuetzt, die ueber seine naechste Ankunft hinwegreichte.
   Die Annahme A6 (`T ≤ H`) war die Bedingung dafuer.

Der Vertrag kennt seit ADR-0020 eine Jitterhuelle (`release_jitter_envelope`,
`release_jitter_ms`). ADR-0032 hat sie auf die Liste der nicht durchgesetzten
Forderungen gesetzt: gespeichert, von nichts ausgewertet.

## Entscheidung

**Die Frist einer erwarteten Ankunft ist die Frist, die der Dispatch fuer
denselben Frame rechnen wird: ab seiner erwarteten Aufnahme.**

```text
ĉ = min(c_vorher, a_vorher) + T        erwartete Aufnahme
e = a_vorher + T                        erwartete Ankunft
δ = e − ĉ                               Transport, aus der letzten Ankunft
```

`δ` ist keine neue Konfigurationsgroesse. Sie steckt in jedem Request:
Aufnahme (`generation_time`) und Ankunft. Eine Aufnahme nach der Ankunft
zaehlt als Ankunft — eine vorgehende Clientuhr soll dem Look-ahead keine
spaetere Frist liefern, als der Dispatch rechnet.

**Eine ueberfaellige Ankunft behaelt ihre Chance, solange sie noch kommen
kann.** Ein spaeter Frame ist spaeter aufgenommen; seine Frist wandert mit,
aber begrenzt:

```text
Ankunft  t = e + min(now − e, W)        fuer now > e
Frist    d = t + max(D − δ, Ĉ)
W = max(min(δ, D), 2 · J_Huelle)
```

* **Mit Jitterhuelle** `J_Huelle`: zwei Aufnahmen, jede hoechstens
  `J_Huelle` neben ihrem Raster, liegen hoechstens `2 · J_Huelle` weiter
  auseinander als die Periode. So lange haelt der Look-ahead den Frame fuer
  ausstehend.
* **Ohne Huelle** ist `W = δ`, und die Frist steht danach bei `e + D` —
  genau dort, wo sie bisher fuer jede ueberfaellige Ankunft stand. Ohne Huelle
  aendert sich an einem verspaeteten Frame nichts.
* Nach `e + W` steht die Frist; die Ankunft wird unrettbar und haelt nichts
  mehr auf. Ein Strom, der aufgehoert hat, sperrt Hintergrundarbeit also
  hoechstens bis `e + W + D − Ĉ`.
* `δ` zaehlt fuer das Fenster hoechstens bis `D`: was darueber liegt, ist eine
  falsch gehende Clientuhr und kein Transport.
* **Nie frueher als der Frame allein fertig waere** (`max(…, Ĉ)`). Haelt ein
  Frame seine Deadline nicht einmal ohne Konkurrenz, gaebe eine Frist ab
  Aufnahme ihn vor seiner Ankunft auf. Er soll dann so frueh wie moeglich
  fertig werden. Innerhalb der Annahmen von NV-23 greift diese Grenze nie.

**Die Jitterhuelle ist fuer ein bewachtes Modell keine unerfuellte Forderung
mehr.** Der Dienst nennt sie beim Start als genutzt, `vig doctor` ebenso; an
einem nicht bewachten Modell bleibt sie auf der Liste von ADR-0032. Gemessen
wird der Jitter weiterhin nicht: die Huelle ist eine Angabe des Betreibers,
und eine zu kleine Huelle schuetzt nur einen Teil der Verspaetung.

**Es gibt keinen festen Horizont mehr.** Die Prognose enthaelt je bewachtem
Modell genau eine Ankunft, die naechste, gleich wie weit sie weg ist. Der
Guard betrachtet jede, die der Kandidat verspaeten kann — also jede vor
seinem Ende (einen praemptierbaren Kandidaten: jede, ADR-0035).
`DEFAULT_HORIZON`, `Scheduler::set_horizon` und der Horizontparameter von
`guard_protected` entfallen. Der Aufwand bleibt derselbe: hoechstens ein
Eintrag je Modell.

## Konsequenzen

**Die Schranke verliert das `δ`.** Unter den Annahmen von NV-23 gilt

```text
f_n − c_n ≤ Δ = 2J + D
Luecke    ≤ max(0, T + D + 4J − A)
```

statt `δ + 2J + D` und `T + D + 4J + δ − A`. Der Jitter bleibt: dass ein
Frame `2J` frueher aufgenommen wird, als die Prognose sagt, kann niemand
vorhersehen. Die Konfigurationsregel fuer lueckenlose Versorgung wird
`A ≥ T + D + 4J`.

**Die Annahmen aendern sich so:**

| | vorher | jetzt |
|---|---|---|
| A4 | `Ĉ + 2J ≤ D` | `Ĉ + δ + max(0, 2J − W) ≤ D`; mit Huelle `≥ J`: `Ĉ + δ ≤ D` |
| A6 | `T ≤ H` oder Hintergrund `≤ H` | entfaellt |
| A7 | `D + 2J < T + Ĉ` | `D + 2J < T + Ĉ + δ` |

A4 ohne Huelle ist `max(Ĉ + 2J, Ĉ + δ) ≤ D`. Neu ist `Ĉ + δ ≤ D`: der Frame
haelt seine Deadline allein. Wo `δ > 2J` und `Ĉ + 2J ≤ D < Ĉ + δ`, stand die
alte Aussage und die neue steht nicht — dort verfehlt jeder Frame seine
Deadline schon ohne Konkurrenz, und die neue Untergrenze schuetzt ihn
staerker als der alte Guard (keine Arbeit vor ihn statt Arbeit bis `e + D`).

**Hintergrundarbeit wartet mit Huelle etwas laenger.** Bei einem Frame, der
nicht kommt, bis zu `2 · J_Huelle − δ` laenger als bisher, bevor der Guard
ihn aufgibt. Das ist der Preis, und er ist begrenzt
(`a_stream_that_stops_does_not_hold_background_back`).

**Etwas strenger als bisher, puenktlich:** die Frist liegt um `δ` frueher,
also passt in jede Luecke `δ` weniger Hintergrundarbeit. `δ` ist alles
zwischen Aufnahme und Ankunft am Governor — Vorverarbeitung beim Client
eingeschlossen; der Shared-Memory-Pfad allein kostet rund 160 µs
([data-plane.md](../benchmark/data-plane.md)). Die Gate-M3-Konfiguration
nennt keine Jitterhuelle und hat Perioden bis 66 ms: dort aendert sich
nichts ausser diesem `δ`.

**API.** `guard_protected` und `guard_protected_with_residual` verlieren den
Parameter `horizon`, `Scheduler::set_horizon` und
`feasibility::DEFAULT_HORIZON` entfallen, `ContractExtension::unenforced`
nimmt `guarded`. Das sind Rust-Schnittstellen der Crates, nicht die API von
[releases.md](../releases.md); Konfiguration, OIP und Kennzahlen bleiben
unveraendert.

## Tests

| Befund | Test |
|---|---|
| 1, mit Huelle | `with_an_envelope_a_late_frame_is_not_given_up` (vig-sim) |
| 1, ohne Huelle bleibt A4 | `a_late_frame_the_guard_gave_up_on_waits_for_the_whole_background_job` |
| 1, Lebendigkeit | `a_stream_that_stops_does_not_hold_background_back` (vig-core) |
| 2 | `the_deadline_counts_from_the_capture_not_the_arrival`, `the_bound_is_reached_exactly` (jetzt `2J + D`) |
| 3 | `an_arrival_beyond_the_old_horizon_is_protected`, `guard_protects_an_arrival_however_far_away_the_candidate_reaches` |
| Frist | `forecast_tests` in `core/scheduler`: puenktlich, ohne und mit Huelle, Untergrenze, falsch gehende Uhr |

Die Gittersuche prueft die neuen Annahmen: das Gitter hat eine Achse mit und
ohne Huelle, und die Deadlines liegen auf den neuen Raendern von A4 und A7.
