# ADR-0004: Backend als explizite Execution Slots modellieren

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec §9.3 (Kern-Datentypen), §10.4 (Slack), §10.7 (Idle), §10.10 (Blocking)

## Kontext

Drei zentrale Stellen der Scheduling-Theorie setzen implizit voraus, dass das
Backend **eine** serielle Ressource ist:

- §10.4: `slack(j,v) = deadline(j) - now - predicted_runtime(j,v)` — eine skalare
  Restzeit ohne Aussage darüber, wann eine Ausführungskapazität frei wird.
- §10.7: das Idle-Beispiel („VLM belegt die GPU 50 ms, Detector kommt bei 8 ms").
- §10.10: `blocking_estimate(variant) = conservative end-to-end backend runtime`.

Real ist das Backend nicht seriell: Triton führt mit `instance_group { count: N }`
Instanzen nebenläufig aus, und mehrere Modelle teilen sich die GPU parallel.
§13.4 (Interference Matrix) **setzt genau diese Parallelität voraus**.

Ein Backend, das für die Slack-Rechnung seriell und für die Interferenzrechnung
parallel ist, ist inkonsistent. Ohne Auflösung ist die Feasibility-Prognose —
und damit der Kern von Admission Control und Variantenwahl — unbegründet.

## Entscheidung

`vig-core` modelliert das Backend als endliche, konfigurierte Menge von
**Execution Slots**.

- Ein **Slot** ist eine Ausführungskapazität, die genau einen Request gleichzeitig
  aufnimmt.
- Slots werden konfiguriert (`backend.slots`), abgeleitet aus Tritons
  Instance-Group-Konfiguration. Sie sind zugleich das Kreditkonto aus ADR-0002.
- Die Look-ahead-Simulation belegt Slots über Zeitintervalle und beantwortet
  damit **„wann wird frühestens ein Slot frei"** statt „wann ist die GPU frei".
- **Feasibility ist eine Aussage über eine Slot-Belegung**, nicht über eine
  skalare Restzeit.
- Interferenz wirkt als Laufzeit-Multiplikator auf gleichzeitig belegte Slots,
  nicht als Serialisierung (siehe ADR-0006 für den MVP-Umfang).
- Das Co-Run-Veto aus ADR-0006 ist eine Belegungsregel über Slots.

**Kompatibilität zur Spec:** Das Slot-Modell mit `N = 1` reproduziert exakt das
serielle Modell. Alle Beispiele in §10.7 und §10.10 bleiben gültige Spezialfälle.

## Konsequenzen

- `Slot` und `SlotSet` werden Kern-Datentypen neben `RequestDescriptor` (§9.3).
- Die Look-ahead-Kosten bleiben beherrschbar, weil N klein ist: Zielgrößen
  ≤ 8 Slots und ≤ 32 aktive logische Queues (§8.1). Die Belegungssimulation über
  einen Horizont von ~100 ms bleibt damit weit unter dem
  100-µs-p99-Entscheidungsziel.
- Slots sind heterogen konfigurierbar (ein Slot kann auf eine Teilmenge von
  Modellen beschränkt sein), was später Multi-Accelerator (WP29) ohne
  Modellbruch aufnimmt: DLA/NPU sind zusätzliche Slots mit anderen
  Laufzeitprofilen.
- Der Simulator und der Live-Scheduler benutzen **dieselbe** Slot-Abstraktion.
  Das ist die Voraussetzung dafür, dass ein Live-Trace offline reproduzierbar
  ist (§30.2).
