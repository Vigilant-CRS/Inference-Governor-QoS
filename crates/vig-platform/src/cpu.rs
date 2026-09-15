//! Fremde Rechenzeit, gelesen aus `/proc/stat`.
//!
//! Die Frage „laeuft gerade etwas anderes auf dieser Maschine?" beantwortet
//! `/proc/loadavg` nicht. Die Last zaehlt auch Threads, die im Kernel warten
//! und nichts rechnen: Ein Pixel 2 stand im Leerlauf bei 3,49 und verbrauchte
//! dabei 0,04 Kerne (15.09.2026). Jede Grenze fuer `loadavg` erklaert ein
//! ruhiges Telefon fuer unruhig oder einen belegten Laptop fuer ruhig.
//!
//! Gelesen wird deshalb die belegte CPU-Zeit aller Kerne, ohne Leerlauf und
//! Warten auf I/O, abzueglich der Zeit dieses Prozesses samt beendeter Kinder.
//! Beide Dateien gibt es auf jedem Linux, auch fuer den Shell-Nutzer auf
//! Android. Wie lange zwischen zwei Stichproben gewartet wird und was davon
//! zu halten ist, entscheidet der Aufrufer.

use std::num::NonZero;

/// Eine Stichprobe der CPU-Zeit: alle Kerne und dieser Prozess, in Ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSample {
    /// Belegte Ticks aller Kerne, ohne Leerlauf und Warten auf I/O.
    pub busy: u64,
    /// Alle Ticks aller Kerne.
    pub total: u64,
    /// Ticks dieses Prozesses: `utime + stime + cutime + cstime`.
    pub own: u64,
}

impl CpuSample {
    /// Liest beide Dateien jetzt. `None`, wo es sie nicht gibt oder sie
    /// unlesbar sind — eine fehlende Beobachtung ist kein Nullwert.
    #[must_use]
    pub fn now() -> Option<Self> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let (busy, total) = parse_cpu_line(stat.lines().next()?)?;
        let own = parse_own_ticks(&std::fs::read_to_string("/proc/self/stat").ok()?)?;
        Some(Self { busy, total, own })
    }

    /// Fremde Last zwischen dieser und einer spaeteren Stichprobe, in
    /// Hundertstel Kernen. `None` ohne Fenster oder bei rueckwaerts laufenden
    /// Zaehlern.
    #[must_use]
    pub fn foreign_cores_centi(self, later: Self) -> Option<u64> {
        let busy = later.busy.checked_sub(self.busy)?;
        let total = later.total.checked_sub(self.total)?;
        let own = later.own.checked_sub(self.own)?;
        if total == 0 {
            return None;
        }
        let cores = u64::try_from(std::thread::available_parallelism().map_or(1, NonZero::get))
            .unwrap_or(1);
        busy.saturating_sub(own)
            .saturating_mul(100)
            .saturating_mul(cores)
            .checked_div(total)
    }
}

/// Die Summenzeile `cpu  user nice system idle iowait irq softirq steal …`
/// als `(belegt, gesamt)`.
#[must_use]
pub fn parse_cpu_line(line: &str) -> Option<(u64, u64)> {
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields
        .take(8)
        .map(|v| v.parse().ok())
        .collect::<Option<_>>()?;
    let idle = values.get(3)?.checked_add(*values.get(4)?)?;
    let total = values
        .iter()
        .try_fold(0_u64, |sum, v| sum.checked_add(*v))?;
    Some((total.checked_sub(idle)?, total))
}

/// Felder 14 bis 17 von `/proc/<pid>/stat`, gezaehlt hinter dem Namen in
/// Klammern — der Name selbst darf Leerzeichen und Klammern enthalten.
#[must_use]
pub fn parse_own_ticks(stat: &str) -> Option<u64> {
    let rest = stat.get(stat.rfind(')')?.checked_add(1)?..)?;
    rest.split_whitespace()
        .skip(11)
        .take(4)
        .map(|v| v.parse::<u64>().ok())
        .try_fold(0_u64, |sum, v| sum.checked_add(v?))
}

#[cfg(test)]
mod tests {
    use super::{CpuSample, parse_cpu_line, parse_own_ticks};

    /// Fremdlast ist fremde Rechenzeit, nicht die Laenge der Warteschlange.
    #[test]
    fn foreign_load_is_cpu_time_not_the_run_queue() {
        assert_eq!(
            parse_cpu_line("cpu  100 0 50 800 50 0 0 0 0 0"),
            Some((150, 1000)),
            "Leerlauf und Warten auf I/O zaehlen nicht als belegt"
        );
        assert_eq!(
            parse_cpu_line("cpu0 1 2 3 4 5 6 7 8"),
            None,
            "nur die Summenzeile"
        );
        assert_eq!(
            parse_own_ticks("42 (vig autotune) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15"),
            Some(11 + 12 + 13 + 14),
            "utime, stime, cutime, cstime hinter dem Namen, auch mit Leerzeichen darin"
        );
        let before = CpuSample {
            busy: 0,
            total: 0,
            own: 0,
        };
        let later = CpuSample {
            busy: 100,
            total: 1000,
            own: 100,
        };
        assert_eq!(
            before.foreign_cores_centi(later),
            Some(0),
            "die eigene Messung ist keine fremde Last"
        );
        assert_eq!(
            before.foreign_cores_centi(before),
            None,
            "kein Fenster, keine Aussage"
        );
        assert_eq!(
            later.foreign_cores_centi(before),
            None,
            "rueckwaerts laufende Zaehler sind keine Beobachtung"
        );
    }
}
