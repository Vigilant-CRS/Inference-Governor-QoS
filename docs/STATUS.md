# Arbeitsstand

Stand: 2026-09-01 · **Gate M3 bestanden** · Phasen 0-4 fertig · WP26 gemessen

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

### Phase 2a — Gateway, Adapter, Konfiguration, CLI

- `onetimer-protocol-oip` — Wire-Typen aus den unveraenderten Triton-.proto-
  Dateien; Extraktion der `onetimer_`-Parameter; Uhrenbehandlung nach ADR-0011.
- `onetimer-config` — YAML-Schema, `diagnose()` sammelt alle Befunde.
- `onetimer-backend-triton` — gRPC-Client mit Verbindungsheilung. Keine
  fachliche Politik im Adapter (Spec 8.4).
- `onetimer-gateway` — Single-Owner-Actor ueber bounded Kanal, gRPC-Dienst mit
  allen 21 OIP-Methoden, Uebersetzung terminaler Zustaende in Statuscodes.
- `onetimer-cli` — `onetimer doctor` und `onetimer serve`.

**Gemessen** gegen ein echtes gRPC-Backend (Release, leere Tensoren):
Zusatzaufwand des Governors rund 0,1 bis 0,2 ms je Request. In einem Burst von
12 Requests bei 60 ms Backendlaufzeit wurden 3 ausgefuehrt und 9 mit
`onetimer-reason: superseded` abgewiesen — die Frische-Semantik wirkt auf dem
Draht, nicht nur im Simulator.

### Phase 2b — Shared-Memory-Referenz-Passthrough

- Shm-Registrierungen werden durchgereicht und **gebucht** (`ShmRegistry`).
  Regionen werden bewusst nicht automatisch beim Verbindungsabbruch
  freigegeben: gRPC kennt keine Sitzung, und eine Zuordnung ueber die
  Gegenstelle waere bei mehreren Clients hinter einem Proxy falsch. Im Zweifel
  wuerde OneTimer eine Region abmelden, die ein anderer noch benutzt — das
  waere schlimmer als ein Leck.
- Transportgrenzen auf beiden Seiten gleich gesetzt: 64 MiB
  Nachrichtenobergrenze (tonics Default von 4 MiB lehnt einen gewoehnlichen
  Kameraframe ab) und 4/8 MiB HTTP/2-Fenster.

**Gemessen:** auf dem Copy-Pfad kostet ein 6,2-MB-Frame 11,7 ms Zusatzaufwand
(89 %), als Shm-Referenz 160 us (2 %). Faktor 73. Details in
`docs/benchmark/data-plane.md`.

### Phase 3 — Online Runtime Estimator (WP11)

- `estimator` — gleitendes Fenster beobachteter Laufzeiten je Modell, Variante
  und **Slot-Belegungsgrad**. Der Belegungsgrad ist der Ersatz fuer die
  Interferenzmatrix (ADR-0006): dieselbe Variante wird unter Nebenlast
  langsamer, und genau das wird gemessen statt modelliert.
- Planungsregel `max(offline_p99, online_p95) * Marge` (Spec 13.2). Der
  Schaetzer darf die Planung **verschaerfen, aber nie optimistischer machen**
  als das Profil — wer sein Profil unterbieten will, misst es neu.
- `MarginController` je Modell: nach einer Vertragsverletzung schnell straffen
  (+10 Prozentpunkte), in ruhigen Phasen langsam entspannen (-1), harte
  Grenzen. Dieselbe Asymmetrie wie bei der Variantenhysterese.
- `ProfileHealth` als Circuit Breaker (Spec 30.3): liegt die Wirklichkeit
  dauerhaft ueber dem Doppelten des Profil-p99, ist nicht die Marge zu klein,
  sondern das Profil falsch — dann hilft eine Meldung und keine groessere Marge.
- Quantile werden auf der **Schreibseite** berechnet. Gelesen wird bei jeder
  Planungsentscheidung, geschrieben nur bei jeder Fertigstellung; das Sortieren
  gehoert deshalb dorthin, wo es seltener passiert (Spec 8.1).

Offen in Phase 3: Profiler-CLI (WP10) und Prometheus-Export (WP13).

### Wire-Benchmark

