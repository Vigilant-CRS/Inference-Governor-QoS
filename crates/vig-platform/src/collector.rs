//! Woher die Momentaufnahmen kommen — und was passiert, wenn sie ausbleiben
//! (NV-04).
//!
//! ## Der Rueckfall ist Teil des Entwurfs
//!
//! Ein Collector kann ausfallen: `nvidia-smi` fehlt, der Treiber wird
//! neu geladen, ein Container sieht das Geraet nicht mehr. Die falsche
//! Antwort darauf waere, den letzten bekannten Zustand weiterzubenutzen —
//! dann plant der Governor auf einer Beobachtung, die es nicht mehr gibt.
//!
//! Die Antwort hier ist [`CollectorHealth`]: nach einer festgelegten Zahl
//! aufeinanderfolgender Fehlversuche gilt der Zustand als
//! [`Fallback::QualifiedProfileOnly`]. Das heisst konkret: **keine staerkere
//! Zulassung als das fest qualifizierte Betriebsprofil.** Der Governor laeuft
//! weiter — er laeuft nur nicht mutiger, als er es belegen kann.
//!
//! ## Warum ein einzelner Fehlversuch nicht reicht
//!
//! `nvidia-smi` kann unter Last einmal in einen Timeout laufen, ohne dass
//! sich an der Hardware etwas geaendert haette. Wuerde schon der erste
//! Fehlversuch den Rueckfall ausloesen, waere die Betriebsart des Governors
//! an die Laune eines Unterprozesses gekoppelt. Deshalb eine Schwelle — und
//! deshalb ist sie klein.

use crate::gpu::{QUERY_FIELDS, parse_query_line};
use crate::snapshot::HardwareSnapshot;
use crate::{Observation, Source, now_ms};

/// Eine Quelle fuer Momentaufnahmen.
pub trait Collector {
    /// Eine Aufnahme, oder der Grund, warum keine zustande kam.
    ///
    /// # Errors
    ///
    /// Wenn die Quelle nicht erreichbar oder ihre Antwort unlesbar ist.
    fn snapshot(&mut self) -> Result<HardwareSnapshot, String>;
}

/// Wie der Governor ohne Hardwarebeobachtung arbeitet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Beobachtung liegt vor; zustandsabhaengige Planung ist zulaessig.
    StateAware,
    /// Keine belastbare Beobachtung: nur das fest qualifizierte
    /// Betriebsprofil, keine staerkere Zulassung.
    QualifiedProfileOnly,
}

/// Der Gesundheitszustand eines Collectors.
///
/// Zaehlt aufeinanderfolgende Fehlversuche und entscheidet daraus die
/// Betriebsart. Ein einzelner Erfolg setzt den Zaehler zurueck: ein
/// Collector, der wieder antwortet, ist wieder brauchbar.
#[derive(Debug, Clone)]
pub struct CollectorHealth {
    consecutive_failures: u32,
    threshold: u32,
    last_reason: Option<String>,
    last_success_ms: Option<u64>,
}

impl CollectorHealth {
    /// Die Voreinstellung: drei Fehlversuche hintereinander.
    ///
    /// Gross genug, dass ein einzelner Timeout unter Last nichts umschaltet;
    /// klein genug, dass ein echter Ausfall binnen weniger Sekunden wirkt.
    pub const DEFAULT_THRESHOLD: u32 = 3;

    /// Ein neuer Gesundheitszustand.
    #[must_use]
    pub const fn new(threshold: u32) -> Self {
        Self {
            consecutive_failures: 0,
            threshold,
            last_reason: None,
            last_success_ms: None,
        }
    }

    /// Eine gelungene Messung.
    pub fn record_success(&mut self, at_ms: u64) {
        self.consecutive_failures = 0;
        self.last_reason = None;
        self.last_success_ms = Some(at_ms);
    }

