//! Wie lange ein Messfenster laufen muss, damit es etwas unterscheidet.
//!
//! `vig-fit` und der Schritt `tune` von `vig autotune` zaehlen verfehlte
//! Takte. Ein festes Fenster von zehn Sekunden sieht bei einer 33-ms-Periode
//! 300 Takte, bei einer 370-ms-Periode 27 — dort ist **ein** verfehlter Takt
//! schon 37 ‰. Auf dem Pixel 2 hat die Suche am 15.09. genau so eine
//! Einstellung behalten und die Bestaetigung sie wieder verworfen: 0 gegen
//! 37 ‰, also null gegen einen Takt (docs/benchmark/validierung-autotune.md).
//!
//! Das Fenster wird deshalb in Takten des langsamsten geschuetzten Stroms
//! bemessen und nicht in Sekunden. Die Sekunden folgen daraus, mit einer
//! Untergrenze fuer schnelle Perioden und einer Obergrenze, damit eine
//! Sekundenperiode keinen Lauf ueber Stunden erzeugt.

use crate::Config;

/// Takte des langsamsten geschuetzten Stroms je Fenster.
///
/// Bei 200 Takten ist ein verfehlter Takt 5 ‰ — so gross wie die kleinste
/// Verbesserung, die `tune` fuer die geschuetzten Stroeme ueberhaupt gelten
/// laesst. Weniger Takte, und die Schwelle waere feiner als die Messung.
pub const CYCLES: u64 = 200;

/// Takte mit `--quick`: ein Probelauf, keine belastbare Zahl.
pub const QUICK_CYCLES: u64 = 60;

/// Untergrenze in Sekunden: auch schnelle Perioden brauchen Anlauf und
/// Verteilung ueber das Fenster.
pub const FLOOR_SECONDS: u64 = 10;

/// Untergrenze mit `--quick`.
pub const QUICK_FLOOR_SECONDS: u64 = 5;

/// Obergrenze in Sekunden. Eine Periode von 900 ms und mehr bekommt weniger als
/// [`CYCLES`] Takte; der Aufrufer nennt dann die tatsaechliche Zahl.
pub const CAP_SECONDS: u64 = 180;

/// Obergrenze mit `--quick`.
pub const QUICK_CAP_SECONDS: u64 = 60;

/// Die laengste Periode unter den geschuetzten Stroemen, in Millisekunden.
///
/// Ohne geschuetzten Strom mit Periode die laengste Periode ueberhaupt; ohne
/// jede Periode `None`.
#[must_use]
pub fn slowest_protected_period_ms(config: &Config) -> Option<u64> {
    let slowest = |protected_only: bool| {
        config
            .models
            .values()
            .filter(|m| !protected_only || m.class == "protected")
            .filter_map(|m| m.contract.period_ms)
            .max()
    };
    slowest(true).or_else(|| slowest(false))
}

/// Sekunden je Fenster fuer eine Periode am niedrigsten Lastpunkt.
///
/// Die Lastpunkte verkuerzen die Periode (`period · 100 / Last`); am
/// niedrigsten Punkt ist sie am laengsten, und dort muss das Fenster reichen.
#[must_use]
pub fn seconds_for(period_ms: Option<u64>, lowest_load_percent: u64, quick: bool) -> u64 {
    let (cycles, floor, cap) = if quick {
        (QUICK_CYCLES, QUICK_FLOOR_SECONDS, QUICK_CAP_SECONDS)
    } else {
        (CYCLES, FLOOR_SECONDS, CAP_SECONDS)
    };
    let Some(period) = period_ms else {
        return floor;
    };
    let at_point = period
        .saturating_mul(100)
        .checked_div(lowest_load_percent.max(1))
        .unwrap_or(period);
    cycles
        .saturating_mul(at_point)
        .saturating_add(999)
        .checked_div(1000)
        .unwrap_or(cap)
        .clamp(floor, cap)
}

/// Sekunden je Fenster fuer eine Konfiguration.
#[must_use]
pub fn seconds(config: &Config, lowest_load_percent: u64, quick: bool) -> u64 {
    seconds_for(
        slowest_protected_period_ms(config),
        lowest_load_percent,
        quick,
    )
}

/// Wie viele Takte einer Periode in ein Fenster passen.
#[must_use]
pub fn cycles_in(seconds: u64, period_ms: u64) -> u64 {
    seconds
        .saturating_mul(1000)
        .checked_div(period_ms.max(1))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{CAP_SECONDS, FLOOR_SECONDS, QUICK_FLOOR_SECONDS, cycles_in, seconds_for};

    /// Der Laptop mit 33-ms-Detektor bleibt bei der Untergrenze; das Pixel 2
    /// mit 370 ms bekommt 74 s statt 10 s.
    #[test]
    fn the_window_follows_the_slowest_protected_period() {
        assert_eq!(seconds_for(Some(33), 100, false), FLOOR_SECONDS);
        assert_eq!(seconds_for(Some(370), 100, false), 74);
        assert_eq!(cycles_in(74, 370), 200);
        // Am 90-%-Punkt ist die Periode laenger, das Fenster auch.
        assert_eq!(seconds_for(Some(370), 90, false), 83);
        assert!(cycles_in(83, 411) >= 200);
    }

    /// Kurz nur mit `--quick`, begrenzt nach oben, und ohne Periode die
    /// Untergrenze.
    #[test]
    fn quick_cap_and_no_period() {
        assert_eq!(seconds_for(Some(370), 110, true), 21);
        assert_eq!(seconds_for(Some(5_000), 100, false), CAP_SECONDS);
        assert_eq!(seconds_for(None, 100, true), QUICK_FLOOR_SECONDS);
        assert_eq!(
            seconds_for(Some(370), 0, false),
            CAP_SECONDS,
            "Last 0 wie 1 %"
        );
    }
}
