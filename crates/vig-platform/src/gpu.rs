//! Der Zustand einer GPU, so wie `nvidia-smi` ihn meldet (NV-04).
//!
//! ## Warum `nvidia-smi` und nicht NVML
//!
//! NVML waere die direktere Quelle und braeuchte kein Unterprozess. Sie
//! braeuchte aber auch eine Bindung an eine Herstellerbibliothek, die zur
//! Treiberversion passen muss — und damit genau die Kopplung, die dieses
//! Projekt an seiner Peripherie vermeidet. `nvidia-smi` liegt auf jedem
//! System mit Treiber, laeuft ohne Root und liefert eine stabile CSV-Zeile.
//! Der Preis ist ein Prozessstart je Messung; bei einer Messfrequenz im
//! Sekundenbereich ist das kein Argument.
//!
//! ## Der Drosselgrund ist die eigentliche Nachricht
//!
//! Temperatur und Takt sind Symptome. Interessant ist, **warum** die Karte
//! langsamer laeuft: ein Power Cap ist eine andere Geschichte als ein
//! thermisches Limit oder eine vom Betreiber gesetzte Taktvorgabe. NVML
//! meldet das als Bitmaske; hier wird sie in benannte Gruende uebersetzt,
//! weil eine Zahl wie `0x4` in einem Betreiber-Dashboard nichts erklaert.

use crate::{Observation, Sample, Source};
use serde::{Deserialize, Serialize};

/// Warum eine Karte gerade nicht mit vollem Takt laeuft.
///
/// Die Bits stammen aus NVMLs `clocksEventReasons`. Unbekannte Bits werden
/// nicht verschwiegen, sondern als [`ThrottleReason::Unknown`] mitgefuehrt —
/// ein neuer Treiber soll keine Information verschlucken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThrottleReason {
    /// Die Karte hat nichts zu tun.
    GpuIdle,
    /// Der Betreiber hat den Takt festgesetzt.
    ApplicationClocks,
    /// Softwareseitiges Leistungslimit.
    SwPowerCap,
    /// Hardwareseitige Notbremse.
    HwSlowdown,
    /// Sync-Boost-Gruppe.
    SyncBoost,
    /// Softwareseitiges thermisches Limit.
    SwThermalSlowdown,
    /// Hardwareseitiges thermisches Limit.
    HwThermalSlowdown,
    /// Externe Leistungsbremse.
    HwPowerBrake,
    /// Taktvorgabe durch die Anzeige.
    DisplayClockSetting,
    /// Ein Bit, das dieser Code nicht kennt.
    Unknown {
        /// Die Bitposition.
        bit: u32,
    },
}

impl ThrottleReason {
    /// Ob dieser Grund die Laufzeit eines Modells veraendert.
    ///
    /// `GpuIdle` tut es nicht — die Karte ist langsam, weil nichts zu tun ist,
    /// und wird schnell, sobald etwas kommt. Ein Profil deshalb fuer
    /// ungueltig zu erklaeren waere ein Fehlalarm bei jedem Leerlauf.
    #[must_use]
    pub const fn affects_runtime(self) -> bool {
        !matches!(self, Self::GpuIdle)
    }
}

/// Uebersetzt die NVML-Bitmaske in benannte Gruende.
#[must_use]
pub fn throttle_reasons(mask: u64) -> Vec<ThrottleReason> {
    const KNOWN: [(u32, ThrottleReason); 9] = [
        (0, ThrottleReason::GpuIdle),
        (1, ThrottleReason::ApplicationClocks),
        (2, ThrottleReason::SwPowerCap),
        (3, ThrottleReason::HwSlowdown),
        (4, ThrottleReason::SyncBoost),
        (5, ThrottleReason::SwThermalSlowdown),
        (6, ThrottleReason::HwThermalSlowdown),
        (7, ThrottleReason::HwPowerBrake),
        (8, ThrottleReason::DisplayClockSetting),
    ];
    let mut out = Vec::new();
    for bit in 0..64_u32 {
        if mask & (1_u64 << bit) == 0 {
            continue;
        }
        match KNOWN.iter().find(|(b, _)| *b == bit) {
            Some((_, reason)) => out.push(*reason),
            None => out.push(ThrottleReason::Unknown { bit }),
        }
    }
    out
}

