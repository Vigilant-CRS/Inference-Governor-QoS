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
use vig_core::Duration;
use vig_core::generative::{Latencies, Plan};

/// Das Gesamturteil eines Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Verdict {
    Ready,
    ReadyWithWarnings,
    NotReady,
}

impl Verdict {
    pub(crate) const fn label(self) -> &'static str {
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

/// Die `FAIL`-Zeilen der letzten Pruefung, als Werte.
///
/// `vig autotune` nennt sie im Bericht, wenn es das Tuning wegen `NOT_READY`
/// auslaesst. Gedruckt und gesammelt wird an derselben Stelle, damit der
/// Bericht nie etwas anderes nennt als das Terminal.
static FAILURES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn fail(text: &str) {
    println!("FAIL {text}");
    FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(text.to_owned());
}

/// Die `FAIL`-Zeilen der zuletzt gelaufenen [`verdict_of`].
pub(crate) fn failures() -> Vec<String> {
    FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
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
    let verdict = Box::pin(verdict_of(path, offline)).await?;
    println!("\nRESULT {}", verdict.label());
    Ok(match verdict {
        Verdict::NotReady => ExitCode::FAILURE,
        Verdict::Ready | Verdict::ReadyWithWarnings => ExitCode::SUCCESS,
    })
}

/// Fuehrt dieselbe Pruefung aus und gibt das Urteil zurueck, statt es zu
/// drucken.
///
/// `vig autotune` braucht das Urteil als Wert, nicht als Zeile auf der
/// Standardausgabe. Ein Werkzeug, das die Ausgabe eines anderen nach Woertern
/// durchsucht, meldet irgendwann das Falsche, weil jemand die Zeile umformuliert
/// hat — und hier haengt an der Zeile die Aussage, ob eine Konfiguration
/// traegt.
///
/// # Errors
///
/// Wenn die Konfigurationsdatei nicht gelesen werden kann.
pub(crate) async fn verdict_of(
    path: &Path,
    offline: bool,
) -> Result<Verdict, Box<dyn std::error::Error>> {
    FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    let text = std::fs::read_to_string(path)?;
    let config = match Config::from_yaml(&text) {
        Ok(c) => c,
        Err(e) => {
            fail(&e.to_string());
            return Ok(Verdict::NotReady);
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
        return Ok(Verdict::NotReady);
    };

    verdict = verdict.max(check_contracts(&resolved));
    verdict = verdict.max(check_capacity(&resolved));
    verdict = verdict.max(check_decomposition_cost(&resolved));
    verdict = verdict.max(check_unenforced_requirements(&resolved));
    verdict = verdict.max(check_backend(&resolved, offline).await);
    verdict = verdict.max(check_capabilities(&resolved, offline).await);
    verdict = verdict.max(check_profiles(&resolved, offline).await);
    verdict = verdict.max(check_variant_signatures(&resolved, offline).await);
    verdict = verdict.max(check_security(&resolved));
    verdict = verdict.max(check_margin_learning(&resolved));
    verdict = verdict.max(check_supply_guard_vs_background(&resolved));
    verdict = verdict.max(check_semantics(&resolved));
    verdict = verdict.max(check_hardware(offline));

    Ok(verdict)
}

/// Versorgungsschutz gegen Mindestfortschritt: schliesst die eine Zusage die
/// andere aus, wird das hier gemeldet (ADR-0043).
///
/// Ein unteilbarer Hintergrundauftrag der Dauer `B` passt nur zwischen zwei
/// geschuetzte Ankuenfte, wenn die naechste danach noch vor dem Ablauf des
/// vorigen Ergebnisses fertig wird. Im unguenstigsten Fall startet er
/// unmittelbar davor: mit konservativer Laufzeit `C` und Hoechstalter `A` des
/// geschuetzten Stroms heisst das `B <= A - 2C`. Darueber gibt es keine
/// Reihenfolge, die beide Zusagen haelt — und ein Regler, der es trotzdem
/// versucht, bricht abwechselnd beide.
///
/// Die Rechnung ist konservativ und beweist nur Unerfuellbarkeit, nicht
/// Erfuellbarkeit: `p99 x Marge` ist keine obere Schranke der Laufzeit.
fn check_supply_guard_vs_background(resolved: &Resolved) -> Verdict {
    if !resolved.protect_supply {
        return Verdict::Ready;
    }
    // Das engste geschuetzte Fenster bestimmt die Luecke.
    let mut narrowest: Option<(&str, Duration)> = None;
    for (i, contract) in resolved.contracts.iter().enumerate() {
        if !contract.criticality.is_guarded() {
            continue;
        }
        let (Some(max_age), Some(best)) = (contract.max_age, contract.variants.get(0)) else {
            continue;
        };
        let Ok(runtime) = best.profile.conservative_at(0, resolved.margin) else {
            continue;
        };
        let room = Duration::from_nanos_unbounded(
            max_age
                .as_nanos()
                .saturating_sub(runtime.as_nanos().saturating_mul(2)),
        );
        let name = resolved.model_names.get(i).map_or("?", String::as_str);
        if narrowest.is_none_or(|(_, previous)| room < previous) {
            narrowest = Some((name, room));
        }
    }
    let Some((guarded, room)) = narrowest else {
        return Verdict::Ready;
    };

    let mut verdict = Verdict::Ready;
    for (i, contract) in resolved.contracts.iter().enumerate() {
        if contract.criticality.is_guarded() {
            continue;
        }
        let Some(pct) = contract
            .extension
            .as_ref()
            .and_then(|e| e.minimum_background_progress_pct)
        else {
            continue;
        };
        let name = resolved.model_names.get(i).map_or("?", String::as_str);
        let longest = contract
            .variants
            .iter()
            .filter_map(|v| v.profile.conservative_at(0, resolved.margin).ok())
            .max();
        let Some(longest) = longest else {
            continue;
        };
        if longest > room {
            fail(&format!(
                "{name}: {pct} % Mindestfortschritt und der Versorgungsschutz von {guarded} \
                 schliessen sich aus — ein unteilbarer Auftrag von {longest} passt nicht in \
                 die Luecke von {room} (Hoechstalter minus zweimal Laufzeit). Zerlegen \
                 (ADR-0014), Rate senken, Hoechstalter anheben, praemptierbar machen \
                 (ADR-0035) oder eine eigene Ressource geben (ADR-0037) — siehe ADR-0043"
            ));
            verdict = Verdict::NotReady;
        } else {
            ok(&format!(
                "{name}: {pct} % Mindestfortschritt passt neben den Versorgungsschutz von \
                 {guarded} ({longest} in einer Luecke von {room})"
            ));
        }
    }
    verdict
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
            // Den Grund nennen und nicht nur den Zustand: es gibt vier, und
            // der Betreiber soll wissen, welcher zutrifft. Der fachliche
            // Widerspruch wird weiter unten einzeln benannt.
            let reason = if contract.stateful {
                "stateful"
            } else if !contract.variants_interchangeable {
                "verschiedene I/O-Signaturen"
            } else if contract.semantic_conflict().is_some() {
                "fachlicher Widerspruch zwischen Varianten, siehe unten"
            } else {
                "quality.source: unknown"
            };
            warn(&format!(
                "{name}: mehrere Varianten, aber keine automatische Wahl ({reason})"
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
    // Die regulaeren Slots: eine Spur fuer praemptierbare Arbeit ist keine
    // zusaetzliche Kapazitaet fuer einen unteilbaren Block (ADR-0035).
    let slots = resolved.slots.regular_len();
    for (i, contract) in resolved.contracts.iter().enumerate() {
        if contract.criticality.is_guarded() {
            continue;
        }
        let name = resolved.model_names.get(i).map_or("?", String::as_str);
        if let Some(Some(preemptible)) = resolved.preemptible.get(i) {
            ok(&format!(
                "{name}: laeuft praemptierbar auf einer eigenen Spur; geschuetzte Arbeit \
                 traegt waehrenddessen {} Restblockierung (ADR-0035)",
                preemptible.residual_blocking
            ));
            continue;
        }
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

/// Nennt den Preis der Zerlegung, statt sie als kostenlos darzustellen (NV-16).
///
/// ADR-0014 zerlegt einen generativen Auftrag in Quanten, und jedes Quantum
/// traegt den gewachsenen Prompt erneut ins Backend. Ohne wirksames
/// Prefix-Caching wird dieser Prompt jedes Mal neu berechnet — die Zerlegung
/// erzeugt dann quadratisch viel Arbeit, wo der ungeteilte Lauf linear
/// gewesen waere. Der `doctor` hat diese Zerlegung bisher als reine Abhilfe
/// gemeldet („wird aber in Quanten zerlegt") und ihren Preis verschwiegen.
///
/// Gerechnet wird mit Quanten in Mindestgroesse — der groessten Zahl von
/// Fortsetzungen, die dieser Vertrag zulaesst — und mit einem **leeren
/// Prompt**. Die tatsaechliche Quantengroesse haengt an der Luecke zur
/// naechsten geschuetzten Ankunft, die Promptlaenge am Aufrufer; beide stehen
/// zur Konfigurationszeit nicht fest.
///
/// Der genannte Aufschlag ist damit eine **Untergrenze**, nicht der
/// unguenstigste Fall. Der Prompt ist der dominierende Term der
/// Prefill-Summe: `n * prompt` gegen `total * (n-1) / 2` aus dem Wachstum. Bei
/// 16 Quanten und einem 500-Token-Prompt sind das 8000 gegen 480
/// Token-Prefills. Was der Betrieb sieht, ist also mehr als das hier — und
/// `worth_decomposing` im Gateway rechnet zur Laufzeit mit dem echten Prompt.
/// Ein Vertrag, den dieses Werkzeug gruen meldet, kann dort trotzdem als zu
/// teuer abgelehnt werden. Das steht deshalb im Text.
///
/// Das ist ein Hinweis und kein Fehler: ob ein Aufschlag tragbar ist,
/// entscheidet der Betreiber und nicht dieses Werkzeug.
fn check_decomposition_cost(resolved: &Resolved) -> Verdict {
    let mut verdict = Verdict::Ready;
    for (i, contract) in resolved.contracts.iter().enumerate() {
        let Some(cooperative) = contract.cooperative else {
            continue;
        };
        let name = resolved.model_names.get(i).map_or("?", String::as_str);
        let cost = cooperative.cost_model();
        let plan = Plan::project(
            cost,
            0,
            cooperative.max_total_tokens,
            cooperative.min_tokens,
        );
        if plan.quanta <= 1 {
            continue;
        }
        let overhead = plan.overhead_permille();
        let percent = overhead.checked_div(10).unwrap_or(0);

        // Dieselbe Schwelle, die auch `worth_decomposing` im Gateway
        // anwendet. Frueher schwieg diese Pruefung bei
        // `prefill_per_token_us == 0`, weil die Null zweideutig ist — waehrend
        // das Gateway denselben Vertrag ablehnte. Zwei Antworten auf eine
        // Konfiguration. `base_cost_us` ist ein Pflichtfeld und gemessen; der
        // Aufschlag aus Round-Trips allein ist eine belastbare Zahl. Die
        // Zweideutigkeit gehoert in den Text, nicht in die Bewertung.
        let ambiguous = if cooperative.prefill_per_token.as_nanos() == 0 {
            " (nur Round-Trips; prefill_per_token_us ist 0, also entweder \
             gemessen wirkungslos oder nicht gemessen)"
        } else {
            " — gerechnet ohne Prompt; mit einem echten Prompt ist es mehr"
        };
        if overhead > 1_000 {
            warn(&format!(
                "{name}: bis zu {} Quanten kosten mindestens {percent} % mehr \
                 Arbeit als der ungeteilte Lauf{ambiguous}. Abhilfe: groesseres \
                 min_tokens, kleineres max_total_tokens oder ein Backend mit \
                 wirksamem Prefix-Cache.",
                plan.quanta
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        } else {
            ok(&format!(
                "{name}: bis zu {} Quanten, mindestens {percent} % Aufschlag \
                 gegenueber dem ungeteilten Lauf{ambiguous}",
                plan.quanta
            ));
        }

        // Was die Zerlegung dem Nutzer bringt und was sie ihn kostet, in den
        // beiden Groessen, an denen ein generativer Dienst gemessen wird. Sie
        // laufen gegenlaeufig: kleine Quanten verkuerzen die Wartezeit auf das
        // erste Token und verlaengern den Abstand zwischen den spaeteren.
        //
        // Auch diese Zahlen rechnen ohne Prompt, und das steht im Text: bei
        // TTFT ist der Prompt-Prefill in einem echten Lauf der groesste
        // Einzelterm, die genannte Zahl also eine Untergrenze.
        let gap = worst_quantum_gap(resolved);
        let latencies = Latencies::project(
            cost,
            0,
            cooperative.max_total_tokens,
            cooperative.min_tokens,
            gap,
        );
        ok(&format!(
            "{name}: erstes Token fruehestens nach {} (TTFT ohne Prompt; mit \
             Prompt kommt dessen Prefill dazu), groesster Tokenabstand {} \
             (TBT, mit {gap} fuer die laengste dazwischenliegende geschuetzte \
             Arbeit)",
            latencies.ttft, latencies.worst_tbt
        ));
    }
    verdict
}

/// Die laengste Wartezeit, die ein Quantum zwischen zwei Fortsetzungen
/// einplanen muss.
///
/// Zwischen zwei Quanten laeuft die geschuetzte Arbeit, fuer die die Zerlegung
/// ueberhaupt gemacht wird. Was das kostet, ist ihre **Ausfuehrungszeit** und
/// nicht ihre Periode — die Periode sagt, wie oft sie kommt, nicht wie lange
/// sie dauert. Und gesucht ist die **laengste**, nicht die kuerzeste: der
/// unguenstigste Fall ist der, in dem das teuerste geschuetzte Modell
/// dazwischenkommt.
///
/// Eine Naeherung bleibt es trotzdem: kommen mehrere geschuetzte Ankuenfte
/// zwischen zwei Quanten, ist die Wartezeit laenger. Der Wert ist eine
/// Untergrenze, und der Bericht sagt das auch.
fn worst_quantum_gap(resolved: &Resolved) -> Duration {
    resolved
        .contracts
        .iter()
        .filter(|c| c.criticality.is_guarded())
        .filter_map(|c| {
            c.variants
                .get(0)
                .and_then(|v| v.profile.conservative_at(0, resolved.margin).ok())
        })
        .max_by_key(|d: &Duration| d.as_nanos())
        .unwrap_or(Duration::ZERO)
}

/// Nennt Vertragsforderungen, die gespeichert und nicht durchgesetzt werden
/// (Review R05).
///
/// Ein gueltiges YAML-Dokument ist kein angenommener Betriebsvertrag. Wer eine
/// Zusage aufschreibt, die niemand einloest, soll es hier erfahren und nicht
/// beim ersten Vorfall. Eine Warnung und kein Fehler: die Konfiguration ist
/// lesbar und der Rest gilt — nur eben dieser Teil nicht.
fn check_unenforced_requirements(resolved: &Resolved) -> Verdict {
    let mut verdict = Verdict::Ready;
    for (i, contract) in resolved.contracts.iter().enumerate() {
        let Some(extension) = contract.extension.as_ref() else {
            continue;
        };
        let name = resolved.model_names.get(i).map_or("?", String::as_str);
        let guarded = contract.criticality.is_guarded();
        if guarded && let Some(envelope) = extension.release_jitter_envelope {
            ok(&format!(
                "{name}: release_jitter_ms {} — der Look-ahead haelt eine \
                 ueberfaellige Ankunft bis zu {} ms ueber die erwartete \
                 hinaus offen, statt den Frame aufzugeben (ADR-0036). \
                 Gemessen wird der Jitter nicht.",
                envelope.as_millis(),
                envelope.as_millis().saturating_mul(2)
            ));
        }
        for item in extension.unenforced(guarded).iter() {
            warn(&format!(
                "{name}: {} ist im Vertrag gefordert und wird nicht \
                 durchgesetzt — {}. Die uebrigen Zusagen gelten; diese nicht.",
                item.field, item.reason
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        }
    }
    verdict
}

/// Nennt die Praemption und prueft, ob ihre Restblockierung in den Slack
/// passt (ADR-0035).
///
/// Eine angegebene statt gemessene Restblockierung ist eine Behauptung ueber
/// den Backendaufbau; der Betreiber soll wissen, dass sie keine Messung ist.
/// Und passt sie nicht in die Frist einer geschuetzten Arbeit, haelt der
/// Look-ahead die Hintergrundarbeit weiter zurueck — die Spur ist dann
/// konfiguriert und wirkungslos.
fn check_preemption(resolved: &Resolved) -> Verdict {
    let mut verdict = Verdict::Ready;
    for (i, entry) in resolved.preemptible.iter().enumerate() {
        let Some(preemptible) = entry else {
            continue;
        };
        let name = resolved.model_names.get(i).map_or("?", String::as_str);
        let residual = preemptible.residual_blocking;
        if preemptible.measured {
            ok(&format!(
                "{name}: praemptierbar, Restblockierung {residual} (gemessen)"
            ));
        } else {
            warn(&format!(
                "{name}: praemptierbar mit angegebener Restblockierung {residual} — nicht \
                 gemessen. `vig calibrate` misst sie gegen den laufenden Aufbau"
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        }
        for (j, contract) in resolved.contracts.iter().enumerate() {
            if !contract.criticality.is_guarded() {
                continue;
            }
            let Some(best) = contract.variants.get(0) else {
                continue;
            };
            let Ok(runtime) = best.profile.conservative_at(0, resolved.margin) else {
                continue;
            };
            let Some(burdened) = runtime.checked_add(residual) else {
                continue;
            };
            if burdened > contract.deadline {
                let guarded = resolved.model_names.get(j).map_or("?", String::as_str);
                warn(&format!(
                    "{guarded}: mit der Restblockierung von {name} ({runtime} + {residual}) \
                     passt die Laufzeit nicht mehr in die Frist {}; der Look-ahead haelt \
                     {name} dann zurueck, und die Spur bleibt wirkungslos",
                    contract.deadline
                ));
                verdict = verdict.max(Verdict::ReadyWithWarnings);
            }
        }
    }
    verdict
}

/// Auslastung, Best-Effort-Machbarkeit und Spuren — je GPU (NV-22, ADR-0037).
///
/// Eine geschuetzte Auslastung ist eine Eigenschaft **einer** GPU. Ueber zwei
/// Domaenen gerechnet stuende ein ueberzeichneter Detektor auf GPU 1 neben
/// einer leeren GPU 0 als halb ausgelastet da.
fn check_capacity(resolved: &Resolved) -> Verdict {
    if resolved.domains.is_empty() {
        return check_utilization(resolved)
            .max(check_best_effort_feasibility(resolved))
            .max(check_preemption(resolved));
    }
    warn(&format!(
        "{} Ressourcendomaenen: erreichbar, nicht qualifiziert — geprueft ist die \
         Logik mit Fake-Backends, nicht das Verhalten zweier GPUs (ADR-0037)",
        resolved.domains.len()
    ));
    let mut verdict = Verdict::ReadyWithWarnings;
    for domain in &resolved.domains {
        let models: Vec<&str> = domain
            .resolved
            .model_names
            .iter()
            .map(String::as_str)
            .collect();
        ok(&format!(
            "Domaene {}: GPU {}, {} Slot(s), Endpunkte {}, Modelle {}",
            domain.name,
            domain.gpu_index,
            domain.resolved.slots.regular_len(),
            domain.resolved.endpoints().join(", "),
            models.join(", ")
        ));
        verdict = verdict
            .max(check_utilization(&domain.resolved))
            .max(check_best_effort_feasibility(&domain.resolved))
            .max(check_preemption(&domain.resolved));
    }
    verdict
}

/// Die einfache Demand-Warnung aus Spec 10.9: `U = Summe(C_i / T_i)`.
///
/// Keine vollstaendige Schedulability-Garantie — bei realer GPU-Konkurrenz und
/// nicht-praeemptiven Abschnitten waere das falsch. Aber sehr nuetzlich, um
/// offensichtlich unmoegliche Vertraege vor dem Start zu erkennen.
///
/// Mindestlaufzeitbudgets (ADR-0046) zaehlen dazu: sie werden aus der Zeit
/// bedient, die die bewachte Arbeit uebrig laesst. Passen beide zusammen nicht
/// in die Slots, ist das Budget eine Zusage, die niemand haelt — die bewachte
/// Arbeit gibt dafuer nichts ab.
fn check_utilization(resolved: &Resolved) -> Verdict {
    let utilization = resolved.protected_utilization_permille();
    let percent = utilization.checked_div(10).unwrap_or(0);
    let slots = resolved.slots.regular_len();
    let budgets = resolved.runtime_budget_permille();
    let budget_percent = budgets.checked_div(10).unwrap_or(0);
    let reserved = utilization.saturating_add(budgets);
    // ADR-0047: Zusagen kommen obendrauf, werden aber getrennt gefuehrt.
    // Eine Sammelsumme wuerde jede Meldung unscharf machen — der Betreiber
    // soll lesen koennen, *woran* es liegt, nicht nur *dass* es klemmt.
    let objectives = resolved.objective_utilization_permille();
    let objective_percent = objectives.checked_div(10).unwrap_or(0);
    let committed = reserved.saturating_add(objectives);

    if utilization > 1_000 {
        fail(&format!(
            "PROTECTED_WORKLOAD_UNSCHEDULABLE: geschuetzte Auslastung {percent} %, \
             ueber {slots} Slot(s) nicht tragbar. Best-Effort-Arbeit kommt damit \
             strukturell nie zum Zug."
        ));
        Verdict::NotReady
    } else if budgets > 0 && reserved > 1_000 {
        fail(&format!(
            "RUNTIME_BUDGET_UNSCHEDULABLE: geschuetzte Auslastung {percent} % plus \
             Mindestlaufzeitbudgets {budget_percent} % ueber {slots} Slot(s) nicht \
             tragbar. Die Budgets werden nur aus dem Rest der bewachten Arbeit \
             bedient und bleiben so unerfuellt (ADR-0046)."
        ));
        Verdict::NotReady
    } else if objectives > 0 && committed > 1_000 {
        fail(&format!(
            "OBJECTIVE_UNSCHEDULABLE: geschuetzte Auslastung {percent} % plus \
             Mindestlaufzeitbudgets {budget_percent} % plus Zusagen \
             {objective_percent} % ueber {slots} Slot(s) nicht tragbar. Die \
             Zusagen werden aus dem Rest bedient und bleiben so unerfuellt \
             (ADR-0047)."
        ));
        Verdict::NotReady
    } else if objectives > 0 && committed > 800 {
        warn(&format!(
            "geschuetzte serialisierte Auslastung {percent} % plus \
             Mindestlaufzeitbudgets {budget_percent} % plus Zusagen \
             {objective_percent} %; fuer die uebrige nachrangige Arbeit bleibt \
             kaum Reserve"
        ));
        Verdict::ReadyWithWarnings
    } else if objectives > 0 {
        ok(&format!(
            "geschuetzte serialisierte Auslastung {percent} % plus Zusagen \
             {objective_percent} %"
        ));
        Verdict::Ready
    } else if budgets > 0 && reserved > 800 {
        warn(&format!(
            "geschuetzte serialisierte Auslastung {percent} % plus \
             Mindestlaufzeitbudgets {budget_percent} %; fuer die uebrige \
             nachrangige Arbeit bleibt kaum Reserve"
        ));
        Verdict::ReadyWithWarnings
    } else if budgets > 0 {
        ok(&format!(
            "geschuetzte serialisierte Auslastung {percent} % plus \
             Mindestlaufzeitbudgets {budget_percent} %"
        ));
        Verdict::Ready
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
/// Nennt, ob und wie sich die Planung an der Karte kalibriert (ADR-0038).
///
/// Eine Warnung, solange auf echter Hardware nicht gemessen ist, was die
/// Kalibrierung tut: erreichbar ist nicht qualifiziert.
fn check_margin_learning(resolved: &Resolved) -> Verdict {
    let Some(learning) = resolved.margin_learning else {
        return Verdict::Ready;
    };
    warn(&format!(
        "Planung kalibriert sich an der Karte (ADR-0038): Faktor auf das Profil-p99 \
         zwischen {} und {} %, Start bei {} %,\n     mutiger erst nach {} Ausfuehrungen \
         je Modell, nie unter dem beobachteten Median. Nicht qualifiziert: auf \
         echter Hardware\n     noch nicht gemessen.",
        learning.min_factor_percent(),
        learning.max_factor_percent(),
        resolved.margin.as_percent(),
        learning.min_observations(),
    ));
    Verdict::ReadyWithWarnings
}

fn check_security(resolved: &Resolved) -> Verdict {
    let s = &resolved.security;
    let mut verdict = Verdict::Ready;

    let identity_checked = s.client_ca.is_some() || s.token_file.is_some();
    if s.tls_enabled() {
        if s.client_ca.is_some() {
            ok("TLS mit Clientzertifikaten (mTLS)");
        } else if s.token_file.is_some() {
            ok("TLS aktiv; Clients werden per Token geprueft");
        } else {
            // Security-Review M3: TLS allein galt hier als geschuetzt.
            warn(
                "TLS aktiv, aber niemand wird geprueft: TLS verschluesselt, es \
                 authentifiziert nicht. `serve` verweigert damit einen Start \
                 ausserhalb von Loopback (ohne --insecure-open); `client_ca` oder \
                 `token_file` einrichten.",
            );
            verdict = verdict.max(Verdict::ReadyWithWarnings);
        }
    }
    if s.token_file.is_some() {
        ok("Bearer-Token-Pruefung eingerichtet");
    }
    if !s.tls_enabled() && s.token_file.is_none() {
        warn(
            "Keine Zugangspruefung eingerichtet. Das ist fuer Loopback in Ordnung — \
             `serve` bindet dort per Voreinstellung und verweigert ohne \
             Identitaetspruefung jeden anderen Start (ausser mit --insecure-open). \
             Soll der Endpunkt ins Netz, `backend.security` einrichten.",
        );
        verdict = verdict.max(Verdict::ReadyWithWarnings);
    }
    if s.admin_token_file.is_some() {
        ok("Administrationsendpunkte nur mit Administrationstoken");
    } else {
        ok("Administrationsendpunkte gesperrt (kein admin_token_file)");
    }
    if !identity_checked && resolved.trust == vig_config::schema::TrustMode::Strict {
        fail(
            "trust: strict ohne Identitaetspruefung: der strikte Modus schuetzt vor \
             fremden Aufrufern, kann sie ohne mTLS (`client_ca`) oder Token \
             (`token_file`) aber nicht unterscheiden — auch Shared-Memory-Regionen \
             gehoeren dann allen gemeinsam.",
        );
        verdict = verdict.max(Verdict::NotReady);
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

/// Die fachliche Austauschbarkeit der Varianten (NV-10).
///
/// Die I/O-Signatur faengt den Fall, in dem ein Variantenwechsel den Client
/// garantiert bricht. Sie faengt nicht den schlimmeren: gleiche Form,
/// gleiche Namen, andere Bedeutung. Zwei Detektoren mit denselben Klassen in
/// anderer Reihenfolge liefern Zahlen, die aussehen wie erwartet und etwas
/// anderes heissen — und das faellt erst auf, wenn ein Roboter danach greift.
fn check_semantics(resolved: &Resolved) -> Verdict {
    let mut verdict = Verdict::Ready;
    for (index, name) in resolved.model_names.iter().enumerate() {
        let Some(contract) = resolved.contracts.get(index) else {
            continue;
        };
        if contract.variants.len() < 2 {
            continue;
        }
        let described = contract.variants.iter().any(|v| v.semantics.is_specified());
        if !described {
            warn(&format!(
                "{name}: Varianten ohne Semantikangabe. Austauschbarkeit haengt \
                 dann allein an der I/O-Signatur — gleiche Form bei anderer \
                 Bedeutung faellt nicht auf. Abhilfe: `semantics` je Variante."
            ));
            verdict = verdict.max(Verdict::ReadyWithWarnings);
            continue;
        }
        match contract.semantic_conflict() {
            None => ok(&format!("{name}: Varianten fachlich austauschbar")),
            Some((left, right, conflict)) => {
                let label = |idx: vig_core::ids::VariantIdx| {
                    resolved
                        .backend_models
                        .get(index)
                        .and_then(|v| v.get(idx.get()))
                        .map_or_else(|| idx.to_string(), Clone::clone)
                };
                warn(&format!(
                    "{name}: {} und {} sind fachlich nicht austauschbar ({conflict}). \
                     Die automatische Variantenwahl bleibt aus; es gilt die \
                     freigegebene feste Variante.",
                    label(left),
                    label(right)
                ));
                verdict = verdict.max(Verdict::ReadyWithWarnings);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{
        Verdict, check_capacity, check_decomposition_cost, check_preemption, check_security,
        check_supply_guard_vs_background,
    };

    /// ADR-0046: Mindestlaufzeitbudgets zaehlen zur geschuetzten Auslastung.
    ///
    /// Zwei Slots, geschuetzt 30 ms konservativ je 50 ms (600 ‰ eines Slots,
    /// 300 ‰ ueber beide). 600 ms je Sekunde passen noch (300 + 300 = 600 ‰),
    /// 1500 ms nicht (300 + 750 = 1050 ‰).
    #[test]
    fn runtime_budgets_count_against_the_slots() {
        let yaml = |budget_ms: u64| {
            format!(
                r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:9201"
  slots: 2
models:
  front:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 50, deadline_ms: 50, max_age_ms: 100 }}
    variants:
      - id: main
        backend_model: front_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 27272, p95_us: 27272, p99_us: 27272, samples: 100 }}
  vlm:
    class: best_effort
    queue: {{ policy: fifo, capacity: 2 }}
    contract:
      deadline_ms: 5000
      max_age_ms: 8000
      min_runtime: {{ budget_ms: {budget_ms}, window_ms: 1000 }}
    variants:
      - id: main
        backend_model: vlm_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 120000, p95_us: 150000, p99_us: 170000, samples: 100 }}
"#
            )
        };
        let resolve = |y: String| Config::from_yaml(&y).unwrap().resolve().unwrap();
        let fits = resolve(yaml(600));
        assert_eq!(fits.runtime_budget_permille(), 300);
        assert_eq!(check_capacity(&fits), Verdict::Ready);
        let too_much = resolve(yaml(1_500));
        assert_eq!(too_much.runtime_budget_permille(), 750);
        assert_eq!(check_capacity(&too_much), Verdict::NotReady);
    }

    /// ADR-0047: Zusagen zaehlen gegen dieselben Slots.
    ///
    /// Geschuetzt sind 30 ms konservativ je 50 ms — 600 ‰ eines Slots. Die
    /// nachrangige Kamera rechnet 66 ms konservativ (p99 60 ms mal Marge) bei
    /// 100 ms Takt, bleibt damit unter dem Hoechstalter der geschuetzten
    /// Kamera und loest keinen Blockierbefund aus (ADR-0035).
    ///
    /// Zwei Slots und 200 ‰ Zusage passen: 300 ‰ plus 66 ‰. Ein Slot und
    /// volle Zusage passen nicht: 600 ‰ plus 660 ‰.
    #[test]
    fn objectives_count_against_the_slots() {
        let yaml = |permille: u16, slots: usize| {
            format!(
                r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:9201"
  slots: {slots}
models:
  front:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 50, deadline_ms: 50, max_age_ms: 100 }}
    variants:
      - id: main
        backend_model: front_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 27272, p95_us: 27272, p99_us: 27272, samples: 100 }}
  rear:
    class: normal
    queue: {{ policy: latest, capacity: 1 }}
    contract:
      period_ms: 100
      deadline_ms: 200
      max_age_ms: 400
      objective: {{ coverage_permille: {permille}, window_ms: 10000 }}
    variants:
      - id: main
        backend_model: rear_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 60000, p95_us: 60000, p99_us: 60000, samples: 100 }}
"#
            )
        };
        let resolve = |y: String| Config::from_yaml(&y).unwrap().resolve().unwrap();

        let fits = resolve(yaml(200, 2));
        assert_eq!(fits.objective_utilization_permille(), 66);
        assert_eq!(check_capacity(&fits), Verdict::Ready);

        // Ein Slot, volle Zusage: 600 ‰ geschuetzt plus 660 ‰ Zusage.
        let too_much = resolve(yaml(1_000, 1));
        assert_eq!(too_much.objective_utilization_permille(), 660);
        assert_eq!(check_capacity(&too_much), Verdict::NotReady);
    }

    /// ADR-0043: Zwei Zusagen, die sich ausschliessen, sollen beim Start
    /// auffallen und nicht im Betrieb abwechselnd gebrochen werden.
    ///
    /// Geschuetzt: Hoechstalter 100 ms, konservative Laufzeit 22 ms (p99
    /// 20 ms mal Marge 110 %) — es bleibt eine Luecke von 100 - 44 = 56 ms.
    /// Der Hintergrund fordert Mindestfortschritt; mit 60 ms p99 (66 ms
    /// konservativ) passt er nicht hinein, mit 20 ms (22 ms) schon.
    #[test]
    fn a_background_promise_that_the_supply_guard_cannot_keep_is_refused() {
        let yaml = |background_us: u64, protect: bool| {
            format!(
                r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:9201"
  slots: 1
  protect_supply: {protect}
models:
  detector:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 33, deadline_ms: 33, max_age_ms: 100 }}
    variants:
      - id: main
        backend_model: detector_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 20000, p95_us: 20000, p99_us: 20000, samples: 100 }}
  report:
    class: best_effort
    queue: {{ policy: latest, capacity: 1 }}
    contract:
      period_ms: 1000
      deadline_ms: 1000
      max_age_ms: 2000
      extension:
        version: 1
        consumer_period_ms: 1000
        observation_window: 100
        minimum_background_progress_pct: 20
    variants:
      - id: main
        backend_model: report_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: {background_us}, p95_us: {background_us}, p99_us: {background_us}, samples: 100 }}
"#
            )
        };
        let resolve = |y: String| Config::from_yaml(&y).unwrap().resolve().unwrap();
        assert_eq!(
            check_supply_guard_vs_background(&resolve(yaml(60_000, true))),
            Verdict::NotReady,
            "60 ms passen nicht in eine Luecke von 56 ms"
        );
        assert_eq!(
            check_supply_guard_vs_background(&resolve(yaml(20_000, true))),
            Verdict::Ready,
            "20 ms passen hinein"
        );
        assert_eq!(
            check_supply_guard_vs_background(&resolve(yaml(60_000, false))),
            Verdict::Ready,
            "ohne Versorgungsschutz gibt es nichts zu pruefen"
        );
    }

    /// NV-22: die geschuetzte Auslastung ist eine Eigenschaft einer GPU. Ein
    /// ueberzeichneter Detektor auf GPU 1 faellt auf, obwohl GPU 0 fast leer
    /// ist — ueber die Anlage gemittelt waere er durchgerutscht.
    #[test]
    fn capacity_is_checked_one_gpu_at_a_time() {
        let yaml = |p99_us: u64| {
            format!(
                r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:9201"
  slots: 1
  domains:
    gpu1:
      gpu_index: 1
      grpc_endpoint: "127.0.0.1:9301"
      slots: 1
models:
  detector:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 100, deadline_ms: 100, max_age_ms: 200 }}
    variants:
      - id: main
        backend_model: detector_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 1000, p95_us: 1000, p99_us: 1000, samples: 100 }}
  pose:
    class: protected
    domain: gpu1
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 33, deadline_ms: 33, max_age_ms: 66 }}
    variants:
      - id: main
        backend_model: pose_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: {p99_us}, p95_us: {p99_us}, p99_us: {p99_us}, samples: 100 }}