    /// Ein Fehlversuch.
    pub fn record_failure(&mut self, reason: &str) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_reason = Some(reason.to_owned());
    }

    /// Die aktuelle Betriebsart.
    #[must_use]
    pub const fn fallback(&self) -> Fallback {
        if self.consecutive_failures >= self.threshold {
            Fallback::QualifiedProfileOnly
        } else {
            Fallback::StateAware
        }
    }

    /// Wie viele Fehlversuche seit dem letzten Erfolg.
    #[must_use]
    pub const fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Der zuletzt genannte Grund.
    #[must_use]
    pub fn last_reason(&self) -> Option<&str> {
        self.last_reason.as_deref()
    }

    /// Wann zuletzt eine Messung gelang.
    #[must_use]
    pub const fn last_success_ms(&self) -> Option<u64> {
        self.last_success_ms
    }
}

impl Default for CollectorHealth {
    fn default() -> Self {
        Self::new(Self::DEFAULT_THRESHOLD)
    }
}

/// Der Collector, der `nvidia-smi` aufruft.
///
/// Startet je Messung einen Unterprozess. Bei einer Messfrequenz im
/// Sekundenbereich ist das guenstiger als eine Bindung an eine
/// Herstellerbibliothek, die zur Treiberversion passen muss.
#[derive(Debug, Clone)]
pub struct NvidiaSmi {
    binary: String,
}

impl Default for NvidiaSmi {
    fn default() -> Self {
        Self {
            binary: "nvidia-smi".to_owned(),
        }
    }
}

impl NvidiaSmi {
    /// Ein Collector, der ein bestimmtes Binary aufruft.
    #[must_use]
    pub fn at(binary: &str) -> Self {
        Self {
            binary: binary.to_owned(),
        }
    }

    /// Die Argumente, mit denen abgefragt wird.
    #[must_use]
    pub fn arguments() -> [String; 2] {
        [
            format!("--query-gpu={QUERY_FIELDS}"),
            "--format=csv,noheader,nounits".to_owned(),
        ]
    }
}

/// Wie lange ein Aufruf des Collectors hoechstens dauern darf.
///
/// `nvidia-smi` antwortet gewoehnlich in Millisekunden. Es kann aber haengen —
/// bei einer Karte im Fehlerzustand, bei einem blockierten Treiber, bei einem
/// Persistence-Daemon, der nicht antwortet. Ohne Frist wartete der Aufrufer
/// unbegrenzt: der Hardwarewaechter meldete dann nie einen Fehlversuch, der
/// letzte bekannte Zustand blieb stehen, und von aussen sah alles gut aus
/// (Review R07).
///
/// Drei Sekunden sind zwei Groessenordnungen ueber der ueblichen Antwortzeit
/// und deutlich unter dem Probenintervall.
pub const SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Wie oft waehrend des Wartens nachgesehen wird, ob der Prozess fertig ist.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// Wie lange nach einem Abbruch auf das Ende des Kindprozesses gewartet wird.
///
/// Danach wird er aufgegeben. Ein Prozess im ununterbrechbaren Zustand nimmt
/// kein Signal an; auf ihn zu warten hiesse, die Frist aufzugeben, die man
/// gerade durchsetzen wollte.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

impl Collector for NvidiaSmi {
    fn snapshot(&mut self) -> Result<HardwareSnapshot, String> {
        let taken_at_ms = now_ms().ok_or_else(|| "Systemuhr vor der Epoche".to_owned())?;
        let output = run_with_timeout(&self.binary, &Self::arguments(), SNAPSHOT_TIMEOUT)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "{} meldet Fehler: {}",
                self.binary,
                stderr.trim().chars().take(200).collect::<String>()
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(parse_output(&text, taken_at_ms))
    }
}

