//! NV-23: die begrenzte Aussage gegen den echten Scheduler.
//!
//! Die Herleitung steht in `docs/analysis/nv23-bounded-claim.md`. Hier wird
//! geprueft, dass sie stimmt (Gittersuche), dass sie nicht zu grob ist
//! (die Schranke wird erreicht) und dass ihre Annahmen nicht schmueckend sind
//! (faellt eine, findet sich ein Gegenbeispiel).

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::print_stdout
)]

use vig_sim::bounded::{
    Background, GridSize, JitterPattern, Params, Violation, check, delivery_bound_us, gap_bound_us,
    grid, planned_us, run, search,
};

/// Ein Parametersatz innerhalb aller Annahmen, ohne Hintergrund.
fn base() -> Params {
    Params {
        period_us: 33_000,
        jitter_us: 2_000,
        jitter: JitterPattern::Alternating,
        transport_us: 1_000,
        deadline_us: 33_000,
        max_age_us: 66_000,
        p50_us: 8_000,
        p99_us: 10_000,
        runtime_us: [10_000, 10_000],
        margin_percent: 100,
        jitter_envelope_us: None,
        background: Vec::new(),
        duration_us: 600_000,
    }
}

// ---------------------------------------------------------------------------
// Die Schranken selbst
// ---------------------------------------------------------------------------

#[test]
fn the_bounds_are_the_formulas_of_the_analysis() {
    let p = base();
    // Δ = 2J + D — seit ADR-0036 ohne δ
    assert_eq!(delivery_bound_us(&p), 4_000 + 33_000);
    // max(0, T + 2J + Δ − A)
    assert_eq!(gap_bound_us(&p), Some((33_000 + 4_000 + 37_000) - 66_000));

    let mut roomy = p.clone();
    roomy.max_age_us = 200_000;
    assert_eq!(gap_bound_us(&roomy), Some(0), "keine negative Luecke");

    // Am Rand Δ = A ist ein Frame gueltig ausgeliefert, aber keinen
    // Augenblick brauchbar: ueber die Luecke sagt die Aussage dann nichts.
    let mut edge = p.clone();
    edge.max_age_us = delivery_bound_us(&p);
    assert_eq!(gap_bound_us(&edge), None);
    edge.max_age_us += 1;
    assert_eq!(gap_bound_us(&edge), Some(33_000 + 4_000 - 1));
}

#[test]
fn the_plan_uses_the_cores_integer_margin() {
    assert_eq!(planned_us(10_000, 100), Some(10_000));
    assert_eq!(planned_us(9_000, 110), Some(9_900));
    assert_eq!(
        planned_us(10_000, 99),
        None,
        "unter 100 % gibt es keine Marge"
    );
}

#[test]
fn every_assumption_is_checked() {
    assert_eq!(check(&base()), Ok(()));

    let mut p = base();
    p.runtime_us = [10_001, 10_000];
    assert_eq!(check(&p), Err(Violation::RuntimeAboveProfile));

    let mut p = base();
    p.deadline_us = 13_000; // ohne Huelle: Ĉ + δ + (2J − δ) = 14 ms
    assert_eq!(check(&p), Err(Violation::NoRoomForLateArrival));
    // Mit Huelle bleibt nur Ĉ + δ = 11 ms (ADR-0036).
    p.jitter_envelope_us = Some(p.jitter_us);
    assert_eq!(check(&p), Ok(()));
    // Eine Huelle kleiner als der Jitter deckt nur einen Teil der
    // Verspaetung: W = 2 ms, es bleibt Ĉ + δ + (2J − W) = 13 ms.
    p.jitter_envelope_us = Some(1_000);
    p.deadline_us = 12_500;
    assert_eq!(check(&p), Err(Violation::NoRoomForLateArrival));
    // Und keine Huelle rettet einen Frame, der allein nicht rechtzeitig ist.
    p.jitter_envelope_us = Some(p.jitter_us);
    p.deadline_us = 10_500;
    assert_eq!(check(&p), Err(Violation::NoRoomForLateArrival));

    let mut p = base();
    p.period_us = 13_000;
    p.deadline_us = 14_000;
    assert_eq!(check(&p), Err(Violation::NoRoomInPeriod));

    let mut p = base();
    p.period_us = 120_000;
    p.deadline_us = 120_000;
    p.max_age_us = 400_000;
    p.background.push(Background {
        period_us: 5_000,
        phase_us: 50_000,
        p50_us: 60_000,
        p99_us: 110_000,
        runtime_us: 110_000,
    });
    assert_eq!(
        check(&p),
        Ok(()),
        "(A6) gibt es nicht mehr: die naechste Ankunft zaehlt immer (ADR-0036)"
    );

    let mut p = base();
    p.jitter_us = 3_000;
    p.deadline_us = 38_000; // D + 2J = 44 >= T + Ĉ + δ = 44
    p.max_age_us = 80_000;
    assert_eq!(check(&p), Err(Violation::MaySupersede));

    let mut p = base();
    p.background.push(Background {
        period_us: 50_000,
        phase_us: 0,
        p50_us: 20_000,
        p99_us: 40_000,
        runtime_us: 40_000,
    });
    assert_eq!(check(&p), Err(Violation::BackgroundBeforeProtected));

    let mut p = base();
    p.max_age_us = 36_999;
    assert_eq!(check(&p), Err(Violation::BoundAboveMaxAge));
}

