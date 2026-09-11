# Changelog

Keep a Changelog-Format, semantische Versionierung. Was „die API" hier
bedeutet, steht in [docs/releases.md](docs/releases.md).

## [Unveroeffentlicht]

### Hinzugefuegt

- **Kontextabhaengige Fortschrittskosten fuer zerlegte generative Auftraege**
  (NV-16, ADR-0031). `cooperative.prefill_per_token_us` im Vertrag; die
  Quantenzuschneidung rechnet mit Prompt plus bisher Erzeugtem statt mit einem
  konstanten Sockel. Ein spaetes Quantum faellt damit kleiner aus als ein
  frueheres. Null heisst „gemessen wirkungslos oder nicht gemessen" und
  verhaelt sich exakt wie vor NV-16.
- **`cooperative.max_overhead_permille`** — kostet die Zerlegung mehr Arbeit
  als die Grenze zulaesst, laeuft der Auftrag ungeteilt (ADR-0014, Rueckfall).
  Ohne gesetzte Grenze aendert sich nichts.
- **`vig calibrate` misst den Kontextanteil.** Zweite Messreihe mit variabler
  Promptlaenge; ohne sie kuerzt sich der Prefill-Anteil definitionsgemaess aus
  der Differenz heraus. Der Sockel wird um den Prefill des Kalibrierprompts
  bereinigt.
- **`vig doctor` nennt den Preis der Zerlegung.** Zahl der Quanten, Aufschlag
  gegenueber dem ungeteilten Lauf (als Untergrenze, gerechnet ohne Prompt),
  dazu TTFT und groesster Tokenabstand.
- **Fuenf Prometheus-Kennzahlen:** `vig_generative_prefill_us_total`,
  `vig_generative_decode_us_total`, `vig_generative_fixed_us_total`,
  `vig_generative_context_tokens`, `vig_decomposition_refused_total`. Prefill
  steht getrennt von Dekodierung, weil ein Re-Prefill kein Token erzeugt; der
  Sockel steht daneben, weil er auf der Messmaschine der groesste Einzelterm
  ist.

- **`backend.prediction: shadow | active`** (NV-06, ADR-0023). Die
  zustandsabhaengige Prognose ist jetzt ein Konfigurationsschritt des
  Betreibers statt einer Methode am Kern. Voreinstellung `shadow`; ein
  vertippter Wert wird abgelehnt. Ein Test belegt, dass `active` eine
  Entscheidung aendert: bei einem Profil, das die Karte ueberschaetzt, laeuft
  die grosse statt der kleinen Variante.
- **Variantenwechsel als Kennzahlen** (Spec 19.7):
  `vig_variant_upgrades_total` und `vig_variant_downgrades_total` je Modell.
  Die erste Wahl eines Modells zaehlt nicht als Wechsel.
- **Datenpfadbudgets** (NV-20). Eine Tabelle in
  `vig-gateway/src/datapath_budget.rs`, zwei Pruefungen dagegen: gegen ein
  Mock-Backend (`datapath_budgets_hold`, `--ignored`, fuer die
  Releasequalifikation) und gegen echten Triton (`shm-latency` mit Urteil
  und Exitcode). Siehe [docs/datapath-budgets.md](docs/datapath-budgets.md).
- **Benchmarks:** `load-ramp bursts` faehrt Lastspitzen ueber einer Grundlast
  (Spec 19.4); `frontier` vergleicht automatische Variantenwahl mit festen
  Varianten (Spec 19.7); `gate-m3` faehrt Modelle mit eigenem Backendprozess
  gleichzeitig und gibt den Schattenvergleich der Prognose aus.
- **Praemptierbare Hintergrund-Lanes** (NV-15, [ADR-0035](docs/adr/0035-preemption-is-a-measured-backend-property.md)).
  `backend.preemptible_lanes` und `preemptible: { residual_blocking_us, source }`
  je Modell. Ein Modell in einem eigenen, niedrig priorisierten Backendprozess
  laeuft auf seiner Lane statt auf dem geschuetzten Slot; geschuetzte Arbeit
  plant waehrenddessen mit dem **gemessenen** Restblocking R, der Look-ahead
  fragt, ob R in die geschuetzte Reserve passt. `vig calibrate` misst R ueber
  mehrere Endpunkte, `vig doctor` warnt, solange es nur erklaert ist. Ohne
  diese Angaben aendert sich keine Entscheidung. Drei neue Kennzahlen.
- **Vig-Edge-Pilot, ein interner Referenzpilot** ([docs/pilot/edge-pilot.md](docs/pilot/edge-pilot.md)):
  annotierte Videosequenzen als Kameras, ein interner 23-Klassen-Detektor als
  geschuetzter Strom, Lageberichte ueber ein LLM als Hintergrundlast.
  Alarmlatenz und Lagebild-Trefferquote gegen die Annotation, Abnahmekriterien
  K1–K8 vor der Messung festgelegt; das Binary `edge-pilot` druckt das Urteil.
