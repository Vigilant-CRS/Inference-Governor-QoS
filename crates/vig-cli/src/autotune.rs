//! `vig autotune` — die Qualifikation findet beim Anwender statt.
//!
//! Jede veroeffentlichte Zahl dieses Projekts stammt von einer Maschine. Was
//! auf der Hardware eines Interessenten passiert, wissen wir nicht — und
//! koennen es nicht wissen. Dieses Werkzeug ist die Antwort darauf: Es misst
//! dort, schreibt das Ergebnis fest und sagt, was es **nicht** festgestellt
//! hat.
//!
//! Es fuehrt die vorhandenen Schritte zusammen:
//!
//! 1. `vig init` — was das Backend ueber seine Modelle weiss.
//! 2. `vig calibrate` — Laufzeiten, Nebenlaeufigkeit, gerichtete Interferenz.
//! 3. `vig-fit` — lohnt sich der Governor auf dieser Last ueberhaupt?
//! 4. `vig doctor` — traegt die entstandene Konfiguration?
//!
//! ## Was dieses Werkzeug nicht darf
//!
//! Ein Werkzeug, das eine Qualifikation *aussprechen* kann, wird benutzt, um
//! sie auszusprechen. Deshalb kann dieses hier sie nur **verweigern oder
//! offenlassen** (ADR-0044):
//!
//! * **Eine verworfene Messreihe bleibt verworfen.** `vig calibrate` verwirft
//!   eine Reihe, wenn der Takt mittendrin wandert. Dann fehlt der Wert — er
//!   wird nicht geschaetzt, und der Bericht nennt, was fehlt und warum.
//! * **Fremdlast entwertet die Zelle.** Lief waehrend der Messung anderes auf
//!   der Maschine, ist das Ergebnis ein Hinweis und keine Qualifikation.
//! * **Ein Ergebnis gegen uns ist ein Ergebnis.** Sagt `vig-fit`, dass der
//!   Governor auf dieser Last nichts bringt, steht genau das im Bericht —
//!   als Feststellung, nicht als Kleingedrucktes.
//!
//! ## Warum die Vertraege vorher dastehen muessen
//!
//! Gemessen wird die Maschine, nicht die Anforderung. Wie oft eine Kamera
//! liefert und wie alt ein Ergebnis sein darf, ist eine Zusage des Betreibers
//! an seine Anwendung — keine Groesse, die ein Messwerkzeug herausfinden kann.
//! Ohne ausgefuellte Vertraege bricht `autotune` nach Schritt 1 ab und sagt,
//! welche Felder fehlen.
//!
//! ## Warum hier nichts NVIDIA-spezifisch ist
//!
//! Derselbe `vig` laeuft statisch auf einem Telefon vor einem
//! TFLite-Backend (`docs/benchmark/arm-serve.md`,
//! `docs/benchmark/android-gpu.md`). Dort gibt es kein `nvidia-smi` und
//! deshalb keinen Hardwarezustand. Das ist kein Fehler und wird auch nicht
//! stillschweigend durch etwas anderes ersetzt: Der Bericht sagt, dass der
//! Takt nicht beobachtbar war und die Messreihen folglich nicht gegen einen
//! wandernden Takt abgesichert werden konnten.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use crate::identity::IdentityArgs;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Ab dieser Systemlast gilt die Maschine als nicht ruhig.
///
/// Eine Eins-zu-eins-Schwelle waere zu streng — der Messprozess selbst zaehlt
/// mit —, eine zu grosszuegige verschweigt fremde Arbeit. 1,5 ist dieselbe
/// Grenze, die die Messskripte dieses Projekts verwenden.
const QUIET_LOADAVG: f64 = 1.5;

/// Was der Anwender vorab hoeren soll, in einem Satz.
///
/// Er steht auch im README und auf der Projektseite. Wenn er hier steht,
/// steht er an der Stelle, an der er eingeloest werden muss.
pub(crate) const PROMISE: &str = "Start it, and in half an hour it has measured your machine and \
     tells you what it can carry. And if it turns out you do not need us, it says that too.";

/// Die Zusage aus [`PROMISE`] in Sekunden.
///
/// Ueberschreitet die Schaetzung sie, sagt das Werkzeug das **vorher** und
/// nennt den kuerzeren Umfang. Eine Zusage, die erst hinterher nicht gilt,
/// ist keine.
const PROMISED_SECONDS: u64 = 30 * 60;

/// Die Schritte, aus denen ein Lauf besteht.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub(crate) enum Step {
    /// Was das Backend ueber seine Modelle weiss.
    Discover,
    /// Laufzeiten, Nebenlaeufigkeit, Interferenz.
    Measure,
    /// Lohnt es sich auf dieser Last?
    Fit,
    /// Traegt die entstandene Konfiguration?
    Check,
}

impl Step {
    /// Alle Schritte in ihrer Reihenfolge.
    pub(crate) const ALL: [Self; 4] = [Self::Discover, Self::Measure, Self::Fit, Self::Check];

    /// Der Name, unter dem der Schritt im Bericht und im Zustand steht.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Discover => "discover",
            Self::Measure => "measure",
            Self::Fit => "fit",
            Self::Check => "check",
        }
    }

    /// Der Schritt zu einem Namen aus der Zustandsdatei.
    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }

    /// Was der Schritt tut, in einem Satz fuer die Fortschrittsanzeige.
    const fn title(self) -> &'static str {
        match self {
            Self::Discover => "read the models from the backend",
            Self::Measure => "measure runtimes, concurrency and interference",
            Self::Fit => "is the governor worth it on this load?",
            Self::Check => "check the resulting configuration",
        }
    }

    /// Grobe Dauer in Sekunden, fuer die Schaetzung vor dem Start.
    ///
    /// Bewusst grob und eher zu hoch: Wer eine halbe Stunde einplant und nach
    /// zwanzig Minuten fertig ist, ist zufrieden; umgekehrt nicht.
    const fn rough_seconds(self, quick: bool) -> u64 {
        match self {
            Self::Discover => 5,
            Self::Measure => {
                if quick {
                    300
                } else {
                    900
                }
            }
            Self::Fit => {
                if quick {
                    60
                } else {
                    150
                }
            }
            Self::Check => 10,
        }
    }
}

