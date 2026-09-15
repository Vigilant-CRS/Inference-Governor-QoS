# NV-15: XSched-Spike — Level 2 funktioniert auf dieser Karte, entgegen der Upstream-Tabelle

Datum: 10.09.2026. Maschine: RTX 3070 Laptop (Ampere, sm86), Treiber
580.173.02, CUDA 12.4. Upstream: `github.com/XpuOS/xsched`, Stand
`f49289f0220931df78de948ed841ecbaf960a919` vom 19.08.2026, Apache-2.0.

Ein Spike mit Abbruchgrenze, kein Portierungsauftrag. Was hier steht, ist
gemessen; was daraus folgt, ist eine Schätzung wert und nicht Teil dieses
Pakets.

## Die Frage

Vigilant hält Arbeit **vor** dem Abschicken zurück. Was einmal auf der GPU
ist, läuft zu Ende — das ist ADR-0012, und daraus folgt die ganze
Quantenzerlegung von ADR-0014: ein 90-ms-Block blockiert 90 ms, und man macht
ihn kleiner, weil man ihn nicht anhalten kann.

XSched verspricht, ihn anhalten zu können. Die Frage des Spikes ist, ob das
auf **dieser** Karte stimmt.

## Drei Ebenen

`include/xsched/types.h`:

```c
kPreemptLevelBlock      = 1,   // nur noch nicht abgeschickte Kommandos
kPreemptLevelDeactivate = 2,   // abgeschickte Queue stilllegen
kPreemptLevelInterrupt  = 3,   // laufende Kernel unterbrechen
```

Level 1 ist das, was Vigilant längst tut — nur auf Kommandoebene statt auf
Requestebene. Der Gewinn läge in Level 2.

Die Upstream-Tabelle (`platforms/cuda/README.md`) sagt für **Ampere sm86**:

| Platform | XPU | Shim | Level-1 | Level-2 | Level-3 |
|---|---|---|---|---|---|
| CUDA | NVIDIA Ampere GPUs (sm86) | ✅ | ✅ | 🚧 | 🚧 |

🚧 heißt „implementation within progress". Nach dieser Tabelle wäre der Spike
hier zu Ende.

## Die API sagt nichts

Eine Sonde, die alle drei Ebenen anlegt, setzt, suspendiert und fortsetzt:

```
Level 1 anlegen: Erfolg  | setzen: Erfolg  | suspend: Erfolg  | resume: Erfolg
Level 2 anlegen: Erfolg  | setzen: Erfolg  | suspend: Erfolg  | resume: Erfolg
Level 3 anlegen: Erfolg  | setzen: Erfolg  | suspend: Erfolg  | resume: Erfolg
```

`kXSchedErrorNotSupported` existiert im Fehlerenum und wird hier für keine
Ebene zurückgegeben — auch nicht für die, die die Dokumentation als
unfertig führt. **Das ist die stillschweigende API-Lücke, die die Abnahme von
NV-15 ausschließen soll.** Wer sich auf den Rückgabewert verlässt, plant auf
einer Ebene, die er vielleicht nicht hat.

Gemessen werden muss also das Verhalten.

## Restblocking, gemessen

Aufbau: `examples/Linux/4_manual_sched`, zwei Ströme auf einer Karte. Der
niedrig priorisierte läuft dauernd, der hoch priorisierte suspendiert ihn vor
jeder eigenen Aufgabe und setzt ihn danach fort. Eine Aufgabe sind 100
Vektoradditionen über 32 MB, rund 100 ms. 30 Aufgaben je Lauf.

Das **Startfenster** (`XQueueSetLaunchConfig`) ist der Hebel: es sagt, wie
viele Kommandos höchstens abgeschickt sind. Genau die kann Level 1 nicht mehr
zurückholen.

| | Fenster 8 | Fenster 64 |
|---|---|---|
| Level 1, high-prio median | 101 ms | **135 ms** |
| Level 1, high-prio p90 | 103 ms | **154 ms** |
| Level 2, high-prio median | 98 ms | 103 ms |
| Level 2, high-prio p90 | 99 ms | **114 ms** |
| Level 3, high-prio median | 99 ms | 102 ms |
| Level 3, high-prio p90 | 100 ms | 115 ms |

