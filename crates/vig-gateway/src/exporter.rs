//! Prometheus-Export der Scheduler-Kennzahlen (WP13, Spec 18).
//!
//! ## Warum die Zahlen ueberhaupt hinaus muessen
//!
//! Der Governor trifft Entscheidungen, die von aussen wie ein Ausfall
//! aussehen: ein Request wird verworfen, ein Ergebnis nicht geliefert, ein
//! Hintergrundjob laeuft nie. Ohne Kennzahlen ist ein Betreiber nicht in der
//! Lage, eine absichtliche Supersession von einem Fehler zu unterscheiden —
//! und wird dem System zu Recht misstrauen.
//!
//! Die wichtigsten Werte sind deshalb nicht die Durchsatzzahlen, sondern die,
//! die zeigen, **was das System bewusst nicht getan hat**:
//! `superseded`, `stale`, `deferred_for_protected`, `best_effort_starved`.
//!
//! ## Was hier fehlt
//!
//! Histogramme. Age of Information und Queue-Wartezeit brauchen Quantile ueber
//! ein Zeitfenster, und die gehoeren nicht in einen `Copy`-Zaehlersatz. Die
//! Abdeckungsmetrik aus ADR-0005 braucht zusaetzlich die Periodenzuordnung.
//! Beides ist bewusst offen und nicht halb umgesetzt: eine Metrik, die
//! aussieht wie ein Quantil und keines ist, waere schlimmer als keine.

use crate::actor::Handle;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use std::fmt::Write as _;
use std::net::SocketAddr;
use vig_core::Metrics;

/// Rendert die Kennzahlen im Prometheus-Textformat.
///
/// Rein und ohne I/O, damit das Format ohne laufenden Server pruefbar ist.
#[must_use]
pub fn render(metrics: &Metrics) -> String {
    let mut out = String::with_capacity(4_096);

    let mut counter = |name: &str, help: &str, value: u64| {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} counter");
        let _ = writeln!(out, "{name} {value}");
    };

    counter(
        "vig_requests_received_total",
        "Am Gateway angenommene Requests.",
        metrics.received,
    );
    counter(
        "vig_requests_forwarded_total",
        "An das Backend weitergereichte Requests.",
        metrics.forwarded,
    );
    counter(
        "vig_requests_superseded_total",
        "Durch einen juengeren Request desselben Scopes ersetzte Requests.",
        metrics.superseded,
    );
    counter(
        "vig_requests_stale_total",
        "Wegen Ueberalterung verworfene Requests.",
        metrics.stale,
    );
    counter(
        "vig_requests_rejected_infeasible_total",
        "Als nicht mehr rechtzeitig machbar abgelehnte Requests.",
        metrics.rejected_infeasible,
    );
    counter(
        "vig_requests_rejected_capacity_total",
        "Wegen erschoepfter Queue-Kapazitaet abgelehnte Requests.",
        metrics.rejected_capacity,
    );
    counter(
        "vig_requests_completed_valid_total",
        "Fertiggestellt und bei Fertigstellung noch aktuell.",
        metrics.completed_valid,
    );
    counter(
        "vig_requests_completed_obsolete_total",
        "Fertiggestellt, aber bei Fertigstellung bereits obsolet.",
        metrics.completed_obsolete,
    );
    counter(
        "vig_backend_failures_total",
        "Fehlgeschlagene Backendaufrufe.",
        metrics.backend_failures,
    );
    counter(
        "vig_backend_timeouts_total",
        "Backendaufrufe, die das Inferenztimeout ueberschritten haben.",
        metrics.backend_timeouts,
    );
    counter(
        "vig_requests_cancelled_total",
        "Vom Client zurueckgezogene Requests, die noch warteten.",
        metrics.cancelled,
    );
    counter(
        "vig_deadline_misses_total",
        "Verletzte Deadlines ueber alle Klassen.",
        metrics.deadline_misses,
    );
    counter(
        "vig_protected_deadline_misses_total",
        "Verletzte Deadlines geschuetzter Klassen.",
        metrics.protected_deadline_misses,
    );
    counter(
        "vig_dispatched_late_total",
        "Requests, die trotz verfehlbarer Deadline gestartet wurden (ADR-0009).",
        metrics.dispatched_late,
    );
    counter(
        "vig_deferred_for_protected_total",
        "Veto-Ereignisse des Protected-Look-ahead; absichtliches Idle (Spec 10.7).",
        metrics.deferred_for_protected,
    );
    counter(
        "vig_best_effort_starved_total",
        "Best-Effort-Requests, die terminal wurden, ohne je gelaufen zu sein (ADR-0012).",
        metrics.best_effort_starved,
    );
    render_generative(&mut out, metrics);

    render_derived(&mut out, metrics);
    out
}

