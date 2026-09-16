# Verifikation · 16. September 2026

Quellstand am Abschluss: `6c689dc22a69ac4cf1db30c4008806fb50115e13` plus die
Arbeitsbaumkorrekturen an Gateway-SHM und dem direkten Vergleichslauf.
Dateihashes und Zähler: [validation.json](validation.json).

| Prüfung | Ergebnis | Nachweis |
|---|---|---|
| Ausgangsstand: Workspace, offline/locked | 961 bestanden, 0 fehlgeschlagen, 6 ignoriert, 65 Suiten | [Log](baseline-tests.log) |
| Final: Workspace mit allen Features, offline/locked | **977 bestanden, 0 fehlgeschlagen, 6 ignoriert, 65 Suiten** | [Log](workspace-tests.log) |
| Separater Android-TFLite-Workspace, Hosttests | **22 bestanden, 0 fehlgeschlagen** | [Log](android-tests.log) |
| Clippy, Workspace, alle Targets und Features, Warnungen als Fehler | bestanden, Exitcode 0 | [Log](clippy.log) |
| `cargo fmt --all --check` | bestanden, Exitcode 0 | während der Prüfung und am Abschluss ausgeführt |
| `git diff --check` | bestanden, Exitcode 0 | am Abschluss ausgeführt |

## Befehle

Die Cargo-Builds liefen über
`InferenceQoS-runtime/quiet-build.sh` mit dem vorhandenen NVMe-Cache und den
Build-/Messsperren. Das vermeidet Build-Artefakte auf dem NTFS-Projektlaufwerk.

```sh
cargo test --workspace --all-features --locked --offline
cargo test --manifest-path backends/android-tflite/Cargo.toml --locked --offline
cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
cargo fmt --all --check
git diff --check
```

## Vor der Reparatur beobachtete Fehler

Die gezielten Tests wurden vor und nach den entsprechenden Reparaturen
ausgeführt. Folgende Gegenproben schlugen nachweislich zunächst fehl:

- `a_second_completion_cannot_fill_the_gap_before_its_delivery`: 666 statt
  333 Promille.
- `a_retained_result_does_not_reset_the_gap_between_deliveries`: 0 statt
  290.000 Mikrosekunden Lieferlücke.
- `a_gap_only_objective_counts_deliveries_without_period_or_max_age`:
  310.000 statt 0 Mikrosekunden nach neuer Lieferung.
- Ergänzung in `doctor::tests::objectives_count_against_the_slots`:
  `NotReady` statt `ReadyWithWarnings` wegen doppelt gezählter Anforderungen.
- `an_uncertain_shm_registration_keeps_the_segment_reserved`: eine fremde
  Reservierung wurde nach verlorener Backendantwort nicht verhindert.

Zusätzliche Tests prüfen Kaltstart ohne Ergebnis, länger nutzbare Ergebnisse,
Bursts, Fenstergrenzen, zeitlich dünne Beobachtung, die numerische Zeitgrenze
und das Routing auf zwei getrennte Mock-Backends. Die einfachen Zyklusfehler
waren beim ersten Lauf dieser zusätzlichen Tests bereits parallel repariert;
sie werden hier nicht als eigener fehlgeschlagener Vorher-Lauf ausgegeben.

## Reichweite

Host-/Mock-/Simulatorprüfungen, keine neue Inferenzqualifikation auf GPU oder
Telefon. Die sechs ignorierten Workspace-Tests bleiben ignoriert; sie wurden
nicht als bestanden gezählt. AArch64-Emulation, Hardware-Datenpfadbudgets,
Release-Gates, ROS-Laufzeitintegration und Modellqualität wurden in dieser
Runde nicht erneut vollständig geprüft. Kein Deployment und kein Neustart
laufender Backend-/Governor-Prozesse.