/// Ob der Takt der Rechenhardware waehrend der Messung beobachtbar war.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Clock {
    /// Es gibt einen Hardwarezustand; eine Reihe kann an einem Taktwechsel
    /// scheitern und wird dann verworfen.
    Observable,
    /// Kein Hardwarezustand — etwa ohne `nvidia-smi`, wie auf dem Telefon.
    ///
    /// Die Messung laeuft trotzdem. Sie ist nur schwaecher abgesichert, und
    /// genau das gehoert in den Bericht statt eines Abbruchs.
    Unobservable { reason: String },
}

/// Wie ein Schritt ausgegangen ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Gelaufen, Ergebnis verwertbar.
    Done,
    /// Gelaufen, aber unter Fremdlast — ein Hinweis, keine Qualifikation.
    Contaminated { reason: String },
    /// Nicht gelaufen, mit Grund (nicht vorhanden, abgewaehlt, uebersprungen).
    Skipped { reason: String },
    /// Gelaufen und gescheitert.
    Failed { reason: String },
}

impl Outcome {
    /// Das Wort, das im Bericht steht.
    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Contaminated { .. } => "contaminated",
            Self::Skipped { .. } => "skipped",
            Self::Failed { .. } => "failed",
        }
    }

    /// Der Grund, soweit es einen gibt.
    pub(crate) fn reason(&self) -> Option<&str> {
        match self {
            Self::Done => None,
            Self::Contaminated { reason } | Self::Skipped { reason } | Self::Failed { reason } => {
                Some(reason)
            }
        }
    }
}

/// Das Ergebnis eines Schritts.
#[derive(Debug, Clone)]
pub(crate) struct StepResult {
    pub(crate) step: Step,
    pub(crate) outcome: Outcome,
    pub(crate) seconds: u64,
    /// Was der Schritt dem Bericht mitzuteilen hat, in ganzen Saetzen.
    pub(crate) notes: Vec<String>,
}

/// Was ein Messschritt ueber seine Qualifikation meldet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SeriesCount {
    pub(crate) qualified: u64,
    pub(crate) discarded: u64,
}

impl SeriesCount {
    /// Wie viele Reihen insgesamt gefahren wurden.
    pub(crate) const fn total(self) -> u64 {
        self.qualified.saturating_add(self.discarded)
    }
}

/// Der gesammelte Stand eines Laufs.
#[derive(Debug, Clone)]
pub(crate) struct Qualification {
    pub(crate) steps: Vec<StepResult>,
    pub(crate) series: SeriesCount,
    /// Das Urteil von `vig-fit`, woertlich — auch ein negatives.
    pub(crate) fit_verdict: Option<String>,
    /// Das Urteil von `vig doctor`: `READY`, `READY_WITH_WARNINGS`, `NOT_READY`.
    pub(crate) doctor: Option<String>,
    /// Die festgeschriebene Konfiguration, falls eine entstanden ist.
    pub(crate) config: Option<PathBuf>,
    /// War der Takt waehrend der Messung beobachtbar?
    pub(crate) clock: Clock,
    /// Das Manifest: worauf sich diese Messung bezieht (ADR-0019).
    pub(crate) manifest: Vec<(String, String)>,
}

impl Default for Qualification {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            series: SeriesCount::default(),
            fit_verdict: None,
            doctor: None,
            config: None,
            clock: Clock::Observable,
            manifest: Vec::new(),
        }
    }
}

impl Qualification {
    /// Lief irgendein Schritt unter Fremdlast?
    pub(crate) fn contaminated(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s.outcome, Outcome::Contaminated { .. }))
    }

    /// Ist ein Schritt gescheitert?
    pub(crate) fn failed(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s.outcome, Outcome::Failed { .. }))
    }

    /// Wurden alle Schritte erfolgreich ausgefuehrt?
    pub(crate) fn complete(&self) -> bool {
        Step::ALL.iter().all(|wanted| {
            self.steps
                .iter()
                .any(|s| s.step == *wanted && matches!(s.outcome, Outcome::Done))
        })
    }

    /// Die Schritte, die ein spaeterer Lauf nicht wiederholen muss.
    fn done_keys(&self) -> Vec<&'static str> {
        self.steps
            .iter()
            .filter(|s| matches!(s.outcome, Outcome::Done))
            .map(|s| s.step.key())
            .collect()
    }

    /// Das Freigabefeld — dieses Werkzeug spricht **nie** eine aus.
    ///
    /// Es kann eine Freigabe verweigern (etwas hielt nicht) oder sie
    /// offenlassen (nichts sprach dagegen, aber ein Messlauf stellt ueber
    /// seine eigene Hardware nichts fest, das ueber das Gemessene hinausgeht).
    /// Der Unterschied gehoert in den Bericht, damit niemand „verweigert" mit
    /// „nicht erteilt" verwechselt.
    pub(crate) fn release(&self) -> Release {
        let mut reasons = Vec::new();
        if self.failed() {
            reasons.push("a step failed".to_owned());
        }
        if self.contaminated() {
            reasons.push("measured under foreign load".to_owned());
        }
        if self.series.discarded > 0 {
            reasons.push(format!(
                "{} of {} measurement series were discarded",
                self.series.discarded,
                self.series.total()
            ));
        }
        if self.doctor.as_deref() == Some("NOT_READY") {
            reasons.push("vig doctor says NOT_READY".to_owned());
        }
        if !self.complete() {
            reasons.push("not every step ran".to_owned());
        }
        if reasons.is_empty() {
            Release::NotIssued
        } else {
            Release::Refused { reasons }
        }
    }
}