// ---------------------------------------------------------------------------
// Die Schranke wird erreicht
// ---------------------------------------------------------------------------

/// Die Konstruktion aus der Analyse, Abschnitt „Die Schranke ist scharf".
///
/// Frame 4 kommt `+J`, Frame 5 `−J` — Frame 5 wird `2J` vor seiner
/// erwarteten Aufnahme `ĉ₅ = c₄ + T` aufgenommen. Genau als Frame 4 fertig
/// wird, liegt ein Hintergrundauftrag bereit, geplant so lang, dass er mit der
/// Prognose von Frame 5 gerade noch vereinbar ist: `s + b + Ĉ = ĉ₅ + D`.
/// Frame 5 wartet bis `ĉ₅ + D − Ĉ`, laeuft `Ĉ` und wird bei `ĉ₅ + D` fertig —
/// sein Alter ist dann genau `2J + D = Δ`.
#[test]
fn the_bound_is_reached_exactly() {
    let mut p = base();
    let c = planned_us(p.p99_us, p.margin_percent).unwrap();
    // Aufnahme von Frame 4: J + 4T + J; Ankunft δ spaeter; fertig nach Ĉ.
    let a4 = p.jitter_us + 4 * p.period_us + p.jitter_us + p.transport_us;
    let b = p.period_us + p.deadline_us - 2 * c - p.transport_us;
    p.background.push(Background {
        period_us: 10_000_000,
        phase_us: a4 + 1,
        p50_us: b / 2,
        p99_us: b,
        runtime_us: b,
    });
    assert_eq!(
        check(&p),
        Ok(()),
        "die Konstruktion liegt innerhalb der Annahmen"
    );

    let outcome = run(&p).unwrap();
    assert_eq!(outcome.satisfies(&p), Ok(()));
    assert_eq!(outcome.worst_frame, Some(5));
    assert_eq!(
        outcome.worst_age_us,
        delivery_bound_us(&p),
        "die Schranke 2J + D wird auf die Mikrosekunde erreicht"
    );
}

// ---------------------------------------------------------------------------
// Die Suche
// ---------------------------------------------------------------------------

fn assert_holds(size: GridSize) {
    let params = grid(size);
    let report = search(&params);
    // Sichtbar mit `--nocapture`: die Zahlen, die in der Analyse stehen.
    println!(
        "NV-23 {size:?}: {} Parametersaetze, {} innerhalb der Annahmen, \
         naechste Annaeherung an Δ {} ‰, ausserhalb {:?}",
        params.len(),
        report.inside,
        report.tightest_permille,
        report.outside
    );
    if let Some((p, outcome, why)) = &report.counterexample {
        panic!("Gegenbeispiel innerhalb der Annahmen: {why}\n{p:#?}\n{outcome:#?}");
    }
    assert_eq!(
        report.unbuildable, 0,
        "jedes Szenario muss sich bauen lassen"
    );
    assert_eq!(
        report.plan_mismatch, 0,
        "der Scheduler muss mit genau der geplanten Laufzeit der Analyse rechnen"
    );
    assert!(
        report.inside > 100,
        "die Suche muss innerhalb der Annahmen tatsaechlich suchen: {report:?}"
    );
    assert!(
        report.tightest_permille >= 900,
        "und nahe an die Schranke kommen, sonst prueft sie nichts: {} ‰",
        report.tightest_permille
    );
}

/// Die Gittersuche fuer jeden Testlauf.
#[test]
fn inside_the_assumptions_no_run_breaks_the_bound() {
    assert_holds(GridSize::Small);
}

/// Die ausfuehrliche Suche: `cargo test --release -p vig-sim --test
/// nv23_bounded -- --ignored`.
#[test]
#[ignore = "ausfuehrliche Suche, einige Minuten im Release-Build"]
fn inside_the_assumptions_no_run_breaks_the_bound_full() {
    assert_holds(GridSize::Full);
}

// ---------------------------------------------------------------------------
// Faellt eine Annahme, faellt die Aussage
// ---------------------------------------------------------------------------