Zwei Wiederholungen bei Fenster 64:

| | Wdh 2 | Wdh 3 |
|---|---|---|
| Level 1, p90 | 149 ms | 155 ms |
| Level 2, p90 | 111 ms | 114 ms |

**Level 2 wirkt.** Bei einem Startfenster von 64 Kommandos sinkt das
Restblocking von rund 50 ms auf rund 14 ms — das Dreifache, reproduziert über
drei Läufe. Bei einem kleinen Fenster gibt es nichts zu gewinnen, weil dann
schon Level 1 kaum etwas abzuschicken hat.

**Level 3 bringt nichts obendrauf.** Es verhält sich wie Level 2. Das passt
zu 🚧: die Ebene wird angenommen und fällt still auf die darunter zurück.

## Was das für Vigilant hieße

Vigilants Restblocking ist heute **eine ganze Inferenz**: 7 ms bei `depth`,
13 ms beim Detektor, 90 ms beim VLM (Gate-M3-Zahlen). Für das VLM ist genau
das der Grund, warum es unter Last nie startet (ADR-0012) und warum es die
kooperative Zerlegung gibt (ADR-0014) — mit dem Preis, den ADR-0031 beziffert.

Level 2 wäre der Weg, diesen Preis nicht zu zahlen. Was er kostet:

- **Ein Shim vor `libcuda`.** XSched schiebt sich zwischen Anwendung und
  Treiber (`libshimcuda.so`). Für Vigilant hieße das, Triton unter diesem Shim
  zu starten — nicht unmöglich, aber eine Aussage über den ganzen Stack und
  nicht über ein Modul.
- **Eine `unsafe`-Entscheidung.** Dieselbe wie bei NV-09 und NV-14: die
  Anbindung ist eine C-FFI, und `Cargo.toml` setzt `unsafe_code = "forbid"`.
- **Eine Wartungslast.** Der Upstream ist Forschungscode mit sechs Plattformen
  und einem Submodulbaum, der beim ersten `make cuda` nicht durchläuft (drei
  Anläufe, siehe unten).

## Die Baukette, geprüft

| Schritt | Ergebnis |
|---|---|
| `git clone --depth 1` | ok, Apache-2.0 |
| `make cuda` | scheitert: `3rdparty/ipc`, `cuxtra`, `CLI11` fehlen |
| gezielte Submodule | scheitert weiter: drei weitere fehlen |
| `git submodule update --init --recursive` | ok |
| `make cuda` | ok, `libpreempt.so`, `libhalcuda.so`, `libshimcuda.so` |
| Beispiel linken mit `nvcc` | scheitert: `__cxa_call_terminate@CXXABI_1.3.15` |
| dasselbe mit `-ccbin g++-13` | ok |

Der Linkfehler ist ein Toolchain-Bruch: XSched baut mit dem System-GCC 15.2,
CUDA 12.4 verlangt für den Hostcode höchstens GCC 13. Beides zusammen geht nur
mit `-ccbin`.

## Urteil

**Positiver Spike, mit Vorbehalt.** Level 2 funktioniert auf sm86, entgegen
der Upstream-Tabelle, und senkt das Restblocking messbar. Die
Präemptionsebene ist damit erreichbar; die Controller-Zuständigkeit
(app-managed vs. `xserver`) und der Betrieb unter Triton sind es noch nicht.

Was NV-15 laut Roadmap liefert, ist damit geliefert: gepinnter Upstreamstand,
geprüfte Lizenz- und Buildkette, die tatsächlich erreichbare Ebene, gemessenes
Restblocking und der Nachweis, dass die API ihre Lücken **nicht** meldet.

Was daraus folgt — eine Portierung, ein Shim unter Triton, die
`unsafe`-Frage — ist ein eigener Auftrag und eine eigene Schätzung. Dieser
Spike verspricht sie nicht.

## Nachtrag 11.09.2026 (Vormittag): scheinbar blockiert

> **Die Ursache in diesem Nachtrag ist falsch zugeordnet.** Es lag nicht an
> CUDA 13, sondern an einer zweiten `libcuda` im Prozess — siehe Nachtrag 2.
> Der Text bleibt stehen, weil er zeigt, wie die Fehldeutung entstand.

