# Erneute Prüfung nach den Änderungen vom 16. September 2026

## Ergebnis und Prüfumfang

**Fünf Fehlergruppen repariert, acht Regressionstests ergänzt.** Sieben der
neuen Gegenproben wurden vor der jeweiligen Reparatur ausgeführt und scheiterten.
Der achte Test sichert den Erhalt gültiger Tensorformen ab.

Basis: `6e554ff`, insbesondere die neue Zulassung kooperativer LLM-Aufträge
aus `1d4ed37`, das neue Beispiel und ihre Wechselwirkung mit dem Gateway.
Die früheren Reparaturen an Ergebniszielen, SHM und Vergleichsläufen stehen
im [vorherigen Bericht](REVIEW.md). Dieser Bericht ergänzt ihn.

Im gemeinsamen Arbeitsbaum liefen parallel Änderungen an `docs/STATUS.md`,
`docs/support-matrix.md`, `docs/use-cases.md`, `site/index.md` und
`examples/cooperative_llm/vig.yaml`.
Diese Änderungen wurden erhalten. Es wurde kein Commit oder Deployment
durch diese Prüfung ausgeführt.

## Reparierte Befunde

### R10 · P1 · Sofortige Quarantäne-Ablehnung verlor das Aufnahmebudget

**Stelle:** `crates/vig-gateway/src/actor.rs`, `Actor::accept`.

Die Funktion legte den `PayloadPermit` bereits in `self.permits` ab, bevor
sie vollständig gesperrte Slots prüfte. Bei der sofortigen Ablehnung gab es
keinen Schedulerabschluss, der den Eintrag wieder entfernte. Der Kommentar,
der Guard werde beim Rücksprung automatisch freigegeben, traf auf einen
bereits in der Map gespeicherten Guard nicht zu.

**Gegenprobe:** 1 MiB Aufnahmebudget, ein hängender Backendaufruf, danach
wiederholte sofort abgewiesene Requests mit jeweils 512 KiB Nutzlast.
Bereits der dritte Versuch erhielt `ResourceExhausted` statt `Unavailable`.
Auch nach Backend-Erholung wären diese Reservierungen liegen geblieben.

**Reparatur:** Die Map übernimmt den Guard erst nach allen unmittelbaren
Ablehnungspfaden. Reservierungen tatsächlich gestarteter Arbeit bleiben bei
unklarem Ausführungsende weiterhin bestehen.

**Test:** `quarantine_rejections_release_the_payload_reservation`.

### R11 · P1 · Zusammengesetzte Textantwort beschädigte das OIP-Format

**Stelle:** `crates/vig-gateway/src/cooperative.rs`, `build_response`.

Der Code klonte sämtliche Ausgabemetadaten, ersetzte aber alle Rohdaten
durch genau einen Textpuffer. Bei beispielsweise `[score, text_output]`
blieben zwei Ausgabenamen und nur ein Rohdatenpuffer übrig; der Text wurde
dem numerischen Score zugeordnet.

**Gegenprobe:** Zwei Ausgaben, Text an zweiter Position. Beobachtet wurde
ein Rohdatenpuffer statt zwei.

**Reparatur:** Nur der eindeutig benannte Textpuffer wird ersetzt. Andere
Ausgabepuffer behalten ihre Zuordnung. Fehlende oder widersprüchliche
Zuordnungen ergeben einen Backendfehler.

**Grenze:** Zusatzausgaben bleiben die Werte des letzten Quantums. Eine
fachliche Aggregation etwa von Tokenstatistik oder Logprobs ist damit nicht
implementiert und darf nicht als solche angeboten werden.

**Test:** `collected_text_keeps_output_metadata_and_payloads_aligned`.

### R12 · P2 · Ungültige Tokenlimits wurden in gültige Arbeit umgeschrieben

**Stelle:** `cooperative.rs`, `from_request`, `build_quantum`.

`max_tokens: 0` wurde durch `.max(1)` zu einem generierten Token. Negative,
gebrochene oder als String übergebene Werte wurden wie ein fehlendes Limit
behandelt und durch das Betreiberbudget ersetzt. Ein erschöpfter Job konnte
über dieselbe Untergrenze nochmals einen Token bestellen.

**Reparatur:** Die Zerlegung akzeptiert ein fehlendes oder positiv
ganzzahliges Limit. Ungültige Vorgaben bleiben unverändert dem Backend
überlassen. Ein Quantum ohne Restbudget wird nicht gebaut.

**Tests:** `invalid_token_limits_are_not_rewritten_into_valid_work`,
`an_exhausted_job_cannot_order_another_token`.

### R13 · P1 · Kurzes Offlineprofil verdeckte ein zu teures Quantum

**Stelle:** `crates/vig-config/src/schema.rs`, `blocking_time`.

Die neue Blockierungsprüfung verwendete
`min(Kosten des Quantums, konservative Profillaufzeit)`. Die Laufzeit eines
Offlineprofils begrenzt jedoch nicht die wiederholte Verarbeitung eines
gewachsenen Prompts.

