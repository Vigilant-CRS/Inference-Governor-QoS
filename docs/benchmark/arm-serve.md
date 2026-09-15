# Der ganze Gateway auf einem Handy

Datum: 11.09.2026. Gerät: Pixel 2 (Snapdragon 835, Android 11) über USB;
Bezug: RTX-3070-Laptop (i7-10870H). Werkzeuge: die `end_to_end`-Tests der
Gateway-Crate, `vig serve`, `serve-latency` (`crates/vig-bench/src/bin/`),
Ablauf `tools/arm/serve-on-phone.sh`. Rohdaten:
`InferenceQoS-runtime/messungen/arm-serve-2026-09-11/`.

## Die Frage

[`arm-phones.md`](arm-phones.md) hat gezeigt, dass eine
Scheduling-Entscheidung auch auf dem ältesten Kern nur Mikrosekunden kostet.
Gemessen war dort der Kern allein, ohne gRPC, ohne Datenpfad, ohne Gateway.
Offen blieb: **was kostet der ganze Governor je Request auf einer schwachen
ARM-CPU** — Protokoll, HTTP/2, Tokio, Scheduler, Weiterleitung —, und läuft
`vig serve` dort überhaupt?

## Aufbau

**Bauen.** `vig` und die Testbinaries sind statisch für
`aarch64-unknown-linux-musl` gebaut und laufen ohne Root aus
`/data/local/tmp`. Anders als `decision-bench` braucht der Gateway C: tonic
bringt über `tls-ring` die Krypto-Bibliothek ring mit, und ohne
Cross-Compiler scheitert der Build an `aarch64-linux-musl-gcc`. Die leichteste
Lösung ohne Root ist zig als C-Compiler aus pip (`ziglang` 0.16.0 in einem
venv unter `~/.cache/vig-zig`), eingebunden über `tools/arm/zig-cc.sh` und
`zig-ar.sh`. Gelinkt wird weiter mit `rust-lld`. Heraus kommt ein
8,5-MB-Binary, das auf dem Handy startet. Das ist derselbe `vig` wie auf dem
Laptop: keine Codeänderung, kein Feature abgeschaltet, TLS eingeschlossen.

**Teil 1: das Datenpfadbudget.** `datapath_budgets_hold`
([datapath-budgets.md](../datapath-budgets.md)) läuft unverändert auf dem
Handy. Mock-gRPC-Backend mit 5 ms Rechenzeit, Governor und Client laufen in
einem Prozess über Loopback. Jede Zeile wird 300 Runden lang gemessen, nach
30 Aufwärmrunden, direkt und über den Governor **abwechselnd**. Je Kerngruppe
gibt es drei Läufe; berichtet wird der Median der drei. Das Handy ist wach
(`svc power stayon usb`), nicht im Doze-Zustand (dazu unten). Der Laptop als
Bezug läuft unter der exklusiven Messsperre auf den Kernen 8–15, Systemlast
1,3–2,9 durch Browser und einen ruhenden Triton.

**Teil 2: `vig serve` als Prozess auf dem Handy**, mit
`tools/arm/serve-phone.yaml`. Das ist eine verkleinerte Gate-M3-Konfiguration
mit den Modellen, die die kleinsten Eingaben haben: `pose_main` und
`depth_main` (je 2,4 MB) und `detector_small` (4,8 MB). Die Konfiguration
nutzt den Copy-Pfad. Shared Memory reicht nicht über Gerätegrenzen, und
Android hat kein `/dev/shm`.

## Ergebnis 1: das Datenpfadbudget hält auf dem Handy nicht

Median-Zusatz des Governors gegen den direkten Aufruf, in µs, Median aus drei
Läufen. Der direkte Aufruf dauert mit 5 ms Rechenzeit auf dem Laptop 6,4 ms
und auf dem Pixel 2 8,0 ms.

| Pfad | Nutzlast | Budget p50 | Laptop | Laptop, musl | Pixel 2 Gold (A73) | Pixel 2 Gold, 1 Kern | Pixel 2 Silber (A53) |
|---|---|---:|---:|---:|---:|---:|---:|
| Shm-Referenz | 150 KB | 300 | +162 | +216 | **+2 132** | +2 122 | **+2 524** |
| Shm-Referenz | 1,2 MB | 300 | +178 | +210 | **+2 148** | +1 955 | **+2 526** |
| Shm-Referenz | 6,2 MB | 300 | +182 | +202 | **+2 181** | +2 055 | **+2 407** |
| gRPC-Kopie | 150 KB | 500 | +317 | +580 | **+6 282** | +7 439 | **+5 383** |
| gRPC-Kopie | 1,2 MB | — | +1 188 | +2 201 | +16 779 | +11 794 | +13 153 |
| gRPC-Kopie | 6,2 MB | — | +4 764 | +10 177 | +34 975 | +30 556 | +44 432 |

