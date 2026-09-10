# Changelog

Keep a Changelog-Format, semantische Versionierung. Was „die API" hier
bedeutet, steht in [docs/releases.md](docs/releases.md).

## [0.2.0] — 2026-09-10

Erste veroeffentlichte Version. `v0.1.0` war getaggt, aber der Release-Workflow
scheiterte im `verify`-Job; es existieren keine Artefakte dazu.

### Behoben — Fehler, die Kapazitaet erfunden haben

- **Slotkredit nach Governor-Neustart.** Der Endnachweis verglich Tritons
  Statistikzaehler mit der Zahl der **eigenen** Auslieferungen. Tritons Zaehler
  laeuft ueber die Lebensdauer des Triton-Prozesses, und der ueberlebt den
  Governor: nach einem Neustart war „das Backend meldet mindestens so viele
  Abschluesse" beim ersten Request sofort wahr, und der Abgleich gab einen
  Slotkredit frei, waehrend die Recheneinheit womoeglich noch rechnete. Jetzt
  gibt es eine Basislinie, die beim Start geholt wird.
- **Look-ahead war zu konservativ.** Die kumulative Reservierung stapelte
  Pessimismus ueber alle erwarteten Ankuenfte im Horizont, auch ueber solche
  nach dem Ende des Kandidaten. Pose und Tiefe wurden dadurch im Dauerlauf zu
  1000 Promille unabgedeckt.
- **Artefakt-Digest umfasste Dokumentation.** Eine Notizdatei neben dem Modell
  verschob den Digest. Digestiert werden jetzt nur Versionsverzeichnisse.

### Neu

- **Profilmanifest (NV-03).** Artefakt-Digest, Runtime, Geraet, Aufteilung,
  Messbedingungen und Gueltigkeitsdomaene je Profil. Fehlende Felder sind
  `unknown`, nie `verified`.
- **Vertragszusaetze (NV-02).** Versioniert und optional: Verbrauchertakt,
  Weakly-hard-Bedingung (M/K/L), Freigabeliste, geforderte Nachweisstufe.
- **Hardwarebeobachtung (NV-04).** Neues Crate `vig-platform`, ausschliesslich
  lesend, kein Root. `vig doctor` meldet jetzt, ob die Karte gedrosselt ist.
- **Messpfad (NV-05).** Absolutes Freigaberaster, vier getrennte Zaehler,
  Uhrpruefung, Zelle verwerfen bei Hardwarewechsel.
- **Zustandsabhaengige Prognose (NV-06).** Diskrete Zellen je Zustandsklasse.
  Laeuft im **Schatten**; Scharfschalten ist eine Betreiberhandlung.
- **Backendnaht (NV-07).** `Executor`-Vertrag, Triton als erste
  Implementierung, Fake-Executor fuer Tests ohne GPU.
- **Semantik der Varianten (NV-10).** Labelreihenfolge, Koordinatenkonvention,
  Einheit und Eingabevertrag. Gleiche Tensorform bei anderer Bedeutung schaltet
  die automatische Variantenwahl ab.
- **Gerichtete Interferenz (NV-11).** Beide Richtungen getrennt, absolute
  Kosten, keine Hochrechnung auf drei Modelle.
- **Gueltigkeitsbewusster DAG (NV-17).** `CaptureId`, Referenzzaehlung.
  Gebaut und getestet, **noch nicht angeschlossen**.
- **Missbudget in Entscheidungen (NV-24).** Voreinstellung **aus**.
- **Verbraucherorientierte Metriken (NV-01).** Consumer Coverage,
  zeitgewichtetes AoI, laengste Luecke — neben den bisherigen Groessen.

### Geaendert

- `vig profile` gibt Profile als Blockmapping mit Manifest aus statt als
  einzeilige Flow-Map.
- `vig calibrate` misst Modellpaare in **beiden** Richtungen.
- Metrikfamilien ohne einen einzigen Messwert werden nicht mehr ausgegeben.

### Dokumentation

Runbook, Supportmatrix, RF-DETR-Variantenbeispiel, ADR-0019 bis ADR-0028.

### Bekannte Grenzen

Unveraendert gegenueber der Supportmatrix: ein Ausfuehrungsgeraet, keine
funktionale Sicherheit, keine Harte-Echtzeit-Garantie, alle Zahlen von einer
RTX 3070 Laptop.