**Gegenprobe:** 32 Token bei 258 Token/s plus 6.277 µs Sockel ergeben rund
130 ms. Ein geschützter Stream verspricht 100 ms. Mit einem kurzen
12-ms-p99-Profil meldete die Prüfung trotzdem keinen Befund.

**Reparatur:** Die errechneten Quantenkosten werden nicht mehr durch das
Offlineprofil verkleinert. Der Kommentar benennt jetzt die Grenze der
Prüfung: Mindestquantengröße bei gewachsenem Ausgabetext, kein Nachweis der
maximalen Blockierung beliebiger Clientprompts.

**Test:** `a_short_profile_cannot_hide_an_expensive_quantum`.

### R14 · P2 · Tensor-Metadaten wurden ignoriert oder still verändert

**Stelle:** `cooperative.rs`, Text-/Samplingleser und Quantenbau.

Ein passend gerahmter String genügte zum Zerlegen, auch bei deklarierter
Form `[2]`, numerischem Datentyp oder mehrfach vorhandenem `text_input`.
Der Quantenbau ersetzte dies durch einen einzelnen BYTES-Tensor. Die
Ausgabelesefunktion nahm ohne `text_output` einfach den ersten Tensor.
So konnten Metadaten als generierter Text in die nächste Anfrage gelangen.

**Reparatur:** Nur eindeutig benannte BYTES-Tensoren mit einem Element
werden verarbeitet. Nicht unterstützte Eingaben bleiben unverändert.
Gültige Formen wie `[1, 1]` und vorhandene Eingabemetadaten bleiben beim
Zuschneiden erhalten. Eine fehlende oder fehlerhafte Textausgabe ist ein
Backendfehler; eine explizite leere Textausgabe beendet weiterhin den Job.

**Tests:** `incompatible_text_metadata_is_not_silently_repaired`,
`a_non_text_output_is_not_interpreted_as_generated_text`,
`a_quantum_preserves_valid_single_element_tensor_shapes`.

## Offene Punkte vor belastbaren Latenzzusagen

Diese Punkte sind durch den Code belegt, aber in dieser Reparaturrunde nicht
mit einer neuen Hardwaremessung oder vollständigen Gegenprobe qualifiziert.

1. **Blockerzahl ist keine Parallelitätsgrenze.** `check_blocking_work`
   vergleicht die Anzahl blockierender Modelle mit der Slotzahl. `SlotSet`
   erlaubt demselben Modell aber mehrere gleichzeitige Aufträge. Ein Modell
   mit zwei langen Aufträgen kann zwei Slots belegen, obwohl die Prüfung nur
   einen Blocker zählt. Nötig sind eine durchgesetzte Parallelitätsgrenze je
   Modell oder eine Prüfung anhand der tatsächlich möglichen Belegung.
2. **Sicherheitsmarge erreicht die Quantenkosten nicht.** `Scheduler::size_quantum`
   ersetzt die vorher konservativ geplante Laufzeit durch
   `cooperative.cost_of_with_context(...)`, ohne `margin_of(model)` anzuwenden.
   Konfigurierte und gelernte Sicherheitsmargen schützen diesen Kostenterm
   damit nicht. Nächster Test: gleicher Prompt und gleiche Last bei 100 und
   200 Prozent Marge; Quantenbudget und vorhergesagte Belegung müssen die
   zusätzliche Reserve berücksichtigen. Eine Änderung muss Budgetinversion
   und Endzeit konsistent behandeln, sonst entstehen neue Fehlzulassungen.
3. **Kontext und Fortsetzung bleiben Näherungen.** Der Prompt wird als
   `Bytes / 4` geschätzt. Das ist keine obere Tokenschranke. `cooperative:`
   garantiert außerdem keine Zerlegung jedes Requests: ungültige Eingaben
   und die Overheadgrenze führen zum ungeteilten Lauf. Ein statisches grünes
   Ergebnis beweist daher keine obere Blockierungszeit. Wiederholtes
   Generieren aus Text statt Tokenzustand erfordert außerdem Modelltests
   für Stop-Sequenzen, frühes Ende und Sampling an Quantengrenzen.

Die geprüfte Commitfassung des Ein-Slot-Beispiels nennt selbst **231 ms
längste Lücke bei 100 ms Zusage**. Die während der Prüfung überarbeitete
Vorlage setzt `min_tokens` von 8 auf 4 und nennt dafür 213–222 ms; auch diese
Angaben überschreiten 100 ms. Die zugrunde liegenden GPU-Messungen wurden in
dieser Prüfung nicht wiederholt. „Konfiguration ohne Befund“ und „Zusage auf Hardware eingehalten“
sind hier nachweislich unterschiedliche Aussagen. Vor einem Erfolgs- oder
Produktionsnachweis müssen die Zusagen unter Burstlast, langen Prompts,
Cacheausfall und Backend-Erholung erneut gemessen werden.

## Verifikation

Die vollständigen Ergebnisse und Dateihashes der abschließenden Prüfung
stehen in [RECHECK-VALIDATION.md](RECHECK-VALIDATION.md).
Die isolierten Gegenproben und die Gateway-/Konfigurationstests wurden
ausgeführt. Echte GPU-Läufe und eine neue Android-Qualifikation gehören
nicht zu dieser erneuten Prüfung.