"#
            )
        };
        let fits = Config::from_yaml(&yaml(10_000)).unwrap().resolve().unwrap();
        assert_eq!(
            check_capacity(&fits),
            Verdict::ReadyWithWarnings,
            "nicht qualifiziert"
        );
        let overloaded = Config::from_yaml(&yaml(40_000)).unwrap().resolve().unwrap();
        assert_eq!(check_capacity(&overloaded), Verdict::NotReady);
    }
    use vig_config::Config;
    use vig_config::schema::{Resolved, SecurityConfig, TrustMode};

    /// Security-Review M3: TLS allein verschluesselt, prueft aber niemanden.
    /// Der `doctor` darf das nicht als geschuetzt melden — und `strict` ohne
    /// Identitaetspruefung ist nicht bereit, weil der Modus Aufrufer
    /// unterscheiden soll, die er gar nicht kennt.
    #[test]
    fn tls_alone_is_not_an_identity_check() {
        // Strikt, damit die Warnung zu `trust: open` nichts ueberdeckt.
        let mut resolved = resolved_with(200, 4);
        resolved.trust = TrustMode::Strict;

        resolved.security.tls_cert = Some("server.pem".into());
        resolved.security.tls_key = Some("server.key".into());
        assert_eq!(
            check_security(&resolved),
            Verdict::NotReady,
            "TLS allein prueft niemanden"
        );

        resolved.security.client_ca = Some("clients.pem".into());
        assert_eq!(check_security(&resolved), Verdict::Ready, "mTLS prueft");

        resolved.security = SecurityConfig::default();
        assert_eq!(check_security(&resolved), Verdict::NotReady, "gar nichts");

        resolved.security.token_file = Some("tokens".into());
        assert_eq!(check_security(&resolved), Verdict::Ready, "Token pruefen");

        // Offen und nur TLS: bereit, aber nicht ohne Warnung.
        resolved.trust = TrustMode::Open;
        resolved.security = SecurityConfig::default();
        resolved.security.tls_cert = Some("server.pem".into());
        resolved.security.tls_key = Some("server.key".into());
        assert_eq!(check_security(&resolved), Verdict::ReadyWithWarnings);
    }

    /// Ein Detektor (15 ms p99 bei 110 % Marge, 33 ms Frist) und ein
    /// praemptierbares VLM mit dieser Restblockierung und Herkunft.
    fn preemptible_with(residual_us: u64, source: &str) -> Resolved {
        let yaml = format!(
            r#"
version: 1
backend:
  type: triton
  grpc_endpoint: "127.0.0.1:9201"
  slots: 1
  preemptible_lanes: 1
models:
  detector:
    class: protected
    queue: {{ policy: latest, capacity: 1 }}
    contract: {{ period_ms: 33, deadline_ms: 33, max_age_ms: 66 }}
    variants:
      - id: main
        backend_model: rfdetr
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 13000, p95_us: 14000, p99_us: 15000, samples: 100 }}
  vlm:
    class: best_effort
    backend_endpoint: "127.0.0.1:9101"
    preemptible: {{ residual_blocking_us: {residual_us}, source: {source} }}
    queue: {{ policy: fifo, capacity: 4 }}
    contract: {{ deadline_ms: 800, max_age_ms: 1500 }}
    variants:
      - id: main
        backend_model: vlm_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 90000, p95_us: 95000, p99_us: 99000, samples: 100 }}