`crates/onetimer-bench` faehrt denselben Workload zweimal durch den echten
Stack — einmal direkt zum Backend, einmal ueber OneTimer. Gleiche Frames,
gleiche Laufzeiten, gleiche Kapazitaet, gleicher Client. Die Baseline wird mit
mehreren Puffertiefen gefahren; je Strom zaehlt ihr bestes Ergebnis.

Daraus entstand ADR-0012: ein nicht unterbrechbarer Best-Effort-Job, der
laenger dauert als die kuerzeste geschuetzte Periode, startet unter Last nie —
unabhaengig von der Auslastung. Das betrifft genau das Szenario, mit dem
Spec 1.3 das Produkt begruendet.

### Phase 4 — Gate M3: bestanden

- `onetimer profile` (WP10) misst echte Laufzeitprofile am Backend. Es
  verlangt bewusst **keine** vorhandenen Profile: sonst muesste der Nutzer von
  Hand hinschreiben, was das Werkzeug gerade messen soll.
- Prometheus-Export (WP13) mit den Zaehlern aus Spec 18. Die wichtigsten sind
  die, die zeigen, was das System bewusst **nicht** getan hat.
- `gate-m3` faehrt denselben Workload gegen echten Triton, einmal direkt und
  einmal ueber OneTimer, ueber System Shared Memory.
- Ergebnis gegen die **getunte** Baseline (Rate Limiter mit Prioritaeten):
  22,4x weniger unabgedeckte Perioden beim Detektor, AoI p95 von 77 ms auf
  33 ms. Details in `docs/benchmark/gate-m3.md`.

Drei Umgebungsdetails, die kein Quickstart erwaehnt und die den Lauf je einmal
zum Absturz gebracht haben, stehen jetzt in `deploy/triton/README.md`:
`--device nvidia.com/gpu=all` statt `--gpus all`, `--allow-client-shm=true`
und `--ipc=host`.

### WP26 — kooperative Quanten

Umgesetzt nach ADR-0014, Sizing-Regel korrigiert durch ADR-0015. Gemessen mit
RF-DETR und Qwen3-0.6B auf einer RTX 3070.

Dabei entstanden zwei Faehigkeiten, die vorher fehlten:

- **Mehrere Backends.** Vision- und Sprachmodelle brauchen unvereinbare
  Bibliotheksstaende und laufen in getrennten Triton-Instanzen. Neue Option
  `backend_endpoint` je Modell. Die Kapazitaetsrechnung bleibt unberuehrt: die
  Slots modellieren die GPU, nicht den Prozess.
- **Decoupled-Backends.** Generative Backends antworten nur ueber den
  Stream-Endpunkt. OneTimer nimmt den Request weiterhin unaer entgegen und
  uebersetzt intern — die Zusage aus Spec 16.1 schuetzt die Frischelogik vor
  Clients, sie sagt nichts darueber, wie der Adapter das Backend anspricht.

Der wichtigste Befund steht in ADR-0015: eine veraltete Erzeugungsrate laesst
die Zerlegung **lautlos versagen**. Sie tut dann nichts, ohne einen Fehler zu
melden. `onetimer profile` muss die Rate messen koennen und `doctor` warnen,
wenn Sockel plus ein Token nicht in die kuerzeste Leerlaufluecke passen —
beides offen.

## Als naechstes

**Phase 3 — Profiler, Online Estimator, Metrics.** Der Scheduler emittiert
`Action::ObservedRuntime` samt Belegungsgrad; verarbeitet wird das noch nicht.

**Der Best-Effort-Fall (ADR-0012).** Das gemessene Ergebnis bestaetigt ihn auf
echter Hardware: der lange Block laeuft nicht. Bis WP26 (kooperative Quanten)
gilt die Zusage aus Spec 3.4 nur ab zwei Slots. Entweder rueckt WP26 vor, oder
die Positionierung wird eingeschraenkt.

**Die Luecken im Messbild.** Bursts und Lastrampe aus Spec 19.4, ein zweiter
Betriebspunkt, die Qualitaets-Deadline-Frontier aus Spec 19.7 und ein Vergleich
gegen Holoscan.

**Product Preview (WP21).** Docker-Compose, Quickstart, Beispielmodelle nach
Lizenz getrennt.

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
