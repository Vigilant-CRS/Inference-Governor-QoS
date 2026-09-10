//! Der gemeinsame Messkern von `vig profile` und `vig calibrate` (Review R08).
//!
//! Vorher hatte jeder Befehl seinen eigenen. `vig profile` gab auf einem
//! **absoluten** Raster frei und buchte Fehlschlaege, Ueberzuege und
//! ausgelassene Freigabepunkte. `vig calibrate` mass Ruecken an Ruecken und
//! ueberging fehlgeschlagene Aufrufe stillschweigend — ausgerechnet der
//! Befehl, der Profile **automatisch in Konfigurationen schreibt**.
//!
//! Der Unterschied ist nicht akademisch. Wer nach jeder Antwort eine Periode
//! wartet, misst bei langsamen Antworten seltener und macht die Messung genau
//! dann gnaedig, wenn sie hart wuerde. Und wer Fehlschlaege ueberspringt,
//! rechnet ein p99 aus den Faellen, die gelungen sind.
//!
//! Was hier herauskommt, ist ein [`CellRun`]: Freigaben, Abschluesse,
//! Ueberzuege, Fehlschlaege, ausgelassene Punkte und der Hardwarezustand vor
//! und nach der Reihe — in **einem** Protokoll, fuer beide Befehle dasselbe.

use std::time::Instant;
use vig_backend_triton::{BackendError, TritonClient};
use vig_platform::measure::{CellId, CellRun, DiscardReason, ReleaseOutcome, ReleaseSchedule};
use vig_platform::{Collector as _, measure};
use vig_protocol_oip::inference::ModelInferRequest;

/// Wie eine Messreihe gefahren wird.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunOptions {
    /// Wie viele Freigabepunkte.
    pub(crate) samples: usize,
    /// Der Freigabetakt in Mikrosekunden.
    ///
    /// `None` heisst Ruecken an Ruecken. Das ist die schwaechere Messung, und
    /// wer sie waehlt, soll es ausdruecklich tun.
    pub(crate) period_us: Option<u64>,
    /// Wie viele Aufrufe vor der Reihe verworfen werden.
    pub(crate) warmup: usize,
    /// Wie viele verwertbare Messwerte die Reihe mindestens braucht.
    pub(crate) min_samples: usize,
    /// Wie viele Fehlschlaege sie hoechstens vertraegt, in Promille.
    pub(crate) max_failure_permille: u64,
}

impl RunOptions {
    /// Die Voreinstellung fuer eine Profilmessung.
    ///
    /// Mindestens hundert verwertbare Messwerte und hoechstens fuenf Prozent
    /// Fehlschlaege; darunter ist das p99 kein Quantil, sondern das Maximum.
    pub(crate) const fn qualified(samples: usize, period_us: Option<u64>, warmup: usize) -> Self {
        Self {
            samples,
            period_us,
            warmup,
            min_samples: if samples < 100 { samples } else { 100 },
            max_failure_permille: 50,
        }
    }
}

/// Faehrt eine Messreihe und gibt ihr Protokoll zurueck.
///
/// Der Rueckgabewert kann **verworfen** sein — `CellRun::discarded()` sagt
/// warum. Das entscheidet der Aufrufer und nicht diese Funktion: ein Profil
/// darf aus einer verworfenen Reihe nicht entstehen, eine Interferenzzahl
/// vielleicht doch, wenn sie als solche gekennzeichnet ist.
///
/// # Errors
///
/// Wenn schon der Warmlauf scheitert. Dann ist nichts zu messen, und eine
/// leere Reihe waere eine Aussage ueber nichts.
pub(crate) async fn run_cell(
    client: &TritonClient,
    cell: CellId,
    request: &ModelInferRequest,
    options: RunOptions,
) -> Result<CellRun, BackendError> {
    for _ in 0..options.warmup {
        client.infer(request.clone()).await?;
    }

    // Vor der Reihe: Zustand merken, damit ein Wechsel hinterher auffaellt.
    let mut collector = vig_platform::NvidiaSmi::default();
    let before = collector.snapshot().ok();

    let mut run = CellRun::with_capacity(cell, options.samples);
    let origin = Instant::now();
    let mut schedule = options
        .period_us
        .map(|us| ReleaseSchedule::new(0, us.saturating_mul(1_000).max(1)));

    for _ in 0..options.samples {
        // Auf dem Raster warten, falls periodisch gemessen wird.
        if let Some(schedule) = schedule.as_mut() {
            let due_ns = schedule.take();
            let now_ns = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
            if due_ns > now_ns {
                tokio::time::sleep(std::time::Duration::from_nanos(
                    due_ns.saturating_sub(now_ns),
                ))
                .await;
            }
        }

        let started = Instant::now();
        let outcome = match client.infer(request.clone()).await {
            Ok(_) => {
                let latency_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                let overrun = schedule.as_ref().is_some_and(|s| {
                    let now_ns = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    now_ns > s.next_release_ns()
                });
                if overrun {
                    ReleaseOutcome::Overrun { latency_ns }
                } else {
                    ReleaseOutcome::Completed { latency_ns }
                }
            }
            // Gebucht, nicht uebersprungen. Ein Fehlschlag ist ein Ergebnis
            // dieser Zelle; ihn wegzulassen rechnet das Quantil aus den
            // Faellen, die gelungen sind.
            Err(e) => ReleaseOutcome::Failed {
                reason: e.to_string(),
            },
        };
        run.record(outcome);

        // Nach einem Ueberzug auf das Raster aufschliessen: die ausgelassenen
        // Punkte werden gebucht, der naechste Freigabepunkt wird **nicht**
        // nach hinten geschoben.
        if let Some(schedule) = schedule.as_mut() {
            let now_ns = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
            run.record_skipped(schedule.catch_up(now_ns));
        }
    }

    run.qualify(options.min_samples, options.max_failure_permille);

    if let (Some(before), Ok(after)) = (before.as_ref(), collector.snapshot())
        && let Some(reason) = measure::hardware_invalidates(before, &after)
    {
        run.discard(reason);
    }
    Ok(run)
}

/// Warum eine Reihe nicht verwertbar ist, als Satz.
pub(crate) fn rejection(run: &CellRun) -> Option<String> {
    run.discarded().map(DiscardReason::to_string)
}
