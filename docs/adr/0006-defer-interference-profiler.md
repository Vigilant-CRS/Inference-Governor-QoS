# ADR-0006: Interferenzprofiler hinter Gate M3, MVP nutzt Co-Run-Veto

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec §13.4 (Interference Matrix), WP12, §25 (Milestone M2)

## Kontext

§13.4 und WP12 fordern offline gemessene paarweise Slowdown-Faktoren für
relevante Modellpaare.

Bewertung:

**Aufwand.** Bei N Modellvarianten sind N(N−1)/2 Paare zu messen, je Paar mit
stabilem Warmup und ausreichender Samplezahl. Bei 8 Varianten sind das 28
Messläufe. Profile sind zusätzlich an einen Umgebungsfingerprint gebunden
(§13.5) und verfallen bei Modell-, TensorRT- oder Triton-Änderung.

**Gültigkeit.** Reale Überlast hat 3–4 gleichzeitig aktive Modelle. Paarweise
Faktoren komponieren nicht zuverlässig zu Tripel- oder Quadrupel-Effekten;
Cache-, SM- und Speicherbandbreitenkonkurrenz sind nicht multiplikativ.

**Redundanz.** Der Online Runtime Estimator (§13.2, WP11) misst die
*tatsächliche* Backend-Laufzeit unter der real herrschenden Nebenläufigkeit. Er
beobachtet den Interferenzeffekt also bereits, ohne ihn zu modellieren — exakt
die Argumentation, die §13.2 für Taktabsenkung und Temperatur führt:

> „Wenn die Hardware aus irgendeinem Grund langsamer wird, steigt die gemessene
> Online-Laufzeit und der Scheduler reagiert auf den Effekt statt auf die
> Ursache."

Dieselbe Begründung gilt für Interferenz. Ein teures Offline-Modell für einen
Effekt zu bauen, den der billige Online-Schätzer ohnehin sieht, ist vor dem
Falsifikationsgate nicht gerechtfertigt.

## Entscheidung

1. **WP12 (automatisierter Interferenzprofiler) wird hinter Gate M3 verschoben.**

2. **Für den MVP wird Interferenz auf ein binäres Co-Run-Veto reduziert.** Die
   Konfiguration kann wenige Paare als `no_corun` markieren — typischer Fall
   Detector + VLM. Das Slot-Modell aus ADR-0004 setzt das als Belegungsregel um.
   Keine Matrix, keine Multiplikatoren, kein Profiler-CLI.

   ```yaml
   backend:
     slots: 2
     no_corun:
       - [detector, vlm]
   ```

3. **Der Online Estimator wird pro Slot-Belegungsgrad geführt**: beobachtete
   Laufzeit bei 1, 2, … gleichzeitig belegten Slots. Damit wird der
   Interferenzeffekt datengetrieben erfasst, ohne ihn offline zu vermessen, und
   die konservative Prognose bleibt konsistent mit ADR-0004.

## Konsequenzen

- Phase 3 wird deutlich kleiner; der Weg zu Gate M3 verkürzt sich.
- Das Feature „interference-aware Zulassung, automatisiert" aus der
  Wettbewerbsmatrix §3.4 ist im MVP **nur teilweise** eingelöst. Das ist in der
  Außendarstellung entsprechend zu kennzeichnen — §3.5 verlangt genau diese
  Ehrlichkeit.
- Konfigurationsschema und Slot-Modell werden so geschnitten, dass die volle
  Matrix später ohne Bruch nachrüstbar ist: `no_corun` ist der Grenzfall
  `slowdown = ∞`.

## Reaktivierungsbedingung

Wenn der M3-Benchmark zeigt, dass die Feasibility-Prognose unter Nebenläufigkeit
systematisch danebenliegt und der Online Estimator zu träge nachzieht, wird WP12
reaktiviert. Diese Bedingung ist als **explizites Messkriterium** im
Benchmark-Report zu führen: Verteilung von
`(beobachtete Laufzeit − prognostizierte Laufzeit)` aufgeschlüsselt nach
Slot-Belegungsgrad.