p99-Zusatz auf dem Shm-Pfad: Laptop +150 bis +256 µs, Pixel 2 Gold +2,6 bis
+3,2 ms, Silber +3,5 bis +6,2 ms. Die Mediane der drei Läufe weichen auf dem
Handy höchstens 17 % von ihrem Median ab (Silber, Shm 6,2 MB), meist unter 10 %.

**Urteil.** Der Laptop besteht jede budgetierte Zeile in drei von drei Läufen.
Das Pixel 2 reißt **jede** budgetierte Zeile in jedem Lauf, auf beiden
Kerngruppen. Auf dem Shm-Pfad kostet der Governor dort rund 2,1 ms je Request
statt 0,17 ms. Das ist das Siebenfache der Grenze und ein Viertel eines
8-ms-Aufrufs. An einer 33-ms-Periode gemessen sind es gut 6 %.

Das Entscheidende ist aber, **wie** die Zahl von der Nutzlast abhängt, und
das hält: auf dem Shm-Pfad wächst der Zusatz auch auf dem Handy nicht mit der
Tensorgröße (+2 132, +2 148, +2 181 µs für 150 KB bis 6,2 MB). ADR-0003 gilt
auf ARM. Die zwei Millisekunden sind ein fester Preis je Request, kein
Datenpfad, der kopiert. Die strukturelle Prüfung
`the_shm_path_never_carries_the_payload` ist auf dem Handy grün, zusammen mit
der ganzen übrigen `end_to_end`-Suite (7 bestanden, 1 ignoriert), darunter
`a_burst_is_superseded_down_to_the_newest_request`.

## Woher die zwei Millisekunden kommen

Vier Gegenproben, alle aus denselben Binaries:

- **Ein Kern statt vier** (Maske `10`): derselbe Zusatz, +1 955 bis
  +2 122 µs. Aufwachlatenzen zwischen Kernen sind es nicht.
- **A53 statt A73:** Silber ist nur rund 15 % langsamer als Gold, auf dem
  Copy-Pfad bei 150 KB und 1,2 MB sogar schneller. In `decision-bench`
  brauchte dieselbe Entscheidung auf Silber im p99 rund doppelt so lange wie
  auf Gold. Rechenleistung im Nutzerraum ist es also auch nicht.
- **CPU-Zeit gegen Wanduhr** (`time`, ein Lauf auf Gold): 90 s Wanduhr,
  23,6 s im Nutzerraum, **56,4 s im Kernel**. Mehr als zwei Drittel der
  CPU-Zeit liegen im Kernel: Systemaufrufe, Loopback-TCP, Scheduler,
  Seitenfehler. Ohne Root lässt sich das auf dem Handy nicht weiter aufteilen.
- **musl statt glibc** (dieselben Tests auf dem Laptop, einmal für
  `x86_64-unknown-linux-musl` gebaut): +20 bis +30 % auf dem Shm-Pfad, etwa
  das Doppelte auf dem Copy-Pfad. Das ist der Allokator von musl; er erklärt
  einen Teil des Copy-Pfads auf dem Handy, nicht die zwei Millisekunden auf
  dem Shm-Pfad.

**Die Zahl beschreibt deshalb vor allem Android auf einem Kernel von 2017,
nicht die ARM-Kerne.** Ein Request über den Governor bedeutet zwei
Loopback-Verbindungen mehr und einige Übergaben zwischen Tasks, und jede davon
kostet auf diesem Gerät ein Vielfaches dessen, was sie auf dem Laptop kostet.

**Doze macht es schlimmer.** Ein Lauf mit ausgeschaltetem Bildschirm
(`mWakefulness=Dozing`, Silber-Kerne in einer Stichprobe bei 749 MHz; noch
ohne `TCP_NODELAY`, was die Mediane nicht berührt) kam auf +2,7 bis +3,5 ms
auf dem Shm-Pfad. Alle Zahlen oben stammen vom wachen Gerät.

## Ergebnis 2: `vig serve` läuft auf dem Handy

`vig serve -c serve-phone.yaml` auf den Gold-Kernen: Konfiguration geladen,
Startprüfungen durchlaufen, drei Modelle, ein Slot, gRPC auf 9001 und die
Metriken auf 9090 (per `adb forward` vom Laptop erreichbar). Auf SIGTERM
folgte der Drain: „alle Requests beantwortet, Governor beendet".

