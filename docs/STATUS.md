# Arbeitsstand

Stand: 2026-08-31 · Gate S bestanden

## Fertig

### Phase 0 — WP0, WP1

- Cargo-Workspace, Edition 2024, Rust 1.98, strikte Workspace-Lints
  (`unsafe_code = forbid`, `unwrap_used`/`panic`/`indexing_slicing`/
  `arithmetic_side_effects` = deny).
- CI: fmt, clippy `-D warnings`, test, `cargo deny`, `reuse lint`.
- Apache-2.0, NOTICE, THIRD_PARTY_NOTICES, SECURITY.md, deny.toml mit der
  Lizenz-Allowlist aus Spec 20.8.
- `onetimer-core::time` — monotone, geprueft ueberlaufsichere Zeitarithmetik.
  Sicherheitsmargen als exakter Bruch statt `f64`, damit der Hot Path
  gleitkommafrei bleibt.
- `onetimer-core::arrayvec` — Vektor fester Hoechstkapazitaet ohne Heap.
- `onetimer-sim` — Discrete-Event-Uhr mit totaler Ereignisordnung,
  PCG32 (selbst implementiert, damit Ergebnisse dauerhaft reproduzierbar
  bleiben), lognormale Laufzeitverteilungen an p50/p99 kalibriert,
  Trace-Digest.
- **WP1-DoD nachgewiesen:** 120 000 Ereignisse, gleicher Seed erzeugt
  bitgleichen Trace und identische Kennzahlen.

### Phase 1 — WP2 bis WP6

- `queue` — Stale Work Collector: LATEST, LATEST_PER_KEY, FIFO, NEVER_DROP,
  Overflow-Policies. Kein Request verlaesst die Queue ohne expliziten
  terminalen Zustand.
- `slots` — Backend als Execution Slots (ADR-0004), Kreditkontrolle gegen das
  Backend (ADR-0002, L-021), binaeres Co-Run-Veto statt Interferenzmatrix
  (ADR-0006).
- `profile` — Laufzeitprofile je Belegungsgrad, geprueft auf Monotonie und
  Stichprobenzahl; Sicherheitsmarge in `[1.0, 3.0]`.
- `feasibility` — Slack, Machbarkeit ueber Slot-Belegung, Protected-Look-ahead
  mit non-work-conserving Veto.
- `variant` — Qualitaetssortierte Wahl mit asymmetrischer Hysterese;
  automatische Wahl deaktiviert bei unbekannter Qualitaetsherkunft (ADR-0007).
- `overload` — Fuenfstufige FSM mit gleitendem Fenster, getrennten Ein- und
  Ausstiegsschwellen und Mindestverweildauer.
- `scheduler` — Single-Owner-Actor, lexikographische Zielordnung, Stufen A/B/C
  der Stale-Pruefung, Metriken nach Spec 18.

### Testabdeckung

80 Tests, alle gruen. Golden Tests aus Spec 28: G-001, G-002, G-003, G-004,
G-005, G-006, G-007, G-008, G-009, G-011, G-012 — vollstaendig bis auf G-010
(Profile Stale), das erst mit dem Profiler in Phase 3 pruefbar wird.

### Phase 1b — Gate S: bestanden

- `coverage` — Fresh Result Coverage und AoI-Perzentile nach ADR-0005.
- `baseline` — bounded FIFO mit modelluebergreifender Prioritaet, also das,
  was ein gut konfigurierter Triton mit Rate Limiter leistet. Kein Strohmann:
  gleiche Slots, gleiche Vertraege, gleiche Profile, gleiche Weckrufe, und
  die Queue-Tiefe wird ueber 1/4/16 gefahren, wobei je Lastpunkt das **beste**
  Baselineergebnis gewertet wird.
- `harness` — gemeinsamer Treiber. Die tatsaechliche Laufzeit eines Frames
  haengt an `(seed, request_id, variant)` und nicht am Fortschritt eines
  gemeinsamen RNG-Stroms; dadurch bekommt derselbe Frame in beiden Laeufen
  dieselbe Laufzeit, unabhaengig von der Dispatchreihenfolge.
- `bin/gate-s` — Report nach `docs/benchmark/gate-s-report.md`.

Zwei Defekte, die erst dieser Lauf sichtbar gemacht hat, sind als ADR-0009 und
ADR-0010 dokumentiert und behoben. Beide waren mit Unit-Tests nicht auffindbar:
sie zeigten sich nur als Systemverhalten unter Dauerlast.

## Als naechstes

**Phase 2 — OIP-Gateway und Triton-Adapter.** Nach ADR-0008:

1. OIP-v2-gRPC-Gateway (WP7), transparenter Compatibility Mode.
2. Triton-Backend-Adapter mit In-Flight-Kreditkontrolle (WP8, ADR-0002). Die
   Kreditlogik liegt bereits im Kern; der Adapter muss sie an echte
   Completions binden statt an simulierte.
3. Shared-Memory-Referenz-Passthrough (WP14, ADR-0003) — vorgezogen, weil der
   gRPC-Copy-Pfad sonst das Performancegate reisst, ohne dass das etwas ueber
   das Scheduling aussagt.
4. Data-Plane-Overhead und Scheduling-Effekt **getrennt** messen.

Vor Phase 2 sinnvoll, aber nicht blockierend:

- `onetimer doctor` (WP4). Die Validierungsregeln liegen bereits als
  `validate()`-Methoden vor. ADR-0010 nennt einen neuen Pflichtcheck:
  ein Vertrag, dessen `max_age` in der Groessenordnung der Laufzeit liegt, ist
  ueberzeichnet und muss vor dem Start gemeldet werden — Szenario A ist genau
  so ein Fall.
- Burst-Lastprofile aus Spec 19.4; bisher laufen nur die stationaeren
  Lastpunkte.

## Offene Punkte

- **G-010** (Profile Stale) wartet auf den Profiler (Phase 3).
- **Online Runtime Estimator** ist vorbereitet: der Scheduler emittiert
  `Action::ObservedRuntime` samt Belegungsgrad, aber noch verarbeitet das
  niemand. Das ist WP11.
- **`onetimer doctor`** existiert noch nicht. Die Validierungsregeln liegen
  bereits als `validate()`-Methoden in `queue`, `model`, `slots` und `overload`
  vor; das CLI muss sie nur noch aufrufen und formatieren.
- **Der `variant`-Wert im Requestdescriptor** wird bislang nicht gesetzt; die
  gewaehlte Variante steht nur in der `Dispatch`-Aktion. Fuer den Kern
  ausreichend, fuer das Gateway spaeter zu klaeren.