Die `unsafe`-Frage ist mit [ADR-0033](../adr/0033-native-code-lives-in-the-backend-process.md)
entschieden: nativer Code gehört in den Backendprozess. Für XSched heißt das,
den Shim in Triton zu laden, nicht in den Governor. Der naheliegende Aufbau
ohne eine Zeile Backendcode sind zwei Tritonprozesse auf einer GPU — die
geschützten Modelle mit Priorität 1, das VLM mit Priorität 0 — und `xserver`
mit HPF dazwischen. Das Startskript liegt unter
`InferenceQoS-runtime/skripte/xsched-triton.sh`.

**Der Build geht.** Im Triton-Image 26.06 (CUDA 13.3, GCC 13.3) baut XSched
ohne den `-ccbin`-Umweg, den der Spike auf dem Host brauchte. `libshimcuda.so`,
`xserver` und `xcli` entstehen.

**Der Betrieb nicht.** Jeder Tritonprozess unter dem Shim stürzt beim Laden
seines ersten Modells ab (SIGSEGV), mit drei Modellen wie mit einem. Triton
ist dabei nicht die Ursache: XSched's eigenes Beispiel
(`examples/Linux/1_transparent_sched`) stürzt im selben Image genauso ab.

| Aufbau | Laufzeit | `libcuda` | Ergebnis |
|---|---|---|---|
| Host, wie im Spike | CUDA 12.4 | 580.178.04 | XQueue angelegt, läuft — Level 1 und 2 |
| Triton-Image 26.06 | CUDA 13.3 | 610.43 (Forward Compatibility) | SIGSEGV beim Anlegen der ersten Queue |
| Triton-Image 26.06 | CUDA 13.3 | 580.178.04 (Host) | SIGSEGV beim Anlegen der ersten Queue |
| dasselbe ohne `XSCHED_AUTO_XQUEUE` | CUDA 13.3 | 610.43 | SIGSEGV |
| dasselbe mit `XSCHED_CUDA_LV3_IMPL=TSG` | CUDA 13.3 | 610.43 | SIGSEGV, Level 1 und 2 |
| zwei Tritonprozesse **ohne** Shim | CUDA 13.3 | 610.43 | laufen |

Der Backtrace aus `cuda-gdb` zeigt die Stelle:

```text
cudaStreamCreate
  → xsched::cuda::XStreamCreate
  → CudaQueueCreate → CudaQueueLv3Trap → CudaQueueLv2
  → InstrumentManager → InstrMemAllocator
  → cuXtraInstrMemBlockAlloc
  → libcuda.so: SIGSEGV
```

**Die Ursache.** Für sm86 wählt XSched immer `CudaQueueLv3Trap`. Ihr
Konstruktor legt Befehlsspeicher über `cuxtra` an — nicht dokumentierte
Interna des Treibers. Mit der 12.4-Laufzeit funktionieren sie, mit 13.3
brechen sie, unabhängig davon, welche `libcuda` darunter liegt. Der Upstream
hat nach dem gepinnten Stand `f49289f` keinen einzigen Commit (Stand
11.09.2026).

**Was daraus folgt.** Präemption auf dem qualifizierten Stack (Triton 2.70,
CUDA 13.3) ist heute nicht erreichbar. Es gibt zwei Wege, und keiner ist eine
Codezeile im Governor:

1. Triton auf einem Release mit CUDA-12-Laufzeit — ein anderer, nicht
   qualifizierter Stack mit eigener Messung.
2. Unterstützung für CUDA 13 im Upstream.

Das ist genau die Wartungslast, die der Spike angekündigt hat: Forschungscode
auf nicht dokumentierten Treiberinterna bricht mit der nächsten
CUDA-Version. Hätte der Governor eine FFI auf XSched, bräche er mit.

**Nebenbefund.** Zwei Tritonprozesse ohne Shim laufen mit explizitem Laden
nebeneinander, und `gate-m3` fährt Modelle mit eigenem Endpunkt jetzt
gleichzeitig. Der Aufbau steht, sobald eine der beiden Voraussetzungen
erfüllt ist.

## Nachtrag 2, 11.09.2026 (Mittag): nicht blockiert — die Ursache war eine zweite `libcuda`

CUDA 13 ist unschuldig.