/// Der Zustand einer GPU.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GpuState {
    /// Der Index, wie der Treiber ihn vergibt.
    pub index: u32,
    /// Der Geraetename.
    pub name: Observation<String>,
    /// Die Treiberversion.
    pub driver: Observation<String>,
    /// Die CUDA Compute Capability.
    pub compute_capability: Observation<String>,
    /// Der Geraetespeicher in MiB.
    pub memory_total_mib: Observation<u64>,
    /// Die Temperatur in Grad Celsius.
    pub temperature_c: Observation<u32>,
    /// Der aktuelle SM-Takt in MHz.
    pub clock_sm_mhz: Observation<u32>,
    /// Der maximale SM-Takt in MHz.
    pub clock_sm_max_mhz: Observation<u32>,
    /// Die Leistungsaufnahme in Milliwatt.
    pub power_draw_mw: Observation<u32>,
    /// Das Leistungslimit in Milliwatt.
    pub power_limit_mw: Observation<u32>,
    /// Der Performance-State, etwa `P0`.
    pub performance_state: Observation<String>,
    /// Ob der Persistence-Mode aktiv ist.
    pub persistence_mode: Observation<bool>,
    /// Warum die Karte gedrosselt wird.
    pub throttle_reasons: Observation<Vec<ThrottleReason>>,
}

