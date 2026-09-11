# ADR-0033: Nativer Code gehoert in den Backendprozess, nicht in den Governor

**Status:** Akzeptiert · 2026-09-11
**Betrifft:** `Cargo.toml` (`unsafe_code = "forbid"`), Pakete NV-09, NV-12,
NV-14, NV-15; ADR-0012, ADR-0014, ADR-0024, ADR-0030
**Ausloeser:** Drei Spikes endeten mit derselben offenen Frage — eine C-FFI
im Workspace oder die `forbid`-Zusage in ihrer heutigen Form

## Kontext

`Cargo.toml` setzt `unsafe_code = "forbid"` fuer den ganzen Workspace. Das ist
die staerkste Zusage, die dieses Projekt macht: der Prozess, der ueber die GPU
entscheidet, hat keine Speicherfehler, die der Compiler haette finden koennen.

Vier Pakete brauchen eine C-Schnittstelle:

| Paket | Was gemessen ist | Wirkung auf den Engpass |
|---|---|---|
| NV-09 TensorRT Direct | 500–730 µs je Inferenz weniger, fast ganz aus Tritons Ein-/Ausgabekopien | keine: der Engpass ist ein 90-ms-Block |
| NV-12 CUDA-Graphs | 3,7 % weniger p50; mehrere Modelle mit Graphs laden nicht mehr | negativ: zerstoert den Mehrmodellbetrieb |
| NV-14 Green Contexts | partitioniert SMs nachweisbar; gegen einen Bandbreitengegner 1,62x | keine: der Engpass ist Zeit, keine SM-Konkurrenz |
| NV-15 XSched | Level 2 wirkt auf sm86: Restblocking ~50 → ~14 ms | **die einzige**, die den Engpass angreift |

Der naheliegende Weg waere ein eigenes Crate mit enger, gepruefter
FFI-Oberflaeche. Er kostet die Zusage — und die Tabelle zeigt, dass drei der
vier Pakete diesen Preis nicht einspielen wuerden.

Das vierte hat eine Eigenschaft, die die Frage anders stellt: **XSched sitzt
nicht im Governor.** Es schiebt sich als Shim vor `libcuda` in den Prozess,
der die Kernel startet. Das ist Triton. Der Governor startet keinen einzigen
Kernel; er entscheidet, welcher Auftrag das Backend erreicht. Eine FFI im
Governor wuerde XSched gar nicht erreichen — sie saesse im falschen Prozess.

## Entscheidung

**Der Governorprozess bleibt frei von `unsafe`. Nativer Code, der die GPU
beruehren muss, laeuft im Backendprozess — dem Prozess, dem die GPU ohnehin
gehoert — und der Governor steuert ihn ueber dieselbe Art Grenze, die er zu
Triton schon hat: einen Socket mit beobachtetem Vertrag.**

1. **Kein FFI-Crate im Workspace.** `unsafe_code = "forbid"` bleibt
   workspaceweit bestehen, ohne Ausnahme und ohne Feature-Flag, das sie
   aufhebt.
2. **NV-15 wird, wenn ueberhaupt, als Eigenschaft des Backends
   angeschlossen.** Der XSched-Shim wird in den Tritonprozess geladen; der
   Governor ruft XSched nie auf. Was er braucht, ist das gemessene
   Restblocking des Backends — als Messwert wie die Interferenztabelle
   (ADR-0026), nicht als Annahme. Eine Praemption, die das Backend nur
   behauptet, ist keine: die XSched-API meldet fuer jede Ebene „Erfolg", auch
   fuer die unfertige (Spike NV-15).
3. **NV-09 wird nicht gebaut.** 0,5 ms je Inferenz tragen keinen eigenen
   nativen Executor. Verlangt ein Pilot einen Edge-Pfad ohne Triton (NV-19:
   Jetson ohne Serverprozess), ist das ein eigener **Executorprozess** hinter
   der Backendnaht (ADR-0024) — nicht ein Crate im Governor.
4. **NV-12 und NV-14 sind auf dieser Plattform abgeschlossen**, mit
   negativem Ergebnis fuer den Engpass. Die Roadmap sieht genau das vor: „ein
   negatives Ergebnis beendet diesen Ausbau auf der Plattform".

## Konsequenzen

**Die Zusage haelt in ihrer heutigen Form.** Kein `unsafe` im Workspace, und
keine Fussnote, die „ausser in diesem Crate" sagt.

**Nativer Code hat einen ausdruecklichen Ort — und sein Absturz kostet das
Backend, nicht den Governor.** Ein fehlschlagendes Backend ist ein Fall, den
der Governor schon behandelt: elf Fehlerbilder in der Fehlerinjektion,
Quarantaene der Slotkredite, Bereitschaft, die auf Transportfehler reagiert.
Ein Segfault in einer FFI-Bruecke im selben Prozess waere keiner davon.

**Der Preis ist die Prozessgrenze.** Der Governor kann keinen Stream im
Backend direkt anhalten; er kann nur entscheiden, was er schickt, und sich
auf ein gemessenes Backendverhalten verlassen. Das ist gewollt: wer den Stream
besitzt, steuert ihn. Der Governor fuehrt die Zusagen, das Backend die
Ausfuehrung — dieselbe Teilung wie heute mit Triton.

**Eine XSched-Anbindung wird eine Frage des Betriebs.** Triton unter einem
Shim zu starten ist, wie der Spike schon sagte, „eine Aussage ueber den ganzen
Stack und nicht ueber ein Modul". Diese Entscheidung macht daraus keine
Codezeile im Governor, sondern eine qualifizierte Backendkonfiguration — mit
eigener Messung, eigener Supportgrenze und eigenem Eintrag in der
Support-Matrix.

## Verworfen

**Ein FFI-Crate hinter einem Feature-Flag, standardmaessig aus.** Aus dem
Standardbuild waere es heraus, die Zusage waere es trotzdem: sie gaelte dann
fuer einen Build, nicht fuer das Projekt. Und fuer NV-15 saesse die FFI im
falschen Prozess.

**Ein eigener Helferprozess im Workspace in C++, der XSched fuer den
Governor aufruft.** XSched steuert Queues im Prozess, der sie angelegt hat.
Ein dritter Prozess erreicht Tritons Queues nur ueber XSched's eigenen
Server (`xserver`) — und der ist Teil des Backendbetriebs, nicht ein
Werkzeug des Governors.
