# Changelog

Keep a Changelog-Format, semantische Versionierung. Was „die API" hier
bedeutet, steht in [docs/releases.md](docs/releases.md).

## [Unveroeffentlicht]

### Hinzugefuegt

- **`vig autotune` stellt den Governor ein, statt ihn nur zu beurteilen**
  ([ADR-0045](docs/adr/0045-autotune-tunes-within-the-contracts.md)). Neuer
  Schritt `tune` zwischen `measure` und `fit`: `pipelining_depth` (0 ↔ 1),
  `protect_supply`, `margin_learning` (aus ↔ Voreinstellungen) und
  `safety_margin_percent` (± 15, in den Grenzen 100 bis 300) werden in dieser
  Reihenfolge je einmal umgestellt und mit `vig-fit` bewertet, nur der
  Governor-Arm, bei 100, 110 und 125 % Last. Behalten wird eine Umstellung
  nur jenseits der Rauschschwelle — geschuetzte Stroeme mindestens
  `max(5 ‰, ein Zehntel)` besser, oder bei nicht schlechteren geschuetzten
  die nachrangigen mindestens `max(10 ‰, ein Zehntel)` —, und nie, wenn sie
  fuer die geschuetzten Stroeme schlechter ist als die gemessene Fassung.
  Die beste Fassung steht danach in `measured.yaml`, `fit` urteilt ueber
  sie; die unverstellte bleibt als `tune/untuned.yaml`, jede versuchte als
  `tune/candidate-<n>.yaml`. Der Bericht fuehrt jede Fassung mit
  Entscheidung und Grund (Abschnitt „Tuning", JSON-Feld `tuning`). Vertraege
  und `backend.slots` werden nie angefasst. Unter Fremdlast wird die
  unverstellte Fassung zurueckgeschrieben. Der Schritt kostet in der
  Schaetzung rund vier Minuten; die zugesagte halbe Stunde haelt fuer die
  Referenzgroesse weiter (529 s geschaetzt).
- **`vig-fit` kennt `VIG_FIT_ARMS=governed`**: nur der Governor-Arm, eine
  Bewertung ohne Vergleich. Das JSON traegt `arms` (`both` oder
  `governed`); ohne direkten Arm stehen `direct_uncovered_permille` und
  `direct_longest_gap_ms` auf `null`, und der Satz nennt sich
  Abstimmungslauf. Ohne die Variable bleibt alles, wie es war.
- Ein `state.json` aus der Fassung mit vier Schritten wird weiter gelesen;
  `tune` fehlt darin und laeuft beim Fortsetzen, `fit` und `check` danach
  erneut.

### Geaendert

- **Das JSON von `vig-fit` traegt `foreign_cores_centi_before` und
  `foreign_cores_centi_after` statt `loadavg_before` und `loadavg_after`**,
  dazu `verdict_en` und `conclusive`. Wer die alten Felder ausliest, muss
  umstellen: `loadavg` war auf einem Telefon keine Aussage ueber fremde
  Arbeit. `vig-fit` und `vig autotune` messen jetzt dieselbe Groesse aus
  `vig_platform::cpu`, `null` heisst nicht beobachtbar.

### Sicherheit

- **`rustls` 0.23.43 → 0.23.45 (RUSTSEC-2026-0285).** TLS-1.3-Handshake-
  Nachrichten wurden ueber Grenzen der Verschluesselungsebene hinweg
  angenommen. Der Governor bietet TLS und mTLS am OIP-Endpunkt an und ist
  damit betroffen, sobald `backend.security.tls_cert` und `tls_key` gesetzt
  sind. Gefunden hat
  es `cargo deny` im Gate am 15.09.; die Meldung ist neuer als die
  `Cargo.lock` vom 14.09., keine Aenderung dieses Standes hat sie verursacht.

### Behoben

- **`vig autotune` konnte auf einer installierten Maschine nie vollstaendig
  werden, und auf einem Telefon nie sauber** (Review vom 15.09.).
  - **Fremdlast war die Laenge der Warteschlange, nicht fremde Arbeit.**
    Die feste Grenze fuer `/proc/loadavg` zaehlte Kernel-Threads, die nichts
    rechnen, und die eigene Messung. Ein Pixel 2 steht im Leerlauf bei 3,5
    und verbraucht 0,04 Kerne: Jeder Lauf dort galt als verschmutzt.
    Gemessen wird jetzt die fremde CPU-Zeit aus `/proc/stat` abzueglich der
    eigenen, vor und nach dem Schritt, Grenze ein ganzer Kern. Eine nicht
    beobachtbare Last gilt nicht mehr als ruhig.
  - **`vig-fit` lag in keinem Auslieferungsweg.** Release-Archiv,
    Container-Image und Telefonskript bauten nur `vig`; der Schritt `fit`
    blieb deshalb ueberall offen. Jetzt liegt es daneben.
  - **Ein einmal gescheiterter Schritt verweigerte jeden spaeteren Lauf.**
    Eine Fortsetzung haengte Schritteintraege an, statt sie zu ersetzen, und
    `failed()` sah den alten Fehlschlag weiter. Ausserdem liefen nach einer
    neuen Messung `fit` und `check` nicht nach — ihr Urteil beschrieb eine
    `measured.yaml`, die es nicht mehr gab. Ein Zustand ohne Fingerabdruck
    wird nicht mehr angerechnet.
  - **Ein verweigerter Lauf begann mit einem Urteil zu unseren Gunsten.**
    Der Laptoplauf vom 15.09. stand mit „der Governor 0 ‰" ganz oben und mit
    „Release: refused" ganz unten. Die Verweigerung steht jetzt vorn; ein
    Urteil gegen uns bleibt direkt dahinter.
  - **`fit` las ein altes Ergebnis.** Der Exitcode von `vig-fit` wurde
    ignoriert, eine liegengebliebene `fit.json` galt als dieses Ergebnis, und
    „kein Strom hat geliefert" ging als Urteil durch. `vig-fit` schreibt
    jetzt `conclusive` und ein englisches Urteil, endet ohne Lieferung mit 1,
    und `autotune` nimmt nur ein frisches, schluessiges Ergebnis.
  - **Ungemessene Modelle zaehlten nicht.** Ein Warmlauf, der scheiterte,
    und ein Modell ohne Metadaten oder Nulltensor verschwanden ohne Spur; der
    Bericht sagte „N von N verwertbar". Beides zaehlt jetzt und verweigert die
    Freigabe.
  - **Die Belegungsstufe mass eine Warteschlange** — der Grund, warum
    `autotune` auf dem Pixel 2 nicht dasselbe ergab wie die von Hand
    getunte Konfiguration. Der TFLite-Server rechnet zwei Auftraege
    desselben Modells nacheinander; die Stufe (Detektor 1,87x, Tiefe 2,05x)
    wurde trotzdem als Laufzeit bei belegtem Nachbarn festgeschrieben, und der
    Governor plante den Detektor neben jedem Modell mit 297 statt 158 ms.
    Ab `(belegt + 1) × 90 %` gilt eine Stufe jetzt als Warteschlange und wird
    nicht geschrieben. Eine verworfene Stufe laesst die folgenden nicht mehr
    eine Stelle nachruecken, und keine Stufe ist schneller als allein.
- **Ein Text-Tensor mit falscher Laengenangabe wurde stillschweigend
  repariert** (`read_length_prefixed`): zu lang angegeben wurde gekuerzt,
  zu kurz oder mit zwei Elementen gingen Elemente verloren. Jetzt wird nur
  ein exaktes Einzelelement zerlegt, alles andere geht unveraendert an das
  Backend, das den Fehler meldet. Die vier auseinandergelaufenen Schreiber
  des Laengenpraefixes sind eine Funktion in `vig-protocol-oip`.
- **`after_a_restart_new_work_is_proven_by_the_new_counter` scheiterte in
  zwei bis drei von zehn Laeufen.** Kein Fehler im Abgleich, sondern ein
  Test aelter als R04: Las der Poller den Zaehler erst nach dem Reset, sah
  der Actor genau den Ablauf, bei dem ADR-0042 die Sperre verlangt. Die
  Tests warten jetzt auf eine Zaehlermeldung nach dem Abbruch; 10 von 10.

- **Vier Befunde aus dem Review von `vig autotune`**, alle in derselben
  Familie: Das Werkzeug verlor oder verwischte Angaben, die es laut ADR-0044
  nennen muss.
  - **Eine Fortsetzung kuerzte den Bericht.** Nach einem Abbruch hinter
    `measure` begann der naechste Aufruf mit einer leeren Qualifikation und
    ueberschrieb `qualification.json`, `.md` und `state.json` damit. Der
    fertige Bericht nannte danach weder die verworfenen Reihen noch die
    eingefrorene Konfiguration (`"config": null`), obwohl beides auf der
    Platte lag; ein zweiter Abbruch haette die ganze Messung wiederholt. Der
    Zustand traegt jetzt den vollstaendigen Stand, und ein Test faehrt den
    Fall ueber zwei Aufrufe.
  - **Ein Stand von woanders galt weiter.** `state.json` nannte nur
    Schrittnamen. Wer zwischen zwei Aufrufen den Endpunkt wechselte oder die
    Konfiguration aenderte, bekam die alten Schritte angerechnet — `fit` und
    `check` liefen dann gegen eine Messung von einer anderen Maschine. Ein
    Fingerabdruck aus Endpunkt und Konfigurations-Hash verwirft solche
    Staende und sagt es.
  - **Eine verworfene Interferenzmessung sah aus wie „gemessen und
    unkritisch".** `vig calibrate` leerte die gerichtete Tabelle
    bedingungslos und schrieb nur die diesmal gelungenen Paare zurueck. Ein
    frueher gemessener Aufschlag verschwand damit spurlos — und ein Lauf mit
    `slots: 1`, der gar keine Paare misst, leerte die Tabelle ebenfalls.
    Entfernt wird jetzt nur, was dieser Lauf neu setzt, serialisiert oder
    verwerfen musste; die verworfenen Paare werden benannt.
  - **Die Dauerzusage konnte sich nicht selbst pruefen.** Die Schaetzung
    bestand aus vier festen Konstanten (Summe 1065 s) und lag damit immer
    unter der Schwelle von 1800 s: Der Hinweis „laenger als die zugesagte
    halbe Stunde, nutze `--quick`" war unerreichbar, und der zugehoerige
    Test verglich eine Konstantensumme mit einer Konstanten. Die Schaetzung
    haengt jetzt an Modellzahl, Slots und `--samples` — die Paarmessung
    waechst quadratisch —, die Ansage nennt die Matrix, auf der sie beruht,
    und sagt dazu, dass sie die Hardware **nicht** kennt. Ein Test belegt,
    dass die Warnung ausloest.

### Hinzugefuegt

- **`vig autotune` qualifiziert die eigene Hardware — ohne uns.** Ein Befehl
  fuehrt die vorhandenen Schritte zusammen: Modelle am Backend lesen
  (`init`), Laufzeiten, Nebenlaeufigkeit und gerichtete Interferenz messen
  (`calibrate`), „lohnt es sich hier?" beantworten (`vig-fit`) und die
  entstandene Konfiguration pruefen (`doctor`). Heraus kommen eine
  eingefrorene Konfiguration, ein Bericht als Markdown **und** JSON und ein
  Manifest nach ADR-0019. Der Lauf schaetzt seine Dauer vorab, schreibt nach
  jedem Schritt auf die Platte und nimmt nach einem Abbruch wieder auf;
  `--quick` und `--only` verkleinern den Umfang.
  **Was der Befehl nicht kann, ist eine Qualifikation zu erteilen**
  (ADR-0044): Er kann sie nur verweigern oder offenlassen. Eine verworfene
  Messreihe bleibt verworfen und die Groesse, die sie ergeben haette,
  ungesetzt; ein Schritt unter Fremdlast gilt als verschmutzt; und ein Urteil
  gegen uns — „der Governor bringt hier nichts" — ist die Schlagzeile des
  Berichts, nicht seine Fussnote. Ohne `nvidia-smi`, etwa auf ARM vor dem
  TFLite-Backend, scheitert nichts und wird nichts stillschweigend ersetzt:
  der Bericht fuehrt „Takt nicht beobachtbar" als eigenen Abschnitt.
- **Ein Reproduktionspaket mit oeffentlichen Modellen** (`tools/repro/`,
  [Anleitung](docs/benchmark/reproduce.md)). Bisher stammte jede
  veroeffentlichte Zahl von Modellen, die niemand ausserhalb hat: einem
  internen Detektionsmodell und ResNet-Stellvertretern mit passenden Formen
  und Laufzeiten. Fuer die Scheduling-Aussage ist das zulaessig — der
  Scheduler sieht Laufzeiten, keine Gewichte —, zum Nachpruefen taugte es
  nicht. `tools/repro/run.sh` faehrt denselben Vergleich mit RT-DETR R18/R50
  und Qwen3-0.6B, alle Apache-2.0; `fetch-models.sh` prueft jede Datei gegen
  eine fest eingetragene SHA-256 und **bricht bei Abweichung ab**, statt gegen
  unbekannte Gewichte zu messen. Die Laufzeitprofile werden dabei auf der
  Maschine des Anwenders gemessen und nicht von uns uebernommen — ein Profil
  gilt nur fuer die Hardware, auf der es entstand (Spec 13.5). Dokumentiert
  sind auch die zwei Stolpersteine, die der Aufbau gekostet hat: Triton laedt
  **gar kein** Modell, wenn die Ausgabedimensionen fest statt dynamisch
  stehen, und die Klassenausgabe von RT-DETR traegt Logits und keine
  Wahrscheinlichkeiten (nachgemessen: −9,8 bis −3,6).

  **Das Ergebnis ist unbequem und steht vollstaendig in der Anleitung:** In
  zwei der drei Lastfaelle ist der direkte Weg besser, der dritte liess sich
  auf der Messkarte nicht kalibrieren. Der Grund ist kein Fehler, sondern der
  Betriebspunkt — die oeffentlichen Modelle landen bei 47 % und 91 %
  geschuetzter Auslastung, waehrend Gate M3 bei 103 % misst und
  [`load-ramp.md`](docs/benchmark/load-ramp.md) den Knick zwischen 100 und
  110 % verortet. Belegt ist damit, dass die Werkzeuge mit fremden Modellen
  laufen und ihre eigene Grenze zuverlaessig anzeigen; die Kernaussage im
  Ueberlastbereich ist mit oeffentlichen Modellen **noch offen**, und die
  Anleitung benennt die Luecke.
- **`wp26` laesst sich auf andere Modelle richten.** Endpunkte, Backend-
  Modellnamen und das Detektorprofil kommen jetzt aus Umgebungsvariablen
  (`VIG_WP26_*`) statt fest aus dem Quelltext; die Vorgaben sind unveraendert,
  damit die frueheren Messungen mit demselben Aufruf reproduzierbar bleiben.
  Ohne das braeuchte das Reproduktionspaket eine zweite Kopie desselben
  Vergleichs — also eine zweite Stelle, an der er auseinanderlaufen kann.
- **`vig init` schreibt die erste Konfiguration aus einem laufenden Backend.**
  Modellnamen, Tensorformen und Datentypen liest das Werkzeug ueber OIP
  (`repository_index` und die Modellmetadaten) und traegt sie ein. Alles, was
  nur der Betreiber weiss — Periode, Frist, Hoechstalter, Wichtigkeitsklasse —
  bleibt als `TODO_`-Platzhalter stehen, jeder mit einem Satz, was die Groesse
  bedeutet. **Die erzeugte Datei laedt absichtlich nicht**, solange ein
  Platzhalter offen ist: eine Vorlage mit erfundenen Perioden startet und
  faellt erst im Betrieb auf, und in einer Konfigurationsdatei sieht eine
  erfundene Zahl aus wie eine gemessene. Bisher war die erste `vig.yaml` die
  eigentliche Einstiegshuerde — nicht das Protokoll, das ist kompatibel.
- **`vig-fit` beantwortet „lohnt sich der Governor hier?"** in rund zwei
  Minuten, mit den Modellen, Vertraegen und der Hardware des Interessenten.
  Es faehrt je Lastpunkt (90, 100, 110, 125 %) erst direkt gegen das Backend
  und dann ueber den Governor und druckt die Verbrauchersicht: unabgedeckte
  Abtastungen je Promille und laengste Luecke, fuer den geschuetzten Strom und
  fuer das, was die nachrangigen dafuer zahlen. Das Urteil steht in einem Satz
  — und **„brauchst du nicht" ist eines seiner normalen Ergebnisse**:
  unterhalb der Saettigung kostet der Governor nur seinen Aufwand, und das
  sagt das Werkzeug, statt eine beeindruckende Zahl zu erzeugen. Ist der
  Governor nicht besser, steht auch das so da. Zusaetzlich als JSON
  (`VIG_FIT_JSON`), damit sich das Ergebnis mailen oder in CI haengen laesst.
  Es liegt in `vig-bench` und nicht in der CLI: es braucht den Lastgenerator,
  den Coverage-Tracker und einen Governor im Prozess — also den Messbaukasten,
  nicht das Produktbinary.

- **Der Look-ahead schuetzt die Versorgung** (opt-in,
  [ADR-0041](docs/adr/0041-the-look-ahead-protects-the-supply-not-only-the-deadline.md)).
  `backend.protect_supply: true` gibt jeder erwarteten geschuetzten Ankunft
  eine zweite Frist: den Ablauf des letzten brauchbaren Ergebnisses
  (`Aufnahme + max_age`). Hintergrundarbeit wird dann auch dann
  zurueckgehalten, wenn der geschuetzte Frame seine Deadline noch haelt, der
  Verbraucher dazwischen aber eine Luecke haette (ADR-0005). Ausloeser war die
  Rampe vom 12.09.: mit gelernter Marge verfehlte der Detektor bei 125 % Last
  205 ‰ der Perioden ohne eine einzige verletzte Deadline — die feste Marge
  hatte ihn durch Pessimismus geschuetzt, nicht durch Planung. **Der Preis
  ist hart:** im Simulator schliesst der Schutz die Luecke (13 → 0 ‰ der
  Abtastungen), und der Hintergrund bekommt bei 25-ms-Auftraegen gar nichts
  mehr (333 → 0 versorgte Fenster, Vetos 2 → 7 980); bei 20 ms zahlt er 9 %,
  obwohl es dort nichts zu retten gab. Ob der Verlust unter
  `minimum_background_progress_pct` gedeckelt werden muss, entscheidet die
  Messung auf der GPU — bis dahin bleibt es opt-in, und ohne die Zeile
  aendert sich nichts.
- **Die Planung kalibriert sich an der Karte** (opt-in,
  [ADR-0038](docs/adr/0038-the-plan-calibrates-to-the-card.md)).
  `backend.margin_learning: {}` laesst den Governor je GPU einen Faktor
  zwischen Profil-p99 und gemessener Laufzeit lernen: derselbe
  Quantilschaetzer wie der Margenregler (ein Prozent Ueberziehungen), aber
  multiplikativ, als Geraetefaktor mal Rest je Modell, und ohne den Boden der
  konfigurierten Marge — er darf unter 100 % fallen, wenn das Profil
  pessimistisch ist, nie unter den beobachteten Median und nie unter
  `min_factor_percent` (50 %). Anlass war der Verlust an der Kante (bei
  100 % Last verwarf Vigilant 165 ‰ eines `high`-Stroms); die Simulation
  derselben Last zeigt aber, dass die Marge dort nichts entscheidet — die
  Kalibrierung macht das Ergebnis unabhaengig vom Profilfehler, sie loest
  nicht die Kante (ADR-0038, Abschnitt Simulation). Nach ADR-0036 hungert
  ein zu pessimistisches Profil mit fester Marge alle anderen Stroeme aus;
  gelernt bekommen sie Arbeit zurueck. Ohne den Block bitgleich; zusammen mit
  `prediction: active` abgelehnt. Neue Kennzahl
  `vig_learned_device_factor_percent`; `vig doctor` nennt den Modus.
  `load-ramp` bekommt dafuer `VIG_RAMP_MARGIN`, `VIG_RAMP_PREDICTION`,
  `VIG_RAMP_MARGIN_LEARNING`, `VIG_RAMP_PROFILE_SCALE`, `VIG_RAMP_POINTS`
  und `VIG_RAMP_PIPELINING`. Auf Hardware noch nicht gemessen.
- **Ein zweites Backend: TFLite auf einer Android-GPU** (NV-25,
  [ADR-0039](docs/adr/0039-a-second-backend-proves-the-seam.md)).
  `backends/android-tflite` ist ein eigener Prozess und Workspace, der das
  OIP-Subset des Governors spricht (Live, Ready, Metadaten, Inferenz,
  Statistik mit Abschlussnachweis) und TFLite 2.16.1 mit GPU-Delegate V2
  ueber GLES laedt; `unsafe` bleibt ausserhalb des Root-Workspace. Nur
  Kopierpfad. Gemessen auf der Adreno 540 eines Pixel 2, Governor und Last
  auf dem Telefon ([android-gpu.md](docs/benchmark/android-gpu.md)).
  `gate-m3` hat dafuer `VIG_GATE_COPY`, `VIG_GATE_NO_HARDWARE` und
  `VIG_GATE_SECONDS`; `tools/android/` beschafft, baut und misst.
- **Der zweite Betriebspunkt, auf der Telefon-GPU gemessen** (Paket „zweiter
  Betriebspunkt", ADR-0004, ADR-0026). `vig calibrate` laeuft jetzt auch
  gegen das TFLite-Backend (`tools/android/gate-on-phone.sh
  STEPS=calibrate`) und misst die gerichtete Interferenz: `pose` leidet
  2,64x neben `depth`, umgekehrt nur 1,25x; zwei Richtungen sind unter Last
  sogar schneller, weil der SoC hochtaktet. Mit `slots: 2` und dieser
  Tabelle (`examples/android_gpu/vig-slots2*.yaml`) haelt der Governor ueber
  Last alle drei Stroeme bei voller Abdeckung und liefert `pose` mehr
  Fenster als das Backend direkt; der geschuetzte Detektor gibt dafuer
  seinen Lueckenvorsprung ab ([Messung](docs/benchmark/android-gpu.md)).
- **`gate-m3` kann echte Bilder schicken** statt Nullen: `VIG_GATE_FRAMES`
  (RGB24, quadratisch, `VIG_GATE_FRAME_SIZE`, `VIG_GATE_FRAME_COUNT`), auf
  dem Kopierpfad reihum nach Frame-Nummer. Fuer ein Faltungsnetz ist der
  Inhalt gleichgueltig, fuer eine Nachbearbeitung im Graphen nicht: auf
  Nullen findet ein Detektor nichts, und seine NMS sortiert nichts.
- **Mehrere Ressourcendomaenen** (NV-22,
  [ADR-0037](docs/adr/0037-a-domain-is-a-gpu-with-one-owner.md)).
  `backend.domains.<name>` beschreibt eine weitere GPU mit `gpu_index`,
  `grpc_endpoint`, `slots` und, wo noetig, `pipelining_depth`, `no_corun`,
  `preemptible_lanes` und `interference`; `domain:` am Modell ordnet es fest
  zu. Je Domaene ein eigener Scheduler mit eigenen Krediten, eigener
  Quarantaene, eigenem Look-ahead und Margenregler; geteilt bleiben
  Nutzlastbudget, Zugang, Hinweise und Timeouts. Die Konfiguration lehnt
  Doppelbesitz ab (eine GPU oder ein Endpunkt in zwei Domaenen, Paare ueber
  Domaenengrenzen). `/metrics` behaelt die Gesamtreihen und ergaenzt
  `vig_domain_*{domain}`; `/readyz` nennt die Domaene, die nicht bereit ist;
  `vig doctor` prueft Auslastung, Spuren und Best-Effort-Machbarkeit je GPU.
  Ohne `domains:` aendert sich nichts. Erreichbar, nicht qualifiziert: auf
  der Messmaschine gibt es eine GPU.
- **Metadaten, Bereitschaft und Konfiguration eines Modells** fragt das
  Gateway bei dem Server, der es rechnet — vorher immer bei
  `backend.grpc_endpoint`, auch fuer Modelle mit eigenem `backend_endpoint`.

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

### Sicherheit — Review vom 11.09. ([docs/security.md](docs/security.md))

Bedrohungsmodell, Checkliste fuer den sicheren Betrieb und die Tabelle
Befund → Fix → Test stehen in docs/security.md. Die hohen Befunde:

- **H1: Der Quickstart stellte Triton am Governor vorbei ins Netz.** Triton
  hat in der Compose-Datei keinen veroeffentlichten Port mehr; `vig`
  veroeffentlicht 9001 und 9090 nur auf `127.0.0.1`, laeuft schreibgeschuetzt,
  ohne Capabilities und mit `no-new-privileges`. `.dockerignore` haelt Token,
  Schluessel und lokale Konfigurationen aus dem Build-Kontext.
- **H2: Shared-Memory-Schluessel liefen ungeprueft durch.** Jede Registrierung
  ueber den Governor prueft Praefix, Ausdehnung und Besitz; unter
  `trust: strict` nennt eine Inferenz nur eigene Regionen.
- **H3: Die Administrationsendpunkte standen jedem Tokeninhaber offen.** Sie
  sind in jedem Modus gesperrt, bis `backend.security.admin_token_file`
  gesetzt ist.

Dazu M1–M5 und N1–N7: Tokenpruefung vor dem Dekodieren, Transportgrenzen,
durchgereichte Modelle im Nutzlastbudget, gepinnte Actions und Basisimages,
Tokennamen statt Hashes im Log, Kennungen im Abhaengigkeitsgraph je
Aufrufer, gedrosselte Zeitstempelwarnung, Obergrenze fuer Hinweis-TTL,
Verbindungsgrenze am Metrikport.

### Geaendert — bricht bestehende Konfigurationen

- **`vig serve` verweigert eine Adresse ausserhalb von Loopback**, solange
  weder mTLS (`client_ca`) noch Token (`token_file`) den Aufrufer pruefen.
  TLS allein genuegt nicht. Der ausdrueckliche Weg vorbei ist
  `--insecure-open`, gedacht fuer einen Containerport, der auf dem Loopback
  des Hosts veroeffentlicht ist.
- **Modelle laden und entladen, Trace, Log und CUDA-Speicher** antworten
  `PERMISSION_DENIED`, bis `backend.security.admin_token_file` gesetzt ist.
- **Token brauchen mindestens 16 Zeichen.** Anwendungshinweise darf nur ein
  benanntes Token senden (`robot:<token>`); die Hinweiskennung kommt aus dem
  Namen.
- **Shared-Memory-Schluessel muessen mit `backend.security.shm_key_prefix`
  beginnen** (Voreinstellung `/vig_`); hoechstens `max_shm_regions` (256)
  Regionen. Alle Regionen abmelden (leerer Name) braucht ein
  Administrationstoken.
- **Durchgereichte Modelle zaehlen gegen das Nutzlastbudget.**
- **Hinweise laenger als `hints.max_ttl_ms`** (Voreinstellung 60 s) werden
  verworfen.
- **`vig doctor`** meldet TLS ohne Identitaetspruefung als Warnung und
  `trust: strict` ohne Identitaetspruefung als nicht bereit.
- **Die Compose-Datei veroeffentlicht Triton nicht mehr.** Wer Triton vom
  Host aus direkt angesprochen hat (Benchmarks), braucht dafuer eine eigene
  Portfreigabe.

### Behoben

- **Die erste Messreihe traf eine kalte Karte.** `vig calibrate` verwarf je
  Reihe zwanzig Aufrufe — genug gegen einen kalten Cache, nichts gegen einen
  kalten Takt. Auf dem RTX-3070-Laptop lief der SM-Takt waehrend der ersten
  Reihe von 1500 auf 1800 MHz, und die Reihe wurde deshalb verworfen; auf dem
  Telefon gibt es keinen Taktmesser, der das haette auffangen koennen, und
  eine Detektor-Grundlinie lag um 31,6 % daneben. Vor der ersten Reihe faehrt
  der Kalibrator die Karte jetzt unter Dauerlast warm und wartet, **bis der
  Takt steht**; wo der Takt lesbar ist, belegt er das mit dem erreichten
  Wert. Wo er nicht lesbar ist — ARM, TFLite —, waermt er eine feste Zeit und
  sagt ausdruecklich, dass er nichts belegen kann: „Vorlauf: 20 s gefahren,
  aber nicht belegt". Ein Vorlauf ohne Nachweis ist besser als eine kalte
  erste Reihe, aber er wird nicht als Nachweis ausgegeben. Betrifft
  `vig calibrate` und damit auch `vig autotune`.
- **Der Qualifikationsbericht zeigte Rust-Innereien statt des Geraets.**
  Geraetename und Treiber standen als `Observed(Sample { value: "...",
  source: NvidiaSmi, observed_at_ms: ... })` im Manifest. In den Bericht
  schaut ein Interessent; jetzt steht dort der Wert oder `not observable`.
- **Ein Pilotlauf ohne auswertbares Kriterium sah aus wie ein bestandener**
  (Review 14.09., R05). `RunStatus::NoVerdict` lieferte Exitcode 0. Jetzt
  liefert eine Teilmatrix **3**, und neben dem Status steht ein
  Freigabeurteil aus fuenf getrennten Feldern — Ablauf, Planung, Anwendung,
  Qualifikation, Freigabe — in der Ausgabe und in `summary.json` unter
  `freigabe`. `qualifikation` lautet nie „bestaetigt": ein Messlauf stellt
  ueber seine eigene Hardware nichts fest, also kann dieses Binary eine
  Freigabe nur verweigern und begruenden. Dazu gilt **P1** nur noch, solange
  der Vergleichsarm mindestens 500 Promille der Perioden versorgt — ein Arm
  mit 0 Promille Abdeckung ist kein Massstab fuer Latenz
  ([edge-pilot.md](docs/pilot/edge-pilot.md)).
- **Das Basisalter eines Lageberichts nannte die juengste Kamera**
  (Review 14.09., R06). Der Prompt enthaelt die Detektionen aller Kameras; aus
  einer 1 s und einer 10 s alten Quelle wurde ein Bericht mit „1 s". Gemeldet
  wird jetzt die **aelteste** verwendete Quelle, und `reports_missing_source`
  zaehlt, wie oft eine Kamera gar nichts beisteuerte.
- **Ein Messarm endete, waehrend seine Inferenzen noch liefen** (Review 14.09.,
  R03, [ADR-0042](docs/adr/0042-an-end-is-proven-not-assumed.md)). `run_arm`
  wartete auf seine Kameraschleifen und schlief danach pauschal 500 ms; die
  Aufrufe selbst liefen frei weiter, und die naechste Messzelle mass neben
  ihnen. Jetzt liegen sie in einem `JoinSet` und werden eingesammelt; bleibt
  nach 60 s einer offen, ist die Zelle ungueltig statt stillschweigend
  weiterzulaufen. Ausserdem gehoert die Quarantaene einer Shared-Memory-Region
  jetzt der **Region** und gilt prozessweit: Der naechste Arm bekommt einen
  gesperrten Puffer nicht zurueck.
- **Ein spaet erkannter Backend-Neustart gab neue Arbeit frei** (Review 14.09.,
  R04, ADR-0042). Fiel der Abschlusszaehler, endeten alle abgebrochenen
  Aufrufe — auch einer, der erst nach dem Neustart in den neuen Prozess ging
  und dort weiterrechnen kann. Beendet wird jetzt nur, was das Backend in
  dieser Epoche nachweislich gezaehlt hat (`seen_in_epoch`); alles andere
  bleibt gesperrt und in `vig_quarantined_slots` sichtbar.

- **Nach einem Backend-Neustart konnte neue Arbeit zu frueh freigegeben
  werden** (Review 11.09., R01, [ADR-0040](docs/adr/0040-a-restart-proves-only-what-died-with-it.md)).
  Der Abgleichs-Poller merkte sich den hoechsten Zaehlerstand selbst und
  nannte jeden niedrigeren einen Neustart; nach 100 → 0 → 0 gab auch die
  dritte Meldung jeden gehaltenen Kredit frei, auch den eines Aufrufs, der
  erst nach dem Reset entstanden war und im neuen Prozess rechnete. Jetzt
  entscheidet der Actor je Epoche: ein Neustart beendet nur Anspruechen,
  deren Verbindung schon abgebrochen ist; Aufrufe mit offener Verbindung
  wandern in die neue Epoche, die beim gemeldeten Stand beginnt. Je
  Identitaet laeuft hoechstens ein Poller, und er endet mit dem Nachweis —
  vorher startete jeder Abbruch einen eigenen, der nie endete.
- **Der Abgleich verwechselte Server und Versionen** (Review 11.09., R02).
  Basislinie und Auslieferungssumme hingen am Modellnamen, und der Abgleich
  fragte den ersten Server mit diesem Namen — auch fuer einen Aufruf an einen
  zweiten Server derselben GPU. Jetzt ist der Schluessel Server plus Modell.
  Die Statistik zaehlt ueber alle Versionen statt nur ueber den ersten
  Eintrag. `vig_reconcile_baseline_missing` zaehlt eindeutige Identitaeten:
  zwei Kameras auf dasselbe Modell erschienen vorher dauerhaft als eine
  fehlende Basislinie.
- **Die Variantenwahl hielt die Deadline, aber nicht die Versorgung.** Sie
  nahm die beste Variante, deren Fertigstellung vor der Deadline des Frames
  lag. Mit `deadline = 1,5 P` und `max_age = 2 P` laeuft das vorige Ergebnis
  aber schon `P` nach der Aufnahme ab; eine grosse Variante mit Laufzeit ueber
  der Periode hielt jede Deadline und liess trotzdem Luecken. In `frontier`
  verfehlte die automatische Wahl deshalb bei 110 und 125 % Last 66 bzw.
  84 ‰, die kleine Variante allein keine; der Simulator trifft beide Zahlen.
  Jetzt gewinnt die beste Variante, die vor `Aufnahme + max_age - P` fertig
  wird, und erst wenn keine das schafft, die beste, die ihre Deadline haelt.
  Ohne Periode oder ohne `max_age` ueber der Periode aendert sich nichts
  ([Analyse](docs/analysis/bursts-and-frontier.md)).
- **Der Abhaengigkeitsgraph lief nach 256 Aufnahmen voll** (NV-17). Kein
  Pfad setzte einen Knoten je auf einen Endzustand; danach lehnte das Gateway
  jede Anfrage mit `vig_capture_id` ab. Jedes Ende — Fertigstellung,
  Verwerfen, Ablehnung, Abbruch, Backendfehler, Drain — schliesst jetzt
  seinen Knoten; ein fertiges Ergebnis haelt fuer das Hoechstalter seines
  Modells (100 ms bis 5 s), unter Druck gehen die aeltesten zuerst.
  Ablehnungen tragen `vig-reason` (`graph_full`, `capture_mismatch`, ...).
  Gefunden live von der ROS-2-Bruecke.
- **Gueltige Samplingparameter wurden zu ungueltigem JSON** (Review R05).
  Beim Zuschneiden eines generativen Auftrags ersetzte eine Textsuche
  `max_tokens`; stand es nicht vorne, entstand
  `{"max_tokens": 8, "temperature":0.7,}`. Die Parameter werden jetzt als
  JSON gelesen und geschrieben (`serde_json`, ohnehin im Baum); nur das
  oberste `max_tokens` zaehlt, ein verschachteltes oder eines in einem String
  nicht mehr. Samplingparameter, die kein JSON-Objekt sind, werden nicht
  repariert: der Auftrag laeuft ungeteilt.
- **Der Pilot konnte einen Bildpuffer ueberschreiben, den noch jemand las**
  (Review R03). Der Puffer war `Folgenummer % Anzahl`; die Semaphore
  begrenzte nur die Zahl offener Auftraege. Jetzt ein Pool konkreter freier
  Puffer (`pilot::RegionPool`): die Puffer-ID reist mit dem Auftrag, frei wird
  er erst bei belegtem Ende, nach Timeout oder unbekanntem Ausgang nie
  wieder. Neue Zaehler je Kamera: `buffers_exhausted`, `buffers_quarantined`.
- **Die ROS-Bruecke gab ein Shared-Memory-Fach nach einem Timeout frei**
  (Review R04), obwohl das Backend weiter lesen konnte. Nur ein belegtes Ende
  gibt es jetzt frei; sonst Quarantaene, und wenn alle Faecher gesperrt sind,
  eine neue Region (`shm_max_epochs`).
- **Der volle Pilotlauf passte nicht in seine Frist** (Review R07, R08). Die
  Standardmatrix braucht rund 80 min; das Werkzeug druckt die Schaetzung jetzt
  vorab, schreibt jede Zelle sofort nach `cells.jsonl`, setzt nach einem
  Abbruch fort und endet mit Exitcode 0 (bestanden), 1 (sauber gelaufen,
  Kriterium verfehlt) oder 2 (Aufbau oder Lauf kaputt), dazu `summary.json`.
- **Die Messwerkzeuge liefen ohne `TCP_NODELAY`.** Jedes Werkzeug, das einen
  Governor oder ein Mock-Backend im Prozess startet, band ueber
  `serve_with_incoming`, und dort uebergeht tonic die Einstellung. Nagle und
  das verzoegerte ACK erzeugten gelegentlich 40 ms auf der Governor-Seite:
  zwei von drei Laeufen der Datenpfadbudgets rissen ihr p99. `vig serve` war
  nie betroffen. Alle Werkzeuge nehmen jetzt `vig_bench::incoming`
  ([arm-serve.md](docs/benchmark/arm-serve.md)).
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

- **Der Pilot urteilt getrennt ueber Planung und Anwendung.** Die Kriterien
  vom 11.09. massen zwei Dinge in einem Urteil: was der Governor entscheidet,
  und was Detektor, Datensatz und Bildrate hergeben. Der erste vollstaendige
  Lauf verfehlte K1 und K4 um ein Vielfaches — und die Referenz ohne jede
  Konkurrenz ebenso (347 ms statt 300, 213 von 1000 Objekten). Jetzt tragen
  P1-P6 (gegen den Arm "Backend direkt", darunter die neuen P3 und P4 fuer
  Trefferquote und Versorgung) allein den Exitcode; A1-A3 gelten zuerst fuer
  die Referenz und heissen sonst "nicht anwendbar: Erkennungsqualitaet".
  Beide Gruppen stehen getrennt im Bericht und in `summary.json`.
- **`edge-pilot --preemptible` und `--residual-us`** legen den Berichtspfad
  auf eine praemptierbare Lane (ADR-0035). Gemessen am 12.09.: Das
  vLLM-Backend laedt unter dem XSched-Shim nicht, eine nur *angegebene* Lane
  bringt dem Bericht 30 statt 2 Lageberichte je Minute und kostet den Alarm
  28 % p95; der Shim selbst kostet am geschuetzten Pfad 17-20 % Laufzeit
  ([Messung](docs/benchmark/pilot-praemption-2026-09-12.md)).
- **Die Gate-M3-Beispielkonfiguration faehrt mit `pipelining_depth: 1`.** Ohne
  Pipelining laeuft die GPU zwischen zwei Auftraegen leer; bei genau 100 %
  Last kostete das 188 Promille Abdeckung des nachrangigen Stroms, mit
  Pipelining sind es 17, zusammen mit dem Versorgungsschutz 4. In Gate M3
  aendern sich die Zahlen dadurch nicht (drei Laeufe,
  [Abnahme](docs/benchmark/abnahme-2026-09-12.md)); die Berichte R03 und R04
  bleiben gueltig.

- **`frontier` und `load-ramp` zeigen die Verbrauchersicht** neben den
  Lieferfenstern. Die Fenstersicht kippt, wenn die Laufzeit an die Periode
  heranreicht: unter Lastspitzen verfehlte sie im Simulator fuer Governor und
  FIFO gleichermassen 6–10 % der Detektorfenster, obwohl der Verbraucher nie
  ohne frisches Ergebnis war, und unter Saettigung meldete sie 7 %, wo 53 %
  der Abtastungen kein brauchbares Ergebnis hatten. Die bisherigen Zahlen
  bleiben in der ersten Tabelle vergleichbar. Dazu `Coverage::
  consumer_uncovered_permille` und `harness::run_captures` fuer
  Ankunftsprozesse ohne festen Takt
  ([Analyse](docs/analysis/bursts-and-frontier.md)).
- **Der Look-ahead rechnet ab der Aufnahme und sieht jede naechste Ankunft**
  ([ADR-0036](docs/adr/0036-the-look-ahead-counts-from-the-capture.md)),
  aus den drei Befunden von NV-23:
  - Die Frist einer erwarteten geschuetzten Ankunft zaehlt ab ihrer
    erwarteten **Aufnahme**, wie die Deadline, die der Dispatch rechnet. Die
    Altersschranke von NV-23 wird damit `2J + D` statt `δ + 2J + D`.
  - Eine ueberfaellige Ankunft wird nicht mehr aufgegeben, solange sie
    innerhalb der Jitterhuelle des Vertrags (`release_jitter_ms`) noch kommen
    kann: ihre Frist wandert mit, hoechstens um die doppelte Huelle. Fuer ein
    bewachtes Modell ist die Huelle damit keine unerfuellte Forderung mehr
    (ADR-0032); Start und `vig doctor` nennen sie als genutzt. Ohne Huelle
    bleibt es beim bisherigen Verhalten.
  - Kein fester Horizont von 100 ms mehr: die naechste Ankunft jedes
    bewachten Stroms zaehlt, gleich wie weit sie weg ist. Ein 5-Hz-Strom ist
    gegen lange Hintergrundarbeit jetzt geschuetzt.
  - Rust-API: `guard_protected` ohne `horizon`, `Scheduler::set_horizon` und
    `feasibility::DEFAULT_HORIZON` entfallen,
    `ContractExtension::unenforced(guarded)`. Konfiguration, OIP und
    Kennzahlen unveraendert.
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