**Zwei `libcuda` in einem Prozess.** `cuxtra` — die vorkompilierte
Bibliothek, die über die Export-Tabellen des Treibers Befehlsspeicher anlegt
— sucht sich ihre `libcuda` selbst: über fest einkompilierte Pfade oder die
eigene Variable `CUXTRA_CUDA_LIB`, **nicht** über `XSCHED_CUDA_LIB`. Im
Triton-Image benutzen Anwendung und XSched die Forward-Compatibility-
Bibliothek (610.43); CDI legt die Host-Bibliothek (580.178) nach
`/lib/x86_64-linux-gnu`, und die fand `cuxtra`. `LD_DEBUG=libs` zeigt beide:

```text
calling init: /lib/x86_64-linux-gnu/libcuda.so               ← cuxtra
calling init: /usr/local/cuda/compat/lib.real/libcuda.so.1   ← XSched / Anwendung
```

Die Export-Tabelle der einen Bibliothek arbeitete auf dem Kontext der
anderen. Auf dem Host gibt es nur eine `libcuda` — deshalb lief es dort. Die
Probe „Host-`libcuda` im Container" aus dem ersten Nachtrag hat genau das
übersehen: sie setzte `XSCHED_CUDA_LIB`, und `cuxtra` lud trotzdem seine
eigene.

**Ein zweiter Fehler, danach sichtbar.** Mit einer einzigen `libcuda`
scheitert der Trap-Pfad (Level 3), den XSched für sm86 immer wählt, sauber:
`invalid device ordinal` in `trap.cpp:20`. Der Level-2-Konstruktor davor
läuft durch. Ein Patch von neun Zeilen in drei Dateien macht mit
`XSCHED_CUDA_LV3_IMPL=LV2` die Level-2-Queue für sm86 wählbar — Level 3
brachte auf dieser Karte ohnehin nichts über Level 2 hinaus (siehe oben).

| Probe | `libcuda` XSched / `cuxtra` | Queue | Ergebnis |
|---|---|---|---|
| bisheriger Aufbau | 610 / 580 (ungewollt) | Lv3Trap | SIGSEGV in `cuXtraInstrMemBlockAlloc` |
| `CUXTRA_CUDA_LIB` gesetzt | 610 / 610 | Lv3Trap | kein Absturz, `invalid device ordinal` |
| beide auf Host-580 | 580 / 580 | Lv3Trap | Queue angelegt, danach SIGSEGV (13.3-Laufzeit ohne Compat-Treiber) |
| `TSG`, Level 1 und 2 | 610 / 610 | Lv3Tsg | läuft |
| `LV2` (Patch), Level 2 | 610 / 610 | Lv2 | läuft |

**Funktionsprobe, keine Messung.** Zwei Prozesse des XSched-Beispiels,
`xserver HPF`, Aufgabendauer des hoch priorisierten:

| Aufbau | hoch priorisiert |
|---|---|
| allein | 96 ms |
| zwei Prozesse ohne XSched | 182–205 ms |
| XSched `LV2`, Level 2 | 98–110 ms |
| XSched `TSG` | 96–116 ms |

Unter Triton 26.06 rechnet Prozess A (RF-DETR, Pose, Tiefe) unter dem Shim
alle drei Modelle, seine Queues sind bei `xserver` angemeldet. Prozess B mit
dem VLM passt nur in den Speicher, wenn A und B den Referenz-Triton
ersetzen, statt neben ihm zu laufen.

**Was daraus folgt.** Präemption auf dem qualifizierten Stack ist
erreichbar — als Backendkonfiguration nach ADR-0033: zwei
Umgebungsvariablen und ein kleiner Patch am gepinnten Upstream, keine Zeile
im Governor. Gemessen ist sie noch nicht. Und sie allein löst das VLM-Problem
nicht: der Governor hält das VLM mit `slots: 1` und `no_corun` weiter
zurück, bis die Planung eine präemptierbare Hintergrundlast als gemessene
Eigenschaft des Backends kennt. Das ist der nächste Schritt.

Laborprotokoll, Patch und Startskript: `InferenceQoS-runtime/archiv/xsched-rootcause.md`,
`xsched-sm86-lv2.patch`, `xsched-triton-alt.sh`.
