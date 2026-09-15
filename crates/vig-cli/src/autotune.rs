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

/// Ab so viel fremder Rechenzeit gilt die Maschine als nicht ruhig, in
/// Hundertstel Kernen.
///
/// Vorher stand hier eine feste Grenze fuer `/proc/loadavg`. Die ist aus zwei
/// Gruenden falsch: Die Last zaehlt auch Threads, die im Kernel warten und
/// nichts rechnen — ein Pixel 2 steht im Leerlauf bei 3,5 und verbraucht dabei
/// 0,04 Kerne (gemessen am 15.09.2026) —, und die Minute danach enthaelt die
/// eigene Messung. Jeder Lauf auf einem Telefon galt deshalb als verschmutzt.
///
/// Gemessen wird jetzt die fremde CPU-Zeit aus `/proc/stat`, abzueglich der
/// dieses Prozesses, in einem kurzen Fenster **vor** und **nach** dem Schritt,
/// wenn das Backend ruht. Ein ganzer fremder Kern ist die Grenze: Der Laptop
/// lag im Leerlauf mit Terminal und Editor bei 0,68, eine daneben laufende
/// Auswertung — der Fall, der am 15.09. drei Laeufe verdorben hat — belegt
/// einen ganzen.
const FOREIGN_CORES_CENTI: u64 = 100;

