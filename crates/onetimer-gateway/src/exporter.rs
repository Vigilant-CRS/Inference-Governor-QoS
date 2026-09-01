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
use onetimer_core::Metrics;
use std::fmt::Write as _;
use std::net::SocketAddr;

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
        "onetimer_requests_received_total",
        "Am Gateway angenommene Requests.",
        metrics.received,
    );
    counter(
        "onetimer_requests_forwarded_total",
        "An das Backend weitergereichte Requests.",
        metrics.forwarded,
    );
    counter(
        "onetimer_requests_superseded_total",
        "Durch einen juengeren Request desselben Scopes ersetzte Requests.",
        metrics.superseded,
    );
    counter(
        "onetimer_requests_stale_total",
        "Wegen Ueberalterung verworfene Requests.",
        metrics.stale,
    );
    counter(
        "onetimer_requests_rejected_infeasible_total",
        "Als nicht mehr rechtzeitig machbar abgelehnte Requests.",
        metrics.rejected_infeasible,
    );
    counter(
        "onetimer_requests_rejected_capacity_total",
        "Wegen erschoepfter Queue-Kapazitaet abgelehnte Requests.",
        metrics.rejected_capacity,
    );
    counter(
        "onetimer_requests_completed_valid_total",
        "Fertiggestellt und bei Fertigstellung noch aktuell.",
        metrics.completed_valid,
    );
    counter(
        "onetimer_requests_completed_obsolete_total",
        "Fertiggestellt, aber bei Fertigstellung bereits obsolet.",
        metrics.completed_obsolete,
    );
    counter(
        "onetimer_backend_failures_total",
        "Fehlgeschlagene Backendaufrufe.",
        metrics.backend_failures,
    );
    counter(
        "onetimer_deadline_misses_total",
        "Verletzte Deadlines ueber alle Klassen.",
        metrics.deadline_misses,
    );
    counter(
        "onetimer_protected_deadline_misses_total",
        "Verletzte Deadlines geschuetzter Klassen.",
        metrics.protected_deadline_misses,
    );
    counter(
        "onetimer_dispatched_late_total",
        "Requests, die trotz verfehlbarer Deadline gestartet wurden (ADR-0009).",
        metrics.dispatched_late,
    );
    counter(
        "onetimer_deferred_for_protected_total",
        "Veto-Ereignisse des Protected-Look-ahead; absichtliches Idle (Spec 10.7).",
        metrics.deferred_for_protected,
    );
    counter(
        "onetimer_best_effort_starved_total",
        "Best-Effort-Requests, die terminal wurden, ohne je gelaufen zu sein (ADR-0012).",
        metrics.best_effort_starved,
    );

    render_derived(&mut out, metrics);
    out
}

/// Zeitsummen und abgeleitete Verhaeltnisse.
fn render_derived(out: &mut String, metrics: &Metrics) {
    let _ = writeln!(
        out,
        "# HELP onetimer_stale_compute_seconds_total Backendzeit, die in bei \
         Fertigstellung bereits obsolete Ergebnisse floss."
    );
    let _ = writeln!(out, "# TYPE onetimer_stale_compute_seconds_total counter");
    let _ = writeln!(
        out,
        "onetimer_stale_compute_seconds_total {}",
        seconds(metrics.stale_compute_nanos)
    );
    let _ = writeln!(
        out,
        "# HELP onetimer_backend_compute_seconds_total Gesamte verbrauchte Backendzeit."
    );
    let _ = writeln!(out, "# TYPE onetimer_backend_compute_seconds_total counter");
    let _ = writeln!(
        out,
        "onetimer_backend_compute_seconds_total {}",
        seconds(metrics.total_compute_nanos)
    );

    // Abgeleitete Groessen aus Spec 18.1 und 18.2. Sie sind aus den Zaehlern
    // berechenbar, werden aber mit ausgegeben: sie sind die Werte, die ein
    // Betreiber tatsaechlich betrachtet, und eine falsch zusammengesetzte
    // Formel im Dashboard waere ein vermeidbarer Fehler.
    let _ = writeln!(
        out,
        "# HELP onetimer_useful_inference_ratio Anteil gueltiger an allen \
         fertiggestellten Inferenzen (Spec 18.1)."
    );
    let _ = writeln!(out, "# TYPE onetimer_useful_inference_ratio gauge");
    let _ = writeln!(
        out,
        "onetimer_useful_inference_ratio {}",
        ratio(u64::from(metrics.useful_inference_permille()))
    );
    let _ = writeln!(
        out,
        "# HELP onetimer_stale_compute_ratio Anteil verschwendeter an der \
         gesamten Backendzeit (Spec 18.2)."
    );
    let _ = writeln!(out, "# TYPE onetimer_stale_compute_ratio gauge");
    let _ = writeln!(
        out,
        "onetimer_stale_compute_ratio {}",
        ratio(u64::from(metrics.stale_compute_permille()))
    );

    for (index, percent) in metrics.margin_percent.iter().enumerate() {
        if index == 0 {
            let _ = writeln!(
                out,
                "# HELP onetimer_margin_percent Aktuell wirksame Sicherheitsmarge je Modell."
            );
            let _ = writeln!(out, "# TYPE onetimer_margin_percent gauge");
        }
        let _ = writeln!(
            out,
            "onetimer_margin_percent{{model=\"{index}\"}} {percent}"
        );
    }

    for (index, count) in metrics.variant_selected.iter().enumerate() {
        if index == 0 {
            let _ = writeln!(
                out,
                "# HELP onetimer_variant_selected_total Wie oft welche Variante \
                 gewaehlt wurde."
            );
            let _ = writeln!(out, "# TYPE onetimer_variant_selected_total counter");
        }
        let _ = writeln!(
            out,
            "onetimer_variant_selected_total{{variant=\"{index}\"}} {count}"
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

async fn health_endpoint(State(handle): State<Handle>) -> StatusCode {
    if handle.metrics().await.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
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
        .with_state(handle);
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
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

        assert!(
            text.contains("onetimer_requests_superseded_total 250"),
            "{text}"
        );
        assert!(
            text.contains("onetimer_useful_inference_ratio 0.985"),
            "{text}"
        );
        assert!(
            text.contains("onetimer_stale_compute_ratio 0.050"),
            "{text}"
        );
        assert!(
            text.contains("onetimer_backend_compute_seconds_total 30.000"),
            "{text}"
        );
    }

    /// Die Zaehler, die zeigen, was das System **nicht** getan hat, sind die
    /// eigentliche Begruendung fuer den Export.
    #[test]
    fn the_deliberate_omissions_are_exported() {
        let text = render(&Metrics::default());
        for name in [
            "onetimer_requests_superseded_total",
            "onetimer_requests_stale_total",
            "onetimer_deferred_for_protected_total",
            "onetimer_best_effort_starved_total",
        ] {
            assert!(text.contains(name), "{name} fehlt im Export");
        }
    }
}
