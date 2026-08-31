# Third-Party Notices

Diese Datei listet alle Komponenten Dritter, die mit OneTimer ausgeliefert oder
gegen die gelinkt wird, samt Lizenz.

Sie wird aus `cargo deny`/`cargo about` erzeugt und muss vor jedem Release
aktualisiert werden (Spec Anhang C, Release-Checkliste).

## Stand

Phase 2. Der Scheduling-Kern `onetimer-core` und der Simulator `onetimer-sim`
haben weiterhin **keine externen Dependencies** — bewusst, siehe
`crates/onetimer-core/Cargo.toml`. Alles Folgende betrifft die Protokoll-,
Gateway- und Backend-Schichten.

### Mitgelieferte Quelldateien Dritter

| Komponente | Herkunft | Lizenz | Verwendung |
|---|---|---|---|
| `proto/oip/grpc_service.proto` | NVIDIA Triton (`triton-inference-server/common`) | BSD-3-Clause | Wire-Definition des Open Inference Protocol; unveraendert uebernommen, Copyright-Header erhalten |
| `proto/oip/model_config.proto` | NVIDIA Triton (`triton-inference-server/common`) | BSD-3-Clause | von `grpc_service.proto` importiert |

Die Dateien werden unveraendert eingebunden, damit ein Standardclient ohne
kundenspezifisches SDK inferieren kann (Spec L-001). Ein handgeschriebener
Nachbau waere weder kompatibel noch wartbar.

### Rust-Dependencies

Vollstaendig aufgeloest durch `cargo deny check licenses`. Die Allowlist steht
in `deny.toml` und folgt Spec 20.8. Zwei Einzelausnahmen sind dort mit
Begruendung dokumentiert:

| Crate | Lizenz | Warum zugelassen |
|---|---|---|
| `foldhash` | Zlib | permissiv, OSI-approved, keine Reziprozitaet; transitiv ueber `hashbrown` |
| `unicode-ident` | Unicode-3.0 | deckt nur die eingebetteten Unicode-Tabellen; Code selbst MIT OR Apache-2.0 |

Vor einem Release ist diese Datei aus `cargo about` zu erzeugen und vollstaendig
auszufuellen (Spec Anhang C).

## Nicht redistributierte Komponenten

NVIDIA Triton Inference Server und die NVIDIA-Containerimages werden **nicht**
mit OneTimer ausgeliefert. Das Compose-Beispiel referenziert das offizielle
NVIDIA-Image; der Nutzer bezieht es selbst aus der offiziellen Registry und
stimmt den NVIDIA-Bedingungen zu (Spec 6.4, 20.3).