- **Der Kern auf ARM gemessen** ([docs/benchmark/arm-phones.md](docs/benchmark/arm-phones.md)):
  `decision-bench` in `vig-sim`, statisch fuer aarch64, auf Pixel 2 und
  Pixel 5 gefahren.
- **Ein begrenzter Nachweis** (NV-23, [docs/analysis/nv23-bounded-claim.md](docs/analysis/nv23-bounded-claim.md)):
  eine Schranke fuer das Alter geschuetzter Frames, bewiesen und per
  Gegenbeispielsuche gegen den echten Scheduler geprueft.
- **ROS-2-Bruecke** (NV-21, `integrations/ros2/`). Ein rclpy-Knoten, der
  Kamera-Topics mit Altersangabe, Aufnahmekennung und Supersession-Schluessel
  an den Governor gibt — ueber Shared Memory auf demselben Host, sonst per
  Kopie, ohne nativen Code (ADR-0033). Ablehnungen des Governors erscheinen
  als Ereignisse, nicht als Fehler. 54 Tests, live geprueft gegen `vig serve`
  vor echtem Triton. Siehe [docs/integrations/ros2.md](docs/integrations/ros2.md).
- **Alle Clientparameter dokumentiert** in docs/getting-started.md, mit
  Abschnitten zu Aufnahmen (NV-17) und Anwendungshinweisen (NV-18). Vorher
  standen dort sechs von vierzehn.

### Behoben — Zusagen, die zwischen den Komponenten zerfielen (Review 10.09., ADR-0032)

- **Der Abschlussabgleich konnte den falschen Slot freigeben.** Tritons
  aggregierter Zaehler wurde gegen die eigene Auslieferungsnummer geprueft.
  Laeuft Auftrag A noch und wird B fertig, galt A als beendet — waehrend seine
  Recheneinheit womoeglich noch rechnete. Umgekehrt hob ein Auftrag, der das
  Backend nie erreicht hat, das Ziel an und konnte einen spaeter tatsaechlich
  abgeschlossenen dauerhaft in Quarantaene halten. Der Zaehler belegt jetzt
  **Ruhe** statt eines Einzelabschlusses.
- **Das Alter eines Ergebnisses zaehlte ab der Fertigstellung.** ADR-0005 sagt:
  ab der Aufnahme. Aufnahme 0 ms, Fertigstellung 50 ms, Abtastung 70 ms,
  Hoechstalter 66 ms war ein Miss und wurde als frisch gezaehlt.
- **Veraltete Lieferungen verkuerzten die gemessene Versorgungsluecke.**
  150 ms ohne ein einziges brauchbares Ergebnis wurden als 90 ms gemeldet.
  Kern und Benchmarktracker rechnen jetzt dieselbe Regel.
- **Das Nutzlastbudget endete mit dem Client.** Nach einem Timeout rechnete das
  Backend weiter und hielt die Nutzlast; das Budget war trotzdem frei. Zwei
  16-Byte-Auftraege liefen bei einem Budget von 16 Bytes.
- **`evidence_required: proven` wurde stillschweigend angenommen**, obwohl
  nichts in diesem Projekt analytisch abgesichert ist. Jetzt abgelehnt.
  `phase` und `minimum_background_progress_pct` werden umgesetzt;
  `release_jitter_envelope` und `delivery_boundary: consumer` beim Start und
  im `doctor` als nicht durchgesetzt gemeldet.
- **Die Aktuationstoleranz federte eine Zusage ab.** 1470 MHz galten als
  Erfuellung eines zugesagten Bodens von 1500 MHz. Der **beobachtete** Takt
  muss jetzt selbst innerhalb aller Grenzen liegen.
- **Zwei Messpfade fuer dieselbe Groesse.** `vig calibrate` mass Ruecken an
  Ruecken und uebersprang fehlgeschlagene Aufrufe; `vig profile` gab auf einem
  absoluten Raster frei und buchte alles. Beide benutzen jetzt denselben Kern,
  und aus einer nicht qualifizierten Reihe entsteht kein Profil.
- **Laufzeitbeobachtungen landeten in der falschen Zustandszelle.** Gebucht
  wurde der Zustand bei der Fertigstellung; richtig ist der beim Dispatch.
- **Der Speichertakt fehlte im beobachteten Zustand.** Eine Karte, die ihn
  heruntertaktet und den SM-Takt haelt, galt als „voller Takt".
- **Der Collector-Unterprozess hatte keine Frist.** Ein haengender Aufruf liess
  den Hardwarewaechter unbegrenzt warten — er meldete nie einen Fehlversuch.
- **Die Startpruefung verglich Geraet und Treiber nicht**, obwohl beides seit
  NV-04 messbar ist.
- **Die Besitzsperre der Aktuation war nicht atomar** (lesen, pruefen,
  abschneiden) und galt fuer alle Geraete gemeinsam. Jetzt `O_EXCL`, je Geraet
  eine. Und eine Beobachtung von **vor** der Anforderung gilt nicht mehr als
  Bestaetigung.
- **Die Tokenobergrenze war eine Schaetzung.** `Bytes / 4` zaehlte vier
  Ein-Byte-Token als eines. Verbraucht wird jetzt das Kleinere aus zwei
  Obergrenzen — der bestellten und der aus den Bytes.