"#
        );
        Config::from_yaml(&yaml).unwrap().resolve().unwrap()
    }

    /// ADR-0035: gemessen und passend ist unauffaellig; angegeben statt
    /// gemessen ist eine Warnung; eine Restblockierung, die die Frist des
    /// Detektors reisst (16,5 + 20 ms gegen 33 ms), auch — die Spur waere
    /// konfiguriert und wirkungslos.
    #[test]
    fn preemption_is_reported_and_checked_against_the_deadline() {
        assert_eq!(
            check_preemption(&preemptible_with(5_000, "measured")),
            Verdict::Ready
        );
        assert_eq!(
            check_preemption(&preemptible_with(5_000, "declared")),
            Verdict::ReadyWithWarnings
        );
        assert_eq!(
            check_preemption(&preemptible_with(20_000, "measured")),
            Verdict::ReadyWithWarnings
        );
    }

    /// Ein Vertrag mit dem angegebenen Prefill-Anteil.
    fn resolved_with(prefill_per_token_us: u64, min_tokens: u32) -> Resolved {
        let yaml = format!(
            r"
version: 1
backend:
  type: triton
  grpc_endpoint: 127.0.0.1:8001
  slots: 1
models:
  vlm:
    class: best_effort
    queue: {{ policy: fifo, capacity: 8 }}
    contract: {{ deadline_ms: 30000 }}
    cooperative:
      tokens_per_second: 242
      min_tokens: {min_tokens}
      max_total_tokens: 64
      base_cost_us: 18000
      prefill_per_token_us: {prefill_per_token_us}
    variants:
      - id: main
        backend_model: vlm_main
        quality: {{ value: 1.0, source: measured }}
        profile: {{ p50_us: 100000, p95_us: 120000, p99_us: 130000, samples: 100 }}
"
        );
        Config::from_yaml(&yaml).unwrap().resolve().unwrap()
    }

    /// Ein gemessener Prefill-Anteil, der die Zerlegung teuer macht, wird
    /// gemeldet.
    ///
    /// Der Fall, den NV-16 sichtbar macht: 16 Quanten a 4 Token, jedes traegt
    /// den gewachsenen Prompt erneut. Der `doctor` hat das bis dahin als reine
    /// Abhilfe gemeldet und den Preis verschwiegen.
    #[test]
    fn a_costly_decomposition_is_reported() {
        let verdict = check_decomposition_cost(&resolved_with(200, 4));
        assert_eq!(
            verdict,
            Verdict::ReadyWithWarnings,
            "ein Aufschlag ueber 100 % gehoert in den Bericht"
        );
    }

    /// Auch ohne gemessenen Prefill-Anteil wird gewarnt, wenn der Aufschlag
    /// die Schwelle reisst.
    ///
    /// Der Befund aus dem Nachreview: diese Pruefung schwieg frueher bei
    /// `prefill_per_token_us == 0`, weil die Null zweideutig ist — waehrend
    /// `worth_decomposing` im Gateway denselben Vertrag ablehnte. Zwei
    /// Antworten auf eine Konfiguration. `base_cost_us` ist ein Pflichtfeld
    /// und gemessen; ein Aufschlag von 197 % aus reinen Round-Trips ist eine
    /// belastbare Zahl. Die Zweideutigkeit steht jetzt im Text.
    #[test]
    fn round_trips_alone_can_already_break_the_threshold() {
        // 32 Quanten zu je 2 Token, 18 ms Sockel: knapp 200 % Aufschlag,
        // ganz ohne Prefill-Term.
        assert_eq!(
            check_decomposition_cost(&resolved_with(0, 2)),
            Verdict::ReadyWithWarnings,
            "der Sockel allein traegt diese Warnung"
        );
        // Und bei 16 Quanten bleibt es darunter — die Schwelle ist eine
        // Schwelle und kein Dauerzustand.
        assert_eq!(
            check_decomposition_cost(&resolved_with(0, 4)),
            Verdict::Ready
        );
    }

    /// Ein Vertrag, der gar nicht zerlegt, wird nicht bewertet.
    ///
    /// `min_tokens = max_total_tokens` heisst: ein einziges Quantum. Dann gibt
    /// es keinen Aufschlag, ueber den zu reden waere.
    #[test]
    fn a_contract_that_never_splits_is_not_judged() {
        assert_eq!(
            check_decomposition_cost(&resolved_with(200, 64)),
            Verdict::Ready
        );
    }
}
