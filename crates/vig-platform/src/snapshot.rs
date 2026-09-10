//! Momentaufnahmen und ihr Vergleich (NV-04).
//!
//! ## Warum vollstaendige Aufnahmen
//!
//! Ein Zustand, der zwischen zwei Messungen wechselt, ist nicht beobachtet,
//! sondern erschlossen. Eine Aufnahme traegt deshalb ihren Zeitstempel und
//! alle Groessen zusammen; erst der Vergleich zweier Aufnahmen ergibt eine
//! Aenderung, und die traegt dann beide Zeitpunkte. Das ist der Unterschied
//! zwischen „der Takt fiel irgendwann" und „der Takt fiel zwischen 12:03:11
//! und 12:03:12".
//!
//! ## Wofuer der Vergleich da ist
//!
//! Fuer die Frage, die ein Betreiber nach einem schlechten Lauf stellt: *War
//! die Hardware vorher schon in einem anderen Zustand?* Faellt der Takt oder
//! kommt ein thermisches Limit hinzu, **bevor** die erste langsame
//! Fertigstellung eintrifft, dann ist die Ursache gefunden, statt gesucht.
//! Deshalb ist der Vergleich billig und aenderungsgetrieben: er meldet, was
//! anders ist, nicht was gleich blieb.

use crate::gpu::GpuState;
use crate::{Observation, Sample, Source};
use serde::{Deserialize, Serialize};

/// Eine vollstaendige Momentaufnahme der beobachtbaren Hardware.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct HardwareSnapshot {
    /// Wann die Aufnahme entstand, als Unix-Zeit in Millisekunden.
    pub taken_at_ms: u64,
    /// Der Zustand jeder gefundenen GPU.
    pub gpus: Vec<GpuState>,
    /// Der Leistungsmodus der Plattform, etwa aus `nvpmodel`.
    ///
    /// Auf einem Desktop oder Laptop gibt es keinen — dort steht hier
    /// [`Observation::Unsupported`] und nicht etwa „Standard".
    pub power_mode: Observation<String>,
}

impl HardwareSnapshot {
    /// Eine leere Aufnahme mit einem Grund.
    ///
    /// Nicht dasselbe wie „keine GPU vorhanden": der Grund bleibt erhalten,
    /// damit der Betreiber den Unterschied zwischen „keine Karte" und
    /// „Collector kaputt" sieht.
    #[must_use]
    pub fn unavailable(taken_at_ms: u64, reason: &str) -> Self {
        Self {
            taken_at_ms,
            gpus: Vec::new(),
            power_mode: Observation::Unavailable {
                reason: reason.to_owned(),
            },
        }
    }

    /// Die GPU mit diesem Index.
    #[must_use]
    pub fn gpu(&self, index: u32) -> Option<&GpuState> {
        self.gpus.iter().find(|g| g.index == index)
    }

    /// Der Leistungsmodus als beobachteter Wert.
    #[must_use]
    pub fn with_power_mode(mut self, mode: &str, source: Source) -> Self {
        self.power_mode = Observation::Observed(Sample {
            value: mode.to_owned(),
            source,
            observed_at_ms: self.taken_at_ms,
        });
        self
    }
}

/// Was sich zwischen zwei Aufnahmen geaendert hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Die betroffene GPU, falls es eine ist.
    pub gpu: Option<u32>,
    /// Welche Groesse, als Pfad wie `clock_sm_mhz`.
    pub field: &'static str,
    /// Der vorherige Zustand, als Text fuer die Ausgabe.
    pub before: String,
    /// Der neue Zustand.
    pub after: String,
    /// Zeitpunkt der vorherigen Aufnahme.
    pub from_ms: u64,
    /// Zeitpunkt der neuen Aufnahme.
    pub to_ms: u64,
}