/// Die Kennzahlen der kooperativen Zerlegung (NV-16).
///
/// Sie stehen zusammen, weil sie nur zusammen etwas sagen: Prefill gegen
/// Dekodierung ist das Verhaeltnis, an dem sich entscheidet, ob die Zerlegung
/// noch traegt.
fn render_generative(out: &mut String, metrics: &Metrics) {
    let mut counter = |name: &str, help: &str, value: u64| {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} counter");
        let _ = writeln!(out, "{name} {value}");
    };
    counter(
        "vig_generative_prefill_us_total",
        "Arbeit in wiederholten Prefill-Berechnungen; erzeugt kein Token (NV-16).",
        metrics.generative_prefill_us,
    );
    counter(
        "vig_generative_decode_us_total",
        "Arbeit, die tatsaechlich Token erzeugt hat (NV-16).",
        metrics.generative_decode_us,
    );
    counter(
        "vig_generative_fixed_us_total",
        "Feste Kosten der Quanten: Round-Trip und Scheduling im Backend (NV-16).",
        metrics.generative_fixed_us,
    );
    counter(
        "vig_decomposition_refused_total",
        "Auftraege, die ungeteilt liefen, weil die Zerlegung zu teuer war (NV-16).",
        metrics.decomposition_refused,
    );
    // Ein Hoechststand, keine Summe: der laengste Kontext faellt nie, aber er
    // beschreibt einen Zustand und keinen Zaehler.
    let name = "vig_generative_context_tokens";
    let _ = writeln!(
        out,
        "# HELP {name} Laengster Kontext, den eine Fortsetzung getragen hat, in Token (NV-16)."
    );
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {}", metrics.generative_context_tokens);
}

/// Zeitsummen und abgeleitete Verhaeltnisse.
/// Rendert eine Kennzahl je Modell als Gauge.
///
/// Nur die belegten Slots: eine Zeitreihe je unbenutztem Modellslot kostet in
/// Prometheus dauerhaft Speicher und macht jede Abfrage unleserlich.
fn render_per_model(out: &mut String, name: &str, help: &str, values: &[u32], count: usize) {
    // Ohne Modelle gibt es keine Reihe. Eine Metrikfamilie mit HELP und TYPE
    // und ohne einen einzigen Messwert ist zwar formal zulaessig, sagt aber
    // nichts — und je mehr Reihen dazukommen, desto mehr Rauschen steht in
    // einem Abzug, der nichts enthaelt.
    if count == 0 {
        return;
    }
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    for (index, value) in values.iter().take(count).enumerate() {
        let _ = writeln!(out, "{name}{{model=\"{index}\"}} {value}");
    }
}