### Behoben

- **Ein nicht zerlegter Auftrag wurde als Quantum eingeplant.** Ein Request
  ohne Texteingang auf einem `cooperative`-Vertrag bekam vom Kern eine
  Quantendauer zugewiesen, waehrend das Backend den ganzen Auftrag rechnete;
  Look-ahead und Slotbelegung planten mit einer Zahl, die um
  Groessenordnungen zu klein war. Der Deskriptor traegt jetzt `decomposable`.
- **Der schlimmste Tokenabstand wurde unterschaetzt**, wenn die Quantengroesse
  den Auftrag nicht glatt teilte, und fuer einen ungeteilten Lauf wurde einer
  erfunden.
- **`vig calibrate` konnte den Sockel auf null druecken.** Mittelwert ueber
  fuenf Runden ohne Warmlaufverwurf: ein einziger Stall reichte, damit der
  errechnete Kontextterm den ganzen Sockel auffrisst. Jetzt Median, ein
  verworfener Warmlauf, und ein Widerspruch zwischen beiden verwirft den
  Kontextterm statt den Sockel.
- **`vig doctor` und das Gateway urteilten verschieden** ueber denselben
  Vertrag: `doctor` schwieg bei `prefill_per_token_us == 0`, das Gateway lehnte
  ab. Jetzt dieselbe Schwelle in beiden.

- **`vig_predictor_active` meldete den Modus erst nach der ersten
  Fertigstellung.** Wer die Prognose scharf schaltete, sah bis dahin eine
  Null. Der Metrikabzug wird jetzt beim Umschalten nachgezogen.
- **Der Testmock zaehlte keine Nutzlastbytes.** `MockBackend::raw_bytes_seen`
  wurde nie erhoeht; die Zusicherung „der Shared-Memory-Pfad traegt keine
  Nutzlast" war dadurch bei jedem Verhalten des Governors gruen. Der Mock
  zaehlt jetzt, und ein Test prueft beide Richtungen.
- **Die Benchmarks massen seit `dd83086` ohne Geraetezustand.** `gate-m3`,
  `load-ramp` und `frontier` rufen den Hardwarewaechter jetzt auf, wie
  `vig serve` es tut.
- **Die scharf geschaltete Prognose plante ohne Sicherheitsmarge** (NV-06).
  Sie nahm das p95 der Zelle, der bisherige Weg `max(p99, p95) × Marge`. Auf
  der Gate-M3-Last liess sie in zwei von drei Laeufen das VLM an und brach
  dafuer die Zusagen an die geschuetzten Stroeme: Detektor bis auf 90 %,
  laengste Luecke 99 statt 15 ms ([Messung](docs/benchmark/nv06-ab.md)). Die
  Zelle ersetzt jetzt das Profil und nicht die Marge; der Schattenvergleich
  rechnet ebenfalls mit Marge.

- **`vig calibrate` mass nach einer verworfenen Paarmessung unter fremder
  Last.** Die Lasttasks der verworfenen Messung liefen weiter, und jede
  spaetere Messung derselben Kalibrierung lief unter einer Last, die in
  keinem Profil stand. Sie werden jetzt vor der Pruefung beendet.

### Geaendert

- **Die Sicherheitsmarge hat ein Ziel**
  ([ADR-0034](docs/adr/0034-the-margin-has-a-target.md)). Der Margenregler
  hielt bisher still, wenn jede elfte Ausfuehrung ihren Plan ueberzog — ein
  Gleichgewicht, das sich aus zwei Schrittweiten ergab und das niemand
  gewaehlt hatte. Jetzt regelt er auf einen ausdruecklichen Anteil: ein
  Prozent, oder das vereinbarte Missbudget des Vertrags (hoechstens fuenf
  Prozent). Die Planung wird dort vorsichtiger, wo die Laufzeit streut. Der
  Boden bleibt die konfigurierte Marge.
- **Kein FFI-Crate im Workspace** ([ADR-0033](docs/adr/0033-native-code-lives-in-the-backend-process.md)).
  Nativer Code gehoert in den Backendprozess; `unsafe_code = "forbid"` bleibt
  ohne Ausnahme. NV-09 wird nicht gebaut, NV-12 und NV-14 sind auf dieser
  Plattform abgeschlossen. NV-15: XSched laeuft unter Triton 26.06, nachdem
  eine zweite `libcuda` im Prozess als Ursache des Absturzes gefunden war
  (`CUXTRA_CUDA_LIB`) und ein kleiner Patch die Level-2-Queue fuer sm86
  waehlbar macht — ungemessen ([Nachtraege](docs/spikes/nv15-xsched.md)).
- **Support-Matrix nachgezogen:** Prognose (NV-06) und
  Abhaengigkeitsgraph (NV-17) erreichbar, TensorRT in Triton und der
  Prefill-Term qualifiziert, Treiber 580.178.04 fuer Gate M3 qualifiziert
  ([R04](docs/benchmark/gate-m3-r04.md)).

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