/// Das Freigabefeld eines Laufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Release {
    /// Etwas hielt nicht — mit Begruendung.
    Refused { reasons: Vec<String> },
    /// Nichts sprach dagegen. Eine Freigabe ist das trotzdem nicht.
    NotIssued,
}

impl Release {
    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Refused { .. } => "refused",
            Self::NotIssued => "not issued",
        }
    }
}

/// Die Schritte als austauschbare Abhaengigkeit.
///
/// Der Wert dieses Werkzeugs liegt nicht im Messen — das koennen die
/// einzelnen Befehle laengst — sondern darin, **wie** es mit unvollstaendigen
/// Ergebnissen umgeht. Genau das laesst sich nur pruefen, wenn die Schritte
/// ersetzbar sind; die Tests fahren deshalb einen Lauf mit verworfenen
/// Reihen, mit Fremdlast und mit einem Urteil gegen uns, ganz ohne GPU.
pub(crate) trait Steps {
    /// Schritt 1: Modelle am Backend lesen und die Vertraege pruefen.
    ///
    /// # Errors
    ///
    /// Wenn das Backend nicht erreichbar ist oder die Konfiguration noch
    /// offene Platzhalter hat.
    async fn discover(&mut self, endpoint: &str, config: &Path) -> Result<Vec<String>, String>;

    /// Schritt 2: messen und die Konfiguration schreiben.
    ///
    /// # Errors
    ///
    /// Wenn die Messung nicht ausgefuehrt werden konnte.
    async fn measure(
        &mut self,
        config: &Path,
        out: &Path,
        quick: bool,
    ) -> Result<SeriesCount, String>;

    /// Schritt 3: lohnt es sich? `None`, wenn das Werkzeug nicht da ist.
    ///
    /// # Errors
    ///
    /// Wenn `vig-fit` gefunden wurde, aber nicht durchlief.
    async fn fit(&mut self, config: &Path, quick: bool) -> Result<Option<String>, String>;

    /// Schritt 4: die entstandene Konfiguration pruefen.
    ///
    /// # Errors
    ///
    /// Wenn die Konfiguration nicht gelesen werden konnte.
    async fn check(&mut self, config: &Path) -> Result<String, String>;

    /// Die Systemlast, fuer die Erkennung von Fremdlast.
    fn loadavg(&self) -> f64;

    /// Ob der Takt beobachtbar ist.
    fn clock(&self) -> Clock;

    /// Schreibt den Zwischenstand weg, nach jedem Schritt.
    ///
    /// Ein Lauf ueber eine halbe Stunde, der bei einem Abbruch nichts
    /// hinterlaesst, ist eine halbe Stunde umsonst.
    fn persist(&mut self, qualification: &Qualification);
}

/// Ist die Maschine ruhig genug, damit eine Messung etwas bedeutet?
pub(crate) fn quiet_enough(loadavg: f64) -> bool {
    loadavg < QUIET_LOADAVG
}

/// Die Schaetzung vor dem Start, in Sekunden.
pub(crate) fn estimate_seconds(steps: &[Step], quick: bool) -> u64 {
    steps
        .iter()
        .fold(0_u64, |sum, s| sum.saturating_add(s.rough_seconds(quick)))
}

/// Welche Schritte dieser Lauf ausfuehrt.
///
/// `only` waehlt einen einzelnen Schritt; `done` sind die Schritte, die ein
/// frueherer Lauf schon erledigt hat und die beim Fortsetzen entfallen.
pub(crate) fn plan(only: Option<Step>, done: &[Step]) -> Vec<Step> {
    Step::ALL
        .iter()
        .copied()
        .filter(|s| only.is_none_or(|wanted| wanted == *s))
        .filter(|s| !done.contains(s))
        .collect()
}

/// Der eine Satz, der vor allen Tabellen steht.
///
/// Er muss ohne die Tabellen darunter verstaendlich sein — und „ihr braucht
/// uns hier nicht" muss darin genauso klar stehen wie das Gegenteil.
pub(crate) fn headline(q: &Qualification) -> String {
    if let Some(verdict) = &q.fit_verdict {
        return verdict.trim().to_owned();
    }
    match q.release() {
        Release::Refused { reasons } => format!(
            "This run did not qualify this machine: {}.",
            reasons.join("; ")
        ),
        Release::NotIssued => "This machine carried the configured load, and nothing in the \
             measurement spoke against it."
            .to_owned(),
    }
}

/// Der Satz, der am Ende auf dem Terminal steht.
pub(crate) fn summary(q: &Qualification) -> String {
    let mut out = headline(q);
    out.push_str("\n\n");
    match q.release() {
        Release::Refused { reasons } => {
            let _ = write!(out, "No qualification: {}.", reasons.join("; "));
        }
        Release::NotIssued => {
            out.push_str(
                "This is not a release: a measurement run establishes nothing about its own \
                 hardware beyond what it measured.",
            );
        }
    }
    if let Clock::Unobservable { reason } = &q.clock {
        let _ = write!(
            out,
            "\n\nClock not observable ({reason}); the series could not be secured against a \
             clock that moves during a measurement."
        );
    }
    out
}