/// Faehrt ein Kommando mit einer Frist.
///
/// Laeuft es laenger, wird es abgebrochen und der Aufrufer bekommt einen
/// Fehler. Ein haengender Waechter ist schlimmer als ein fehlender: er meldet
/// nie etwas, und das liest sich wie „alles in Ordnung".
///
/// Die Ausgabe wird erst nach dem Ende gelesen. Fuer `nvidia-smi` traegt das:
/// seine Ausgabe sind wenige hundert Bytes und passt in den Pipe-Puffer. Ein
/// Kommando mit grosser Ausgabe braeuchte einen mitlesenden Thread.
///
/// # Errors
///
/// Wenn das Kommando nicht startbar ist, die Frist reisst oder es mit einem
/// Fehlercode endet.
fn run_with_timeout(
    binary: &str,
    args: &[String],
    limit: std::time::Duration,
) -> Result<std::process::Output, String> {
    let mut child = std::process::Command::new(binary)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{binary} nicht ausfuehrbar: {e}"))?;

    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => return Err(format!("{binary}: {e}")),
        }
        if started.elapsed() >= limit {
            // Abbrechen — und **nicht** auf das Ende warten.
            //
            // `kill` wirkt nur auf einen Prozess, der Signale annehmen kann.
            // Steckt er im ununterbrechbaren Zustand (`D`), weil der
            // Grafiktreiber nicht antwortet, nimmt er keines an, und ein
            // `wait()` daneben wartet mit ihm — die Frist waere dann keine.
            //
            // Auf dieser Maschine ist genau das passiert: 188 nicht beendbare
            // `nvidia-smi`-Prozesse, jeder mit seinem Waechter daran. Ein
            // Zombie ist das kleinere Uebel als ein Waechter, der nie wieder
            // etwas meldet.
            let _ = child.kill();
            let grace = std::time::Instant::now();
            while grace.elapsed() < KILL_GRACE {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            return Err(format!(
                "{binary} hat nach {} ms nicht geantwortet und wurde abgebrochen",
                limit.as_millis()
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    child
        .wait_with_output()
        .map_err(|e| format!("{binary}: {e}"))
}

/// Baut eine Aufnahme aus der vollstaendigen Ausgabe von `nvidia-smi`.
///
/// Eine unlesbare Zeile wird uebersprungen und nicht geraten: eine halb
/// geparste Karte waere schlimmer als eine fehlende.
#[must_use]
pub fn parse_output(text: &str, taken_at_ms: u64) -> HardwareSnapshot {
    let gpus = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| parse_query_line(line, taken_at_ms))
        .collect();
    HardwareSnapshot {
        taken_at_ms,
        gpus,
        // Ein Leistungsmodus im Sinne von `nvpmodel` existiert nur auf
        // Jetson. Ihn hier zu erfinden waere die Art von Vollstaendigkeit,
        // die eine Luecke versteckt.
        power_mode: Observation::Unsupported {
            reason: "nvidia-smi meldet keinen Plattform-Leistungsmodus".to_owned(),
        },
    }
}

/// Ein Collector, der aufgezeichnete Aufnahmen abspielt.
///
/// Damit lassen sich Zustandswechsel testen, die sich auf einem Messrechner
/// nicht bestellen lassen — ein thermisches Limit tritt ein, wenn es eintritt,
/// und nicht, wenn ein Test es braucht.
#[derive(Debug, Clone)]
pub struct Recorded {
    snapshots: Vec<HardwareSnapshot>,
    position: usize,
}

impl Recorded {
    /// Aus einer Reihe von Aufnahmen.
    #[must_use]
    pub const fn new(snapshots: Vec<HardwareSnapshot>) -> Self {
        Self {
            snapshots,
            position: 0,
        }
    }

    /// Aus einer YAML-Aufzeichnung.
    ///
    /// # Errors
    ///
    /// Wenn der Text keine Liste von Aufnahmen ist.
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        let snapshots: Vec<HardwareSnapshot> =
            serde_norway::from_str(text).map_err(|e| e.to_string())?;
        Ok(Self::new(snapshots))
    }

    /// Die Aufzeichnung als YAML.
    ///
    /// # Errors
    ///
    /// Wenn die Aufnahmen nicht serialisierbar sind.
    pub fn to_yaml(&self) -> Result<String, String> {
        serde_norway::to_string(&self.snapshots).map_err(|e| e.to_string())
    }

    /// Wie viele Aufnahmen noch kommen.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.snapshots.len().saturating_sub(self.position)
    }

    /// Die Quelle jeder abgespielten Aufnahme auf `Recorded` setzen.
    ///
    /// Ein Replay soll sich nicht als Messung ausgeben.
    #[must_use]
    pub fn marked(mut self) -> Self {
        for snapshot in &mut self.snapshots {
            for gpu in &mut snapshot.gpus {
                mark(&mut gpu.name);
                mark(&mut gpu.driver);
                mark(&mut gpu.compute_capability);
                mark(&mut gpu.performance_state);
            }
        }
        self
    }
}

