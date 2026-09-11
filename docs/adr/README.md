# Architecture Decision Records

Diese ADRs dokumentieren Entscheidungen, die von der
`Vigilant_Inference_Governor_Specification_v1.0.md` abweichen oder sie präzisieren.

Die Spezifikation v1.0 bleibt unverändert als Baseline erhalten. Wo ein ADR und
die Spec sich widersprechen, gilt das ADR — jedes ADR benennt die betroffene
Spec-Stelle explizit.

| ADR | Titel | Betrifft Spec | Status |
|---|---|---|---|
| [0001](0001-simulation-first-gate-s.md) | Simulation-First-Entwicklung mit Gate S | §24, §25, §38 | Akzeptiert |
| [0002](0002-backend-in-flight-control.md) | Backend In-Flight Control (L-021) | §3.1, §7.1, §10.3 | Akzeptiert |
| [0003](0003-shared-memory-passthrough.md) | Shm-Referenz-Passthrough als primärer Datenpfad | §17.3, §4.4, §19.8 | Akzeptiert |
| [0004](0004-execution-slots.md) | Backend als explizite Execution Slots | §10.4, §10.7, §10.10 | Akzeptiert |
| [0005](0005-freshness-success-metric.md) | Erfolgsmetrik für LATEST-Streams | §4.4, Anhang D | Akzeptiert |
| [0006](0006-defer-interference-profiler.md) | Interferenzprofiler hinter Gate M3 | §13.4, WP12 | Akzeptiert |
| [0007](0007-variant-quality-provenance.md) | Herkunft der Varianten-Qualitätswerte | §12.2, §12.3 | Akzeptiert |
| [0008](0008-risk-driven-phase-order.md) | Risikogetriebene Phasenordnung | §24, §25 | Akzeptiert |
| [0009](0009-infeasibility-does-not-mean-worthless.md) | Verworfen wird, was wertlos ist — nicht, was zu spaet kommt | §10.3, §10.6 | Akzeptiert |
| [0010](0010-pessimistic-promises-optimistic-discards.md) | Pessimistisch versprechen, optimistisch verwerfen | §10.3, §13.2 | Akzeptiert |
| [0011](0011-client-clock-domains.md) | Die Erzeugungszeit kommt aus einer fremden Uhr | §16.2, §10.2, L-019 | Akzeptiert |
| [0012](0012-best-effort-starvation.md) | Aushungerung ist ein Befund, kein Nebeneffekt | §1.3, §10.6, §15 | Akzeptiert |
| [0013](0013-margin-corrects-forecasts-not-contracts.md) | Die Marge korrigiert Prognosefehler, nicht Vertragsverletzungen | §13.3 | Akzeptiert |
| [0014](0014-cooperative-quanta.md) | Das Quantum ist so gross, wie der Slack es zulaesst | §15.3, WP26 | Akzeptiert |
| [0015](0015-quantum-sizing-must-not-spend-the-deadline-reserve.md) | Ein Quantum darf die Deadline-Reserve nicht aufzehren | ADR-0014 | Akzeptiert |
| [0016](0016-unverified-profiles-widen-the-margin.md) | Ein unbestaetigtes Profil weitet die Marge, es verweigert nicht den Start | G-010, L-014 | Akzeptiert |
| [0017](0017-load-that-breaks-the-contract-is-a-finding.md) | Eine Last, die den Vertrag sprengt, ist ein Befund | soak.md, L-017 | Akzeptiert |
| [0018](0018-calibrate-hardware-not-requirements.md) | Der Kalibrator misst Hardware, keine Anforderungen | WP12, ADR-0006 | Akzeptiert |
| [0019](0019-profile-identity-beyond-a-metadata-hash.md) | Profilidentitaet ist mehr als ein Metadaten-Hash | NV-03, ADR-0016, G-010 | Akzeptiert |
| [0020](0020-contract-extensions-are-additive-and-versioned.md) | Vertragszusaetze sind additiv, versioniert und vom Betreiber | NV-02 | Akzeptiert |
| [0021](0021-hardware-is-read-never-set.md) | Die Hardware wird gelesen, nie gestellt | NV-04, ADR-0019 | Akzeptiert |
| [0022](0022-measurement-is-a-method-not-a-loop.md) | Messen ist eine Methode, keine Schleife | NV-05, ADR-0021 | Akzeptiert |
| [0023](0023-state-aware-prediction-runs-in-the-shadow-first.md) | Die zustandsabhaengige Prognose laeuft erst im Schatten | NV-06, ADR-0021 | Akzeptiert |
| [0024](0024-the-backend-is-a-seam-not-a-type.md) | Das Backend ist eine Naht, kein Typ | NV-07, NV-00 | Akzeptiert |
| [0025](0025-the-same-shape-is-not-the-same-meaning.md) | Gleiche Form ist nicht gleiche Bedeutung | NV-10, NV-02, ADR-0007 | Akzeptiert |
| [0026](0026-interference-is-directed-and-not-additive.md) | Interferenz ist gerichtet und nicht additiv | NV-11, ADR-0006 | Akzeptiert |
| [0027](0027-a-miss-budget-that-decides-not-only-observes.md) | Ein Missbudget, das entscheidet — auf ausdrueckliche Handlung | NV-24, NV-02 | Akzeptiert |
| [0028](0028-a-fusion-needs-a-common-capture.md) | Eine Zusammenfuehrung braucht eine gemeinsame Aufnahme | NV-17, ADR-0005 | Akzeptiert |
| [0029](0029-a-hint-may-tighten-never-loosen.md) | Ein Hinweis darf verschaerfen, nie lockern | NV-18 | Akzeptiert |
| [0030](0030-actuation-is-an-exception-and-must-be-observed.md) | Aktuation ist eine Ausnahme, und sie muss beobachtet werden | NV-13, ADR-0021 | Akzeptiert |
| [0031](0031-a-re-prefill-is-not-free-progress.md) | Ein Re-Prefill ist kein kostenloser Fortschritt | NV-16, ADR-0012, ADR-0014 | Akzeptiert |
| [0032](0032-four-promises-that-fell-apart-between-components.md) | Beendet, frisch, angenommen, verfuegbar — vier Zusagen zwischen den Komponenten | Review 10.09., ADR-0005, ADR-0018 | Akzeptiert |
| [0033](0033-native-code-lives-in-the-backend-process.md) | Nativer Code gehoert in den Backendprozess, nicht in den Governor | NV-09, NV-12, NV-14, NV-15, ADR-0024 | Akzeptiert |
| [0034](0034-the-margin-has-a-target.md) | Die Sicherheitsmarge hat ein Ziel | Spec 13.3, NV-06, ADR-0016, ADR-0027 | Akzeptiert |
| [0037](0037-a-domain-is-a-gpu-with-one-owner.md) | Eine Domaene ist eine GPU mit genau einem Besitzer | NV-22, ADR-0004, ADR-0020, ADR-0024 | Akzeptiert |

## Format

Kontext → Entscheidung → Konsequenzen. Kurz halten. Ein ADR beschreibt *eine*
Entscheidung und die Begründung, die zum Zeitpunkt der Entscheidung galt.
ADRs werden nicht rückwirkend umgeschrieben; sie werden durch neue ADRs abgelöst.