/// Der Bericht als Markdown.
pub(crate) fn markdown(q: &Qualification, endpoint: &str) -> String {
    let mut out = String::new();
    out.push_str("# Qualification report\n\n");
    let _ = writeln!(out, "**{}**\n", headline(q));
    let _ = writeln!(
        out,
        "Written by `vig autotune` against `{endpoint}`. This report states what was measured \
         **on this machine**, under which conditions, and what was discarded. It is not a \
         release; see the bottom of this file.\n"
    );

    out.push_str("## What ran\n\n");
    out.push_str("| Step | What it does | Outcome | Seconds |\n|---|---|---|---:|\n");
    for step in &q.steps {
        let reason = step
            .outcome
            .reason()
            .map_or_else(String::new, |r| format!(" — {r}"));
        let _ = writeln!(
            out,
            "| `{}` | {} | {}{} | {} |",
            step.step.key(),
            step.step.title(),
            step.outcome.label(),
            reason,
            step.seconds
        );
    }

    if q.series.total() > 0 {
        let _ = write!(
            out,
            "\n## Measurement series\n\n{} of {} series were usable.\n",
            q.series.qualified,
            q.series.total()
        );
        if q.series.discarded > 0 {
            let _ = write!(
                out,
                "\n**{} series were discarded, and the values they would have produced are \
                 simply not set.** The usual cause on a laptop or on a power-capped card is a \
                 clock that moves during the series: the measurement then describes no operating \
                 point at all. The way out is a pinned clock (`nvidia-smi -lgc`, needs \
                 permissions) or a machine that holds its clock, and a quiet machine. The \
                 threshold is not relaxed for this.\n",
                q.series.discarded
            );
        }
    }

    if let Clock::Unobservable { reason } = &q.clock {
        let _ = write!(
            out,
            "\n## Clock not observable\n\n{reason}. The series ran, but they could **not** be \
             secured against a clock that moves during a measurement — on this platform nothing \
             reports one. Nothing was substituted for it.\n"
        );
    }

    if let Some(verdict) = &q.fit_verdict {
        let _ = write!(
            out,
            "\n## Is the governor worth it here?\n\n{}\n",
            verdict.trim()
        );
    }

    if let Some(doctor) = &q.doctor {
        let _ = write!(out, "\n## Configuration check\n\n`vig doctor`: {doctor}\n");
    }

    if !q.manifest.is_empty() {
        out.push_str("\n## What this measurement refers to\n\n| | |\n|---|---|\n");
        for (key, value) in &q.manifest {
            let _ = writeln!(out, "| {key} | {value} |");
        }
    }

    out.push_str("\n## What this report does not say\n\n");
    let _ = writeln!(
        out,
        "- **It is not a release.** Release: {}.",
        q.release().label()
    );
    if let Release::Refused { reasons } = q.release() {
        for reason in reasons {
            let _ = writeln!(out, "  - {reason}");
        }
    }
    out.push_str(
        "- **It says nothing about other hardware.** A measurement holds for the machine it ran \
         on.\n\
         - **It says nothing about detection quality.** Supply is measured, not accuracy.\n\
         - **It says nothing about hours.** For that there is the soak run.\n",
    );
    out
}

/// Der Bericht als JSON, fuer die Weiterverarbeitung.
pub(crate) fn json(q: &Qualification, endpoint: &str) -> String {
    let steps: Vec<serde_json::Value> = q
        .steps
        .iter()
        .map(|s| {
            serde_json::json!({
                "step": s.step.key(),
                "outcome": s.outcome.label(),
                "reason": s.outcome.reason(),
                "seconds": s.seconds,
                "notes": s.notes,
            })
        })
        .collect();
    let release = q.release();
    let clock = match &q.clock {
        Clock::Observable => serde_json::json!({ "observable": true }),
        Clock::Unobservable { reason } => {
            serde_json::json!({ "observable": false, "reason": reason })
        }
    };
    serde_json::json!({
        "tool": "vig autotune",
        "endpoint": endpoint,
        "headline": headline(q),
        "steps": steps,
        "series": {
            "qualified": q.series.qualified,
            "discarded": q.series.discarded,
        },
        "clock": clock,
        "fit_verdict": q.fit_verdict,
        "doctor": q.doctor,
        "config": q.config.as_ref().map(|p| p.display().to_string()),
        "manifest": q
            .manifest
            .iter()
            .cloned()
            .collect::<std::collections::BTreeMap<_, _>>(),
        "contaminated": q.contaminated(),
        "complete": q.complete(),
        "release": release.label(),
        "release_reasons": match &release {
            Release::Refused { reasons } => reasons.clone(),
            Release::NotIssued => Vec::new(),
        },
        "summary": summary(q),
    })
    .to_string()
}

/// Fuehrt einen Lauf aus und sammelt das Ergebnis ein.
///
/// Die Reihenfolge ist fest, und ein gescheiterter Schritt beendet den Lauf:
/// Messen ohne Konfiguration, Pruefen ohne Messung — beides waere ein Bericht
/// ueber nichts.
pub(crate) async fn execute<S: Steps + ?Sized>(
    steps: &mut S,
    plan: &[Step],
    endpoint: &str,
    config: &Path,
    out_config: &Path,
    quick: bool,
) -> Qualification {
    let mut q = Qualification {
        clock: steps.clock(),
        ..Qualification::default()
    };
    for step in plan {
        println!("\n[{}] {}", step.key(), step.title());
        let started = Instant::now();
        let before = steps.loadavg();
        let mut notes = Vec::new();

        let outcome = match *step {
            Step::Discover => match steps.discover(endpoint, config).await {
                Ok(models) => {
                    notes.push(format!("{} models found at the backend", models.len()));
                    Outcome::Done
                }
                Err(reason) => Outcome::Failed { reason },
            },
            Step::Measure => match steps.measure(config, out_config, quick).await {
                Ok(series) => {
                    q.series = series;
                    if series.discarded > 0 {
                        notes.push(format!(
                            "{} of {} series discarded; the values they would have produced are \
                             not set",
                            series.discarded,
                            series.total()
                        ));
                    }
                    q.config = Some(out_config.to_path_buf());
                    Outcome::Done
                }
                Err(reason) => Outcome::Failed { reason },
            },
            Step::Fit => match steps.fit(out_config, quick).await {
                Ok(Some(verdict)) => {
                    q.fit_verdict = Some(verdict);
                    Outcome::Done
                }
                Ok(None) => Outcome::Skipped {
                    reason: "vig-fit not found; run it yourself to answer whether the governor \
                             is worth it here"
                        .to_owned(),
                },
                Err(reason) => Outcome::Failed { reason },
            },
            Step::Check => match steps.check(out_config).await {
                Ok(verdict) => {
                    q.doctor = Some(verdict);
                    Outcome::Done
                }
                Err(reason) => Outcome::Failed { reason },
            },
        };

        // Fremdlast entwertet das Ergebnis eines Messschritts, nicht das eines
        // Lesevorgangs: `discover` und `check` rechnen nicht.
        let after = steps.loadavg();
        let measuring = matches!(*step, Step::Measure | Step::Fit);
        let outcome = match outcome {
            Outcome::Done if measuring && !(quiet_enough(before) && quiet_enough(after)) => {
                Outcome::Contaminated {
                    reason: format!(
                        "system load {before:.2} before and {after:.2} after; something else was \
                         running"
                    ),
                }
            }
            other => other,
        };

        println!(
            "    {} ({} s)",
            outcome.label(),
            started.elapsed().as_secs()
        );
        if let Some(reason) = outcome.reason() {
            println!("    {reason}");
        }

        let failed = matches!(outcome, Outcome::Failed { .. });
        q.steps.push(StepResult {
            step: *step,
            outcome,
            seconds: started.elapsed().as_secs(),
            notes,
        });
        // Nach jedem Schritt, nicht am Ende: ein Abbruch soll das bisher
        // Gemessene nicht mitnehmen.
        steps.persist(&q);
        if failed {
            break;
        }
    }
    q
}

