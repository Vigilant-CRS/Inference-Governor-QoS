//! `vig doctor` — Konfigurations- und Backendpruefung (Spec 23).
//!
//! Der Anspruch: **vor** dem Start sagen, was nicht funktionieren wird, und
//! zwar alles auf einmal. Wer eine Konfiguration repariert, will nicht nach
//! jedem Lauf ein neues Problem entdecken.

#![allow(clippy::print_stdout)]

use std::path::Path;
use std::process::ExitCode;
use vig_backend_triton::TritonClient;
use vig_config::Config;
use vig_config::schema::Resolved;

/// Das Gesamturteil eines Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    Ready,
    ReadyWithWarnings,
    NotReady,
}

impl Verdict {
    const fn label(self) -> &'static str {
        match self {
            Self::Ready => "READY",
            Self::ReadyWithWarnings => "READY_WITH_WARNINGS",
            Self::NotReady => "NOT_READY",
        }
    }
}

fn ok(text: &str) {
    println!("OK   {text}");
}

fn warn(text: &str) {
    println!("WARN {text}");
}

fn fail(text: &str) {
    println!("FAIL {text}");
}

/// Fuehrt die Pruefung aus.
///
/// # Errors
///
/// Wenn die Konfigurationsdatei nicht gelesen werden kann.
pub(crate) async fn run(
    path: &Path,
    offline: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let config = match Config::from_yaml(&text) {
        Ok(c) => c,
        Err(e) => {
            fail(&e.to_string());
            println!("\nRESULT {}", Verdict::NotReady.label());
            return Ok(ExitCode::FAILURE);
        }
    };

    let mut verdict = Verdict::Ready;

    let findings = config.diagnose();
    if findings.is_empty() {
        ok("Konfigurationsschema gueltig");
    } else {
        for finding in &findings {
            fail(&finding.to_string());
        }
        verdict = Verdict::NotReady;
    }

    let Ok(resolved) = config.resolve() else {
        println!("\nRESULT {}", Verdict::NotReady.label());
        return Ok(ExitCode::FAILURE);
    };

    verdict = verdict.max(check_contracts(&resolved));
    verdict = verdict.max(check_utilization(&resolved));
    verdict = verdict.max(check_best_effort_feasibility(&resolved));
    verdict = verdict.max(check_backend(&resolved, offline).await);
    verdict = verdict.max(check_capabilities(&resolved, offline).await);
    verdict = verdict.max(check_profiles(&resolved, offline).await);
    verdict = verdict.max(check_variant_signatures(&resolved, offline).await);
    verdict = verdict.max(check_security(&resolved));
    verdict = verdict.max(check_hardware(offline));

    println!("\nRESULT {}", verdict.label());
    Ok(match verdict {
        Verdict::NotReady => ExitCode::FAILURE,
        Verdict::Ready | Verdict::ReadyWithWarnings => ExitCode::SUCCESS,
    })
}

