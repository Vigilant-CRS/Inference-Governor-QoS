# Third-Party Notices

Diese Datei listet alle Komponenten Dritter, die mit OneTimer ausgeliefert oder
gegen die gelinkt wird, samt Lizenz.

Sie wird aus `cargo deny`/`cargo about` erzeugt und muss vor jedem Release
aktualisiert werden (Spec Anhang C, Release-Checkliste).

## Stand

Phase 0. Der Scheduling-Kern `onetimer-core` und der Simulator `onetimer-sim`
haben **keine externen Dependencies** — bewusst, siehe `crates/onetimer-core/Cargo.toml`.

| Komponente | Version | Lizenz | Verwendung |
|---|---|---|---|
| *(keine)* | | | |

## Nicht redistributierte Komponenten

NVIDIA Triton Inference Server und die NVIDIA-Containerimages werden **nicht**
mit OneTimer ausgeliefert. Das Compose-Beispiel referenziert das offizielle
NVIDIA-Image; der Nutzer bezieht es selbst aus der offiziellen Registry und
stimmt den NVIDIA-Bedingungen zu (Spec 6.4, 20.3).
