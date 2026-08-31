//! `onetimer doctor` — Konfigurations- und Backendpruefung (Spec 23).
//!
//! Der Anspruch: **vor** dem Start sagen, was nicht funktionieren wird, und
//! zwar alles auf einmal. Wer eine Konfiguration repariert, will nicht nach
//! jedem Lauf ein neues Problem entdecken.

#![allow(clippy::print_stdout)]

use onetimer_backend_triton::TritonClient;
use onetimer_config::Config;
use onetimer_config::schema::Resolved;
use std::path::Path;
use std::process::ExitCode;

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
    verdict = verdict.max(check_backend(&resolved, offline).await);

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

        // ADR-0010: ohne max_age kann OneTimer keine Arbeit als wertlos
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

/// Die einfache Demand-Warnung aus Spec 10.9: `U = Summe(C_i / T_i)`.
///
/// Keine vollstaendige Schedulability-Garantie — bei realer GPU-Konkurrenz und
/// nicht-praeemptiven Abschnitten waere das falsch. Aber sehr nuetzlich, um
/// offensichtlich unmoegliche Vertraege vor dem Start zu erkennen.
fn check_utilization(resolved: &Resolved) -> Verdict {
    let mut protected_permille = 0_u64;

    for contract in resolved.contracts.iter() {
        if !contract.criticality.is_guarded() {
            continue;
        }
        let (Some(period), Some(best)) = (contract.period, contract.variants.get(0)) else {
            continue;
        };
        let Ok(runtime) = best.profile.conservative_at(0, resolved.margin) else {
            continue;
        };
        let share = runtime
            .as_nanos()
            .saturating_mul(1_000)
            .checked_div(period.as_nanos().max(1))
            .unwrap_or(0);
        protected_permille = protected_permille.saturating_add(share);
    }

    let slots = u64::try_from(resolved.slots.len()).unwrap_or(1).max(1);
    let utilization = protected_permille.checked_div(slots).unwrap_or(0);
    let percent = utilization.checked_div(10).unwrap_or(0);

    if utilization > 1_000 {
        fail(&format!(
            "PROTECTED_WORKLOAD_UNSCHEDULABLE: geschuetzte Auslastung {percent} %, \
             ueber {slots} Slot(s) nicht tragbar"
        ));
        Verdict::NotReady
    } else if utilization > 800 {
        warn(&format!("geschuetzte serialisierte Auslastung {percent} %"));
        Verdict::ReadyWithWarnings
    } else {
        ok(&format!("geschuetzte serialisierte Auslastung {percent} %"));
        Verdict::Ready
    }
}

/// Erreichbarkeit des Backends und Bereitschaft aller Varianten.
async fn check_backend(resolved: &Resolved, offline: bool) -> Verdict {
    if offline {
        warn("Backend nicht geprueft (--offline)");
        return Verdict::ReadyWithWarnings;
    }

    let client = TritonClient::new(&resolved.backend_endpoint);
    match client.health().await {
        Ok(health) if health.live && health.ready => {
            ok(&format!(
                "Backend erreichbar unter {}",
                resolved.backend_endpoint
            ));
        }
        Ok(health) => {
            fail(&format!(
                "Backend antwortet, ist aber nicht bereit (live={}, ready={})",
                health.live, health.ready
            ));
            return Verdict::NotReady;
        }
        Err(e) => {
            fail(&e.to_string());
            return Verdict::NotReady;
        }
    }

    let mut verdict = Verdict::Ready;
    for (i, names) in resolved.backend_models.iter().enumerate() {
        let logical = resolved.model_names.get(i).map_or("?", String::as_str);
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