fn render_derived(out: &mut String, metrics: &Metrics) {
    let _ = writeln!(
        out,
        "# HELP vig_stale_compute_seconds_total Backendzeit, die in bei \
         Fertigstellung bereits obsolete Ergebnisse floss."
    );
    let _ = writeln!(out, "# TYPE vig_stale_compute_seconds_total counter");
    let _ = writeln!(
        out,
        "vig_stale_compute_seconds_total {}",
        seconds(metrics.stale_compute_nanos)
    );
    let _ = writeln!(
        out,
        "# HELP vig_backend_compute_seconds_total Gesamte verbrauchte Backendzeit."
    );
    let _ = writeln!(out, "# TYPE vig_backend_compute_seconds_total counter");
    let _ = writeln!(
        out,
        "vig_backend_compute_seconds_total {}",
        seconds(metrics.total_compute_nanos)
    );

    // Abgeleitete Groessen aus Spec 18.1 und 18.2. Sie sind aus den Zaehlern
    // berechenbar, werden aber mit ausgegeben: sie sind die Werte, die ein
    // Betreiber tatsaechlich betrachtet, und eine falsch zusammengesetzte
    render_health(out, metrics);

    // Formel im Dashboard waere ein vermeidbarer Fehler.
    let _ = writeln!(
        out,
        "# HELP vig_useful_inference_ratio Anteil gueltiger an allen \
         fertiggestellten Inferenzen (Spec 18.1)."
    );
    let _ = writeln!(out, "# TYPE vig_useful_inference_ratio gauge");
    let _ = writeln!(
        out,
        "vig_useful_inference_ratio {}",
        ratio(u64::from(metrics.useful_inference_permille()))
    );
    let _ = writeln!(
        out,
        "# HELP vig_stale_compute_ratio Anteil verschwendeter an der \
         gesamten Backendzeit (Spec 18.2)."
    );
    let _ = writeln!(out, "# TYPE vig_stale_compute_ratio gauge");
    let _ = writeln!(
        out,
        "vig_stale_compute_ratio {}",
        ratio(u64::from(metrics.stale_compute_permille()))
    );

    // NV-06: die Frage vor jeder Umstellung — schlaegt die
    // zustandsabhaengige Prognose den alten Weg, oder lehnt sie nur mehr ab?
    for (name, help, value) in [
        (
            "vig_predictor_comparisons_total",
            "Wie oft die zustandsabhaengige Prognose verglichen wurde.",
            metrics.predictor_comparisons,
        ),
        (
            "vig_predictor_fallbacks_total",
            "Wie oft sie keine Aussage hatte und der bisherige Weg galt.",
            metrics.predictor_fallbacks,
        ),
        (
            "vig_predictor_more_conservative_total",
            "Wie oft sie mehr Zeit veranschlagte als der bisherige Weg.",
            metrics.predictor_more_conservative,
        ),
        (
            "vig_predictor_more_optimistic_total",
            "Wie oft sie weniger Zeit veranschlagte.",
            metrics.predictor_more_optimistic,
        ),
    ] {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} counter");
        let _ = writeln!(out, "{name} {value}");
    }
    let _ = writeln!(
        out,
        "# HELP vig_predictor_active 1, wenn die zustandsabhaengige Prognose \
         scharf geschaltet ist."
    );
    let _ = writeln!(out, "# TYPE vig_predictor_active gauge");
    let _ = writeln!(out, "vig_predictor_active {}", metrics.predictor_active);

    render_scalar(
        out,
        "vig_reconcile_baseline_missing",
        "Backendmodelle ohne Abgleichs-Basislinie.",
        metrics.reconcile_baseline_missing,
    );

    render_per_model_series(out, metrics);

    for (index, count) in metrics.variant_selected.iter().enumerate() {
        if index == 0 {
            let _ = writeln!(
                out,
                "# HELP vig_variant_selected_total Wie oft welche Variante \
                 gewaehlt wurde."
            );
            let _ = writeln!(out, "# TYPE vig_variant_selected_total counter");
        }
        let _ = writeln!(
            out,
            "vig_variant_selected_total{{variant=\"{index}\"}} {count}"
        );
    }
}