/// Statische Vertragspruefungen, die kein Backend brauchen.
fn check_contracts(resolved: &Resolved) -> Verdict {
    let mut verdict = Verdict::Ready;

    for (i, contract) in resolved.contracts.iter().enumerate() {
        let name = resolved.model_names.get(i).map_or("?", String::as_str);

        // ADR-0010: ohne max_age kann Vigilant keine Arbeit als wertlos
        // erkennen und verliert sein staerkstes Werkzeug.
        if contract.max_age.is_none() {
            warn(&format!(
                "{name}: kein max_age_ms konfiguriert; Frische-Verwerfen ist damit inaktiv"
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        }

        // Ohne Periode gibt es keine Ankunftsprognose und damit kein
        // absichtliches Idle (Spec 10.7, 10.8).
        if contract.period.is_none() && contract.criticality.is_guarded() {
            warn(&format!(
                "{name}: geschuetzt, aber ohne period_ms; der Protected-Look-ahead ist inaktiv"
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        }

        if !contract.auto_variant_selection() && contract.variants.len() > 1 {
            warn(&format!(
                "{name}: mehrere Varianten, aber keine automatische Wahl \
                 (quality.source: unknown oder stateful)"
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        }

        // ADR-0010: ein max_age in der Groessenordnung der Laufzeit ist ein
        // ueberzeichneter Vertrag. Genau daran ist Szenario A im Gate-S-Lauf
        // gescheitert, bevor es aufgefallen war.
        if let (Some(max_age), Some(best)) = (contract.max_age, contract.variants.get(0))
            && let Ok(runtime) = best.profile.conservative_at(0, resolved.margin)
            && runtime.as_nanos().saturating_mul(2) > max_age.as_nanos()
        {
            warn(&format!(
                "{name}: max_age {max_age} liegt unter dem Doppelten der konservativen \
                 Laufzeit {runtime}; unter Last wird fast jeder Request verworfen"
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        }
    }
    verdict
}

/// Warnt vor Best-Effort-Modellen, die unter Last strukturell nie starten.
///
/// ADR-0012: ein nicht unterbrechbarer Job, der laenger dauert als die kuerzeste
/// geschuetzte Periode, gefaehrdet immer die naechste geschuetzte Ankunft — der
/// Look-ahead verschiebt ihn dann bei jeder Gelegenheit. Das ist zur
/// Konfigurationszeit ausrechenbar, und der Nutzer soll es hier erfahren und
/// nicht nach zwei Wochen aus einer leeren Metrik.
fn check_best_effort_feasibility(resolved: &Resolved) -> Verdict {
    let shortest_guarded = resolved
        .contracts
        .iter()
        .enumerate()
        .filter(|(_, c)| c.criticality.is_guarded())
        .filter_map(|(i, c)| c.period.map(|p| (i, p)))
        .min_by_key(|(_, p)| p.as_nanos());

    let Some((guarded_index, guarded_period)) = shortest_guarded else {
        return Verdict::Ready;
    };
    let guarded_name = resolved
        .model_names
        .get(guarded_index)
        .map_or("?", String::as_str);

    let mut verdict = Verdict::Ready;
    let slots = resolved.slots.len();
    for (i, contract) in resolved.contracts.iter().enumerate() {
        if contract.criticality.is_guarded() {
            continue;
        }
        let name = resolved.model_names.get(i).map_or("?", String::as_str);
        let Some(fastest) = contract.variants.iter().last() else {
            continue;
        };
        let Ok(runtime) = fastest.profile.conservative_at(0, resolved.margin) else {
            continue;
        };
        if slots <= 1 && runtime.as_nanos() > guarded_period.as_nanos() {
            if contract.cooperative.is_some() {
                // ADR-0014: genau dafuer gibt es die Zerlegung. Der Hinweis
                // bleibt trotzdem stehen — der Betreiber soll wissen, dass
                // dieses Modell ohne Zerlegung nicht laufen wuerde.
                ok(&format!(
                    "{name}: Laufzeit {runtime} uebersteigt die geschuetzte Periode \
                     {guarded_period} ({guarded_name}), wird aber in Quanten zerlegt"
                ));
            } else {
                warn(&format!(
                    "{name}: konservative Laufzeit {runtime} uebersteigt die kuerzeste \
                     geschuetzte Periode {guarded_period} ({guarded_name}). Auf einem \
                     Slot wird das Modell unter Last nie starten. Abhilfe: mehr Slots, \
                     Zerlegung in Quanten (cooperative) oder eine hoehere Klasse."
                ));
                verdict = verdict.max(Verdict::ReadyWithWarnings);
            }
        }
    }
    verdict
}

/// Die einfache Demand-Warnung aus Spec 10.9: `U = Summe(C_i / T_i)`.
///
/// Keine vollstaendige Schedulability-Garantie — bei realer GPU-Konkurrenz und
/// nicht-praeemptiven Abschnitten waere das falsch. Aber sehr nuetzlich, um
/// offensichtlich unmoegliche Vertraege vor dem Start zu erkennen.
fn check_utilization(resolved: &Resolved) -> Verdict {
    let utilization = resolved.protected_utilization_permille();
    let percent = utilization.checked_div(10).unwrap_or(0);
    let slots = resolved.slots.len();

    if utilization > 1_000 {
        fail(&format!(
            "PROTECTED_WORKLOAD_UNSCHEDULABLE: geschuetzte Auslastung {percent} %, \
             ueber {slots} Slot(s) nicht tragbar. Best-Effort-Arbeit kommt damit \
             strukturell nie zum Zug."
        ));
        Verdict::NotReady
    } else if utilization > 800 {
        warn(&format!(
            "geschuetzte serialisierte Auslastung {percent} %; fuer Best-Effort \
             bleibt kaum Reserve"
        ));
        Verdict::ReadyWithWarnings
    } else {
        ok(&format!("geschuetzte serialisierte Auslastung {percent} %"));
        Verdict::Ready
    }
}

/// Was kann das Backend, und was folgt daraus?
///
/// Vigilant spricht einen offenen Standard und laeuft nicht nur gegen Triton.
/// Was ein konkreter Server beherrscht, steht in seinen Metadaten — das ist
/// eine Abfrage und keine Annahme. Der Betreiber soll hier erfahren, welchen
/// Datenpfad er auf *seiner* Maschine bekommt, und nicht erst im Betrieb.
async fn check_capabilities(resolved: &Resolved, offline: bool) -> Verdict {
    if offline {
        return Verdict::ReadyWithWarnings;
    }
    let mut verdict = Verdict::Ready;
    for endpoint in resolved.endpoints() {
        let client = TritonClient::new(&endpoint);
        let Ok(mut raw) = client.raw().await else {
            continue;
        };
        let Ok(response) = raw
            .server_metadata(vig_protocol_oip::inference::ServerMetadataRequest {})
            .await
        else {
            warn(&format!("{endpoint}: Servermetadaten nicht abfragbar"));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
            continue;
        };
        let meta = response.into_inner();
        let caps = vig_backend_triton::Capabilities::from_metadata(&meta);

        ok(&format!("{endpoint}: {} {}", meta.name, meta.version));
        if caps.can_pass_references() {
            ok(&format!(
                "{endpoint}: Shared Memory verfuegbar — Tensoren werden als \
                 Referenz durchgereicht"
            ));
        } else {
            // Gemessen: ein 6,2-MB-Bild kostet auf dem Kopierpfad +11,7 ms
            // statt +160 us. Wer das erst im Betrieb merkt, hat die falsche
            // Hardware gekauft.
            warn(&format!(
                "{endpoint}: kein Shared Memory. Grosse Tensoren laufen ueber den \
                 Kopierpfad;\n     gemessen kostet ein 6,2-MB-Bild dort +11,7 ms \
                 statt +160 us (Faktor 73)."
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        }
        if !caps.has(vig_backend_triton::Extension::Sequence)
            && resolved.contracts.iter().any(|c| c.stateful)
        {
            fail(&format!(
                "{endpoint}: Konfiguration enthaelt `stateful: true`, aber der \
                 Server meldet keine\n     Sequence-Unterstuetzung."
            ));
            verdict = Verdict::NotReady;
        }
    }
    verdict
}

/// Gehoeren die hinterlegten Profile noch zur laufenden Umgebung? (G-010)
///
/// Kein `FAIL`: ein veraltetes Profil macht die Konfiguration nicht ungueltig,
/// es macht sie unzuverlaessig. `serve` startet trotzdem, plant aber
/// vorsichtiger (ADR-0016) — und der Betreiber soll hier erfahren, warum.
/// Sind die Varianten eines Modells ueberhaupt austauschbar?
///
/// Der Governor waehlt sie je Request und sagt es dem Client nicht. Wer hier
/// eine Warnung sieht, hat zwei Modelle unter einem Namen, die verschiedene
/// Dinge zurueckgeben — der gefaehrlichste Befund dieses Werkzeugs, weil er
/// im Betrieb erst nach einem Variantenwechsel auffaellt.
/// Ist der Endpunkt so abgesichert, wie er betrieben werden soll?
///
/// Der Governor steuert eine ganze GPU. Ohne Identitaetspruefung im Netz gibt
/// er diese Steuerung an jeden weiter, der ihn erreicht. Das ist fuer ein
/// abgeschlossenes Geraet in Ordnung und sonst nicht — und der Unterschied
/// gehoert vor den Start, nicht in ein Postmortem.
fn check_security(resolved: &Resolved) -> Verdict {
    let s = &resolved.security;
    let mut verdict = Verdict::Ready;

    if s.tls_enabled() {
        if s.client_ca.is_some() {
            ok("TLS mit Clientzertifikaten (mTLS)");
        } else {
            ok("TLS aktiv; Clients werden nicht per Zertifikat geprueft");
        }
    }
    if s.token_file.is_some() {
        ok("Bearer-Token-Pruefung eingerichtet");
    }
    if !s.tls_enabled() && s.token_file.is_none() {
        warn(
            "Keine Zugangspruefung eingerichtet. Das ist fuer Loopback in Ordnung — \
             `serve` bindet dort per Voreinstellung. Wird der Endpunkt geoeffnet, \
             uebernimmt jeder im Netz die Steuerung der GPU: dann `backend.security` \
             einrichten oder eine authentifizierende Instanz davorstellen.",
        );
        verdict = verdict.max(Verdict::ReadyWithWarnings);
    }
    if resolved.trust == vig_config::schema::TrustMode::Open {
        warn(
            "trust: open — unkonfigurierte Modelle werden unveraendert durchgereicht \
             (Spec L-002), und ein Client darf seine Wichtigkeitsklasse selbst \
             angeben. Fuer eine Installation ausserhalb eines abgeschlossenen Netzes \
             ist `trust: strict` die richtige Wahl.",
        );
        verdict = verdict.max(Verdict::ReadyWithWarnings);
    }
    verdict
}

async fn check_variant_signatures(resolved: &Resolved, offline: bool) -> Verdict {
    if offline {
        return Verdict::Ready;
    }
    // Zuerst die Zusage, falls es eine gibt: sie ist die staerkere Aussage.
    let violations = crate::verify::contract_violations(resolved).await;
    if !violations.is_empty() {
        for v in &violations {
            fail(&format!(
                "{}: Variante {} erfuellt die zugesagte io_signature nicht — \
                 zugesagt {}, gemeldet {}. `serve` wird damit nicht starten.",
                v.logical,
                v.physical,
                v.declared.describe(),
                v.actual.describe(),
            ));
        }
        return Verdict::NotReady;
    }

    let conflicts = crate::verify::signature_conflicts(resolved).await;
    if conflicts.is_empty() {
        return Verdict::Ready;
    }
    for conflict in &conflicts {
        warn(&format!(
            "{}: {} und {} haben verschiedene I/O-Signaturen — {} gegen {}. \
             Die automatische Variantenwahl wird fuer dieses Modell \
             abgeschaltet; es laeuft nur auf seiner besten Variante.",
            conflict.logical,
            conflict.reference.0,
            conflict.divergent.0,
            conflict.reference.1.describe(),
            conflict.divergent.1.describe(),
        ));
    }
    Verdict::ReadyWithWarnings
}

async fn check_profiles(resolved: &Resolved, offline: bool) -> Verdict {
    if offline {
        warn("Profile nicht gegen das Backend geprueft (--offline)");
        return Verdict::ReadyWithWarnings;
    }

    let checked = crate::verify::check(resolved).await;
    if checked.is_empty() {
        return Verdict::Ready;
    }

    let mut verdict = Verdict::Ready;
    for entry in &checked {
        let name = format!("{} -> {}", entry.logical, entry.physical);
        match &entry.trust {
            crate::verify::Trust::Verified => {
                ok(&format!("{name}: Profil passt zur Umgebung"));
            }
            crate::verify::Trust::Missing => {
                warn(&format!(
                    "{name}: Profil ohne Fingerabdruck — nicht pruefbar. \
                     Neu messen mit `vig profile`."
                ));
                verdict = verdict.max(Verdict::ReadyWithWarnings);
            }
            crate::verify::Trust::Mismatch { declared, actual } => {
                warn(&format!(
                    "{name}: Profil gehoert zu einer anderen Umgebung \
                     (hinterlegt {declared}, gemessen {actual}). Es wird bis auf \
                     Weiteres mit erhoehter Marge geplant; neu messen mit \
                     `vig profile`."
                ));
                verdict = verdict.max(Verdict::ReadyWithWarnings);
            }
            crate::verify::Trust::Unavailable(reason) => {
                warn(&format!("{name}: nicht pruefbar ({reason})"));
                verdict = verdict.max(Verdict::ReadyWithWarnings);
            }
        }

        // NV-03: der Fingerabdruck sagt "die Metadatenlage stimmt". Das
        // Manifest sagt, ob auch das Artefakt, die Runtime und die
        // Geraeteaufteilung noch dieselben sind — und benennt, welches Feld
        // nicht mehr passt.
        let Some(comparison) = entry.manifest.as_ref() else {
            continue;
        };
        match comparison.verdict() {
            vig_config::manifest::ManifestVerdict::Invalid => {
                for field in comparison.divergences() {
                    if let vig_config::manifest::FieldVerdict::Divergent { declared, actual } =
                        &field.verdict
                    {
                        warn(&format!(
                            "{name}: {} weicht ab (hinterlegt {declared}, beobachtet {actual}).                              Das Profil gilt fuer diese Umgebung nicht; neu messen mit                              `vig calibrate`.",
                            field.field
                        ));
                    }
                }
                verdict = verdict.max(Verdict::ReadyWithWarnings);
            }
            vig_config::manifest::ManifestVerdict::Unverified => {
                let gaps: Vec<&str> = comparison.unknowns().map(|f| f.field).collect();
                warn(&format!(
                    "{name}: Profilherkunft unvollstaendig belegt ({}).                      Unbekannt heisst hier unbekannt, nicht geprueft.",
                    gaps.join(", ")
                ));
                verdict = verdict.max(Verdict::ReadyWithWarnings);
            }
            vig_config::manifest::ManifestVerdict::Verified => {
                ok(&format!("{name}: Profilherkunft vollstaendig belegt"));
            }
        }
    }
    verdict
}

/// Der beobachtbare Hardwarezustand (NV-04).
///
/// Lesend und ohne Root. Ein Befund hier verhindert keinen Start — die
/// Hardware ist, wie sie ist. Er sagt dem Betreiber aber vor der Messung,
/// was seine Zahlen spaeter bedeuten werden: ein Profil, das unter einem
/// Leistungslimit entstand, beschreibt nicht die Karte, sondern die Karte
/// unter diesem Limit.
fn check_hardware(offline: bool) -> Verdict {
    use vig_platform::Collector as _;

    if offline {
        warn("Hardware nicht gelesen (--offline)");
        return Verdict::ReadyWithWarnings;
    }

    let mut collector = vig_platform::NvidiaSmi::default();
    let snapshot = match collector.snapshot() {
        Ok(s) => s,
        Err(reason) => {
            // Kein Fehler: der Governor braucht die Beobachtung nicht zum
            // Laufen. Ohne sie plant er nur nicht zustandsabhaengig.
            warn(&format!(
                "Hardwarezustand nicht lesbar ({reason}). \
                 Der Governor laeuft, plant aber ohne Geraetezustand."
            ));
            return Verdict::ReadyWithWarnings;
        }
    };

    if snapshot.gpus.is_empty() {
        warn("nvidia-smi meldet keine GPU");
        return Verdict::ReadyWithWarnings;
    }

    let mut verdict = Verdict::Ready;
    for gpu in &snapshot.gpus {
        let name = gpu.name.value().map_or("?", String::as_str);
        let driver = gpu.driver.value().map_or("?", String::as_str);
        let cc = gpu.compute_capability.value().map_or("?", String::as_str);
        let mib = gpu.memory_total_mib.value().copied().unwrap_or(0);
        ok(&format!(
            "GPU {}: {name}, Treiber {driver}, CC {cc}, {mib} MiB",
            gpu.index
        ));

        let limiting = gpu.limiting_reasons();
        if !limiting.is_empty() {
            let clocks = match (gpu.clock_sm_mhz.value(), gpu.clock_sm_max_mhz.value()) {
                (Some(current), Some(max)) => format!(" ({current} von {max} MHz)"),
                _ => String::new(),
            };
            warn(&format!(
                "GPU {}: gedrosselt{clocks}, Grund {limiting:?}. Ein hier gemessenes \
                 Profil gilt nur fuer diesen Zustand.",
                gpu.index
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        }
    }
    verdict
}

/// Erreichbarkeit des Backends und Bereitschaft aller Varianten.
async fn check_backend(resolved: &Resolved, offline: bool) -> Verdict {
    if offline {
        warn("Backend nicht geprueft (--offline)");
        return Verdict::ReadyWithWarnings;
    }

    // Jedes Backend einzeln pruefen: Vision- und Sprachmodelle laufen in
    // getrennten Servern, weil ihre Backends unvereinbare Bibliotheksstaende
    // brauchen. Ein erreichbarer Server sagt nichts ueber den anderen.
    let mut verdict = Verdict::Ready;
    for endpoint in resolved.endpoints() {
        let client = TritonClient::new(&endpoint);
        match client.health().await {
            Ok(health) if health.live && health.ready => {
                ok(&format!("Backend erreichbar unter {endpoint}"));
            }
            Ok(health) => {
                fail(&format!(
                    "Backend {endpoint} antwortet, ist aber nicht bereit \
                     (live={}, ready={})",
                    health.live, health.ready
                ));
                verdict = Verdict::NotReady;
            }
            Err(e) => {
                fail(&format!("{endpoint}: {e}"));
                verdict = Verdict::NotReady;
            }
        }
    }
    if verdict == Verdict::NotReady {
        return verdict;
    }

    for (i, names) in resolved.backend_models.iter().enumerate() {
        let logical = resolved.model_names.get(i).map_or("?", String::as_str);
        let client = TritonClient::new(
            resolved.endpoint_of(vig_core::ModelIdx(u16::try_from(i).unwrap_or(0))),
        );
        for physical in names {
            match client.model_ready(physical).await {
                Ok(true) => ok(&format!("{logical} -> {physical} bereit")),
                Ok(false) => {
                    fail(&format!("{logical} -> {physical} ist nicht bereit"));
                    verdict = Verdict::NotReady;
                }
                Err(e) => {
                    fail(&format!("{logical} -> {physical}: {e}"));
                    verdict = Verdict::NotReady;
                }
            }
        }
    }
    verdict
}