/// Liest die Systemlast der letzten Minute.
///
/// `/proc/loadavg` gibt es auf jedem Linux, auch auf Android. Fehlt sie,
/// gilt die Maschine als ruhig — eine fehlende Beobachtung ist kein Beleg
/// fuer Fremdlast.
pub(crate) fn system_loadavg() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0)
}

/// Was der Befehl ausfuehren soll.
#[derive(Debug, Clone)]
pub(crate) struct Options {
    pub(crate) endpoint: String,
    pub(crate) config: PathBuf,
    pub(crate) out_dir: PathBuf,
    pub(crate) samples: usize,
    pub(crate) period_us: Option<u64>,
    pub(crate) quick: bool,
    pub(crate) only: Option<Step>,
    pub(crate) offline: bool,
    pub(crate) restart: bool,
}

/// Die echten Schritte: die vorhandenen Befehle, im selben Prozess.
struct Live {
    endpoint: String,
    out_dir: PathBuf,
    samples: usize,
    period_us: Option<u64>,
    offline: bool,
    identity: IdentityArgs,
}

impl Live {
    /// Wo `vig-fit` liegt.
    ///
    /// `vig-fit` gehoert zum Messkasten (`vig-bench`) und nicht zum Produkt;
    /// `vig-cli` darf nicht davon abhaengen (ADR-0044). Also wird es gesucht,
    /// nicht eingebunden — und wenn es fehlt, ist die Frage offen und nicht
    /// beantwortet.
    fn fit_binary() -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var("VIG_FIT_BIN") {
            let path = PathBuf::from(explicit);
            return path.is_file().then_some(path);
        }
        let beside = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("vig-fit")));
        match beside {
            Some(path) if path.is_file() => Some(path),
            _ => None,
        }
    }
}

impl Steps for Live {
    async fn discover(&mut self, endpoint: &str, config: &Path) -> Result<Vec<String>, String> {
        if !config.exists() {
            println!("    {} does not exist; writing a draft", config.display());
            Box::pin(crate::init::run(endpoint, Some(config), false))
                .await
                .map_err(|e| e.to_string())?;
        }
        let text = std::fs::read_to_string(config).map_err(|e| e.to_string())?;
        let open = crate::init::open_placeholders(&text);
        if !open.is_empty() {
            let list: Vec<String> = open
                .iter()
                .map(|(name, count)| format!("{name} ({count}x)"))
                .collect();
            return Err(format!(
                "{} still has open placeholders: {}. These are contracts, not measurements — how \
                 often your source delivers and how long a result stays useful is a promise you \
                 make to your application, and no measurement can find it out. Fill them in, \
                 then run autotune again.",
                config.display(),
                list.join(", ")
            ));
        }
        let parsed = vig_config::Config::from_yaml(&text).map_err(|e| e.to_string())?;
        Ok(parsed.models.keys().cloned().collect())
    }

    async fn measure(
        &mut self,
        config: &Path,
        out: &Path,
        quick: bool,
    ) -> Result<SeriesCount, String> {
        let samples = if quick {
            self.samples.min(50)
        } else {
            self.samples
        };
        crate::calibrate::reset_qualification();
        Box::pin(crate::calibrate::run(
            config,
            samples,
            self.period_us,
            Some(out),
            &self.identity,
        ))
        .await
        .map_err(|e| e.to_string())?;
        let (qualified, discarded) = crate::calibrate::qualification();
        if qualified == 0 && discarded == 0 {
            return Err(
                "no measurement series ran at all; the backend answered nothing measurable"
                    .to_owned(),
            );
        }
        Ok(SeriesCount {
            qualified,
            discarded,
        })
    }