impl GpuState {
    /// Die Gruende, die tatsaechlich Laufzeit kosten.
    #[must_use]
    pub fn limiting_reasons(&self) -> Vec<ThrottleReason> {
        self.throttle_reasons
            .value()
            .map(|reasons| {
                reasons
                    .iter()
                    .copied()
                    .filter(|r| r.affects_runtime())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Ob die Karte gerade unter ihrem Maximaltakt laeuft.
    ///
    /// `None`, wenn einer der beiden Takte fehlt — „nicht gemessen" ist keine
    /// Aussage ueber den Takt.
    #[must_use]
    pub fn below_max_clock(&self) -> Option<bool> {
        let current = self.clock_sm_mhz.value()?;
        let max = self.clock_sm_max_mhz.value()?;
        Some(current < max)
    }
}

/// Die Abfragefelder, in der Reihenfolge, in der sie geparst werden.
///
/// Steht hier und nicht im Collector, damit Abfrage und Parser nicht
/// auseinanderlaufen koennen: eine Spalte hier zu ergaenzen und dort zu
/// vergessen waere ein stiller Versatz aller folgenden Felder.
pub const QUERY_FIELDS: &str = "index,name,driver_version,compute_cap,memory.total,\
                               temperature.gpu,clocks.current.sm,clocks.max.sm,\
                               power.draw,power.limit,pstate,persistence_mode,\
                               clocks_event_reasons.active";

/// Wie viele Spalten [`QUERY_FIELDS`] hat.
const COLUMNS: usize = 13;

/// Liest eine CSV-Zeile von `nvidia-smi --query-gpu`.
///
/// Erwartet `--format=csv,noheader,nounits` und die Felder aus
/// [`QUERY_FIELDS`]. Gibt `None`, wenn die Zeile nicht die erwartete Form
/// hat — eine halb geparste Zeile waere schlimmer als keine.
///
/// Ein Feld, das die Karte nicht kennt, meldet `nvidia-smi` als `[N/A]` oder
/// `[Not Supported]`. Das wird zu [`Observation::Unsupported`] und nicht zu
/// null: ein Laptop-Ampere ohne gemeldetes Leistungslimit ist kein Ausfall.
#[must_use]
pub fn parse_query_line(line: &str, observed_at_ms: u64) -> Option<GpuState> {
    let fields: Vec<&str> = line.split(',').map(str::trim).collect();
    if fields.len() != COLUMNS {
        return None;
    }
    let field = |i: usize| -> &str { fields.get(i).copied().unwrap_or("") };
    let index: u32 = field(0).parse().ok()?;

    let at = observed_at_ms;
    Some(GpuState {
        index,
        name: text(field(1), at),
        driver: text(field(2), at),
        compute_capability: text(field(3), at),
        memory_total_mib: number(field(4), at),
        temperature_c: number(field(5), at),
        clock_sm_mhz: number(field(6), at),
        clock_sm_max_mhz: number(field(7), at),
        power_draw_mw: milliwatts(field(8), at),
        power_limit_mw: milliwatts(field(9), at),
        performance_state: text(field(10), at),
        persistence_mode: boolean(field(11), at),
        throttle_reasons: mask(field(12), at),
    })
}

/// Ob `nvidia-smi` das Feld als nicht unterstuetzt meldet.
fn unsupported(raw: &str) -> Option<Observation<()>> {
    let flat = raw.trim();
    if flat.eq_ignore_ascii_case("[N/A]")
        || flat.eq_ignore_ascii_case("N/A")
        || flat.eq_ignore_ascii_case("[Not Supported]")
        || flat.eq_ignore_ascii_case("Not Supported")
        || flat.eq_ignore_ascii_case("[Unknown Error]")
    {
        return Some(Observation::Unsupported {
            reason: format!("nvidia-smi meldet {flat}"),
        });
    }
    if flat.is_empty() {
        return Some(Observation::Unavailable {
            reason: "leeres Feld".to_owned(),
        });
    }
    None
}

/// Ueberfuehrt einen als nicht unterstuetzt erkannten Wert in den Zieltyp.
fn carry<T>(marker: Observation<()>) -> Observation<T> {
    match marker {
        Observation::Unsupported { reason } => Observation::Unsupported { reason },
        Observation::Unavailable { reason } => Observation::Unavailable { reason },
        // `unsupported` gibt niemals `Observed`; der Arm existiert nur, weil
        // der Typ ihn kennt.
        Observation::Observed(_) => Observation::Unavailable {
            reason: "unerwarteter Messwert".to_owned(),
        },
    }
}

fn text(raw: &str, at: u64) -> Observation<String> {
    if let Some(marker) = unsupported(raw) {
        return carry(marker);
    }
    Observation::Observed(Sample {
        value: raw.to_owned(),
        source: Source::NvidiaSmi,
        observed_at_ms: at,
    })
}

fn number<T: core::str::FromStr>(raw: &str, at: u64) -> Observation<T> {
    if let Some(marker) = unsupported(raw) {
        return carry(marker);
    }
    match raw.parse::<T>() {
        Ok(value) => Observation::Observed(Sample {
            value,
            source: Source::NvidiaSmi,
            observed_at_ms: at,
        }),
        Err(_) => Observation::Unavailable {
            reason: format!("unlesbare Zahl {raw:?}"),
        },
    }
}

/// Watt mit Nachkommastelle als Milliwatt.
///
/// Ganzzahlig, weil Gleitkomma im Vergleich zweier Snapshots Aenderungen
/// erfinden wuerde, die keine sind.
fn milliwatts(raw: &str, at: u64) -> Observation<u32> {
    if let Some(marker) = unsupported(raw) {
        return carry(marker);
    }
    let Ok(watts) = raw.parse::<f64>() else {
        return Observation::Unavailable {
            reason: format!("unlesbare Leistung {raw:?}"),
        };
    };
    let scaled = watts * 1000.0;
    if !scaled.is_finite() || scaled < 0.0 || scaled > f64::from(u32::MAX) {
        return Observation::Unavailable {
            reason: format!("Leistung ausserhalb des Darstellbaren: {raw:?}"),
        };
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Bereich zuvor geprueft"
    )]
    let value = scaled.round() as u32;
    Observation::Observed(Sample {
        value,
        source: Source::NvidiaSmi,
        observed_at_ms: at,
    })
}

fn boolean(raw: &str, at: u64) -> Observation<bool> {
    if let Some(marker) = unsupported(raw) {
        return carry(marker);
    }
    let value = match raw.to_ascii_lowercase().as_str() {
        "enabled" | "1" | "true" => true,
        "disabled" | "0" | "false" => false,
        _ => {
            return Observation::Unavailable {
                reason: format!("unlesbarer Schalter {raw:?}"),
            };
        }
    };
    Observation::Observed(Sample {
        value,
        source: Source::NvidiaSmi,
        observed_at_ms: at,
    })
}

fn mask(raw: &str, at: u64) -> Observation<Vec<ThrottleReason>> {
    if let Some(marker) = unsupported(raw) {
        return carry(marker);
    }
    let hex = raw.trim_start_matches("0x").trim_start_matches("0X");
    let Ok(bits) = u64::from_str_radix(hex, 16) else {
        return Observation::Unavailable {
            reason: format!("unlesbare Drosselmaske {raw:?}"),
        };
    };
    Observation::Observed(Sample {
        value: throttle_reasons(bits),
        source: Source::NvidiaSmi,
        observed_at_ms: at,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// Eine echte Zeile dieses Messrechners, waehrend der Dauerlauf lief.
    const REAL: &str = "0, NVIDIA GeForce RTX 3070 Laptop GPU, 580.173.02, 8.6, 8192, \
                        80, 1740, 2100, 129.55, [N/A], P0, Disabled, \
                        0x0000000000000004";

    #[test]
    fn a_real_line_parses_completely() {
        let gpu = parse_query_line(REAL, 1_000).unwrap();
        assert_eq!(gpu.index, 0);
        assert_eq!(
            gpu.name.value().map(String::as_str),
            Some("NVIDIA GeForce RTX 3070 Laptop GPU")
        );
        assert_eq!(gpu.driver.value().map(String::as_str), Some("580.173.02"));
        assert_eq!(
            gpu.compute_capability.value().map(String::as_str),
            Some("8.6")
        );
        assert_eq!(gpu.memory_total_mib.value(), Some(&8192));
        assert_eq!(gpu.temperature_c.value(), Some(&80));
        assert_eq!(gpu.clock_sm_mhz.value(), Some(&1740));
        assert_eq!(gpu.clock_sm_max_mhz.value(), Some(&2100));
        assert_eq!(gpu.power_draw_mw.value(), Some(&129_550));
        assert_eq!(
            gpu.performance_state.value().map(String::as_str),
            Some("P0")
        );
        assert_eq!(gpu.persistence_mode.value(), Some(&false));
    }

    #[test]
    fn a_field_the_card_does_not_report_is_unsupported_not_zero() {
        // Diese Karte meldet kein Leistungslimit. Das ist kein Ausfall — und
        // vor allem nicht null Watt.
        let gpu = parse_query_line(REAL, 1_000).unwrap();
        assert!(
            matches!(gpu.power_limit_mw, Observation::Unsupported { .. }),
            "{:?}",
            gpu.power_limit_mw
        );
        assert_eq!(gpu.power_limit_mw.value(), None);
    }

    #[test]
    fn the_throttle_mask_becomes_a_named_reason() {
        let gpu = parse_query_line(REAL, 1_000).unwrap();
        assert_eq!(
            gpu.throttle_reasons.value(),
            Some(&vec![ThrottleReason::SwPowerCap]),
            "0x4 ist das Leistungslimit — und der Grund, warum diese Karte \
             waehrend des Dauerlaufs 1740 statt 2100 MHz laeuft"
        );
        assert_eq!(gpu.limiting_reasons(), vec![ThrottleReason::SwPowerCap]);
        assert_eq!(gpu.below_max_clock(), Some(true));
    }

    #[test]
    fn an_idle_card_is_not_a_throttled_card() {
        let reasons = throttle_reasons(0x1);
        assert_eq!(reasons, vec![ThrottleReason::GpuIdle]);
        assert!(!reasons[0].affects_runtime());
    }

    #[test]
    fn an_unknown_bit_is_kept_not_dropped() {
        let reasons = throttle_reasons(1 << 40);
        assert_eq!(reasons, vec![ThrottleReason::Unknown { bit: 40 }]);
        assert!(reasons[0].affects_runtime());
    }

    #[test]
    fn several_reasons_come_back_in_bit_order() {
        let reasons = throttle_reasons(0x4 | 0x40);
        assert_eq!(
            reasons,
            vec![
                ThrottleReason::SwPowerCap,
                ThrottleReason::HwThermalSlowdown
            ]
        );
    }

    #[test]
    fn a_line_with_the_wrong_column_count_is_rejected() {
        assert!(parse_query_line("0, only, three", 1).is_none());
        assert!(parse_query_line(&format!("{REAL}, extra"), 1).is_none());
    }

    #[test]
    fn an_unreadable_number_is_unavailable_not_zero() {
        let broken = REAL.replace(" 8192, ", " achttausend, ");
        let gpu = parse_query_line(&broken, 1).unwrap();
        assert!(matches!(
            gpu.memory_total_mib,
            Observation::Unavailable { .. }
        ));
    }

    #[test]
    fn a_missing_clock_makes_no_statement_about_the_clock() {
        let broken = REAL.replace(" 1740, ", " [N/A], ");
        let gpu = parse_query_line(&broken, 1).unwrap();
        assert_eq!(
            gpu.below_max_clock(),
            None,
            "nicht gemessen ist keine Aussage ueber den Takt"
        );
    }

    #[test]
    fn the_query_field_list_matches_the_parser() {
        assert_eq!(QUERY_FIELDS.split(',').count(), COLUMNS);
    }
}