/// (A4) `Ĉ + 2J ≤ D`. Kommt ein Frame spaeter als erwartet und rechnet der
/// Look-ahead ihn schon fuer unrettbar — ohne den Kandidaten waere seine
/// Deadline ab der Prognose nicht mehr zu halten —, vetoiert er nicht mehr.
/// Ausgerechnet dann startet lange Hintergrundarbeit, und der Frame, der
/// gleich eintrifft, wartet ihre volle Laufzeit.
#[test]
fn a_late_frame_the_guard_gave_up_on_waits_for_the_whole_background_job() {
    let mut p = base();
    p.deadline_us = 11_000; // Ĉ + 2J = 14 ms > D
    p.max_age_us = 200_000;
    p.jitter = JitterPattern::Alternating;
    // Frame 5 (ungerade, −J) kommt puenktlich zu seiner Prognose, Frame 6
    // (gerade, +J) 2J danach: e₆ = a₅ + T, a₆ = e₆ + 2J.
    let a5 = p.jitter_us + 5 * p.period_us - p.jitter_us + p.transport_us;
    let e6 = a5 + p.period_us;
    // Der Hintergrund trifft ein, wenn e₆ + D − Ĉ verstrichen ist, Frame 6
    // aber noch nicht da: ab der Prognose gerechnet ist er nicht mehr zu
    // retten, also vetoiert der Look-ahead nicht.
    p.background.push(Background {
        period_us: 10_000_000,
        phase_us: e6 + 2_000,
        p50_us: 40_000,
        p99_us: 80_000,
        runtime_us: 80_000,
    });
    assert_eq!(check(&p), Err(Violation::NoRoomForLateArrival));
    let outcome = run(&p).unwrap();
    assert!(
        outcome.worst_age_us > delivery_bound_us(&p),
        "ohne (A4) muss die Schranke reissen: {outcome:?}"
    );
}

/// (A3) Laufzeit hoechstens Profil. Ueberzieht Hintergrundarbeit ihren Plan,
/// blockiert sie laenger, als der Look-ahead zugelassen hat — nicht
/// unterbrechbar ist nicht unterbrechbar.
#[test]
fn a_background_job_that_overruns_its_plan_breaks_the_bound() {
    let mut p = base();
    let c = planned_us(p.p99_us, p.margin_percent).unwrap();
    let a4 = p.jitter_us + 4 * p.period_us + p.jitter_us + p.transport_us;
    let b = p.period_us + p.deadline_us - 2 * c - p.transport_us;
    p.background.push(Background {
        period_us: 10_000_000,
        phase_us: a4 + 1,
        p50_us: b / 2,
        p99_us: b,
        runtime_us: b + 5_000,
    });
    assert_eq!(check(&p), Err(Violation::RuntimeAboveProfile));
    let outcome = run(&p).unwrap();
    assert!(
        outcome.worst_age_us > delivery_bound_us(&p),
        "ohne (A3) muss die Schranke reissen: {outcome:?}"
    );
}

/// (A8) Kein Hintergrund vor der ersten geschuetzten Ankunft. Vorher gibt es
/// keine Prognose und damit keinen Look-ahead: ein langer Auftrag, der beim
/// Start schon wartet, laeuft durch.
#[test]
fn background_before_the_first_frame_is_the_start_up_exception() {
    let mut p = base();
    p.max_age_us = 200_000;
    // 33 ms: lang genug, dass Frame 0 (Aufnahme 4 ms) erst mit 39 ms Alter
    // fertig wird, kurz genug, dass Frame 1 ihn nicht verdraengt.
    p.background.push(Background {
        period_us: 10_000_000,
        phase_us: 0,
        p50_us: 20_000,
        p99_us: 33_000,
        runtime_us: 33_000,
    });
    assert_eq!(check(&p), Err(Violation::BackgroundBeforeProtected));
    let outcome = run(&p).unwrap();
    assert!(
        outcome.worst_age_us > delivery_bound_us(&p),
        "ohne (A8) muss die Schranke im Anlauf reissen: {outcome:?}"
    );
    assert_eq!(outcome.worst_frame, Some(0), "und zwar beim ersten Frame");
}