Gegen **Triton** lief er in dieser Messung nicht. Der Laptop war ab 15:05
für eine GPU-Messkette reserviert, und jede Inferenz über USB wäre Last auf
genau dieser GPU gewesen. Gemessen wurde deshalb ohne erreichbares Backend
(`adb reverse` entfernt), mit von Hand gebauten gRPC-Rahmen per `curl`. Damit
lässt sich alles zeigen, was vor dem Backend passiert:

| Prüfung | Ergebnis |
|---|---|
| `/healthz` | 200 |
| `/readyz` | 503, mit Grund: „backend: 1 von 1 Endpunkten haben die letzte Probe nicht beantwortet" |
| ein `ModelInfer` für `pose` | `UNAVAILABLE`, „Backend 127.0.0.1:8001 nicht erreichbar" |
| 40 Requests, 20 gleichzeitig | 14 × `ABORTED` „durch einen neueren Request desselben Streams ersetzt", 26 × `UNAVAILABLE` |
| `/metrics` danach | 41 empfangen, 27 weitergeleitet, 14 verdrängt, 27 Backendfehler |

Die Frische-Entscheidung fällt also im laufenden `vig serve` auf dem Handy:
`pose` hat eine LATEST-Warteschlange mit Kapazität 1. 14 der 40 Requests
wurden verdrängt, weil ein neuerer desselben Streams ankam, während sie noch
warteten; sie bekamen sofort `ABORTED`, statt hinter ihm zu warten. Die Zähler im Metrikendpunkt stimmen mit den Antworten
überein.

Eine Beobachtung dazu: ohne Backend schreibt der Actor alle 250 ms je Modell
eine Warnung „noch keine Abgleichs-Basislinie vom Backend", in 1,3 Sekunden
18 Zeilen. Für ein Gerät, dessen Backend einmal länger fehlt, ist
das zu viel Protokoll.

## Nebenbefund: 40 ms, die nicht der Governor waren

Die ersten Läufe auf dem Laptop rissen das p99-Budget in zwei von drei
Läufen, mit Zusätzen von +34 bis +37 ms bei einem Median von +120 bis
+210 µs. Die direkte Seite hatte dieselben Ausreißer: p99 des direkten
Copy-Aufrufs 41 ms bei einem Median von 6,5 ms. 40 ms ist die Mindestzeit des
verzögerten ACK unter Linux; das Muster ist Nagle.

**Die Ursache liegt im Testaufbau, nicht im Produkt.** Mock-Backend und
Gateway im Test starteten über `serve_with_incoming(TcpListenerStream)`, und
dort übergeht tonic die Einstellung `tcp_nodelay` des Builders. `vig serve`
bindet über `serve_with_shutdown(address)` und bekommt `TCP_NODELAY` per
Voreinstellung. Triton setzt es ebenfalls. Seit dieser Messung nehmen beide
Testserver `TcpIncoming::from(listener).with_nodelay(Some(true))`.

| Laptop, 3 Läufe | Shm 150 KB, p99-Zusatz | Copy 150 KB, direkt p99 | Urteil |
|---|---:|---:|---|
| ohne `TCP_NODELAY` | +0 / +37 130 / +185 µs | 41 109 / 40 740 / 41 241 µs | 2 von 3 FAIL |
| mit `TCP_NODELAY` | +342 / +253 / +148 µs | 6 909 / 6 947 / 6 808 µs | 3 von 3 PASS |

Die Mediane ändern sich nicht; die Schwänze verschwinden. Auf dem Handy
genauso: der erste wache Lauf ohne `TCP_NODELAY` hatte einen p99-Zusatz von
+31 ms in der Shm-Zeile bei 1,2 MB, danach höchstens +7,2 ms (Silber).

**Dasselbe Muster stand in allen `vig-bench`-Werkzeugen**, die einen
Governor im Prozess starten (`shm-latency`, `gate-m3`, `load-ramp`,
`frontier`, `soak`, `wire-bench`, `wp26`, `diy-baseline`, `edge-pilot`,
`oip-check`): ihre Governor-Seite lief ohne `TCP_NODELAY`. Seit diesem Befund
nehmen alle `vig_bench::incoming`, das die Einstellung setzt. Berichte vor dem
11.09. sind damit gemessen; wo ein Ausreißer von rund 40 ms auf der
Vigilant-Seite steht, gehört er möglicherweise dem Messaufbau. Die
Messkette vom 11.09. läuft mit der Korrektur.

## Nebenbefund: der Stack der Testthreads

