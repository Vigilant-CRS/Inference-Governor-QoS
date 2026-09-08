# Datenebene: was der Governor auf dem Draht kostet

Stand: 2026-08-31 · Release-Build · gegen ein echtes gRPC-Backend, nicht gegen Triton

> **Kein Vergleich gegen ein anderes Produkt.** Gemessen wird OneTimer gegen
> einen *direkten* Aufruf desselben Backends. Die Frage lautet: was kostet die
> Zwischenschicht? Nicht: ist OneTimer besser als X.

## Warum das getrennt gemessen wird

Spec 4.4 nennt weniger als 3–5 % End-to-End-Regression als Produktgate, und
Spec 19.8 macht mehr als 5 % zum Kill-Kriterium. Eine einzelne
End-to-End-Zahl vermischt dabei zwei voellig verschiedene Dinge:

* den **Scheduling-Effekt** — der Produktnutzen,
* den **Transportaufwand** — eine Eigenschaft des gewaehlten Datenpfads.

Wer beides zusammenwirft, kann einen realen Scheduling-Gewinn hinter einem
behebbaren Transportverlust verstecken oder umgekehrt (ADR-0003).

## Aufbau

Beide Seiten bekommen dieselbe Behandlung, wie Spec 19.1 es verlangt:
dieselben HTTP/2-Fenster (4 MiB Stream, 8 MiB Verbindung), dieselbe
Nachrichtenobergrenze (64 MiB), `tcp_nodelay`. Ein Direktclient mit
tonic-Voreinstellungen waere bei Tensornutzlasten kuenstlich langsam gewesen
und haette den Proxy besser aussehen lassen, als er ist.

Backend-Rechenzeit 5 ms, 40 Runden je Punkt nach 10 Aufwaermrunden.

## Ergebnis

| Nutzlast | direkt | über OneTimer | Zusatz | relativ |
|---|---:|---:|---:|---:|
| 150 KB (224×224×3) | 8130 µs | 8368 µs | +238 µs | 2 % |
| 1,2 MB (640×640×3) | 9592 µs | 12151 µs | +2559 µs | 26 % |
| 6,2 MB (1920×1080×3) | 13126 µs | 24818 µs | +11692 µs | **89 %** |
| **6,2 MB als Shm-Referenz** | 6229 µs | 6389 µs | **+160 µs** | **2 %** |

Die Messung schwankt zwischen Laeufen um einige hundert Mikrosekunden; die
Groessenordnungen und der Trend sind stabil.

## Was daraus folgt

**Der gRPC-Copy-Pfad ist fuer Kameraframes unbrauchbar.** Bei 6,2 MB
verdoppelt der Proxy praktisch die Uebertragungszeit — er deserialisiert die
Nutzlast und serialisiert sie wieder. Das reisst das Kill-Kriterium aus
Spec 19.8 um mehr als das Fuenfzehnfache.

**Der Shm-Referenz-Pfad loest das vollstaendig.** Derselbe nominale Tensor
kostet 160 µs statt 11 692 µs — Faktor 73. Der Grund ist kein Tuning, sondern
Struktur: im Request steht nur, *wo* die Daten liegen. OneTimer beruehrt sie
nie, und der Aufwand wird unabhaengig von der Tensorgroesse.

Das bestaetigt ADR-0003 empirisch: der Shm-Referenz-Passthrough ist nicht eine
spaetere Optimierungsstufe, sondern der Pfad, gegen den das Performancegate zu
messen ist. Der Copy-Pfad bleibt als Kompatibilitaetsweg erhalten — funktional
vollwertig, aber nicht der Pfad, auf dem das Produkt beurteilt wird.

## Nebenbefund: die Voreinstellungen luegen

Vor dem Transporttuning sah der Copy-Pfad bei 150 KB nach 17 % Zusatzaufwand
aus, und ein 6,2-MB-Frame wurde ueberhaupt abgelehnt:

```text
OutOfRange: decoded message length too large:
found 6220824 bytes, the limit is: 4194304 bytes
```

Ursachen waren tonics Voreinstellungen: 4 MiB Nachrichtengrenze — die einen
gewoehnlichen Kameraframe ablehnt — und ein 64-KiB-HTTP/2-Fenster, das bei
Tensornutzlasten eine Kette von WINDOW_UPDATE-Runden erzwingt. Beides trifft
einen Proxy doppelt, weil er zwei Verbindungen bedient.

Das ist kein Ergebnis ueber OneTimer, sondern eines ueber die Messung. Es ist
zugleich eine Warnung fuer Gate M3: eine Triton-Baseline mit Voreinstellungen
zu schlagen waere wertlos.

## Reproduzieren

```bash
cargo test --release -p vig-gateway --test end_to_end report_data_plane -- --nocapture
```