    async fn fit(&mut self, config: &Path, quick: bool) -> Result<Option<String>, String> {
        let Some(binary) = Self::fit_binary() else {
            return Ok(None);
        };
        let json_path = self.out_dir.join("fit.json");
        let seconds = if quick { "5" } else { "10" };
        // `tokio::process` und nicht `std::process`: `vig-fit` laeuft ein bis
        // zwei Minuten. Ein blockierendes `status()` haelt solange einen
        // Worker der Laufzeit fest — auf einem Telefon mit wenigen Kernen ist
        // das der Unterschied zwischen „misst" und „haengt".
        let status = tokio::process::Command::new(&binary)
            .arg(config)
            .env("VIG_FIT_JSON", &json_path)
            .env("VIG_FIT_SECONDS", seconds)
            .status()
            .await
            .map_err(|e| format!("{} could not be started: {e}", binary.display()))?;
        let text = std::fs::read_to_string(&json_path).map_err(|e| {
            format!(
                "{} exited with {status} and wrote no result: {e}",
                binary.display()
            )
        })?;
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("fit result unreadable: {e}"))?;
        Ok(parsed
            .get("verdict")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned))
    }

    async fn check(&mut self, config: &Path) -> Result<String, String> {
        Box::pin(crate::doctor::verdict_of(config, self.offline))
            .await
            .map(|v| v.label().to_owned())
            .map_err(|e| e.to_string())
    }

    fn loadavg(&self) -> f64 {
        system_loadavg()
    }

    fn clock(&self) -> Clock {
        use vig_platform::Collector as _;
        let mut collector = vig_platform::NvidiaSmi::default();
        match collector.snapshot() {
            Ok(_) => Clock::Observable,
            Err(reason) => Clock::Unobservable {
                reason: reason.clone(),
            },
        }
    }

    fn persist(&mut self, qualification: &Qualification) {
        let mut q = qualification.clone();
        q.manifest = manifest(&self.endpoint, q.config.as_deref());
        let json_path = self.out_dir.join("qualification.json");
        let md_path = self.out_dir.join("qualification.md");
        let state_path = self.out_dir.join("state.json");
        if let Err(e) = std::fs::write(&json_path, json(&q, &self.endpoint)) {
            eprintln!("    Bericht nicht schreibbar: {e}");
        }
        if let Err(e) = std::fs::write(&md_path, markdown(&q, &self.endpoint)) {
            eprintln!("    Bericht nicht schreibbar: {e}");
        }
        let state = serde_json::json!({ "done": q.done_keys() });
        if let Err(e) = std::fs::write(&state_path, state.to_string()) {
            eprintln!("    Zustand nicht schreibbar: {e}");
        }
    }
}

/// Worauf sich diese Messung bezieht (ADR-0019).
fn manifest(endpoint: &str, config: Option<&Path>) -> Vec<(String, String)> {
    use vig_platform::Collector as _;

    let mut entries = vec![
        (
            "governor".to_owned(),
            format!("vig {}", env!("CARGO_PKG_VERSION")),
        ),
        ("endpoint".to_owned(), endpoint.to_owned()),
        (
            "unix_seconds".to_owned(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or_else(|_| "unknown".to_owned(), |d| d.as_secs().to_string()),
        ),
        (
            "platform".to_owned(),
            format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        ),
    ];

    let mut collector = vig_platform::NvidiaSmi::default();
    match collector.snapshot() {
        Ok(snapshot) => match snapshot.gpu(0) {
            Some(gpu) => {
                // Der Wert, nicht die Rust-Darstellung des Wertes: in den
                // Bericht schaut ein Interessent, und `Observed(Sample { .. })`
                // ist fuer ihn kein Geraetename.
                let plain = |o: &vig_platform::Observation<String>| {
                    o.value()
                        .cloned()
                        .unwrap_or_else(|| "not observable".to_owned())
                };
                entries.push(("device".to_owned(), plain(&gpu.name)));
                entries.push(("driver".to_owned(), plain(&gpu.driver)));
                // Quelle und Zeitpunkt gehen dabei nicht verloren — sie
                // bekommen eigene Felder. Wo nichts beobachtbar ist, steht
                // im Geraetefeld `not observable`, und diese beiden fehlen;
                // das ist dieselbe Aussage, nur ohne Rauschen.
                if let vig_platform::Observation::Observed(sample) = &gpu.name {
                    entries.push(("observed_by".to_owned(), format!("{:?}", sample.source)));
                    entries.push((
                        "observed_at_ms".to_owned(),
                        sample.observed_at_ms.to_string(),
                    ));
                }
            }
            None => entries.push(("device".to_owned(), "no GPU reported".to_owned())),
        },
        Err(reason) => {
            // Kein Rateschluss: „nicht beobachtbar" ist die ehrliche Angabe.
            entries.push(("device".to_owned(), format!("not observable: {reason}")));
        }
    }

    if let Some(path) = config
        && let Ok(bytes) = std::fs::read(path)
    {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(&bytes);
        entries.push((
            "frozen_config_sha256".to_owned(),
            format!("{digest:x}").chars().take(16).collect(),
        ));
    }
    entries
}

/// Liest, welche Schritte ein frueherer Lauf schon erledigt hat.
fn completed_steps(state_path: &Path) -> Vec<Step> {
    let Ok(text) = std::fs::read_to_string(state_path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    parsed
        .get("done")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter_map(Step::from_key)
                .collect()
        })
        .unwrap_or_default()
}

/// Sagt vorher, wie lange es dauert — und was zu tun ist, wenn das zu lang ist.
fn announce(steps: &[Step], quick: bool) {
    let seconds = estimate_seconds(steps, quick);
    let minutes = seconds.saturating_add(59).saturating_div(60);
    println!("{PROMISE}\n");
    println!("Planned steps ({minutes} minutes, estimated generously):");
    for step in steps {
        let each = step
            .rough_seconds(quick)
            .saturating_add(59)
            .saturating_div(60);
        println!("  {:<9} {} (~{each} min)", step.key(), step.title());
    }
    if seconds > PROMISED_SECONDS {
        println!(
            "\nThat is longer than the half hour promised above. `--quick` measures a smaller \
             matrix and answers the same question less precisely; `--only measure` or \
             `--only check` run a single step."
        );
    }
    println!(
        "\nResults are written to disk after every step. An interrupted run resumes where it \
         stopped."
    );
}

