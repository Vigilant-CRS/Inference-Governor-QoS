# ADR-0039: Ein zweites Backend beweist die Naht, nicht die Hardware

**Status:** Akzeptiert · 2026-09-11
**Betrifft:** `backends/android-tflite/` (neu), `crates/vig-bench/src/bin/gate-m3.rs`,
Paket NV-25; ADR-0003, ADR-0022, ADR-0024, ADR-0033
**Ausloeser:** Alle GPU-Zahlen stammen von einer Maschine, und die
Backendnaht hatte genau eine Implementierung

## Kontext

ADR-0024 hat die Backendnaht eingefuehrt: der Actor spricht mit einem
`Executor`, nicht mit Triton. Die Support-Matrix sagte dazu ehrlich: "hat
genau eine Implementierung". Funktional war schon belegt, dass ein anderer
OIP-Server antwortet (OpenVINO Model Server, 150 von 150 Requests,
`portability.md`) — auf derselben Maschine, ohne Lastvergleich. Eine Naht,
hinter der nie ein anderes Backend unter Last gemessen wurde, ist eine
Behauptung.

Dazu kommt die Frage, auf wie vielen Maschinen die Aussagen von Gate M3
gelten. Die Antwort war: auf einer, einem RTX 3070 Laptop, mit zwei
Treiberversionen. Auf ARM war nur der Entscheidungspfad gemessen
(`arm-phones.md`) und der Datenpfad des Gateways (`arm-serve.md`), keine
Inferenz.

Ein Pixel 2 hat eine eigene GPU (Adreno 540, GLES 3.2, Vulkan, kein
oeffentliches OpenCL) und ist ein isoliertes Geraet: keine andere Sitzung
kompiliert darauf, kein Browser zieht am Leistungsbudget. Triton gibt es fuer
Android nicht.

## Entscheidung

**Das zweite Backend ist ein eigener Prozess, der OIP spricht — kein zweiter
`Executor`-Typ im Governor.**

1. **Die Naht ist das Protokoll.** Der `TritonExecutor` ist in Wahrheit ein
   OIP-Executor: Lebendigkeit, Bereitschaft, Metadaten, `ModelInfer` und die
   Statistik, deren Abschlusszaehler einen Slotkredit beendet (NV-00). Wer
   genau das beantwortet, ist ein Backend. Im Governor aendert sich keine
   Zeile; das ist der Beweis, um den es geht.
2. **`backends/android-tflite/`, eigener Workspace.** TFLite wird ueber seine
   C-API zur Laufzeit geladen (`libloading`), aus den unveraenderten AARs von
   Maven Central. `unsafe` steht dort und nur dort; der Root-Workspace
   schliesst das Verzeichnis aus und behaelt `unsafe_code = "forbid"` ohne
   Ausnahme (ADR-0033).
3. **Ein Modell, ein Thread, eine FIFO-Schlange.** Der GL-Delegate bindet
   seinen Kontext an den Thread, der ihn anlegt. Zugleich entspricht das
   Triton mit einer Instanz je Modell — der Vergleich "Backend direkt" gegen
   "ueber den Governor" bleibt derselbe wie in Gate M3.
4. **Der GPU-Delegate ist Pflicht.** Lehnt er ein Modell ab, startet der
   Server nicht; es gibt keinen stillen Rueckfall auf die CPU. `--cpu` ist ein
   ausdruecklicher Vergleichsmodus. Welcher Anteil des Graphen auf dem
   Delegate liegt, meldet TFLite nur ins Log; das Messskript haelt es fest.
   Modelle, die nur teilweise delegiert werden (MoveNet: 97 von 297 Knoten),
   sind kein GPU-Stellvertreter und werden nicht verwendet.
5. **Nur der Kopierpfad.** Android hat kein `/dev/shm`. Ein Request mit
   Shared-Memory-Parametern wird abgelehnt, nicht falsch gelesen. `gate-m3`
   bekommt dafuer `VIG_GATE_COPY=1`; beide Seiten des Vergleichs zahlen
   denselben Transport (ADR-0003).
6. **Gemessen wird auf dem Geraet.** Backend, Governor und Lastgenerator
   laufen auf dem Telefon; der Laptop schiebt nur Dateien. Ohne `nvidia-smi`
   plant der Governor mit dem Profil, wie ADR-0022 es fuer "unbekannt"
   vorsieht (`VIG_GATE_NO_HARDWARE=1`).

## Was das belegt, und was nicht

Es belegt, dass die Scheduling-Logik und ihr Vorteil nicht an Triton und
nicht an einer NVIDIA-Karte haengen. Es belegt nicht, dass ein Telefon eine
Zielplattform ist: ein Geraet von 2017 mit passiver Kuehlung drosselt, und
seine Zahlen uebertragen sich auf kein anderes Geraet. Die Zeile "Jetson"
der Support-Matrix bleibt ungetestet.

## Konsequenzen

- Die Backendnaht hat zwei Implementierungen, und die zweite ist nicht von
  Triton abgeleitet.
- Die Konfiguration nimmt dafuer `backend.type: oip`, das der Validator neben
  `triton` und `kserve` schon kannte. Alle drei fuehren zum selben Executor;
  erst jetzt steht hinter `oip` ein Backend, das nicht Triton ist.
- Ein zweiter Workspace heisst: eigenes Gate, eigene `Cargo.lock`, eigene
  Abhaengigkeitspruefung. `cargo deny` des Root-Workspace sieht ihn nicht.
- Das Backend ist so klein wie moeglich: keine entkoppelten Modelle, kein
  Laden und Entladen, keine Batches. Was fehlt, antwortet `Unimplemented`.
