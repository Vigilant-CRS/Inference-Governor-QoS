# ADR-0002: Backend In-Flight Control (neue Anforderung L-021)

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec §3.1, §7.1 (Lastenheft), §10.3 (Stufe B), §23 (doctor)

## Kontext

§3.1 stellt korrekt fest, dass Tritons Request Cancellation nur begrenzt
garantiert ist, und leitet daraus ab, stale Arbeit **vor** dem Backend-Dispatch
zu eliminieren. Die Umkehrung wird nicht adressiert: **Triton besitzt eigene
Warteschlangen** — Instance-Group-Queues, den Dynamic Batcher mit
`max_queue_delay_microseconds`, den Rate Limiter.

Sobald OneTimer mehr Requests forwarded, als Triton gleichzeitig ausführen kann,
entsteht hinter dem Governor eine zweite Queue, die OneTimer weder sieht noch
beeinflusst.

Folgen:

1. Reihenfolgeentscheidungen des Schedulers werden backendseitig neu geordnet
   oder verzögert. Der Governor verliert die Kontrolle exakt im entscheidenden
   Moment.
2. `predicted_finish` in §10.3 Stufe B ist unbegründet, weil die unbekannte
   Triton-Queue-Zeit im Modell fehlt.
3. Supersession verliert Wirkung: ein Request, den OneTimer noch superseden
   könnte, ist bereits `FORWARDED` und wartet unerreichbar in Tritons Queue.
4. Der M3-Benchmark misst eine Mischung aus OneTimer- und Triton-Scheduling und
   ist damit nicht interpretierbar.

Dies ist die gefährlichste offene Stelle der Architektur, weil sie den Kernnutzen
still aushebelt, ohne einen Fehler zu erzeugen.

## Entscheidung

**1. Neue Muss-Anforderung L-021 — Backend In-Flight Control.**

OneTimer führt pro Backend-Executionslot (ADR-0004) ein Kreditkonto. Ein Request
wird nur dann forwarded, wenn ein Kredit frei ist; der Kredit wird bei Completion
oder Fehler zurückgegeben.

Ziel-Invariante:

```text
backend_queue_depth ≈ 0
```

Das Backend ist beschäftigt, aber nicht gepuffert — „starved but busy". Alle
Warteschlangen liegen sichtbar und steuerbar in OneTimer, keine im Backend.

**2. `onetimer doctor` prüft die Backend-Konfiguration.** Warnung, wenn
Triton-Modellkonfiguration eigenes Queuing aktiviert, das OneTimers Kontrolle
unterläuft: `dynamic_batching` mit `max_queue_delay_microseconds > 0`,
gesetzte `max_queue_size`, aktiver Rate Limiter.

**3. Referenzkonfiguration im Repo.** Für Protected-Modelle Dynamic Batching aus
bzw. `max_queue_delay_microseconds: 0`; `instance_group.count` explizit gesetzt
und OneTimer über `backend.slots` bekannt gemacht.

## Konsequenzen

**Kosten:** Strikte Kreditgrenze erzeugt zwischen Completion und nächstem
Dispatch eine Lücke von einer Round-Trip-Zeit; die GPU liegt kurz brach.

**Gegenmittel:** Die Kreditzahl ist `slots + dispatch_pipelining_depth` mit
Default `dispatch_pipelining_depth = 1`. Ein Extra-Kredit hält das Backend
warm, hält die Backend-Queue aber bei höchstens 1. Der Trade-off zwischen
Auslastung und Kontrollverlust wird in Phase 2 **gemessen, nicht geraten**.

**Testbarkeit:** Die Invariante ist im Simulator als Property testbar
(`in_flight <= slots + pipelining_depth` zu jedem Zeitpunkt) und in Phase 2
gegen Tritons `nv_inference_pending_request_count` verifizierbar.