/// Eine einzelne Messgroesse mit HELP und TYPE.
fn render_scalar(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

/// Die Reihen, die je Modell einen Wert haben.
///
/// Getrennt von [`render_derived`], weil sie als Block wachsen: jede neue
/// Verbrauchersicht kommt hier dazu, und eine Funktion, die zwei Themen
/// mischt, wird von beiden laenger.
fn render_per_model_series(out: &mut String, metrics: &Metrics) {
    // Verbrauchersicht: was eine Abdeckungszahl nicht zeigt. Zehn verstreute
    // Ausfaelle und ein Block von zehn ergeben dieselbe Rate — fuer eine
    // Regelung ist das der ganze Unterschied.
    render_per_model(
        out,
        "vig_longest_gap_us",
        "Laengste Zeit ohne gueltiges Ergebnis je Modell.",
        &metrics.longest_gap_us,
        metrics.models,
    );
    render_per_model(
        out,
        "vig_consecutive_misses",
        "Aufeinanderfolgende Requests ohne gueltiges Ergebnis je Modell.",
        &metrics.consecutive_misses,
        metrics.models,
    );
    // NV-02: gezaehlt ueber den Vertragstakt, nicht ueber angenommene
    // Requests. Ein Governor, der alles ablehnt, faellt hier auf.
    render_per_model(
        out,
        "vig_weakly_hard_misses",
        "Fehlversorgte Verbraucherzyklen im laufenden Fenster je Modell.",
        &metrics.weakly_hard_misses,
        metrics.models,
    );
    render_per_model(
        out,
        "vig_weakly_hard_misses_left",
        "Wie viele Misses das laufende Fenster noch vertraegt je Modell.",
        &metrics.weakly_hard_misses_left,
        metrics.models,
    );
    render_per_model(
        out,
        "vig_weakly_hard_violated",
        "1, wo die Weakly-hard-Bedingung im letzten vollstaendigen Fenster verletzt ist.",
        &metrics.weakly_hard_violated,
        metrics.models,
    );
    render_per_model(
        out,
        "vig_arrival_period_us",
        "Beobachteter Ankunftsabstand je Modell.",
        &metrics.arrival_period_us,
        metrics.models,
    );
    render_per_model(
        out,
        "vig_contract_period_us",
        "Konfigurierte Periode je Modell.",
        &metrics.contract_period_us,
        metrics.models,
    );
    render_per_model(
        out,
        "vig_margin_percent",
        "Aktuell wirksame Sicherheitsmarge je Modell.",
        &metrics.margin_percent,
        metrics.models,
    );
}

/// Nanosekunden als Sekunden mit Millisekundenaufloesung.
fn seconds(nanos: u64) -> String {
    let whole = nanos.checked_div(1_000_000_000).unwrap_or(0);
    let millis = nanos
        .checked_rem(1_000_000_000)
        .unwrap_or(0)
        .checked_div(1_000_000)
        .unwrap_or(0);
    format!("{whole}.{millis:03}")
}

/// Promille als Verhaeltnis zwischen 0 und 1.
fn ratio(permille: u64) -> String {
    let whole = permille.checked_div(1_000).unwrap_or(0);
    let rest = permille.checked_rem(1_000).unwrap_or(0);
    format!("{whole}.{rest:03}")
}

async fn metrics_endpoint(State(handle): State<Handle>) -> (StatusCode, String) {
    match handle.metrics().await {
        Ok(metrics) => (StatusCode::OK, render(&metrics)),
        // Antwortet der Scheduler nicht, ist das selbst die wichtigste
        // Information. Ein leerer Erfolg waere hier eine Luege.
        Err(status) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("# Scheduler antwortet nicht: {status}\n"),
        ),
    }
}