/// Was sich zwischen zwei Aufnahmen geaendert hat.
///
/// Verglichen wird auf Wechsel der **Bedeutung**, nicht auf jede Ziffer:
/// Temperatur und Leistungsaufnahme schwanken staendig und wuerden den
/// Vergleich sonst mit Rauschen fluten. Gemeldet werden Identitaet, Takt,
/// Drosselgrund, Persistence-Mode, Performance-State und Leistungsmodus —
/// die Groessen, an denen sich ein Laufzeitprofil entscheidet.
#[must_use]
pub fn diff(before: &HardwareSnapshot, after: &HardwareSnapshot) -> Vec<Change> {
    let mut changes = Vec::new();
    let (from_ms, to_ms) = (before.taken_at_ms, after.taken_at_ms);

    let mut push = |gpu: Option<u32>, field: &'static str, a: String, b: String| {
        if a != b {
            changes.push(Change {
                gpu,
                field,
                before: a,
                after: b,
                from_ms,
                to_ms,
            });
        }
    };

    push(
        None,
        "power_mode",
        describe(&before.power_mode),
        describe(&after.power_mode),
    );

    for new in &after.gpus {
        let Some(old) = before.gpu(new.index) else {
            push(
                Some(new.index),
                "presence",
                "nicht vorhanden".to_owned(),
                "vorhanden".to_owned(),
            );
            continue;
        };
        let g = Some(new.index);
        push(g, "name", describe(&old.name), describe(&new.name));
        push(g, "driver", describe(&old.driver), describe(&new.driver));
        push(
            g,
            "compute_capability",
            describe(&old.compute_capability),
            describe(&new.compute_capability),
        );
        push(
            g,
            "memory_total_mib",
            describe(&old.memory_total_mib),
            describe(&new.memory_total_mib),
        );
        push(g, "clock_sm_mhz", describe_clock(old), describe_clock(new));
        push(
            g,
            "performance_state",
            describe(&old.performance_state),
            describe(&new.performance_state),
        );
        push(
            g,
            "persistence_mode",
            describe(&old.persistence_mode),
            describe(&new.persistence_mode),
        );
        push(
            g,
            "throttle_reasons",
            describe_reasons(old),
            describe_reasons(new),
        );
    }

    for old in &before.gpus {
        if after.gpu(old.index).is_none() {
            changes.push(Change {
                gpu: Some(old.index),
                field: "presence",
                before: "vorhanden".to_owned(),
                after: "nicht vorhanden".to_owned(),
                from_ms,
                to_ms,
            });
        }
    }

    changes
}

/// Ein Beobachtungszustand als Text.
///
/// „unsupported" und „unavailable" bleiben unterscheidbar: ein Wechsel von
/// „meldet die Karte nicht" zu „konnte gerade nicht gelesen werden" ist eine
/// Nachricht ueber den Collector, keine ueber die Karte.
fn describe<T: core::fmt::Debug>(observation: &Observation<T>) -> String {
    match observation {
        Observation::Observed(sample) => format!("{:?}", sample.value),
        Observation::Unsupported { .. } => "unsupported".to_owned(),
        Observation::Unavailable { .. } => "unavailable".to_owned(),
    }
}

/// Wie fein ein Taktwechsel gemeldet wird, in MHz.
///
/// Der SM-Takt schwankt unter Last staendig um einige zehn MHz. Jede dieser
/// Schwankungen zu melden ergaebe einen Aenderungsstrom, in dem die eine
/// wichtige Nachricht — der Takt ist eingebrochen — untergeht. 100 MHz ist
/// grob genug fuer Ruhe und fein genug, um einen Einbruch zu sehen: bei einer
/// Karte mit 2100 MHz Maximaltakt sind das unter fuenf Prozent.
const CLOCK_BUCKET_MHZ: u32 = 100;

/// Der Takt, auf [`CLOCK_BUCKET_MHZ`] gerundet.
fn describe_clock(gpu: &GpuState) -> String {
    match gpu.clock_sm_mhz.value() {
        Some(mhz) => {
            let bucket = mhz
                .checked_div(CLOCK_BUCKET_MHZ)
                .unwrap_or(0)
                .saturating_mul(CLOCK_BUCKET_MHZ);
            format!("~{bucket}")
        }
        None => describe(&gpu.clock_sm_mhz),
    }
}