fn mark<T>(observation: &mut Observation<T>) {
    if let Observation::Observed(sample) = observation {
        sample.source = Source::Recorded;
    }
}

impl Collector for Recorded {
    fn snapshot(&mut self) -> Result<HardwareSnapshot, String> {
        let Some(snapshot) = self.snapshots.get(self.position) else {
            return Err("Aufzeichnung zu Ende".to_owned());
        };
        self.position = self.position.saturating_add(1);
        Ok(snapshot.clone())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::gpu::ThrottleReason;
    use crate::snapshot::diff;

    const TWO_CARDS: &str = "0, NVIDIA GeForce RTX 3070 Laptop GPU, 580.173.02, 8.6, 8192, \
                             80, 1740, 2100, 129.55, [N/A], P0, Disabled, 0x0000000000000004, 7001, 7001, GPU-test\n\
                             1, NVIDIA RTX A2000, 580.173.02, 8.6, 12288, \
                             55, 1200, 1500, 40.00, 70.00, P2, Enabled, 0x0000000000000000, 7001, 7001, GPU-test\n";

    #[test]
    fn several_cards_are_parsed_into_one_snapshot() {
        let snapshot = parse_output(TWO_CARDS, 1_000);
        assert_eq!(snapshot.gpus.len(), 2);
        assert_eq!(
            snapshot.gpu(1).unwrap().memory_total_mib.value(),
            Some(&12288)
        );
        assert_eq!(
            snapshot.gpu(1).unwrap().power_limit_mw.value(),
            Some(&70_000)
        );
        assert!(snapshot.gpu(1).unwrap().limiting_reasons().is_empty());
    }

    #[test]
    fn an_unreadable_line_is_skipped_not_guessed() {
        let mixed = format!("{TWO_CARDS}kaputte Zeile ohne Spalten\n");
        let snapshot = parse_output(&mixed, 1_000);
        assert_eq!(snapshot.gpus.len(), 2);
    }

    #[test]
    fn nvidia_smi_reports_no_platform_power_mode() {
        let snapshot = parse_output(TWO_CARDS, 1_000);
        assert!(matches!(
            snapshot.power_mode,
            Observation::Unsupported { .. }
        ));
    }

    #[test]
    fn the_query_arguments_ask_for_exactly_the_parsed_fields() {
        let args = NvidiaSmi::arguments();
        assert!(args[0].ends_with(QUERY_FIELDS));
        assert_eq!(args[1], "--format=csv,noheader,nounits");
    }

    // -- Rueckfall ---------------------------------------------------------

    #[test]
    fn a_single_failure_does_not_change_the_operating_mode() {
        let mut health = CollectorHealth::default();
        assert_eq!(health.fallback(), Fallback::StateAware);
        health.record_failure("Timeout");
        assert_eq!(
            health.fallback(),
            Fallback::StateAware,
            "ein Unterprozess mit schlechtem Tag darf die Betriebsart nicht umschalten"
        );
    }

    #[test]
    fn the_threshold_switches_to_the_qualified_profile() {
        let mut health = CollectorHealth::default();
        for _ in 0..CollectorHealth::DEFAULT_THRESHOLD {
            health.record_failure("nvidia-smi nicht ausfuehrbar");
        }
        assert_eq!(health.fallback(), Fallback::QualifiedProfileOnly);
        assert_eq!(
            health.last_reason(),
            Some("nvidia-smi nicht ausfuehrbar"),
            "der Betreiber soll den Grund sehen, nicht nur den Zustand"
        );
    }

    #[test]
    fn one_success_restores_the_operating_mode() {
        let mut health = CollectorHealth::default();
        for _ in 0..10 {
            health.record_failure("weg");
        }
        assert_eq!(health.fallback(), Fallback::QualifiedProfileOnly);
        health.record_success(5_000);
        assert_eq!(health.fallback(), Fallback::StateAware);
        assert_eq!(health.consecutive_failures(), 0);
        assert_eq!(health.last_success_ms(), Some(5_000));
        assert_eq!(health.last_reason(), None);
    }

    // -- Replay ------------------------------------------------------------

    #[test]
    fn a_recording_replays_in_order_and_then_ends() {
        let first = parse_output(TWO_CARDS, 1_000);
        let second = parse_output(&TWO_CARDS.replace(" 1740, ", " 900, "), 2_000);
        let mut recorded = Recorded::new(vec![first.clone(), second.clone()]);
        assert_eq!(recorded.remaining(), 2);
        assert_eq!(recorded.snapshot().unwrap(), first);
        assert_eq!(recorded.snapshot().unwrap(), second);
        assert!(recorded.snapshot().is_err(), "danach kommt nichts mehr");
    }

    #[test]
    fn a_recording_survives_a_roundtrip_and_replays_the_state_change() {
        // Der Zustandswechsel, den man auf echter Hardware nicht bestellen
        // kann: ein thermisches Limit tritt hinzu.
        let calm = parse_output(TWO_CARDS, 1_000);
        let hot = parse_output(
            &TWO_CARDS.replace(
                "0x0000000000000004, 7001, 7001, GPU-test",
                "0x0000000000000044, 7001, 7001, GPU-test",
            ),
            2_000,
        );
        let text = Recorded::new(vec![calm, hot]).to_yaml().unwrap();

        let mut replay = Recorded::from_yaml(&text).unwrap();
        let before = replay.snapshot().unwrap();
        let after = replay.snapshot().unwrap();
        let changes = diff(&before, &after);
        assert!(
            changes
                .iter()
                .any(|c| c.field == "throttle_reasons" && c.after.contains("HwThermalSlowdown")),
            "{changes:?}"
        );
        assert!(
            after
                .gpu(0)
                .unwrap()
                .limiting_reasons()
                .contains(&ThrottleReason::HwThermalSlowdown)
        );
    }

    #[test]
    fn a_replay_does_not_pass_itself_off_as_a_measurement() {
        let mut replay = Recorded::new(vec![parse_output(TWO_CARDS, 1_000)]).marked();
        let snapshot = replay.snapshot().unwrap();
        let Observation::Observed(sample) = &snapshot.gpu(0).unwrap().name else {
            panic!("Name muss beobachtet sein");
        };
        assert_eq!(sample.source, Source::Recorded);
    }

    #[test]
    fn a_missing_binary_is_an_error_not_an_empty_snapshot() {
        let mut collector = NvidiaSmi::at("/nonexistent-nvidia-smi");
        let error = collector.snapshot().unwrap_err();
        assert!(error.contains("nicht ausfuehrbar"), "{error}");
    }

    /// Ein haengendes Kommando wird abgebrochen, nicht abgewartet
    /// (Review R07).
    ///
    /// Ohne Frist wartete der Hardwarewaechter unbegrenzt: er meldete nie
    /// einen Fehlversuch, der letzte bekannte Zustand blieb stehen, und von
    /// aussen sah alles gut aus. Ein haengender Waechter ist schlimmer als
    /// ein fehlender.
    #[test]
    fn a_hanging_command_is_aborted_not_awaited() {
        let started = std::time::Instant::now();
        let result = super::run_with_timeout(
            "/bin/sleep",
            &["30".to_owned()],
            std::time::Duration::from_millis(150),
        );
        assert!(result.is_err(), "die Frist muss greifen");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "und sie muss es rechtzeitig tun: {:?}",
            started.elapsed()
        );
        let message = result.err().unwrap_or_default();
        assert!(message.contains("abgebrochen"), "{message}");
    }

    /// Ein Kommando, das rechtzeitig antwortet, kommt unveraendert durch.
    #[test]
    fn a_prompt_command_returns_its_output() {
        let output = super::run_with_timeout(
            "/bin/echo",
            &["hallo".to_owned()],
            std::time::Duration::from_secs(5),
        )
        .unwrap_or_else(|e| panic!("echo antwortet: {e}"));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hallo");
    }
}