/// Liveness: laeuft der Prozess ueberhaupt noch?
///
/// Bewusst die schwaechste Frage. Ein Orchestrator toetet den Prozess, wenn
/// diese Pruefung faellt — deshalb darf sie **nicht** an einem kaputten Backend
/// haengen. Ein Neustart repariert kein fremdes Triton, er verwirft nur die
/// gelernten Profile und die Queue.
async fn health_endpoint(State(handle): State<Handle>) -> StatusCode {
    if handle.metrics().await.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Readiness: kann dieser Governor gerade Arbeit annehmen?
///
/// Die staerkere Frage, und die, nach der ein Loadbalancer Verkehr lenkt.
/// Getrennt von der Liveness, weil die beiden verschiedene Konsequenzen haben:
/// „nicht bereit" heisst *keinen Verkehr schicken*, „nicht lebendig" heisst
/// *neu starten*. Ein Governor, dessen Slots alle in Quarantaene stehen, ist
/// nicht bereit — aber ein Neustart hilft ihm nicht, denn das haengende
/// Backend startet dabei nicht mit.
async fn ready_endpoint(State(handle): State<Handle>) -> (StatusCode, String) {
    let Ok(metrics) = handle.metrics().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "scheduler: keine Antwort\n".to_owned(),
        );
    };
    match readiness(&metrics) {
        Ok(()) => (StatusCode::OK, "ready\n".to_owned()),
        Err(reason) => (StatusCode::SERVICE_UNAVAILABLE, format!("{reason}\n")),
    }
}

/// Die Bereitschaftsentscheidung, getrennt vom HTTP-Rahmen.
///
/// Als eigene Funktion, damit sie ohne laufenden Server pruefbar ist — eine
/// Bereitschaftsaussage, die nur im Integrationstest erreichbar ist, wird im
/// Zweifel nicht geprueft.
///
/// # Errors
///
/// Der Grund, aus dem kein Verkehr geschickt werden sollte.
pub fn readiness(metrics: &Metrics) -> Result<(), String> {
    // Alle Slots in Quarantaene heisst: nichts kann mehr starten. Der Prozess
    // lebt, aber er kann nichts ausrichten — genau der Unterschied zwischen
    // Liveness und Readiness.
    if metrics.slots > 0 && metrics.quarantined >= metrics.slots {
        return Err(format!(
            "backend: {} von {} Slots in Quarantaene; das Backend antwortet nicht",
            metrics.quarantined, metrics.slots
        ));
    }

    // Ein abgelehnter Verbindungsaufbau erzeugt **keine** Quarantaene: der
    // Aufruf kehrt sofort mit einem Fehler zurueck, und der Slotkredit wird
    // regulaer frei. Auf die Quarantaene allein zu schauen hiess deshalb, den
    // haeufigsten Backendausfall zu uebersehen — das Backend ist weg, und die
    // Bereitschaftspruefung meldet gruen.
    //
    // Gezaehlt werden nur **Transport**fehler. Ein Modellfehler sagt etwas
    // ueber einen Request, nicht ueber die Erreichbarkeit; er darf den
    // Governor nicht aus der Rotation nehmen. Ein einziger Transportfehler
    // genuegt dagegen: er wird beim naechsten Erfolg zurueckgesetzt, und
    // solange keiner gelingt, ist hier nichts auszurichten.
    if metrics.consecutive_transport_failures > 0 {
        return Err(format!(
            "backend: {} Transportfehler seit dem letzten Erfolg; das Backend ist \
             nicht erreichbar",
            metrics.consecutive_transport_failures
        ));
    }
    Ok(())
}