/// (A7) Keine Verdraengung. Wartet ein Frame noch, wenn der naechste
/// eintrifft, ersetzt der naechste ihn — `LATEST` tut, wofuer es da ist, und
/// die Aussage „jeder Frame wird ausgeliefert" gilt dann nicht mehr.
#[test]
fn when_a_frame_can_still_wait_at_the_next_arrival_it_is_superseded() {
    let mut p = base();
    p.jitter_us = 3_000;
    p.jitter = JitterPattern::Descending;
    p.deadline_us = 38_000; // D + 2J = 44 = T + Ĉ + δ
    p.max_age_us = 120_000;
    let c = planned_us(p.p99_us, p.margin_percent).unwrap();
    // Frame 3 spaet (+J), Frame 4 auf dem Raster, Frame 5 frueh (−J).
    // Genau als Frame 3 fertig wird, liegt ein Auftrag bereit, der Frame 4
    // bis ĉ₄ + D − Ĉ warten laesst — und das ist genau a₅.
    let a3 = p.jitter_us + 3 * p.period_us + p.jitter_us + p.transport_us;
    let b = p.period_us + p.deadline_us - 2 * c - p.transport_us;
    p.background.push(Background {
        period_us: 10_000_000,
        phase_us: a3 + 1,
        p50_us: b / 2,
        p99_us: b,
        runtime_us: b,
    });
    assert_eq!(check(&p), Err(Violation::MaySupersede));
    let outcome = run(&p).unwrap();
    assert!(
        outcome.lost > 0,
        "ohne (A7) wird ein wartender Frame verdraengt: {outcome:?}"
    );
    assert!(outcome.satisfies(&p).is_err());
}

// ---------------------------------------------------------------------------
// Die drei Schwaechen des Look-ahead (ADR-0036)
// ---------------------------------------------------------------------------

/// Befund 1: derselbe verspaetete Frame wie im Gegenbeispiel zu (A4) — nur
/// erklaert der Vertrag jetzt seine Jitterhuelle. Dann weiss der Look-ahead,
/// wie spaet ein Frame hoechstens kommt, und gibt ihn bis dahin nicht auf.
#[test]
fn with_an_envelope_a_late_frame_is_not_given_up() {
    let mut p = base();
    p.deadline_us = 11_000; // Ĉ + 2J = 14 ms > D, aber Ĉ + δ = 11 ms = D
    p.max_age_us = 200_000;
    p.jitter_envelope_us = Some(p.jitter_us);
    let a5 = p.jitter_us + 5 * p.period_us - p.jitter_us + p.transport_us;
    let e6 = a5 + p.period_us;
    p.background.push(Background {
        period_us: 10_000_000,
        phase_us: e6 + 2_000,
        p50_us: 40_000,
        p99_us: 80_000,
        runtime_us: 80_000,
    });
    let outcome = run(&p).unwrap();
    assert!(
        outcome.worst_age_us <= p.deadline_us + 2 * p.jitter_us,
        "mit Huelle haelt der Look-ahead den spaeten Frame: {outcome:?}"
    );
    assert_eq!(outcome.lost, 0);
}

/// Befund 2: die Deadline eines Frames beginnt bei seiner Aufnahme. Rechnet
/// der Look-ahead sie ab der erwarteten **Ankunft**, laesst er Arbeit zu, bis
/// der Frame `δ` nach seiner Deadline fertig ist — die Konstruktion, die die
/// alte Schranke `δ + 2J + D` erreichte. Ab Aufnahme gerechnet bleibt es bei
/// `2J + D`: der Jitter ist nicht vorherzusehen, der Transport schon.
#[test]
fn the_deadline_counts_from_the_capture_not_the_arrival() {
    let mut p = base();
    let c = planned_us(p.p99_us, p.margin_percent).unwrap();
    let a4 = p.jitter_us + 4 * p.period_us + p.jitter_us + p.transport_us;
    let b = p.period_us + p.deadline_us - 2 * c;
    p.background.push(Background {
        period_us: 10_000_000,
        phase_us: a4 + 1,
        p50_us: b / 2,
        p99_us: b,
        runtime_us: b,
    });
    let outcome = run(&p).unwrap();
    assert!(
        outcome.worst_age_us <= 2 * p.jitter_us + p.deadline_us,
        "ab Aufnahme gerechnet traegt die Schranke kein δ: {outcome:?}"
    );
}

/// Befund 3: die naechste Ankunft eines bewachten Stroms gehoert in die
/// Prognose, gleich wie weit sie weg ist. Ein 120-ms-Strom ist gegen einen
/// 115-ms-Auftrag geschuetzt, auch wenn er ueber jeden festen Horizont von
/// 100 ms hinausreicht.
#[test]
fn an_arrival_beyond_the_old_horizon_is_protected() {
    let mut p = base();
    p.period_us = 120_000;
    p.jitter_us = 0;
    p.jitter = JitterPattern::None;
    p.deadline_us = 11_000; // Ĉ + δ
    p.max_age_us = 100_000;
    p.background.push(Background {
        period_us: 1_000,
        phase_us: p.first_arrival_us() + 1,
        p50_us: 60_000,
        p99_us: 115_000,
        runtime_us: 115_000,
    });
    let outcome = run(&p).unwrap();
    assert!(
        outcome.worst_age_us <= p.deadline_us,
        "die Ankunft nach 120 ms ist geschuetzt: {outcome:?}"
    );
    assert_eq!(outcome.lost, 0);
}