/// Die Drosselgruende als stabiler Text.
fn describe_reasons(gpu: &GpuState) -> String {
    match gpu.throttle_reasons.value() {
        Some(reasons) => {
            let mut sorted = reasons.clone();
            sorted.sort_unstable();
            format!("{sorted:?}")
        }
        None => describe(&gpu.throttle_reasons),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::gpu::{ThrottleReason, parse_query_line};

    const LINE: &str = "0, NVIDIA GeForce RTX 3070 Laptop GPU, 580.173.02, 8.6, 8192, \
                        80, 1740, 2100, 129.55, [N/A], P0, Disabled, \
                        0x0000000000000004, 7001, 7001, GPU-test";

    fn snapshot(line: &str, at: u64) -> HardwareSnapshot {
        HardwareSnapshot {
            taken_at_ms: at,
            gpus: vec![parse_query_line(line, at).unwrap()],
            power_mode: Observation::Unsupported {
                reason: "kein nvpmodel auf dieser Plattform".to_owned(),
            },
        }
    }

    #[test]
    fn an_unchanged_state_produces_no_events() {
        let a = snapshot(LINE, 1_000);
        let b = snapshot(LINE, 2_000);
        assert!(diff(&a, &b).is_empty(), "{:?}", diff(&a, &b));
    }

    #[test]
    fn a_thermal_limit_appearing_is_visible_with_both_timestamps() {
        // Genau der Fall, um den es geht: der Zustand wechselt, und die
        // Aenderung ist sichtbar, bevor die erste langsame Fertigstellung
        // eintrifft.
        let before = snapshot(LINE, 1_000);
        let after = snapshot(
            &LINE.replace(
                "0x0000000000000004, 7001, 7001, GPU-test",
                "0x0000000000000044, 7001, 7001, GPU-test",
            ),
            2_000,
        );
        let changes = diff(&before, &after);
        let reason = changes
            .iter()
            .find(|c| c.field == "throttle_reasons")
            .unwrap();
        assert_eq!(reason.gpu, Some(0));
        assert_eq!(reason.from_ms, 1_000);
        assert_eq!(reason.to_ms, 2_000);
        assert!(reason.after.contains("HwThermalSlowdown"), "{reason:?}");
        assert!(
            after.gpus[0]
                .limiting_reasons()
                .contains(&ThrottleReason::HwThermalSlowdown)
        );
    }

    #[test]
    fn noise_does_not_produce_events() {
        // Temperatur und Leistungsaufnahme schwanken staendig. Wuerden sie
        // gemeldet, ginge die eine wichtige Nachricht im Rauschen unter.
        let before = snapshot(LINE, 1_000);
        let after = snapshot(
            &LINE.replace(" 80, ", " 74, ").replace("129.55", "88.20"),
            2_000,
        );
        assert!(diff(&before, &after).is_empty());
    }

    #[test]
    fn a_clock_drop_is_reported() {
        let before = snapshot(LINE, 1_000);
        let after = snapshot(&LINE.replace(" 1740, ", " 900, "), 2_000);
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field, "clock_sm_mhz");
        assert_eq!(changes[0].before, "~1700");
        assert_eq!(changes[0].after, "~900");
    }

    #[test]
    fn clock_jitter_is_not_a_state_change() {
        // Unter Last schwankt der SM-Takt staendig. Wuerde jede Schwankung
        // gemeldet, ginge der eine Einbruch im Strom unter.
        let before = snapshot(LINE, 1_000);
        let after = snapshot(&LINE.replace(" 1740, ", " 1785, "), 2_000);
        assert!(diff(&before, &after).is_empty());
    }

    #[test]
    fn a_disappearing_card_is_an_event() {
        let before = snapshot(LINE, 1_000);
        let after = HardwareSnapshot::unavailable(2_000, "nvidia-smi nicht ausfuehrbar");
        let changes = diff(&before, &after);
        assert!(changes.iter().any(|c| c.field == "presence"));
        assert!(
            changes.iter().any(|c| c.field == "power_mode"),
            "der Wechsel von unsupported nach unavailable ist eine Nachricht \
             ueber den Collector, keine ueber die Plattform"
        );
    }

    #[test]
    fn an_appearing_card_is_an_event() {
        let before = HardwareSnapshot::unavailable(1_000, "noch nichts gemessen");
        let after = snapshot(LINE, 2_000);
        let changes = diff(&before, &after);
        let presence = changes.iter().find(|c| c.field == "presence").unwrap();
        assert_eq!(presence.after, "vorhanden");
    }

    #[test]
    fn the_reason_order_does_not_matter() {
        // Der Treiber darf die Bits melden, wie er mag; ein Vergleich soll
        // daraus keine Aenderung machen.
        let a = snapshot(LINE, 1_000);
        let mut b = snapshot(LINE, 2_000);
        if let Observation::Observed(sample) = &mut b.gpus[0].throttle_reasons {
            sample.value = vec![ThrottleReason::SwPowerCap];
        }
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn a_snapshot_survives_a_roundtrip() {
        let before = snapshot(LINE, 1_000).with_power_mode("MAXN", Source::Proc);
        let text = serde_norway::to_string(&before).unwrap();
        let after: HardwareSnapshot = serde_norway::from_str(&text).unwrap();
        assert_eq!(before, after);
        assert!(diff(&before, &after).is_empty());
    }
}