/// Startet den Metrik-Endpunkt.
///
/// # Errors
///
/// Wenn die Adresse nicht gebunden werden kann.
pub async fn serve(
    handle: Handle,
    address: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = Router::new()
        .route("/metrics", get(metrics_endpoint))
        .route("/healthz", get(health_endpoint))
        .route("/readyz", get(ready_endpoint))
        .with_state(handle);
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Der Betriebszustand: Quarantaene, Transportfehler, offene Aufrufe.
///
/// Eigene Funktion, weil sie eine andere Frage beantwortet als die
/// Requestzaehler darueber: nicht "was ist passiert", sondern "kann dieses
/// System gerade etwas ausrichten".
fn render_health(out: &mut String, metrics: &Metrics) {
    // Ein Messwert, kein Zaehler: er faellt wieder, wenn das Backend doch noch
    // antwortet. Erreicht er die Slotzahl, kann nichts mehr starten.
    let _ = writeln!(
        out,
        "# HELP vig_quarantined_slots Slotkredite, die wegen eines \
         Backendtimeouts gehalten werden."
    );
    let _ = writeln!(out, "# TYPE vig_quarantined_slots gauge");
    let _ = writeln!(out, "vig_quarantined_slots {}", metrics.quarantined);

    let _ = writeln!(
        out,
        "# HELP vig_consecutive_transport_failures Backendaufrufe, die seit dem \
         letzten Erfolg am Transport scheiterten."
    );
    let _ = writeln!(out, "# TYPE vig_consecutive_transport_failures gauge");
    let _ = writeln!(
        out,
        "vig_consecutive_transport_failures {}",
        metrics.consecutive_transport_failures
    );

    let _ = writeln!(
        out,
        "# HELP vig_degraded_profiles Modelle, deren hinterlegtes Laufzeitprofil \
         nicht mehr zu den Beobachtungen passt (Spec 30.3)."
    );
    let _ = writeln!(out, "# TYPE vig_degraded_profiles gauge");
    let _ = writeln!(out, "vig_degraded_profiles {}", metrics.degraded_profiles);

    let _ = writeln!(
        out,
        "# HELP vig_requests_rejected_quarantined_total Requests, die wegen \
         vollstaendiger Quarantaene sofort abgewiesen wurden."
    );
    let _ = writeln!(
        out,
        "# TYPE vig_requests_rejected_quarantined_total counter"
    );
    let _ = writeln!(
        out,
        "vig_requests_rejected_quarantined_total {}",
        metrics.rejected_quarantined
    );

    let _ = writeln!(
        out,
        "# HELP vig_execution_reconciled_total Ausfuehrungsenden, die durch \
         Abgleich mit dem Backend belegt wurden."
    );
    let _ = writeln!(out, "# TYPE vig_execution_reconciled_total counter");
    let _ = writeln!(out, "vig_execution_reconciled_total {}", metrics.reconciled);

    let _ = writeln!(
        out,
        "# HELP vig_outstanding_backend_calls Backendaufrufe, die noch offen sind."
    );
    let _ = writeln!(out, "# TYPE vig_outstanding_backend_calls gauge");
    let _ = writeln!(
        out,
        "vig_outstanding_backend_calls {}",
        metrics.outstanding_backend_calls
    );
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;

    #[test]
    fn the_rendered_format_is_parseable_prometheus() {
        let metrics = Metrics {
            received: 1_000,
            forwarded: 700,
            superseded: 250,
            completed_valid: 690,
            completed_obsolete: 10,
            stale_compute_nanos: 1_500_000_000,
            total_compute_nanos: 30_000_000_000,
            ..Metrics::default()
        };

        let text = render(&metrics);

        // Jede Metrikzeile braucht ihr HELP und ihr TYPE davor, sonst lehnt
        // Prometheus den Datensatz ab.
        let mut seen_help = 0_usize;
        let mut seen_type = 0_usize;
        let mut seen_value = 0_usize;
        for line in text.lines() {
            if line.starts_with("# HELP") {
                seen_help += 1;
            } else if line.starts_with("# TYPE") {
                seen_type += 1;
            } else if !line.is_empty() {
                seen_value += 1;
            }
        }
        assert!(seen_help > 10, "zu wenige HELP-Zeilen: {seen_help}");
        assert_eq!(seen_help, seen_type);
        assert!(seen_value >= seen_type);

        assert!(text.contains("vig_requests_superseded_total 250"), "{text}");
        assert!(text.contains("vig_useful_inference_ratio 0.985"), "{text}");
        assert!(text.contains("vig_stale_compute_ratio 0.050"), "{text}");
        assert!(
            text.contains("vig_backend_compute_seconds_total 30.000"),
            "{text}"
        );
    }

    /// NV-02: eine verletzte Weakly-hard-Bedingung muss im Dashboard stehen.
    ///
    /// Ein Vertrag, dessen Bruch nur im Log auftaucht, wird erst nachtraeglich
    /// bemerkt — und dann meist von jemandem, der die Folgen schon gesehen hat.
    #[test]
    fn a_weakly_hard_violation_is_exported() {
        let mut metrics = Metrics {
            models: 2,
            ..Metrics::default()
        };
        metrics.weakly_hard_misses[0] = 7;
        metrics.weakly_hard_violated[0] = 1;

        let text = render(&metrics);
        assert!(
            text.contains(r#"vig_weakly_hard_misses{model="0"} 7"#),
            "{text}"
        );
        assert!(
            text.contains(r#"vig_weakly_hard_violated{model="0"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"vig_weakly_hard_violated{model="1"} 0"#),
            "ein Modell ohne Budget meldet 0 und nicht nichts: {text}"
        );
    }

    /// Die Zaehler, die zeigen, was das System **nicht** getan hat, sind die
    /// eigentliche Begruendung fuer den Export.
    #[test]
    fn the_deliberate_omissions_are_exported() {
        let text = render(&Metrics::default());
        for name in [
            "vig_requests_superseded_total",
            "vig_requests_stale_total",
            "vig_deferred_for_protected_total",
            "vig_best_effort_starved_total",
            "vig_generative_prefill_us_total",
            "vig_generative_decode_us_total",
            "vig_generative_fixed_us_total",
            "vig_decomposition_refused_total",
            "vig_generative_context_tokens",
        ] {
            assert!(text.contains(name), "{name} fehlt im Export");
        }
    }

    /// Readiness und Liveness beantworten verschiedene Fragen.
    ///
    /// Ein Governor, dessen Slots alle in Quarantaene stehen, lebt — ein
    /// Neustart wuerde ihm nichts nuetzen, denn das haengende Backend startet
    /// dabei nicht mit. Er ist aber nicht bereit, und ein Loadbalancer soll
    /// ihm keinen Verkehr mehr schicken.
    #[test]
    fn readiness_fails_when_every_slot_is_quarantined() {
        let healthy = Metrics {
            slots: 2,
            quarantined: 1,
            ..Metrics::default()
        };
        assert!(readiness(&healthy).is_ok(), "ein freier Slot genuegt");

        // Ein bestaetigter Transportfehler nimmt ihn ebenfalls aus der
        // Rotation — ein abgelehnter Verbindungsaufbau erzeugt keine
        // Quarantaene, das Backend ist aber genauso weg.
        let refused = Metrics {
            slots: 2,
            consecutive_transport_failures: 1,
            ..Metrics::default()
        };
        assert!(readiness(&refused).is_err());

        // Ein Modellfehler dagegen nicht: der betrifft einen Request.
        let model_error = Metrics {
            slots: 2,
            backend_failures: 5,
            ..Metrics::default()
        };
        assert!(readiness(&model_error).is_ok());

        let stuck = Metrics {
            slots: 2,
            quarantined: 2,
            ..Metrics::default()
        };
        let reason = readiness(&stuck).expect_err("nichts kann mehr starten");
        assert!(reason.contains("Quarantaene"), "{reason}");

        // Und ohne Backendproblem bleibt er bereit.
        assert!(
            readiness(&Metrics {
                slots: 1,
                ..Metrics::default()
            })
            .is_ok()
        );
    }

    /// Ein Zaehler, den niemand abfragen kann, ist kein Zaehler.
    ///
    /// `cancelled` ist die Gesundheitsgroesse fuer Clients, die in ihre
    /// eigenen Timeouts laufen, waehrend Vigilant ihre Arbeit noch plant.
    #[test]
    fn the_cancellation_counter_reaches_prometheus() {
        let text = render(&Metrics {
            cancelled: 3,
            ..Metrics::default()
        });
        assert!(text.contains("vig_requests_cancelled_total 3"));
    }
}
