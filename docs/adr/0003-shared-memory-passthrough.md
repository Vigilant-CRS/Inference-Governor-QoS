# ADR-0003: Shared-Memory-Referenz-Passthrough als primärer Datenpfad

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec §17.3 (MVP-Stufen), §4.4 (Performancegate), §19.8 (Kill-Kriterium)

## Kontext

§17.3 stuft die Datenpfade als M0 (gRPC-Copy) → M1 (System Shm) → M2 (CUDA Shm)
→ M3 (Jetson). Gleichzeitig nennt §4.4 weniger als 3–5 % End-to-End-Regression
als Produktgate, und §19.8 macht über 5 % Regression zum **Kill-Kriterium**.

Rechnung: ein 1920×1080×3-uint8-Frame sind 6,2 MB. Über den gRPC-Copy-Pfad
durchläuft die Payload pro Hop mindestens eine Deserialisierung in den
OneTimer-Prozess und eine Reserialisierung Richtung Triton — zwei zusätzliche
Kopien plus Protobuf-Encoding, dazu Allokationsdruck bei 30 Hz über mehrere
Modelle.

Dieser Overhead trifft exakt das Kill-Kriterium. Er ist aber **keine Eigenschaft
des Scheduling-Konzepts**, sondern eine Eigenschaft des Bootstrap-Transports. Das
Produkt an einer Eigenschaft seines Übergangspfads scheitern zu lassen, wäre ein
Messfehler, kein Erkenntnisgewinn.

Wesentliche Beobachtung: Bei Tritons Shared-Memory-Extension überträgt der Client
im Infer-Request **keine Tensordaten**, sondern eine Referenz (Region-Name,
Offset, Byte-Größe). Reicht OneTimer die Registrierungs-Calls an Triton weiter
und im Infer-Request nur die Referenz durch, **berührt OneTimer die Payload
nie**. Der Data-Plane-Overhead des Governors geht gegen null und wird unabhängig
von der Tensorgröße.

## Entscheidung

1. **Der Shm-Referenz-Passthrough ist der primäre, produktdefinierende
   Datenpfad**, nicht eine spätere Optimierungsstufe. Er wird in Phase 2
   gemeinsam mit dem Gateway gebaut; WP14 wird vor WP10–WP13 gezogen
   (siehe ADR-0008).

2. Der gRPC-Copy-Pfad bleibt als Kompatibilitäts- und Bootstrap-Pfad erhalten
   (§16.3 Compatibility Mode). Er ist funktional vollwertig, aber **nicht der
   Pfad, gegen den das Performancegate gemessen wird**.

3. Der Benchmark weist **Data-Plane-Overhead und Scheduling-Effekt getrennt**
   aus. Ein einzelner End-to-End-Zahlenwert, der beides vermischt, ist für die
   Produktentscheidung unbrauchbar: er kann einen realen Scheduling-Gewinn hinter
   einem behebbaren Transportverlust verstecken.

4. OneTimer verwaltet Shm-Regionen als eigenen Zustand — Registrierung,
   Lebenszyklus, Unregister bei Client-Disconnect — damit ein abgestürzter Client
   keine Regionen im Backend leaked.

## Konsequenzen

- Phase 2 wird umfangreicher als das reine „transparente Forwarding" aus WP7.
- Der Compatibility Mode muss beide Pfade beherrschen und pro Request
  entscheiden, welcher gilt. Das ist Gateway-Logik, keine Scheduler-Logik:
  `onetimer-core` bleibt payloadfrei und kennt nur `PayloadRef` (§9.3).
- CUDA-Shm (x86/dGPU) und Jetson bleiben wie in §17.3 gestuft. **Nur System-Shm
  wird vorgezogen.**