/// Wie lange je Beobachtung gemessen wird.
const FOREIGN_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

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
    const fn rough_seconds(self, quick: bool, shape: RunShape) -> u64 {
        match self {
            Self::Discover => 5,
            Self::Measure => shape.measure_seconds(quick),
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

/// Wie gross der Messlauf ist, den dieser Aufruf vorhat.
///
/// Vorher hing die Schaetzung an nichts als dem Schritt selbst. Ein Anwender
/// mit zwoelf Modellen und `--samples 500` bekam dieselbe Ansage wie einer mit
/// vieren — und die eingebaute Warnung, dass die zugesagte halbe Stunde nicht
/// reicht, konnte gar nicht ausloesen, weil die Summe fester Konstanten sie
/// nie erreichte. Eine Zusage, die sich selbst nicht pruefen kann, ist keine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunShape {
    /// Modelle in der Konfiguration.
    pub(crate) models: u64,
    /// Slots des Backends — ab zwei werden Paare gemessen.
    pub(crate) slots: u64,
    /// Proben je Messreihe.
    pub(crate) samples: u64,
}

impl RunShape {
    /// Die Groesse, mit der dieses Werkzeug validiert wurde
    /// (`docs/benchmark/validierung-autotune.md`): vier Modelle, zwei Slots,
    /// 200 Proben. Sie gilt, solange die Konfiguration nicht lesbar ist.
    pub(crate) const REFERENCE: Self = Self {
        models: 4,
        slots: 2,
        samples: 200,
    };

    /// Messzellen: jedes Modell solo, dazu jedes geordnete Paar.
    ///
    /// Die Paarmessung ist quadratisch (`measure_pairs`, beide Richtungen
    /// getrennt) und laeuft nur ab zwei Slots. Genau daran waechst ein Lauf,
    /// und genau das hat die alte Schaetzung nicht gewusst.
    const fn cells(self) -> u64 {
        let pairs = if self.slots > 1 {
            self.models.saturating_mul(self.models.saturating_sub(1))
        } else {
            0
        };
        self.models.saturating_add(pairs)
    }

    /// Grobe Dauer des Messschritts in Sekunden.
    ///
    /// Der Faktor stammt aus dem validierten Laptoplauf: 16 Zellen a 200
    /// Proben in 28 s, also rund 9 ms je Probe. Hier stehen 20 ms — mehr als
    /// das Doppelte, weil eine Schaetzung eher zu hoch sein soll.
    ///
    /// **Was er nicht kann:** die Laufzeit der Modelle vorhersagen, die er
    /// noch nicht gemessen hat. Auf dem Pixel 2 dauert ein Detektoraufruf
    /// 158 ms statt 15; dort ist diese Zahl deutlich zu niedrig. Die Ansage
    /// sagt das, statt eine Genauigkeit zu behaupten, die vor der ersten
    /// Messung niemand haben kann.
    const fn measure_seconds(self, quick: bool) -> u64 {
        let samples = if quick && self.samples > 50 {
            50
        } else {
            self.samples
        };
        self.cells()
            .saturating_mul(samples)
            .saturating_mul(MILLIS_PER_SAMPLE)
            .saturating_div(1000)
            // Aufbau, Warmlauf und Vorlauf je Zelle, unabhaengig von der Groesse.
            .saturating_add(60)
    }
}

/// Angesetzte Dauer einer einzelnen Probe auf der Referenzmaschine.
const MILLIS_PER_SAMPLE: u64 = 20;

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
    /// Modelle, die sich gar nicht vermessen liessen. Ihr Profil bleibt
    /// ungemessen aus der Eingabe stehen.
    pub(crate) skipped_models: u64,
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
        if self.series.skipped_models > 0 {
            reasons.push(format!(
                "{} models could not be measured at all",
                self.series.skipped_models
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

    /// Fremde Rechenzeit in Hundertstel Kernen, ueber ein kurzes Fenster.
    ///
    /// `None`, wenn sie auf dieser Plattform nicht beobachtbar ist. Das ist
    /// **kein** Beleg fuer eine ruhige Maschine.
    async fn foreign_load(&mut self) -> Option<u64>;

    /// Ob der Takt beobachtbar ist.
    fn clock(&self) -> Clock;

    /// Schreibt den Zwischenstand weg, nach jedem Schritt.
    ///
    /// Ein Lauf ueber eine halbe Stunde, der bei einem Abbruch nichts
    /// hinterlaesst, ist eine halbe Stunde umsonst.
    fn persist(&mut self, qualification: &Qualification);
}

/// Ist die Maschine ruhig genug, damit eine Messung etwas bedeutet?
///
/// Eine nicht beobachtbare Fremdlast ist nicht ruhig: Fehlt die Beobachtung,
/// fehlt der Beleg, und ADR-0044 laesst nichts an seine Stelle treten.
pub(crate) fn quiet_enough(foreign_cores_centi: Option<u64>) -> bool {
    foreign_cores_centi.is_some_and(|centi| centi < FOREIGN_CORES_CENTI)
}

/// Fremde CPU-Zeit ueber `window`, in Hundertstel Kernen.
///
/// Die Rechnung liegt in [`vig_platform::cpu`], damit `vig-fit` dieselbe
/// Groesse misst und keine zweite Kopie davon pflegt.
pub(crate) async fn sample_foreign_load(window: std::time::Duration) -> Option<u64> {
    let before = vig_platform::cpu::CpuSample::now()?;
    tokio::time::sleep(window).await;
    before.foreign_cores_centi(vig_platform::cpu::CpuSample::now()?)
}

/// Die Schaetzung vor dem Start, in Sekunden.
pub(crate) fn estimate_seconds(steps: &[Step], quick: bool, shape: RunShape) -> u64 {
    steps.iter().fold(0_u64, |sum, s| {
        sum.saturating_add(s.rough_seconds(quick, shape))
    })
}

/// Welche Schritte dieser Lauf ausfuehrt.
///
/// `only` waehlt einen einzelnen Schritt; `done` sind die Schritte, die ein
/// frueherer Lauf schon erledigt hat und die beim Fortsetzen entfallen.
///
/// Ab dem ersten nicht erledigten Schritt laeuft alles Folgende mit: Wird neu
/// gemessen, beschreiben ein frueheres `fit` und `check` eine
/// `measured.yaml`, die es so nicht mehr gibt.
pub(crate) fn plan(only: Option<Step>, done: &[Step]) -> Vec<Step> {
    let first_open = Step::ALL
        .iter()
        .position(|s| !done.contains(s))
        .unwrap_or(Step::ALL.len());
    Step::ALL
        .get(first_open..)
        .unwrap_or_default()
        .iter()
        .copied()
        .filter(|s| only.is_none_or(|wanted| wanted == *s))
        .collect()
}

/// Der eine Satz, der vor allen Tabellen steht.
///
/// Er muss ohne die Tabellen darunter verstaendlich sein — und „ihr braucht
/// uns hier nicht" muss darin genauso klar stehen wie das Gegenteil.
///
/// Ein verweigerter Lauf beginnt mit der Verweigerung. Vorher stand dort das
/// Urteil von `vig-fit`, sobald es eines gab: Der Laptoplauf vom 15.09.
/// begann mit „der Governor 0 ‰", und erst ganz unten stand „Release:
/// refused". Das Urteil bleibt im eigenen Abschnitt; ist es gegen uns, steht
/// es zusaetzlich direkt hinter der Verweigerung.
pub(crate) fn headline(q: &Qualification) -> String {
    match q.release() {
        Release::Refused { reasons } => {
            let refusal = format!(
                "This run did not qualify this machine: {}.",
                reasons.join("; ")
            );
            match &q.fit_verdict {
                Some(verdict) if verdict_is_against_us(verdict) => {
                    format!("{refusal} {}", verdict.trim())
                }
                _ => refusal,
            }
        }
        Release::NotIssued => q.fit_verdict.as_deref().map_or_else(
            || {
                "This machine carried the configured load, and nothing in the measurement \
                 spoke against it."
                    .to_owned()
            },
            |verdict| verdict.trim().to_owned(),
        ),
    }
}

/// Sagt das Urteil, dass der Governor hier nichts bringt?
///
/// An den festen Wendungen beider Fassungen von `vig-fit` erkannt. Ein
/// unbekannter Wortlaut gilt als nicht dagegen — dann steht er nur in seinem
/// eigenen Abschnitt, und nichts geht verloren.
fn verdict_is_against_us(verdict: &str) -> bool {
    ["against us", "not worth it", "gegen uns", "lohnt sich der"]
        .iter()
        .any(|phrase| verdict.contains(phrase))
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
    // Als Zitat, nicht fett: das Urteil von `vig-fit` traegt selbst `**` und
    // zerbrach die Hervorhebung.
    for line in headline(q).lines() {
        let _ = writeln!(out, "> {}", line.trim());
    }
    out.push('\n');
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
                 not set.** Where the input configuration already carried values, they stay in \
                 the frozen configuration **unmeasured**; the file does not mark them, this \
                 report does. The usual cause on a laptop or on a power-capped card is a \
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
            "skipped_models": q.series.skipped_models,
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
#[expect(
    clippy::too_many_lines,
    reason = "ein Schritt, seine Fremdlastbeobachtung und sein Eintrag gehoeren zusammen"
)]
pub(crate) async fn execute<S: Steps + ?Sized>(
    steps: &mut S,
    plan: &[Step],
    endpoint: &str,
    config: &Path,
    out_config: &Path,
    quick: bool,
    prior: Qualification,
) -> Qualification {
    // Auf dem bisherigen Stand aufbauen, nicht bei null anfangen. Sonst
    // verliert eine Fortsetzung genau das, was der Bericht nennen muss:
    // verworfene Reihen, den Pfad der eingefrorenen Konfiguration und die
    // Schritte, die vor dem Abbruch liefen.
    let mut q = Qualification {
        clock: steps.clock(),
        ..prior
    };
    for step in plan {
        println!("\n[{}] {}", step.key(), step.title());
        // Fremdlast entwertet das Ergebnis eines Messschritts, nicht das eines
        // Lesevorgangs: `discover` und `check` rechnen nicht. Beobachtet wird
        // davor und danach, wenn das Backend ruht — nie waehrend der eigenen
        // Last, die sonst als fremde zaehlte.
        let measuring = matches!(*step, Step::Measure | Step::Fit);
        let before = if measuring {
            steps.foreign_load().await
        } else {
            None
        };
        let started = Instant::now();
        let mut notes = Vec::new();
        // Eine neue Messung macht alles ungueltig, was auf der alten beruhte.
        // Sonst stuende nach einer Fortsetzung das Urteil ueber eine
        // `measured.yaml` im Bericht, die es so nicht mehr gibt.
        if *step == Step::Measure {
            q.series = SeriesCount::default();
            q.fit_verdict = None;
            q.doctor = None;
        }

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
                             not set, earlier values stay unmeasured",
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

        let seconds = started.elapsed().as_secs();
        let outcome = match outcome {
            Outcome::Done if measuring => {
                let after = steps.foreign_load().await;
                if quiet_enough(before) && quiet_enough(after) {
                    Outcome::Done
                } else {
                    Outcome::Contaminated {
                        reason: foreign_load_reason(before, after),
                    }
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
        // Ein Schritt steht einmal im Bericht, mit seinem letzten Ausgang.
        // Vorher wurde angehaengt: ein frueher gescheitertes `measure` blieb
        // neben dem spaeter gelungenen stehen, `failed()` sah es weiter, und
        // jede Fortsetzung verweigerte die Freigabe fuer immer.
        q.steps.retain(|s| s.step != *step);
        q.steps.push(StepResult {
            step: *step,
            outcome,
            seconds,
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

/// Der Grund fuer „verschmutzt", mit beiden Beobachtungen.
fn foreign_load_reason(before: Option<u64>, after: Option<u64>) -> String {
    let show = |value: Option<u64>| {
        value.map_or_else(
            || "not observable".to_owned(),
            |centi| {
                format!(
                    "{}.{:02} cores",
                    centi.checked_div(100).unwrap_or(0),
                    centi.checked_rem(100).unwrap_or(0)
                )
            },
        )
    };
    format!(
        "foreign CPU load {} before and {} after (limit {}.{:02} cores); something else was \
         running, or the load could not be observed",
        show(before),
        show(after),
        FOREIGN_CORES_CENTI.checked_div(100).unwrap_or(0),
        FOREIGN_CORES_CENTI.checked_rem(100).unwrap_or(0)
    )
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
    /// Endpunkt und Konfigurations-Hash dieses Laufs.
    fingerprint: String,
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
        let skipped_models = crate::calibrate::skipped_models();
        if qualified == 0 && discarded == 0 {
            return Err(
                "no measurement series ran at all; the backend answered nothing measurable"
                    .to_owned(),
            );
        }
        Ok(SeriesCount {
            qualified,
            discarded,
            skipped_models,
        })
    }

    async fn fit(&mut self, config: &Path, quick: bool) -> Result<Option<String>, String> {
        let Some(binary) = Self::fit_binary() else {
            return Ok(None);
        };
        let json_path = self.out_dir.join("fit.json");
        // Ein Ergebnis von einem frueheren Lauf darf nicht als dieses gelesen
        // werden. Vorher blieb `fit.json` liegen: scheiterte `vig-fit`, las
        // dieser Schritt die alte Datei und meldete „done" mit einem Urteil
        // aus einem anderen Lauf.
        match std::fs::remove_file(&json_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "the old {} could not be removed: {e}",
                    json_path.display()
                ));
            }
        }
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
        if !status.success() {
            return Err(format!(
                "{} exited with {status}; no verdict is taken from it",
                binary.display()
            ));
        }
        let text = std::fs::read_to_string(&json_path)
            .map_err(|e| format!("{} wrote no result: {e}", binary.display()))?;
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("fit result unreadable: {e}"))?;
        if parsed
            .get("conclusive")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            return Err("vig-fit saw no stream deliver; that is no verdict".to_owned());
        }
        // Die englische Fassung, wo es sie gibt: der Bericht ist englisch.
        parsed
            .get("verdict_en")
            .or_else(|| parsed.get("verdict"))
            .and_then(serde_json::Value::as_str)
            .map(|verdict| Some(verdict.to_owned()))
            .ok_or_else(|| "the fit result carries no verdict".to_owned())
    }

    async fn check(&mut self, config: &Path) -> Result<String, String> {
        Box::pin(crate::doctor::verdict_of(config, self.offline))
            .await
            .map(|v| v.label().to_owned())
            .map_err(|e| e.to_string())
    }

    async fn foreign_load(&mut self) -> Option<u64> {
        sample_foreign_load(FOREIGN_WINDOW).await
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
        if let Err(e) = std::fs::write(&state_path, state_json(&q, &self.fingerprint)) {
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

/// Die Groesse des Laufs, aus der Konfiguration gelesen.
///
/// Ist die Datei noch nicht da — `discover` schreibt sie erst — oder nicht
/// lesbar, gilt die Groesse, mit der dieses Werkzeug validiert wurde. Das ist
/// ehrlicher als eine Schaetzung aus geratenen Zahlen, und die Ansage nennt
/// beides: worauf sie beruht und dass sie die Hardware nicht kennt.
fn run_shape(config: &Path, samples: usize) -> RunShape {
    let samples = u64::try_from(samples).unwrap_or(RunShape::REFERENCE.samples);
    let fallback = RunShape {
        samples,
        ..RunShape::REFERENCE
    };
    let Ok(text) = std::fs::read_to_string(config) else {
        return fallback;
    };
    let Ok(parsed) = vig_config::Config::from_yaml(&text) else {
        return fallback;
    };
    RunShape {
        models: u64::try_from(parsed.models.len()).unwrap_or(RunShape::REFERENCE.models),
        slots: u64::try_from(parsed.backend.slots.max(1)).unwrap_or(RunShape::REFERENCE.slots),
        samples,
    }
}

/// Woran ein Lauf erkennt, dass er denselben Fall fortsetzt.
///
/// Der Zustand nannte bisher nur Schrittnamen. Wer zwischen zwei Aufrufen den
/// Endpunkt wechselte oder die Konfiguration aenderte, bekam die alten
/// Schritte trotzdem als erledigt angerechnet — `fit` und `check` liefen dann
/// gegen eine `measured.yaml` von einer anderen Maschine, ohne ein Wort
/// darueber. Der Fingerabdruck macht daraus einen erkennbaren Fall.
fn fingerprint_of(endpoint: &str, config: &Path) -> String {
    use sha2::Digest as _;
    let bytes = std::fs::read(config).unwrap_or_default();
    let digest = sha2::Sha256::digest(&bytes);
    let short: String = format!("{digest:x}").chars().take(16).collect();
    format!("{endpoint}|{short}")
}

/// Was ein frueherer Lauf hinterlassen hat.
struct PriorRun {
    /// Die Schritte, die er erledigt hat.
    done: Vec<Step>,
    /// Sein gesammelter Stand, als Grundlage fuer den Bericht.
    qualification: Qualification,
    /// Der Fingerabdruck, unter dem er lief.
    fingerprint: Option<String>,
}

/// Liest den Stand eines frueheren Laufs.
///
/// Vorher las diese Stelle ausschliesslich die Namen der erledigten Schritte.
/// Alles andere — verworfene Messreihen, das Urteil von `vig-fit`, der Pfad
/// der eingefrorenen Konfiguration — ging bei einer Fortsetzung verloren, weil
/// `execute` mit einer leeren `Qualification` begann und `persist` die Dateien
/// damit ueberschrieb. Der abschliessende Bericht verschwieg dann genau das,
/// was ADR-0044 verlangt: was gemessen wurde und was verworfen.
/// Ein einzelner Schritteintrag aus der Zustandsdatei.
///
/// `None`, wenn der Eintrag keinen bekannten Schritt nennt — ein Zustand aus
/// einer aelteren oder neueren Fassung soll den Lauf nicht zum Absturz
/// bringen, sondern nur weniger beitragen.
fn step_from_state(entry: &serde_json::Value) -> Option<StepResult> {
    let step = entry
        .get("step")
        .and_then(serde_json::Value::as_str)
        .and_then(Step::from_key)?;
    let reason = entry
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let outcome = match entry.get("outcome").and_then(serde_json::Value::as_str) {
        Some("done") => Outcome::Done,
        Some("contaminated") => Outcome::Contaminated {
            reason: reason.unwrap_or_default(),
        },
        Some("skipped") => Outcome::Skipped {
            reason: reason.unwrap_or_default(),
        },
        // Ein unbekanntes Wort als „gescheitert" zu lesen ist die sichere
        // Richtung: Es verweigert die Freigabe, statt sie aus einem
        // Tippfehler abzuleiten.
        _ => Outcome::Failed {
            reason: reason.unwrap_or_else(|| "unreadable state entry".to_owned()),
        },
    };
    Some(StepResult {
        step,
        outcome,
        seconds: entry
            .get("seconds")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        notes: entry
            .get("notes")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn prior_state(state_path: &Path) -> PriorRun {
    let empty = PriorRun {
        done: Vec::new(),
        qualification: Qualification::default(),
        fingerprint: None,
    };
    let Ok(text) = std::fs::read_to_string(state_path) else {
        return empty;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
        return empty;
    };

    let mut q = Qualification::default();
    if let Some(steps) = parsed.get("steps").and_then(serde_json::Value::as_array) {
        q.steps = steps.iter().filter_map(step_from_state).collect();
    }
    if let Some(series) = parsed.get("series") {
        q.series = SeriesCount {
            qualified: series
                .get("qualified")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            discarded: series
                .get("discarded")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            skipped_models: series
                .get("skipped_models")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        };
    }
    q.fit_verdict = parsed
        .get("fit_verdict")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    q.doctor = parsed
        .get("doctor")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    q.config = parsed
        .get("config")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from);

    let done = parsed
        .get("done")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter_map(Step::from_key)
                .collect()
        })
        .unwrap_or_default();

    PriorRun {
        done,
        qualification: q,
        fingerprint: parsed
            .get("fingerprint")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }
}

/// Der Zustand, den ein Lauf hinterlaesst.
///
/// Er traegt den **ganzen** bisherigen Stand und nicht nur die Schrittnamen:
/// Ein Lauf, der fortsetzt, muss den Bericht vervollstaendigen koennen und
/// nicht bei null anfangen.
fn state_json(q: &Qualification, fingerprint: &str) -> String {
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
    serde_json::json!({
        "done": q.done_keys(),
        "fingerprint": fingerprint,
        "steps": steps,
        "series": {
            "qualified": q.series.qualified,
            "discarded": q.series.discarded,
            "skipped_models": q.series.skipped_models,
        },
        "fit_verdict": q.fit_verdict,
        "doctor": q.doctor,
        "config": q.config.as_ref().map(|p| p.display().to_string()),
    })
    .to_string()
}

/// Sagt vorher, wie lange es dauert — und was zu tun ist, wenn das zu lang ist.
fn announce(steps: &[Step], quick: bool, shape: RunShape) {
    let seconds = estimate_seconds(steps, quick, shape);
    let minutes = seconds.saturating_add(59).saturating_div(60);
    println!("{PROMISE}\n");
    println!("Planned steps ({minutes} minutes, estimated generously):");
    for step in steps {
        let each = step
            .rough_seconds(quick, shape)
            .saturating_add(59)
            .saturating_div(60);
        println!("  {:<9} {} (~{each} min)", step.key(), step.title());
    }
    println!(
        "  based on {} models, {} slots, {} samples per series — {} cells in all.",
        shape.models,
        shape.slots,
        shape.samples,
        shape.cells()
    );
    // Die Grenze der Schaetzung gehoert neben die Schaetzung, nicht in eine
    // Fussnote: Sie kennt die Groesse der Matrix, aber nicht die Laufzeit der
    // Modelle, die sie noch nicht gemessen hat. Auf dem Pixel 2 dauert ein
    // Detektoraufruf zehnmal so lange wie auf der Referenzmaschine.
    println!(
        "  The estimate scales with that matrix, not with your hardware: it assumes runtimes \
         like the machine this was validated on. On a slower device it takes longer."
    );
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
    let fingerprint = fingerprint_of(&options.endpoint, &options.config);
    let prior = prior_state(&state_path);
    // Ein Stand von einem anderen Endpunkt oder einer geaenderten
    // Konfiguration ist kein Stand dieses Laufs. Ihn anzurechnen hiesse,
    // `fit` und `check` gegen eine Messung von woanders laufen zu lassen.
    //
    // Ein Zustand ganz ohne Fingerabdruck stammt aus einer aelteren Fassung
    // und laesst sich diesem Lauf nicht zuordnen — also ebenfalls neu.
    let had_state = !prior.done.is_empty() || !prior.qualification.steps.is_empty();
    let stale = had_state
        && prior
            .fingerprint
            .as_ref()
            .is_none_or(|seen| *seen != fingerprint);
    let (done, prior_qualification) = if options.restart || stale {
        if stale {
            println!(
                "The saved state belongs to a different endpoint or configuration; measuring \
                 again from the start."
            );
        }
        (Vec::new(), Qualification::default())
    } else {
        (prior.done, prior.qualification)
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
    announce(
        &steps,
        options.quick,
        run_shape(&options.config, options.samples),
    );

    let out_config = options.out_dir.join("measured.yaml");
    let mut live = Live {
        endpoint: options.endpoint.clone(),
        out_dir: options.out_dir.clone(),
        samples: options.samples,
        period_us: options.period_us,
        offline: options.offline,
        identity: identity.clone(),
        fingerprint,
    };

    let qualification = Box::pin(execute(
        &mut live,
        &steps,
        &options.endpoint,
        &options.config,
        &out_config,
        options.quick,
        prior_qualification,
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
        Clock, Outcome, Qualification, Release, RunShape, SeriesCount, Step, StepResult, Steps,
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
        load: Option<u64>,
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
        async fn foreign_load(&mut self) -> Option<u64> {
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
            Qualification::default(),
        )
        .await
    }

    /// Eine Fortsetzung darf den Bericht nicht kuerzen.
    ///
    /// Der Fall aus dem Review: Lauf 1 misst und verwirft drei von vier
    /// Reihen, dann bricht er ab. Lauf 2 faehrt nur noch `fit` und `check`.
    /// Vorher begann dieser zweite Lauf mit einer leeren `Qualification` und
    /// ueberschrieb den Bericht damit — die verworfenen Reihen, die
    /// eingefrorene Konfiguration und die beiden ersten Schritte waren weg.
    #[tokio::test]
    async fn resuming_keeps_what_the_earlier_run_measured() {
        let prior = Qualification {
            steps: vec![
                StepResult {
                    step: Step::Discover,
                    outcome: Outcome::Done,
                    seconds: 1,
                    notes: vec!["4 models found at the backend".to_owned()],
                },
                StepResult {
                    step: Step::Measure,
                    outcome: Outcome::Done,
                    seconds: 28,
                    notes: vec!["3 of 4 series discarded".to_owned()],
                },
            ],
            series: SeriesCount {
                qualified: 1,
                discarded: 3,
                skipped_models: 0,
            },
            config: Some(PathBuf::from("measured.yaml")),
            ..Qualification::default()
        };
        let mut fake = clean();
        let q = execute(
            &mut fake,
            &[Step::Fit, Step::Check],
            "127.0.0.1:8001",
            Path::new("vig.yaml"),
            Path::new("measured.yaml"),
            false,
            prior,
        )
        .await;

        assert!(q.complete(), "alle vier Schritte im Bericht: {:?}", q.steps);
        assert_eq!(
            q.series,
            SeriesCount {
                qualified: 1,
                discarded: 3,
                skipped_models: 0,
            },
            "die verworfenen Reihen des ersten Laufs bleiben im Bericht"
        );
        assert_eq!(
            q.config,
            Some(PathBuf::from("measured.yaml")),
            "und die eingefrorene Konfiguration ebenfalls"
        );
        let Release::Refused { reasons } = q.release() else {
            panic!("drei verworfene Reihen duerfen nichts offenlassen");
        };
        assert!(
            reasons.iter().any(|r| r.contains("discarded")),
            "der Grund muss die Reihen des ersten Laufs nennen: {reasons:?}"
        );
        assert!(
            markdown(&q, "x").contains("not set"),
            "und der Bericht muss weiterhin sagen, dass Werte fehlen"
        );
    }

    fn clean() -> Fake {
        Fake {
            series: SeriesCount {
                qualified: 4,
                discarded: 0,
                skipped_models: 0,
            },
            load: Some(30),
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
                skipped_models: 0,
            },
            load: Some(30),
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
            load: Some(300),
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
            load: Some(30),
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

    /// Die Zusage gilt fuer die Groesse, mit der sie validiert wurde.
    #[test]
    fn the_reference_run_fits_into_the_promised_half_hour() {
        let reference = RunShape::REFERENCE;
        assert!(
            estimate_seconds(&Step::ALL, false, reference) <= super::PROMISED_SECONDS,
            "die Schaetzung ueberschreitet die zugesagte halbe Stunde: {} s",
            estimate_seconds(&Step::ALL, false, reference)
        );
        assert!(
            estimate_seconds(&Step::ALL, true, reference)
                < estimate_seconds(&Step::ALL, false, reference)
        );
    }

    /// Und sie muss reissen koennen — sonst ist die Warnung toter Code.
    ///
    /// Der alte Test verglich die Summe fester Konstanten mit einer festen
    /// Schwelle. Er konnte gar nicht fehlschlagen, auch dann nicht, wenn ein
    /// echter Lauf Stunden gebraucht haette. Dieser hier haelt fest, dass die
    /// Schaetzung mit der Matrix waechst **und** dass sie die Zusage
    /// ueberschreitet, sobald die Matrix gross genug ist.
    #[test]
    fn a_large_matrix_breaks_the_promise_and_the_tool_says_so() {
        let reference = RunShape::REFERENCE;
        let large = RunShape {
            models: 16,
            slots: 4,
            samples: 500,
        };
        assert!(
            estimate_seconds(&Step::ALL, false, large)
                > estimate_seconds(&Step::ALL, false, reference),
            "eine groessere Matrix muss laenger dauern"
        );
        assert!(
            estimate_seconds(&Step::ALL, false, large) > super::PROMISED_SECONDS,
            "die Warnung muss erreichbar sein: {} s bei {} Zellen",
            estimate_seconds(&Step::ALL, false, large),
            large.cells()
        );
    }

    /// Ein Slot heisst: keine Paarmessung, also eine viel kleinere Matrix.
    #[test]
    fn a_single_slot_measures_no_pairs() {
        let one = RunShape {
            models: 8,
            slots: 1,
            samples: 200,
        };
        let two = RunShape { slots: 2, ..one };
        assert_eq!(one.cells(), 8, "acht Solomessungen, keine Paare");
        assert_eq!(
            two.cells(),
            8 + 8 * 7,
            "und mit zwei Slots beide Richtungen"
        );
        assert!(
            estimate_seconds(&Step::ALL, false, one) < estimate_seconds(&Step::ALL, false, two)
        );
    }

    /// Ein ganzer fremder Kern ist Fremdlast, und eine Luecke ist keine Ruhe.
    #[test]
    fn a_whole_foreign_core_is_foreign_load_and_a_blind_spot_is_not_quiet() {
        assert!(quiet_enough(Some(99)));
        assert!(!quiet_enough(Some(100)));
        assert!(
            !quiet_enough(None),
            "nicht beobachtbar ist kein Beleg fuer eine ruhige Maschine"
        );
    }

    /// Ein einmal gescheiterter Schritt verweigert nicht jeden spaeteren Lauf.
    #[tokio::test]
    async fn a_step_that_failed_once_does_not_refuse_every_later_run() {
        let prior = Qualification {
            steps: vec![
                StepResult {
                    step: Step::Discover,
                    outcome: Outcome::Done,
                    seconds: 1,
                    notes: Vec::new(),
                },
                StepResult {
                    step: Step::Measure,
                    outcome: Outcome::Failed {
                        reason: "backend unreachable".to_owned(),
                    },
                    seconds: 1,
                    notes: Vec::new(),
                },
            ],
            ..Qualification::default()
        };
        let mut fake = clean();
        let q = execute(
            &mut fake,
            &plan(None, &[Step::Discover]),
            "127.0.0.1:8001",
            Path::new("vig.yaml"),
            Path::new("measured.yaml"),
            false,
            prior,
        )
        .await;
        assert!(
            !q.failed(),
            "der alte Fehlschlag ist ersetzt: {:?}",
            q.steps
        );
        assert_eq!(q.steps.len(), 4, "jeder Schritt steht einmal da");
        assert_eq!(q.release(), Release::NotIssued);
    }

    /// Wird neu gemessen, laeuft alles nach, was auf der alten Messung beruhte.
    #[test]
    fn a_new_measurement_reruns_what_depended_on_the_old_one() {
        assert_eq!(
            plan(None, &[Step::Discover, Step::Check]),
            vec![Step::Measure, Step::Fit, Step::Check]
        );
    }

    /// Ein verweigerter Lauf beginnt mit der Verweigerung, nicht mit einem
    /// Urteil zu unseren Gunsten — der Laptoplauf vom 15.09.
    #[tokio::test]
    async fn a_refused_run_is_not_headlined_by_a_verdict_in_our_favour() {
        let mut fake = Fake {
            series: SeriesCount {
                qualified: 3,
                discarded: 1,
                skipped_models: 0,
            },
            fit: Some(
                "From 90 % load the direct path misses 996 ‰ of the protected stream's cycles, \
                 the governor 0 ‰."
                    .to_owned(),
            ),
            ..clean()
        };
        let q = run(&mut fake).await;
        assert!(
            headline(&q).starts_with("This run did not qualify this machine"),
            "{}",
            headline(&q)
        );
        assert!(!headline(&q).contains("996"), "{}", headline(&q));
        assert!(
            markdown(&q, "x").contains("996 ‰"),
            "das Urteil bleibt im eigenen Abschnitt"
        );
    }

    /// Ein Urteil gegen uns steht auch bei einer Verweigerung ganz oben.
    #[tokio::test]
    async fn a_verdict_against_us_stays_on_top_of_a_refusal() {
        let mut fake = Fake {
            load: Some(300),
            fit: Some(
                "That is a result against us: on this load it brings nothing here.".to_owned(),
            ),
            ..clean()
        };
        let q = run(&mut fake).await;
        let top = headline(&q);
        assert!(top.starts_with("This run did not qualify"), "{top}");
        assert!(top.contains("against us"), "{top}");
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