/// Fuehrt `vig autotune` aus.
///
/// # Errors
///
/// Wenn das Ausgabeverzeichnis nicht angelegt werden kann.
pub(crate) async fn run(
    options: &Options,
    identity: &IdentityArgs,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&options.out_dir)?;
    let state_path = options.out_dir.join("state.json");
    let done = if options.restart {
        Vec::new()
    } else {
        completed_steps(&state_path)
    };
    if !done.is_empty() {
        let names: Vec<&str> = done.iter().map(|s| s.key()).collect();
        println!("Resuming; already done: {}", names.join(", "));
    }

    let steps = plan(options.only, &done);
    if steps.is_empty() {
        println!("Nothing left to do. Use --restart to measure again.");
        return Ok(ExitCode::SUCCESS);
    }
    announce(&steps, options.quick);

    let out_config = options.out_dir.join("measured.yaml");
    let mut live = Live {
        endpoint: options.endpoint.clone(),
        out_dir: options.out_dir.clone(),
        samples: options.samples,
        period_us: options.period_us,
        offline: options.offline,
        identity: identity.clone(),
    };

    let qualification = Box::pin(execute(
        &mut live,
        &steps,
        &options.endpoint,
        &options.config,
        &out_config,
        options.quick,
    ))
    .await;

    println!("\n{}\n", summary(&qualification));
    println!(
        "Report: {} and {}",
        options.out_dir.join("qualification.md").display(),
        options.out_dir.join("qualification.json").display()
    );

    // Ein Urteil gegen uns ist kein Programmfehler. Nur ein Schritt, der nicht
    // durchlief, und eine Messung, die nichts behalten hat, sind einer.
    let nothing_measured = qualification.series.total() > 0 && qualification.series.qualified == 0;
    Ok(if qualification.failed() || nothing_measured {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

#[cfg(test)]
// Der Fake wartet auf nichts — er *ist* das Ersatzstueck fuer die Schritte,
// die im Ernstfall warten. Das `async` steht hier, weil der Trait es verlangt,
// nicht weil etwas zu tun waere.
#[allow(clippy::unwrap_used, clippy::panic, clippy::unused_async_trait_impl)]
mod tests {
    use super::{
        Clock, Outcome, Qualification, Release, SeriesCount, Step, StepResult, Steps,
        estimate_seconds, execute, headline, json, markdown, plan, quiet_enough, summary,
    };
    use std::path::{Path, PathBuf};

    /// Ein Lauf mit austauschbarem Ausgang, damit sich die Ehrlichkeitsregeln
    /// ohne GPU pruefen lassen.
    #[derive(Default)]
    struct Fake {
        series: SeriesCount,
        fit: Option<String>,
        doctor: String,
        load: f64,
        measure_fails: Option<String>,
        fit_missing: bool,
        clock_blind: bool,
        persisted: usize,
    }

    impl Steps for Fake {
        async fn discover(
            &mut self,
            _endpoint: &str,
            _config: &Path,
        ) -> Result<Vec<String>, String> {
            Ok(vec!["detector".to_owned()])
        }
        async fn measure(
            &mut self,
            _config: &Path,
            _out: &Path,
            _quick: bool,
        ) -> Result<SeriesCount, String> {
            self.measure_fails.clone().map_or(Ok(self.series), Err)
        }
        async fn fit(&mut self, _config: &Path, _quick: bool) -> Result<Option<String>, String> {
            if self.fit_missing {
                return Ok(None);
            }
            Ok(Some(self.fit.clone().unwrap_or_else(|| {
                "Der Governor lohnt sich hier ab der Saettigung.".to_owned()
            })))
        }
        async fn check(&mut self, _config: &Path) -> Result<String, String> {
            Ok(if self.doctor.is_empty() {
                "READY".to_owned()
            } else {
                self.doctor.clone()
            })
        }
        fn loadavg(&self) -> f64 {
            self.load
        }
        fn clock(&self) -> Clock {
            if self.clock_blind {
                Clock::Unobservable {
                    reason: "nvidia-smi nicht gefunden".to_owned(),
                }
            } else {
                Clock::Observable
            }
        }
        fn persist(&mut self, _qualification: &Qualification) {
            self.persisted = self.persisted.saturating_add(1);
        }
    }

    async fn run(fake: &mut Fake) -> Qualification {
        execute(
            fake,
            &Step::ALL,
            "127.0.0.1:8001",
            Path::new("vig.yaml"),
            Path::new("measured.yaml"),
            false,
        )
        .await
    }

    fn clean() -> Fake {
        Fake {
            series: SeriesCount {
                qualified: 4,
                discarded: 0,
            },
            load: 0.3,
            ..Fake::default()
        }
    }

    /// Der gute Fall: alles laeuft, nichts wird verworfen — und trotzdem
    /// spricht das Werkzeug keine Freigabe aus.
    #[tokio::test]
    async fn a_clean_run_still_does_not_issue_a_release() {
        let q = run(&mut clean()).await;
        assert!(q.complete(), "alle Schritte gelaufen: {:?}", q.steps);
        assert_eq!(q.release(), Release::NotIssued);
        assert!(
            summary(&q).contains("not a release"),
            "der Satz muss den Unterschied nennen: {}",
            summary(&q)
        );
    }

    /// Eine verworfene Messreihe macht aus dem Lauf keine Qualifikation, und
    /// der Bericht sagt, was fehlt und wie man es behebt.
    #[tokio::test]
    async fn a_discarded_series_refuses_the_release_and_says_why() {
        let mut fake = Fake {
            series: SeriesCount {
                qualified: 0,
                discarded: 4,
            },
            load: 0.3,
            ..Fake::default()
        };
        let q = run(&mut fake).await;
        let Release::Refused { reasons } = q.release() else {
            panic!("mit vier verworfenen Reihen darf nichts offenbleiben");
        };
        assert!(
            reasons.iter().any(|r| r.contains("discarded")),
            "der Grund muss die Reihen nennen: {reasons:?}"
        );
        let report = markdown(&q, "127.0.0.1:8001");
        assert!(
            report.contains("not set"),
            "der Bericht muss sagen, dass die Werte fehlen"
        );
        assert!(report.contains("nvidia-smi -lgc"), "und wie man es behebt");
    }

    /// Fremdlast entwertet die Messschritte — und nur die.
    #[tokio::test]
    async fn foreign_load_marks_the_measuring_steps_contaminated() {
        let mut fake = Fake {
            load: 3.0,
            ..clean()
        };
        let q = run(&mut fake).await;
        assert!(q.contaminated(), "Fremdlast muss auffallen");
        for step in &q.steps {
            match step.step {
                Step::Measure | Step::Fit => assert!(
                    matches!(step.outcome, Outcome::Contaminated { .. }),
                    "{:?} misst und muss als verschmutzt gelten",
                    step.step
                ),
                Step::Discover | Step::Check => assert!(
                    matches!(step.outcome, Outcome::Done),
                    "{:?} rechnet nicht und bleibt gueltig",
                    step.step
                ),
            }
        }
        assert!(matches!(q.release(), Release::Refused { .. }));
        assert!(json(&q, "x").contains("\"contaminated\":true"));
    }

    /// Ein Urteil gegen uns steht woertlich da — und zwar ganz oben.
    #[tokio::test]
    async fn a_result_against_us_is_the_headline() {
        let mut fake = Fake {
            fit: Some("Unterhalb der Saettigung bringt der Governor hier nichts.".to_owned()),
            ..clean()
        };
        let q = run(&mut fake).await;
        assert_eq!(
            headline(&q),
            "Unterhalb der Saettigung bringt der Governor hier nichts.",
            "das Urteil gegen uns ist die Schlagzeile, nicht das Kleingedruckte"
        );
        assert!(summary(&q).starts_with("Unterhalb der Saettigung"));
        assert!(markdown(&q, "x").contains("bringt der Governor hier nichts"));
        assert!(json(&q, "x").contains("bringt der Governor hier nichts"));
    }

    /// Ohne `nvidia-smi` laeuft die Messung, aber der Bericht sagt, dass sie
    /// schwaecher abgesichert ist. Das ist der Fall auf dem Telefon.
    #[tokio::test]
    async fn without_a_readable_clock_the_report_says_so_instead_of_failing() {
        let mut fake = Fake {
            clock_blind: true,
            ..clean()
        };
        let q = run(&mut fake).await;
        assert!(!q.failed(), "eine fehlende Beobachtung ist kein Fehlschlag");
        assert!(q.complete(), "und kein Grund, Schritte auszulassen");
        assert!(
            matches!(q.clock, Clock::Unobservable { .. }),
            "der Zustand gehoert in den Bericht"
        );
        let report = markdown(&q, "x");
        assert!(report.contains("Clock not observable"));
        assert!(
            report.contains("Nothing was substituted for it."),
            "es darf nicht still etwas anderes gemessen werden"
        );
        assert!(json(&q, "x").contains("\"observable\":false"));
    }

    /// Ein fehlendes `vig-fit` ist kein Fehler, aber auch keine Antwort.
    #[tokio::test]
    async fn a_missing_fit_tool_leaves_the_question_open() {
        let mut fake = Fake {
            fit_missing: true,
            ..clean()
        };
        let q = run(&mut fake).await;
        assert!(!q.failed(), "fehlendes Werkzeug ist kein Fehlschlag");
        assert!(!q.complete(), "aber der Lauf ist unvollstaendig");
        assert!(matches!(q.release(), Release::Refused { .. }));
    }

    /// Ein gescheiterter Schritt beendet den Lauf; die folgenden laufen nicht.
    #[tokio::test]
    async fn a_failed_step_ends_the_run() {
        let mut fake = Fake {
            load: 0.3,
            measure_fails: Some("backend unreachable".to_owned()),
            ..Fake::default()
        };
        let q = run(&mut fake).await;
        assert!(q.failed());
        assert_eq!(
            q.steps.len(),
            2,
            "nach dem Fehlschlag darf nichts mehr laufen: {:?}",
            q.steps
        );
        assert!(q.doctor.is_none(), "ohne Messung wird nichts geprueft");
    }

    /// `NOT_READY` von `vig doctor` verweigert die Freigabe.
    #[tokio::test]
    async fn a_not_ready_configuration_refuses_the_release() {
        let mut fake = Fake {
            doctor: "NOT_READY".to_owned(),
            ..clean()
        };
        let q = run(&mut fake).await;
        let Release::Refused { reasons } = q.release() else {
            panic!("NOT_READY darf nichts offenlassen");
        };
        assert!(reasons.iter().any(|r| r.contains("NOT_READY")));
    }

    /// Nach jedem Schritt liegt ein Zwischenstand vor.
    #[tokio::test]
    async fn every_step_is_written_out_before_the_next_one_starts() {
        let mut fake = clean();
        let q = run(&mut fake).await;
        assert_eq!(
            fake.persisted,
            q.steps.len(),
            "ein Abbruch darf das Gemessene nicht mitnehmen"
        );
    }

    #[test]
    fn resuming_skips_what_is_done() {
        assert_eq!(
            plan(None, &[Step::Discover, Step::Measure]),
            vec![Step::Fit, Step::Check]
        );
        assert_eq!(plan(Some(Step::Check), &[]), vec![Step::Check]);
        assert_eq!(plan(None, &Step::ALL), Vec::new());
    }

    /// Die Zusage aus der Ueberschrift muss der Schaetzung standhalten.
    #[test]
    fn the_full_run_fits_into_the_promised_half_hour() {
        assert!(
            estimate_seconds(&Step::ALL, false) <= super::PROMISED_SECONDS,
            "die Schaetzung ueberschreitet die zugesagte halbe Stunde: {} s",
            estimate_seconds(&Step::ALL, false)
        );
        assert!(estimate_seconds(&Step::ALL, true) < estimate_seconds(&Step::ALL, false));
    }

    #[test]
    fn the_quiet_threshold_is_the_one_the_scripts_use() {
        assert!(quiet_enough(1.4));
        assert!(!quiet_enough(1.5));
    }

    /// Das Manifest gehoert in beide Fassungen des Berichts.
    #[test]
    fn the_manifest_travels_into_both_reports() {
        let mut q = Qualification {
            manifest: vec![("device".to_owned(), "a card".to_owned())],
            config: Some(PathBuf::from("measured.yaml")),
            ..Qualification::default()
        };
        q.steps.push(StepResult {
            step: Step::Discover,
            outcome: Outcome::Done,
            seconds: 1,
            notes: Vec::new(),
        });
        assert!(markdown(&q, "x").contains("a card"));
        assert!(json(&q, "x").contains("a card"));
    }
}