Direkt aufgerufen, ohne `cargo test`, läuft `datapath_budgets_hold` mit den
2 MiB eines Testthreads über, im Release-Build, **auf dem Laptop wie auf dem
Handy**. `cargo test` setzt über `.cargo/config.toml` `RUST_MIN_STACK` auf
16 MiB. Der Kommentar dort sagt, im Release-Build liefen dieselben Tests mit
dem Standardstack durch; für diesen Test stimmt das nicht. Das Skript setzt
den Wert deshalb selbst.

## Was daraus folgt

**Für eine Jetson-Klasse-CPU ist das eine Warnung, kein Urteil.** Die
Laptop-Budgets übertragen sich nicht; das sagt
[datapath-budgets.md](../datapath-budgets.md) schon im Abschnitt „Scope", und
diese Messung liefert den Grund in Zahlen. Auf dem ältesten gemessenen Gerät
kostet ein Request über den Governor rund 2 ms statt 0,17 ms, und der Preis
steckt fast ganz im Kernel und im Transport, nicht im Scheduler (dessen
Entscheidung kostet dort 10 µs, [arm-phones.md](arm-phones.md)). Ein Jetson
Orin hat neuere Kerne und vor allem einen gewöhnlichen Linux-Kernel mit
glibc statt Android mit musl; wie viel von den 2 ms dort bleibt, zeigt erst
eine Messung dort. Das Werkzeug dafür ist jetzt da: derselbe Test läuft auf
jedem aarch64-Linux ohne Anpassung.

**Was es für den Produktentwurf heißt, gilt unabhängig davon.** Der
Governor-Zusatz ist ein fester Betrag je Request, keiner je Byte, solange der
Shm-Pfad genutzt wird. Auf einem schwachen Gerät zählt deshalb die **Zahl**
der Requests: Modelle mit 2–5 ms Laufzeit zahlen relativ am meisten. Der
Copy-Pfad ist auf dem Handy für Kameraframes gar keine Option. +17 ms bei
1,2 MB ist die Hälfte einer 33-ms-Periode.

**Und für die Releaseprüfung:** die p99-Grenzen, die
[datapath-budgets.md](../datapath-budgets.md) noch als Annahme führt, halten
auf dem Laptop in drei von drei Läufen, seit der Testaufbau `TCP_NODELAY`
setzt. Davor wären sie an einem Artefakt des Testaufbaus gescheitert.

## Was das nicht zeigt

- **Keine End-to-End-Zeit über USB und kein USB-Anteil.** Geplant war,
  direkt, über ein reines Relais auf dem Handy (`toybox nc -L`) und über den
  Governor gegen Triton auf dem Laptop abwechselnd zu messen. Dafür waren
  Triton und Host nicht frei. Das Skript enthält den Ablauf
  (`STEPS="profile serve"`), gelaufen ist er nicht.
- **Kein `vig serve` mit echtem Backend auf dem Handy.** Der ganze
  Gateway-Pfad mit Backend ist auf dem Handy nur im Prozess gemessen
  (Teil 1), der Prozess `vig serve` nur bis vor das Backend (Teil 2).
- **Keine Aufteilung der Kernelzeit.** Ohne Root gibt es kein `perf` für
  Kernelpfade. Welcher Anteil auf Loopback-TCP, Seitenfehler des
  musl-Allokators oder Scheduler entfällt, ist offen.
- **Keine Jetson-Qualifikation**, aus den Gründen oben. Die Zeile „Jetson
  Orin, Xavier" der Support-Matrix bleibt ungetestet.
- **Kein Pixel 5.** Es war nicht angeschlossen.

## Reproduzieren

```bash
python3 -m venv ~/.cache/vig-zig && ~/.cache/vig-zig/bin/pip install ziglang==0.16.0
tools/arm/serve-on-phone.sh                              # bauen, Suite, Budget Handy + Laptop
SKIP_BUILD=1 STEPS=offline tools/arm/serve-on-phone.sh   # vig serve ohne Backend
SKIP_BUILD=1 STEPS=profile tools/arm/serve-on-phone.sh   # Profile über USB (Triton nötig)
SKIP_BUILD=1 STEPS=serve tools/arm/serve-on-phone.sh     # Latenz über USB (Triton nötig)
```

Die Gegenprobe mit musl auf dem Laptop:
`rustup target add x86_64-unknown-linux-musl`, dann derselbe Testbuild mit
`--target x86_64-unknown-linux-musl`, `CC_x86_64_unknown_linux_musl` auf
`tools/arm/zig-cc.sh` und `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld`.
