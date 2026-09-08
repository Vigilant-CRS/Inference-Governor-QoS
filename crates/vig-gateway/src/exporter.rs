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

    render_derived(&mut out, metrics);
    out
}

/// Zeitsummen und abgeleitete Verhaeltnisse.
/// Rendert eine Kennzahl je Modell als Gauge.
///
/// Nur die belegten Slots: eine Zeitreihe je unbenutztem Modellslot kostet in
/// Prometheus dauerhaft Speicher und macht jede Abfrage unleserlich.
fn render_per_model(out: &mut String, name: &str, help: &str, values: &[u32], count: usize) {
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
    // Ein Messwert, kein Zaehler: er faellt wieder, wenn das Backend doch noch
    // antwortet. Erreicht er die Slotzahl, kann nichts mehr starten.
    let _ = writeln!(
        out,
        "# HELP vig_quarantined_slots Slotkredite, die wegen eines \
         Backendtimeouts gehalten werden."
    );
    let _ = writeln!(out, "# TYPE vig_quarantined_slots gauge");
    let _ = writeln!(out, "vig_quarantined_slots {}", metrics.quarantined);

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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
