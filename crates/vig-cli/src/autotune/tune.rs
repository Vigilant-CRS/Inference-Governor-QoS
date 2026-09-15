//! Schritt `tune` — der Governor wird auf die Last des Anwenders eingestellt,
//! innerhalb seiner Vertraege (ADR-0045).
//!
//! Bis hierher hat `autotune` gemessen und geurteilt. Die Stellgroessen des
//! Governors — Pipelining-Tiefe, Versorgungsschutz, Kalibrierung an der Karte,
//! Sicherheitsmarge — standen dabei so da, wie der Anwender sie hingeschrieben
//! hatte, oder auf ihrer Voreinstellung. Ob eine andere Einstellung auf
//! **seiner** Last weniger Takte verliert, hat niemand ausprobiert; `vig-fit`
//! hat danach nur gesagt, ob sich die unverstellte Fassung lohnt.
//!
//! ## Wie gesucht wird
//!
//! Eine Koordinatensuche mit **einem** Durchgang: Die Stellgroessen werden in
//! fester Reihenfolge je einzeln umgestellt, ausgehend von der besten bisher
//! gefundenen Einstellung, und jede Fassung faehrt `vig-fit` nur mit dem
//! Governor-Arm. Behalten wird eine Umstellung nur, wenn sie die bisher beste
//! um mehr als die Rauschschwelle schlaegt. Eine Stellgroesse wird nicht noch
//! einmal versucht, nachdem eine spaetere sich geaendert hat — das steht so im
//! Bericht, damit niemand „das Optimum" liest, wo „der beste von N Versuchen"
//! steht.
//!
//! ## Was nie angefasst wird
//!
//! Vertraege (`period_ms`, `deadline_ms`, `max_age_ms`, `class`) sind Zusagen
//! des Betreibers an seine Anwendung, und `backend.slots` beschreibt das
//! Backend (ADR-0004). Beides ist keine Einstellung des Governors. Jede
//! Fassung wird vor der Bewertung darauf geprueft, dass Modelle, Slots und
//! Domaenen unveraendert sind; eine Fassung, die daran etwas aendern wuerde,
//! wird verweigert und nicht bewertet.
//!
//! ## Was nie geschrieben wird
//!
//! Eine Einstellung, die fuer die geschuetzten Stroeme schlechter ist als die
//! unverstellte, wird nicht behalten — auch dann nicht, wenn sie den
//! nachrangigen Stroemen viel bringt. Lief der Schritt unter Fremdlast, wird
//! die unverstellte Fassung zurueckgeschrieben: Dann weiss niemand, ob der
//! Vorsprung von der Einstellung kam oder von der anderen Arbeit.
//!
//! ## Machbarkeit und Bestaetigung
//!
//! Zwei Befunde vom Pixel 2 (15.09., `vig-slots2-heavy.yaml`) haben die erste
//! Fassung dieses Schritts widerlegt (ADR-0045, „Bestaetigung und
//! Machbarkeit"):
//!
//! * **Vor der Suche** muss die gemessene Fassung `vig doctor` bestehen. Sagt
//!   die Pruefung `NOT_READY`, wird nicht gesucht: Auf einer Maschine, die die
//!   Vertraege nicht planen kann, misst die Suche nur Streuung.
//! * **Vor dem Schreiben** muss eine behaltene Einstellung sich bestaetigen:
//!   unverstellt und eingestellt noch einmal, abwechselnd, in zwei Paaren,
//!   und in **jedem** Paar muss die eingestellte nach derselben Regel
//!   gewinnen. Ein einzelnes 10-s-Fenster auf einer gesaettigten Maschine
//!   streut weiter als der Vorsprung, den die Suche behalten hatte.

use super::{Outcome, Steps};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use vig_config::Config;
use vig_config::schema::{MarginLearningConfig, PredictionMode};

/// Unterverzeichnis neben `measured.yaml`, in dem jede Fassung liegt.
const TUNE_DIR: &str = "tune";

/// Die unverstellte Fassung, wie `measure` sie geschrieben hat.
const UNTUNED_FILE: &str = "untuned.yaml";

/// Mindestvorsprung fuer die geschuetzten Stroeme, in Promille.
///
/// Zwei Laeufe derselben Einstellung liegen nicht auf dasselbe Promille
/// genau; wer jede kleinere Differenz behielte, stellte den Governor nach dem
/// Rauschen ein. Fuenf Promille sind eine bewusst gesetzte Grenze, keine
/// gemessene Streuung — und darueber gilt ein Zehntel des bisher besten Werts,
/// weil die Streuung mit der Hoehe der Verluste waechst.
const PROTECTED_MIN_GAIN_PERMILLE: u64 = 5;

/// Mindestvorsprung fuer die nachrangigen Stroeme, in Promille.
///
/// Hoeher als bei den geschuetzten: Ein nachrangiger Strom darf einer
/// Umstellung nur dann den Ausschlag geben, wenn der Gewinn deutlich ist.
const BACKGROUND_MIN_GAIN_PERMILLE: u64 = 10;

/// Der relative Teil der Rauschschwelle: ein Zehntel des bisher besten Werts.
const RELATIVE_GAIN_DIVISOR: u64 = 10;

/// Um so viel wird die Sicherheitsmarge je Versuch verschoben, in Prozent.
///
/// Aus der Voreinstellung 110 werden damit 125 und 100 — die untere Grenze,
/// die die Konfigurationspruefung zulaesst.
const MARGIN_STEP_PERCENT: u32 = 15;

/// Die Grenzen der Sicherheitsmarge, wie `Config::diagnose` sie prueft.
const MARGIN_MIN_PERCENT: u32 = 100;
const MARGIN_MAX_PERCENT: u32 = 300;

/// Bewertungen eines ueblichen Laufs: die unverstellte Fassung und fuenf
/// Umstellungen (Tiefe, Versorgungsschutz, Kalibrierung, zwei Margen).
const TYPICAL_EVALUATIONS: u64 = 6;

/// Paare der Bestaetigung: je einmal unverstellt, dann eingestellt.
///
/// Zwei und nicht eines: Ein einzelnes Paar kann denselben Zufall zweimal
/// treffen, der schon die Suche getaeuscht hat. Mehr als zwei kosten je Paar
/// zwei weitere Bewertungen, und die Regel verlangt ohnehin jedes Paar.
const CONFIRMATION_PAIRS: u64 = 2;

/// Bewertungen der Bestaetigung.
const CONFIRMATION_EVALUATIONS: u64 = CONFIRMATION_PAIRS.saturating_mul(2);

/// Aufwand je Aufruf von `vig-fit` jenseits der Messfenster, in Sekunden:
/// Start, Metadaten, zwei Fenster fuer die Fremdlast von je drei Sekunden.
/// Der Laptoplauf vom 15.09. brauchte fuer 80 s Messfenster 82 s, die
/// Telefone 87 bis 93 s — zehn Sekunden sind eher zu viel.
const OVERHEAD_SECONDS_PER_EVALUATION: u64 = 10;

/// Lastpunkte einer Bewertung.
///
/// Nicht 90 %: Unterhalb der Saettigung verliert keine Einstellung etwas, und
/// ein Punkt, an dem alle Fassungen null Promille haben, kostet Zeit, ohne zu
/// unterscheiden.
pub(crate) const fn eval_points(quick: bool) -> &'static str {
    if quick { "110,125" } else { "100,110,125" }
}

/// Wie viele Lastpunkte [`eval_points`] nennt.
const fn eval_point_count(quick: bool) -> u64 {
    if quick { 2 } else { 3 }
}

/// Messdauer je Lastpunkt einer Bewertung, in Sekunden.
pub(crate) const fn eval_seconds(quick: bool) -> u64 {
    if quick { 5 } else { 10 }
}

/// Grobe Dauer des Schritts in Sekunden, fuer die Schaetzung vor dem Start.
///
/// Anders als der Messschritt haengt sie kaum an der Hardware: Die Messfenster
/// von `vig-fit` sind feste Wanduhrzeit. Was sie nicht kennt, ist die Zahl der
/// Stellgroessen, die tatsaechlich versucht werden — sechs Bewertungen sind
/// der uebliche Fall —, und ob etwas behalten wird. Gerechnet wird, als waere
/// es so: dann kommen die vier Bewertungen der Bestaetigung dazu.
pub(crate) const fn rough_seconds(quick: bool) -> u64 {
    TYPICAL_EVALUATIONS
        .saturating_add(CONFIRMATION_EVALUATIONS)
        .saturating_mul(
            eval_point_count(quick)
                .saturating_mul(eval_seconds(quick))
                .saturating_add(OVERHEAD_SECONDS_PER_EVALUATION),
        )
}

/// Ein Strom an einem Lastpunkt, unter dem Governor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamMiss {
    pub(crate) stream: String,
    pub(crate) protected: bool,
    /// Unabgedeckte Abtastungen aus Verbrauchersicht, je Promille.
    pub(crate) governed_permille: u64,
}

/// Ein Lastpunkt einer Bewertung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadPoint {
    pub(crate) load_percent: u64,
    pub(crate) streams: Vec<StreamMiss>,
}

/// Was `vig-fit` ueber eine Fassung gemessen hat, nur der Governor-Arm.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Evaluation {
    pub(crate) points: Vec<LoadPoint>,
}

impl Evaluation {
    /// Die Zielgroesse; `None` ohne einen einzigen Lastpunkt.
    ///
    /// `protected_worst` ist der schlechteste geschuetzte Strom am
    /// schlechtesten Lastpunkt, `background_mean` der Mittelwert der
    /// nachrangigen Stroeme je Lastpunkt, gemittelt ueber die Lastpunkte
    /// (abgerundet; null, wo es keinen nachrangigen Strom gibt).
    ///
    /// Mittelwert und nicht der schlechteste nachrangige Strom: Im Laptoplauf
    /// vom 15.09. stand ein unteilbarer 95-ms-Block in jeder Fassung bei
    /// 1000 ‰, weil er neben einer 33-ms-Periode nie passt (ADR-0012). Als
    /// Maximum verdeckte er jede Verbesserung der anderen nachrangigen Stroeme
    /// — das Tuning haette dort nie etwas gewinnen koennen.
    pub(crate) fn objective(&self) -> Option<Objective> {
        if self.points.is_empty() {
            return None;
        }
        let mut protected_worst = 0_u64;
        let mut background_sum = 0_u64;
        for point in &self.points {
            let protected = point
                .streams
                .iter()
                .filter(|s| s.protected)
                .map(|s| s.governed_permille)
                .max()
                .unwrap_or(0);
            let (sum, count) = point.streams.iter().filter(|s| !s.protected).fold(
                (0_u64, 0_u64),
                |(sum, count), s| {
                    (
                        sum.saturating_add(s.governed_permille),
                        count.saturating_add(1),
                    )
                },
            );
            protected_worst = protected_worst.max(protected);
            background_sum = background_sum.saturating_add(sum.checked_div(count).unwrap_or(0));
        }
        let count = u64::try_from(self.points.len()).unwrap_or(u64::MAX);
        Some(Objective {
            protected_worst,
            background_mean: background_sum.checked_div(count).unwrap_or(0),
        })
    }

    /// Welche Stroeme an welchen Lastpunkten geliefert haben.
    ///
    /// Zwei Bewertungen sind nur vergleichbar, wenn diese Menge gleich ist.
    /// Sonst koennte eine Fassung besser aussehen, weil ein Strom in ihrer
    /// Bewertung gar nicht vorkommt.
    fn cells(&self) -> BTreeSet<(u64, String)> {
        self.points
            .iter()
            .flat_map(|p| p.streams.iter().map(|s| (p.load_percent, s.stream.clone())))
            .collect()
    }
}

/// Liest die Bewertung aus dem JSON von `vig-fit`.
///
/// # Errors
///
/// Wenn `vig-fit` keine Lieferung gesehen hat oder eine Zelle unvollstaendig
/// ist. Eine halb gelesene Bewertung ist keine.
pub(crate) fn evaluation_from_fit_json(parsed: &serde_json::Value) -> Result<Evaluation, String> {
    if parsed
        .get("conclusive")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
    {
        return Err("vig-fit saw no stream deliver; that is no evaluation".to_owned());
    }
    let cells = parsed
        .get("cells")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "the evaluation carries no cells".to_owned())?;
    let mut points: Vec<LoadPoint> = Vec::new();
    for cell in cells {
        let incomplete = || format!("an evaluation cell is incomplete: {cell}");
        let load_percent = cell
            .get("load_percent")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(incomplete)?;
        let miss = StreamMiss {
            stream: cell
                .get("stream")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(incomplete)?
                .to_owned(),
            protected: cell
                .get("protected")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(incomplete)?,
            governed_permille: cell
                .get("governed_uncovered_permille")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(incomplete)?,
        };
        match points.iter_mut().find(|p| p.load_percent == load_percent) {
            Some(point) => point.streams.push(miss),
            None => points.push(LoadPoint {
                load_percent,
                streams: vec![miss],
            }),
        }
    }
    if points.is_empty() {
        return Err("the evaluation has no load point".to_owned());
    }
    Ok(Evaluation { points })
}

/// Warum eine Fassung nicht bewertet wurde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unevaluated {
    /// `vig-fit` ist nicht da.
    Missing,
    /// Die Konfigurationspruefung hat die Fassung abgelehnt (`vig-fit`
    /// Exitcode 2) — mit den Befunden.
    Refused { reason: String },
    /// Die Bewertung lief nicht durch.
    Failed { reason: String },
}

/// Was `vig doctor` ueber die gemessene Fassung sagt, bevor gesucht wird.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Feasibility {
    /// `READY`, `READY_WITH_WARNINGS` oder `NOT_READY`.
    pub(crate) verdict: String,
    /// Die `FAIL`-Zeilen der Pruefung, soweit es welche gibt.
    pub(crate) failures: Vec<String>,
}

impl Feasibility {
    /// Verweigert die Pruefung die Fassung?
    fn refuses(&self) -> bool {
        self.verdict == "NOT_READY"
    }
}

/// Die Zielgroesse einer Bewertung, in Promille.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Objective {
    pub(crate) protected_worst: u64,
    pub(crate) background_mean: u64,
}

impl Objective {
    /// `geschuetzt/nachrangig ‰`, fuer Begruendungen.
    fn short(self) -> String {
        format!("{}/{} ‰", self.protected_worst, self.background_mean)
    }
}

/// Ein Paar der Bestaetigung: unverstellt, dann eingestellt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfirmationPair {
    /// 1 oder 2, wie in `tune/confirm-<pair>-{untuned,tuned}.json`.
    pub(crate) pair: u64,
    /// `None`, wenn die Bewertung nicht zustande kam.
    pub(crate) untuned: Option<Objective>,
    pub(crate) tuned: Option<Objective>,
    /// Gewinnt die eingestellte Fassung in diesem Paar nach [`decide`]?
    pub(crate) held: bool,
    pub(crate) reason: String,
}

/// Die Bestaetigung einer behaltenen Einstellung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Confirmation {
    pub(crate) pairs: Vec<ConfirmationPair>,
}

impl Confirmation {
    /// Hat sie in **jedem** Paar gehalten?
    pub(crate) fn held(&self) -> bool {
        !self.pairs.is_empty() && self.pairs.iter().all(|p| p.held)
    }

    /// Die Zahlen aller Paare in einer Zeile, fuer `withheld`.
    fn summary(&self) -> String {
        let show =
            |o: Option<Objective>| o.map_or_else(|| "not evaluated".to_owned(), Objective::short);
        self.pairs
            .iter()
            .map(|p| {
                format!(
                    "pair {} untuned {}, tuned {} ({})",
                    p.pair,
                    show(p.untuned),
                    show(p.tuned),
                    if p.held { "held" } else { "not held" }
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Behalten oder nicht — mit Begruendung in beiden Faellen.
///
/// Behalten wird eine Fassung, wenn sie
///
/// * die geschuetzten Stroeme um mindestens `max(5 ‰, ein Zehntel)` des
///   bisher besten Werts besser versorgt, **oder**
/// * sie nicht schlechter versorgt und die nachrangigen Stroeme um mindestens
///   `max(10 ‰, ein Zehntel)` besser.
///
/// Nie behalten wird eine Fassung, die fuer die geschuetzten Stroeme
/// schlechter ist als die unverstellte. Aus den beiden Regeln folgt das
/// bereits — die bisher beste ist nie schlechter als die unverstellte —, es
/// steht trotzdem als erste Pruefung da, weil es die Zusage ist, auf die es
/// ankommt.
///
/// # Errors
///
/// Die Begruendung, warum die Fassung nicht behalten wird.
pub(crate) fn decide(
    candidate: Objective,
    best: Objective,
    untuned: Objective,
) -> Result<String, String> {
    if candidate.protected_worst > untuned.protected_worst {
        return Err(format!(
            "protected streams miss {} ‰, worse than the {} ‰ of the untuned configuration",
            candidate.protected_worst, untuned.protected_worst
        ));
    }
    let protected_gain = PROTECTED_MIN_GAIN_PERMILLE.max(
        best.protected_worst
            .checked_div(RELATIVE_GAIN_DIVISOR)
            .unwrap_or(0),
    );
    if let Some(limit) = best.protected_worst.checked_sub(protected_gain)
        && candidate.protected_worst <= limit
    {
        return Ok(format!(
            "protected streams {} ‰ instead of {} ‰ (at most {limit} ‰ needed)",
            candidate.protected_worst, best.protected_worst
        ));
    }
    if candidate.protected_worst > best.protected_worst {
        return Err(format!(
            "protected streams miss {} ‰, more than the {} ‰ of the best setting so far",
            candidate.protected_worst, best.protected_worst
        ));
    }
    let background_gain = BACKGROUND_MIN_GAIN_PERMILLE.max(
        best.background_mean
            .checked_div(RELATIVE_GAIN_DIVISOR)
            .unwrap_or(0),
    );
    if let Some(limit) = best.background_mean.checked_sub(background_gain)
        && candidate.background_mean <= limit
    {
        return Ok(format!(
            "lower-priority streams {} ‰ instead of {} ‰ (at most {limit} ‰ needed), protected \
             streams no worse ({} ‰)",
            candidate.background_mean, best.background_mean, candidate.protected_worst
        ));
    }
    Err(format!(
        "within the noise threshold: protected {} ‰ against {} ‰ (needs {protected_gain} ‰ \
         less), lower-priority {} ‰ against {} ‰ (needs {background_gain} ‰ less)",
        candidate.protected_worst,
        best.protected_worst,
        candidate.background_mean,
        best.background_mean
    ))
}

/// Was aus einer Fassung wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Besser als die bisher beste, jenseits der Rauschschwelle.
    Kept,
    /// Bewertet (oder nicht bewertbar) und nicht besser.
    Rejected,
    /// Von der Konfigurationspruefung abgelehnt, nicht bewertet.
    Refused,
}

impl Decision {
    /// Das Wort im Bericht.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Rejected => "rejected",
            Self::Refused => "refused",
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        [Self::Kept, Self::Rejected, Self::Refused]
            .into_iter()
            .find(|d| d.label() == label)
    }
}

/// Eine versuchte Fassung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    /// Laufende Nummer, wie in `tune/candidate-<n>.yaml`.
    pub(crate) number: u64,
    /// Die eine Umstellung gegenueber der bisher besten Fassung.
    pub(crate) change: String,
    /// Wie sich die Fassung von der unverstellten unterscheidet.
    pub(crate) settings: Vec<String>,
    /// `None`, wenn nicht bewertet — dann steht auch keine Zahl im Bericht.
    pub(crate) objective: Option<Objective>,
    pub(crate) decision: Decision,
    pub(crate) reason: String,
}

/// Was der Schritt `tune` festgestellt hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tuning {
    /// Die unverstellte Fassung, so wie `measure` sie geschrieben hat.
    pub(crate) untuned: Objective,
    /// Die beste Fassung; gleich `untuned`, wenn nichts behalten wurde.
    pub(crate) tuned: Objective,
    pub(crate) candidates: Vec<Candidate>,
    /// Stellgroessen, die nicht versucht wurden, mit Grund.
    pub(crate) not_tried: Vec<String>,
    /// Steht die beste Fassung in `measured.yaml`?
    pub(crate) applied: bool,
    /// Warum eine gefundene bessere Fassung **nicht** geschrieben wurde.
    pub(crate) withheld: Option<String>,
    /// Die Bestaetigung; `None`, wenn nichts behalten wurde.
    pub(crate) confirmation: Option<Confirmation>,
}

impl Tuning {
    /// Wurde irgendeine Umstellung behalten?
    pub(crate) fn improved(&self) -> bool {
        self.candidates.iter().any(|c| c.decision == Decision::Kept)
    }
}

/// Die Stellgroessen, in der Reihenfolge, in der sie versucht werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Knob {
    PipeliningDepth,
    ProtectSupply,
    MarginLearning,
    SafetyMargin,
}

impl Knob {
    /// Feste Reihenfolge: zuerst, was die Belegung des Backends aendert, dann
    /// die Policy, dann die Planung. Die Reihenfolge ist eine Wahl, keine
    /// Messung; der Bericht nennt sie.
    const ORDER: [Self; 4] = [
        Self::PipeliningDepth,
        Self::ProtectSupply,
        Self::MarginLearning,
        Self::SafetyMargin,
    ];

    const fn field(self) -> &'static str {
        match self {
            Self::PipeliningDepth => "backend.pipelining_depth",
            Self::ProtectSupply => "backend.protect_supply",
            Self::MarginLearning => "backend.margin_learning",
            Self::SafetyMargin => "backend.safety_margin_percent",
        }
    }
}

/// Eine einzelne Umstellung.
#[derive(Debug, Clone, Copy)]
enum Change {
    PipeliningDepth(usize),
    ProtectSupply(bool),
    MarginLearning(Option<MarginLearningConfig>),
    SafetyMargin(u32),
}

impl Change {
    fn apply(self, config: &mut Config) {
        match self {
            Self::PipeliningDepth(depth) => config.backend.pipelining_depth = depth,
            Self::ProtectSupply(on) => config.backend.protect_supply = on,
            Self::MarginLearning(learning) => config.backend.margin_learning = learning,
            Self::SafetyMargin(percent) => config.backend.safety_margin_percent = percent,
        }
    }

    fn describe(self) -> String {
        match self {
            Self::PipeliningDepth(depth) => format!("backend.pipelining_depth → {depth}"),
            Self::ProtectSupply(on) => format!("backend.protect_supply → {on}"),
            Self::MarginLearning(learning) => format!(
                "backend.margin_learning → {}",
                if learning.is_some() {
                    "on (defaults)"
                } else {
                    "off"
                }
            ),
            Self::SafetyMargin(percent) => format!("backend.safety_margin_percent → {percent}"),
        }
    }
}

/// Die Umstellungen einer Stellgroesse, ausgehend von der bisher besten
/// Fassung.
///
/// # Errors
///
/// Warum die Stellgroesse nicht versucht wird.
fn changes(knob: Knob, best: &Config) -> Result<Vec<Change>, String> {
    let backend = &best.backend;
    match knob {
        // 0 ↔ 1; eine hoehere Tiefe wird mit 0 verglichen, nicht schrittweise
        // abgebaut — ein Durchgang, eine Umstellung je Stellgroesse.
        Knob::PipeliningDepth => Ok(vec![Change::PipeliningDepth(usize::from(
            backend.pipelining_depth == 0,
        ))]),
        Knob::ProtectSupply => Ok(vec![Change::ProtectSupply(!backend.protect_supply)]),
        Knob::MarginLearning => {
            if backend.margin_learning.is_some() {
                return Ok(vec![Change::MarginLearning(None)]);
            }
            if backend.prediction == PredictionMode::Active {
                return Err(
                    "not tried: `prediction: active` is set, and the configuration \
                            check refuses it together with margin_learning (ADR-0038)"
                        .to_owned(),
                );
            }
            // Genau das, was `margin_learning: {}` ergibt — die Voreinstellungen
            // stehen an einer Stelle, am Schema, und nicht ein zweites Mal hier.
            serde_json::from_str::<MarginLearningConfig>("{}")
                .map(|defaults| vec![Change::MarginLearning(Some(defaults))])
                .map_err(|e| format!("not tried: the defaults could not be built: {e}"))
        }
        Knob::SafetyMargin => {
            let base = backend.safety_margin_percent;
            let up = base
                .saturating_add(MARGIN_STEP_PERCENT)
                .clamp(MARGIN_MIN_PERCENT, MARGIN_MAX_PERCENT);
            let down = base
                .saturating_sub(MARGIN_STEP_PERCENT)
                .clamp(MARGIN_MIN_PERCENT, MARGIN_MAX_PERCENT);
            let mut values = Vec::new();
            for value in [up, down] {
                if value != base && !values.contains(&value) {
                    values.push(value);
                }
            }
            Ok(values.into_iter().map(Change::SafetyMargin).collect())
        }
    }
}

/// Die Stellgroessen einer Fassung, als Text fuer den Vergleich.
fn settings_of(config: &Config) -> [(&'static str, String); 4] {
    let backend = &config.backend;
    [
        (
            Knob::PipeliningDepth.field(),
            backend.pipelining_depth.to_string(),
        ),
        (
            Knob::ProtectSupply.field(),
            backend.protect_supply.to_string(),
        ),
        (
            Knob::MarginLearning.field(),
            if backend.margin_learning.is_some() {
                "on"
            } else {
                "off"
            }
            .to_owned(),
        ),
        (
            Knob::SafetyMargin.field(),
            backend.safety_margin_percent.to_string(),
        ),
    ]
}

/// Wie sich `candidate` von `untuned` unterscheidet.
fn diff(untuned: &Config, candidate: &Config) -> Vec<String> {
    settings_of(untuned)
        .into_iter()
        .zip(settings_of(candidate))
        .filter(|((_, before), (_, after))| before != after)
        .map(|((field, before), (_, after))| format!("{field}: {before} → {after}"))
        .collect()
}

/// Sind Vertraege, Modelle, Slots und Domaenen unveraendert?
///
/// Nach Bauart immer — `Change::apply` fasst nur Stellgroessen an. Die
/// Pruefung steht trotzdem da, weil „nie" hier eine Zusage ist und keine
/// Beobachtung ueber den heutigen Code.
fn untouched(untuned: &Config, candidate: &Config) -> bool {
    let models = |c: &Config| serde_json::to_value(&c.models).ok();
    let domains = |c: &Config| serde_json::to_value(&c.backend.domains).ok();
    untuned.backend.slots == candidate.backend.slots
        && models(untuned).is_some()
        && models(untuned) == models(candidate)
        && domains(untuned) == domains(candidate)
}

/// Prueft eine Fassung wie `vig doctor` und gibt ihren Text zurueck.
///
/// # Errors
///
/// Die Befunde, wenn die Fassung nicht besteht.
fn prepare(untuned: &Config, candidate: &Config) -> Result<String, String> {
    if !untouched(untuned, candidate) {
        return Err(
            "the setting would change a contract, a model or the slots, and tuning never does"
                .to_owned(),
        );
    }
    let text = candidate.to_yaml().map_err(|e| e.to_string())?;
    let reread = Config::from_yaml(&text).map_err(|e| e.to_string())?;
    let findings = reread.diagnose();
    if findings.is_empty() {
        Ok(text)
    } else {
        Err(findings
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "))
    }
}

/// Wo die Fassungen liegen: neben `measured.yaml`.
fn tune_dir(measured: &Path) -> PathBuf {
    measured
        .parent()
        .map_or_else(|| PathBuf::from(TUNE_DIR), |dir| dir.join(TUNE_DIR))
}

/// Vergisst die unverstellte Fassung eines frueheren `tune`.
///
/// `measure` ruft das vor jeder neuen Messung. Danach gilt: Liegt
/// `tune/untuned.yaml` da, ist es die unverstellte Fassung **dieser**
/// Messung, und ein wiederholtes `tune` beginnt von ihr und nicht von einer
/// schon verstellten `measured.yaml`.
///
/// # Errors
///
/// Wenn die alte Datei da ist und nicht entfernt werden kann.
pub(crate) fn forget(measured: &Path) -> Result<(), String> {
    let path = tune_dir(measured).join(UNTUNED_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "the untuned configuration of an earlier run, {}, could not be removed: {e}",
            path.display()
        )),
    }
}

/// Schreibt die unverstellte Fassung zurueck, weil der Vergleich nicht traegt.
///
/// # Errors
///
/// Wenn sie nicht zurueckgeschrieben werden kann. Dann steht eine verstellte
/// Fassung in `measured.yaml`, deren Vorsprung nicht belegt ist — das ist ein
/// Fehlschlag und kein Hinweis.
pub(crate) fn withhold(measured: &Path, tuning: &mut Tuning, why: &str) -> Result<(), String> {
    if !tuning.applied {
        return Ok(());
    }
    let untuned = tune_dir(measured).join(UNTUNED_FILE);
    std::fs::copy(&untuned, measured).map_err(|e| {
        format!(
            "{why}; the untuned configuration could not be restored from {}: {e}",
            untuned.display()
        )
    })?;
    tuning.applied = false;
    tuning.withheld = Some(why.to_owned());
    Ok(())
}

/// Fuehrt den Schritt aus.
///
/// Gibt den Ausgang und — wenn eine unverstellte Bewertung zustande kam —
/// das Ergebnis der Suche zurueck.
pub(crate) async fn run<S: Steps + ?Sized>(
    steps: &mut S,
    measured: &Path,
    quick: bool,
    notes: &mut Vec<String>,
) -> (Outcome, Option<Tuning>) {
    match search(steps, measured, quick).await {
        Ok(tuning) => {
            let evaluated = tuning
                .candidates
                .iter()
                .filter(|c| c.objective.is_some())
                .count();
            notes.push(format!(
                "{} settings tried besides the measured one, {evaluated} of them evaluated; {}",
                tuning.candidates.len(),
                match (tuning.improved(), tuning.applied) {
                    (true, true) => {
                        "the best one held up in confirmation and is written to the frozen \
                         configuration"
                    }
                    (true, false) => "the best one is not written (see the report)",
                    (false, _) => "none beat the measured configuration beyond the noise threshold",
                }
            ));
            (Outcome::Done, Some(tuning))
        }
        // Ohne Ergebnis bleibt keine Einstellung stehen, die der Bericht nicht
        // nennt — auch keine aus einem frueheren `tune` derselben Messung.
        Err(outcome) => match restore_untuned(measured) {
            Ok(()) => (outcome, None),
            Err(reason) => (
                Outcome::Failed {
                    reason: outcome
                        .reason()
                        .map_or_else(|| reason.clone(), |first| format!("{first}; {reason}")),
                },
                None,
            ),
        },
    }
}

/// Schreibt die unverstellte Fassung zurueck, falls es eine gibt und
/// `measured.yaml` von ihr abweicht.
///
/// # Errors
///
/// Wenn sie da ist, aber nicht gelesen oder zurueckgeschrieben werden kann.
fn restore_untuned(measured: &Path) -> Result<(), String> {
    let path = tune_dir(measured).join(UNTUNED_FILE);
    let untuned = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{} is unreadable: {e}", path.display())),
    };
    if std::fs::read_to_string(measured).is_ok_and(|current| current == untuned) {
        return Ok(());
    }
    std::fs::write(measured, &untuned).map_err(|e| {
        format!(
            "the untuned configuration could not be restored to {}: {e}",
            measured.display()
        )
    })
}

/// Bewertet eine Fassung fuer die Bestaetigung.
///
/// # Errors
///
/// Warum keine vergleichbare Zielgroesse zustande kam.
async fn confirmation_objective<S: Steps + ?Sized>(
    steps: &mut S,
    path: &Path,
    expected: &BTreeSet<(u64, String)>,
    quick: bool,
) -> Result<Objective, String> {
    match steps.evaluate(path, quick).await {
        Ok(evaluation) if evaluation.cells() == *expected => evaluation
            .objective()
            .ok_or_else(|| "the evaluation has no load point".to_owned()),
        Ok(_) => Err("not comparable: other streams or load points than the search".to_owned()),
        Err(Unevaluated::Missing) => Err("vig-fit is no longer found".to_owned()),
        Err(Unevaluated::Refused { reason }) => {
            Err(format!("refused by the configuration check: {reason}"))
        }
        Err(Unevaluated::Failed { reason }) => Err(reason),
    }
}

/// Faehrt unverstellt und eingestellt abwechselnd, [`CONFIRMATION_PAIRS`]-mal.
///
/// Abwechselnd und direkt hintereinander: Was sich an der Maschine zwischen
/// Suche und Bestaetigung aendert — Temperatur, Takt, Hintergrund —, trifft
/// beide Fassungen eines Paars gleich. Verglichen wird nur innerhalb eines
/// Paars, nie mit den Zahlen der Suche.
///
/// # Errors
///
/// Wenn eine Fassung nicht geschrieben werden kann.
async fn confirm<S: Steps + ?Sized>(
    steps: &mut S,
    dir: &Path,
    untuned_text: &str,
    tuned_text: &str,
    expected: &BTreeSet<(u64, String)>,
    quick: bool,
) -> Result<Confirmation, Outcome> {
    let mut pairs = Vec::new();
    for pair in 1..=CONFIRMATION_PAIRS {
        let mut objectives = Vec::new();
        for (label, text) in [("untuned", untuned_text), ("tuned", tuned_text)] {
            let path = dir.join(format!("confirm-{pair}-{label}.yaml"));
            std::fs::write(&path, text)
                .map_err(|e| failed(format!("{} could not be written: {e}", path.display())))?;
            objectives.push(confirmation_objective(steps, &path, expected, quick).await);
        }
        let mut objectives = objectives.into_iter();
        let untuned = objectives
            .next()
            .unwrap_or_else(|| Err("not evaluated".to_owned()));
        let tuned = objectives
            .next()
            .unwrap_or_else(|| Err("not evaluated".to_owned()));
        let (held, reason) = match (&untuned, &tuned) {
            // Dieselbe Regel wie in der Suche, mit der unverstellten Fassung
            // dieses Paars als bisher bester und als unverstellter.
            (Ok(before), Ok(after)) => match decide(*after, *before, *before) {
                Ok(why) => (true, why),
                Err(why) => (false, why),
            },
            (Err(why), _) => (false, format!("untuned not evaluated: {why}")),
            (_, Err(why)) => (false, format!("tuned not evaluated: {why}")),
        };
        println!(
            "    confirmation pair {pair}: {} — {reason}",
            if held { "held" } else { "not held" }
        );
        pairs.push(ConfirmationPair {
            pair,
            untuned: untuned.ok(),
            tuned: tuned.ok(),
            held,
            reason,
        });
    }
    Ok(Confirmation { pairs })
}

fn failed(reason: String) -> Outcome {
    Outcome::Failed { reason }
}

/// Die Suche selbst.
///
/// # Errors
///
/// Der Ausgang des Schritts, wenn es keine unverstellte Bewertung gibt oder
/// eine Datei nicht geschrieben werden kann.
#[expect(
    clippy::too_many_lines,
    reason = "Bewertung, Entscheidung und Eintrag einer Fassung gehoeren zusammen"
)]
async fn search<S: Steps + ?Sized>(
    steps: &mut S,
    measured: &Path,
    quick: bool,
) -> Result<Tuning, Outcome> {
    let dir = tune_dir(measured);
    std::fs::create_dir_all(&dir)
        .map_err(|e| failed(format!("{} could not be created: {e}", dir.display())))?;
    let measured_text = std::fs::read_to_string(measured).map_err(|e| {
        failed(format!(
            "the measured configuration {} is unreadable: {e}",
            measured.display()
        ))
    })?;
    let untuned_path = dir.join(UNTUNED_FILE);
    let untuned_text = match std::fs::read_to_string(&untuned_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&untuned_path, &measured_text).map_err(|e| {
                failed(format!(
                    "{} could not be written: {e}",
                    untuned_path.display()
                ))
            })?;
            measured_text.clone()
        }
        Err(e) => {
            return Err(failed(format!(
                "{} is unreadable: {e}",
                untuned_path.display()
            )));
        }
    };
    let untuned = Config::from_yaml(&untuned_text)
        .map_err(|e| failed(format!("the measured configuration is unreadable: {e}")))?;
    let findings = untuned.diagnose();
    if !findings.is_empty() {
        return Err(failed(format!(
            "the measured configuration does not pass the configuration check, so there is \
             nothing to tune from: {}",
            findings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }

    // Machbarkeit vor der Suche. Auf dem Pixel 2 hat die Suche eine Maschine
    // eingestellt, die die Vertraege gar nicht planen konnte (126 % geschuetzte
    // Auslastung auf zwei Slots) — und dabei nur Streuung behalten.
    let feasibility = steps.feasibility(&untuned_path).await.map_err(|e| {
        failed(format!(
            "the configuration check could not run on the measured configuration: {e}"
        ))
    })?;
    if feasibility.refuses() {
        let mut reason = format!(
            "the measured configuration is not ready on this machine (vig doctor: {}), so the \
             contracts cannot be served as configured — no governor setting can serve them; \
             tuning skipped",
            feasibility.verdict
        );
        if !feasibility.failures.is_empty() {
            let _ = write!(
                reason,
                ". vig doctor: {}",
                feasibility
                    .failures
                    .iter()
                    .map(|line| format!("FAIL {}", line.replace('|', "/")))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        return Err(Outcome::Skipped { reason });
    }

    let baseline = match steps.evaluate(&untuned_path, quick).await {
        Ok(evaluation) => evaluation,
        Err(Unevaluated::Missing) => {
            return Err(Outcome::Skipped {
                reason: "vig-fit not found; without it no setting can be evaluated, and the \
                         measured configuration stays as it is"
                    .to_owned(),
            });
        }
        Err(Unevaluated::Refused { reason }) => {
            return Err(failed(format!(
                "the untuned configuration was refused by the configuration check: {reason}"
            )));
        }
        Err(Unevaluated::Failed { reason }) => {
            return Err(failed(format!(
                "the untuned configuration could not be evaluated: {reason}"
            )));
        }
    };
    let Some(untuned_objective) = baseline.objective() else {
        return Err(failed(
            "the evaluation of the untuned configuration has no load point".to_owned(),
        ));
    };
    let expected_cells = baseline.cells();

    let mut best = untuned.clone();
    let mut best_objective = untuned_objective;
    let mut candidates = Vec::new();
    let mut not_tried = Vec::new();
    let mut number = 0_u64;
    for knob in Knob::ORDER {
        let knob_changes = match changes(knob, &best) {
            Ok(list) => list,
            Err(reason) => {
                not_tried.push(format!("`{}`: {reason}", knob.field()));
                continue;
            }
        };
        for change in knob_changes {
            number = number.saturating_add(1);
            let mut candidate = best.clone();
            change.apply(&mut candidate);
            let settings = diff(&untuned, &candidate);
            let refused = |reason: &str| {
                (
                    None,
                    Decision::Refused,
                    format!("refused by the configuration check: {reason}"),
                )
            };
            let (objective, decision, reason) = match prepare(&untuned, &candidate) {
                Err(reason) => refused(&reason),
                Ok(text) => {
                    let path = dir.join(format!("candidate-{number}.yaml"));
                    std::fs::write(&path, &text).map_err(|e| {
                        failed(format!("{} could not be written: {e}", path.display()))
                    })?;
                    match steps.evaluate(&path, quick).await {
                        Err(Unevaluated::Refused { reason }) => refused(&reason),
                        Err(Unevaluated::Missing) => (
                            None,
                            Decision::Rejected,
                            "not evaluated: vig-fit is no longer found".to_owned(),
                        ),
                        Err(Unevaluated::Failed { reason }) => {
                            (None, Decision::Rejected, format!("not evaluated: {reason}"))
                        }
                        Ok(evaluation) => match evaluation.objective() {
                            Some(objective) if evaluation.cells() == expected_cells => {
                                match decide(objective, best_objective, untuned_objective) {
                                    Ok(why) => {
                                        best = candidate;
                                        best_objective = objective;
                                        (Some(objective), Decision::Kept, why)
                                    }
                                    Err(why) => (Some(objective), Decision::Rejected, why),
                                }
                            }
                            _ => (
                                None,
                                Decision::Rejected,
                                "not comparable: the evaluation did not report the same streams \
                                 at the same load points as the untuned one"
                                    .to_owned(),
                            ),
                        },
                    }
                }
            };
            println!(
                "    #{number} {}: {}{}",
                change.describe(),
                decision.label(),
                objective.map_or_else(String::new, |o| format!(
                    " (protected worst {} ‰, lower-priority mean {} ‰)",
                    o.protected_worst, o.background_mean
                ))
            );
            candidates.push(Candidate {
                number,
                change: change.describe(),
                settings,
                objective,
                decision,
                reason,
            });
        }
    }

    let improved = candidates.iter().any(|c| c.decision == Decision::Kept);
    let mut confirmation = None;
    let mut withheld = None;
    let mut written = untuned_text.clone();
    if improved {
        let tuned_text = best.to_yaml().map_err(|e| {
            failed(format!(
                "the tuned configuration could not be written as YAML: {e}"
            ))
        })?;
        let checked = confirm(
            steps,
            &dir,
            &untuned_text,
            &tuned_text,
            &expected_cells,
            quick,
        )
        .await?;
        if checked.held() {
            written = tuned_text;
        } else {
            withheld = Some(format!(
                "did not hold up in confirmation (protected worst/lower-priority mean): {}",
                checked.summary()
            ));
        }
        confirmation = Some(checked);
    }
    // Ohne bestaetigte Verbesserung wird die unverstellte Fassung Byte fuer
    // Byte zurueckgeschrieben — auch dann, wenn ein frueheres `tune` auf
    // derselben Messung etwas behalten hatte, das dieser Lauf nicht bestaetigt.
    let applied = written != untuned_text;
    if written != measured_text {
        std::fs::write(measured, &written)
            .map_err(|e| failed(format!("{} could not be written: {e}", measured.display())))?;
    }
    Ok(Tuning {
        untuned: untuned_objective,
        tuned: best_objective,
        candidates,
        not_tried,
        applied,
        withheld,
        confirmation,
    })
}

/// Eine Zelle einer Markdown-Tabelle: kein `|`, kein Zeilenumbruch.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// Wie gesucht, entschieden und bestaetigt wurde — mit den Schwellen, die der
/// Code tatsaechlich benutzt, und nicht mit abgeschriebenen Zahlen.
fn method_markdown(out: &mut String) {
    let _ = write!(
        out,
        "\nEvery setting ran through `vig-fit` with the governor arm only, at {} % load ({} % \
         with `--quick`), and the numbers are uncovered samples from the consumer's view. \
         *Protected worst* is the worst protected stream at the worst load point; \
         *lower-priority mean* is the mean over the lower-priority streams per load point, \
         averaged over the load points. Only the governor's own settings were tried — \
         contracts, models and `backend.slots` are never changed. The search is **one pass** \
         over the settings in a fixed order (coordinate descent): a setting is not tried again \
         after a later one changed, so this is the best of the settings tried, not an optimum. \
         A setting is kept only if it beats the best one so far beyond the noise threshold — \
         the protected worst by at least {} ‰ or a tenth, whichever is larger, or, with the \
         protected streams no worse, the lower-priority mean by at least {} ‰ or a tenth. \
         Nothing worse for the protected streams than the untuned configuration is ever kept. \
         A kept setting is written only if it also holds up in back-to-back confirmation: the \
         untuned and the tuned configuration run again, interleaved, in {} pairs, and the tuned \
         one must beat that pair's untuned one by the same rule in every pair — one measuring \
         window on a saturated machine scatters more than the effects the search keeps. The \
         search does not run at all if `vig doctor` says NOT_READY for the measured \
         configuration. Each setting is kept as `tune/candidate-<n>.yaml`, the untuned one as \
         `tune/untuned.yaml`, the confirmation evaluations as \
         `tune/confirm-<pair>-untuned.json` and `-tuned.json`.\n\n",
        eval_points(false),
        eval_points(true),
        PROTECTED_MIN_GAIN_PERMILLE,
        BACKGROUND_MIN_GAIN_PERMILLE,
        CONFIRMATION_PAIRS
    );
}

/// Der Abschnitt „Tuning" des Berichts.
pub(crate) fn markdown(tuning: &Tuning, out: &mut String) {
    out.push_str("\n## Tuning\n\n");
    let tried = tuning.candidates.len().saturating_add(1);
    match (&tuning.withheld, tuning.improved()) {
        (Some(why), true) => {
            let _ = writeln!(
                out,
                "A setting did better in the search, but it was **not** written: {why}. The \
                 frozen configuration is the measured one, untuned."
            );
        }
        (_, true) => {
            let _ = write!(
                out,
                "Tuned: the protected streams miss at worst {} ‰ instead of {} ‰ with the \
                 untuned governor; the lower-priority streams {} ‰ instead of {} ‰.",
                tuning.tuned.protected_worst,
                tuning.untuned.protected_worst,
                tuning.tuned.background_mean,
                tuning.untuned.background_mean
            );
            if let Some(confirmation) = &tuning.confirmation {
                let held = confirmation.pairs.iter().filter(|p| p.held).count();
                let _ = write!(
                    out,
                    " The setting held up in {held} of {} back-to-back confirmation pairs.",
                    confirmation.pairs.len()
                );
            }
            out.push('\n');
        }
        (_, false) => {
            let _ = writeln!(
                out,
                "The measured configuration was already the best of {tried} settings tried."
            );
        }
    }
    method_markdown(out);
    out.push_str(
        "| # | Setting tried | Differs from the measured configuration | Protected worst ‰ | \
         Lower-priority mean ‰ | Decision |\n|---:|---|---|---:|---:|---|\n",
    );
    let _ = writeln!(
        out,
        "| 0 | untuned (as measured) | — | {} | {} | baseline |",
        tuning.untuned.protected_worst, tuning.untuned.background_mean
    );
    for candidate in &tuning.candidates {
        let (protected, background) = candidate.objective.map_or_else(
            || ("—".to_owned(), "—".to_owned()),
            |o| (o.protected_worst.to_string(), o.background_mean.to_string()),
        );
        let settings = if candidate.settings.is_empty() {
            "—".to_owned()
        } else {
            candidate.settings.join(", ")
        };
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {protected} | {background} | {} — {} |",
            candidate.number,
            cell(&candidate.change),
            cell(&settings),
            candidate.decision.label(),
            cell(&candidate.reason)
        );
    }
    if !tuning.not_tried.is_empty() {
        out.push('\n');
        for line in &tuning.not_tried {
            let _ = writeln!(out, "- {line}");
        }
    }
    if let Some(confirmation) = &tuning.confirmation {
        confirmation_markdown(confirmation, out);
    }
}

/// Der Unterabschnitt „Confirmation": jedes Paar mit seinen Zahlen.
///
/// Eine nicht bewertete Fassung hat keine Zahl, sondern einen Strich und den
/// Grund in der letzten Spalte.
fn confirmation_markdown(confirmation: &Confirmation, out: &mut String) {
    let _ = writeln!(
        out,
        "\n### Confirmation\n\nThe best setting of the search against the untuned one, run back \
         to back, untuned first. {}\n",
        if confirmation.held() {
            "It held up in every pair."
        } else {
            "It did **not** hold up in every pair, so it was not written."
        }
    );
    out.push_str(
        "| Pair | Untuned protected worst ‰ | Untuned lower-priority mean ‰ | Tuned protected \
         worst ‰ | Tuned lower-priority mean ‰ | Held |\n|---:|---:|---:|---:|---:|---|\n",
    );
    let number = |o: Option<Objective>, protected: bool| {
        o.map_or_else(
            || "—".to_owned(),
            |o| {
                if protected {
                    o.protected_worst
                } else {
                    o.background_mean
                }
                .to_string()
            },
        )
    };
    for pair in &confirmation.pairs {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} — {} |",
            pair.pair,
            number(pair.untuned, true),
            number(pair.untuned, false),
            number(pair.tuned, true),
            number(pair.tuned, false),
            if pair.held { "yes" } else { "no" },
            cell(&pair.reason)
        );
    }
}

fn objective_json(objective: Option<Objective>) -> serde_json::Value {
    objective.map_or(serde_json::Value::Null, |o| {
        serde_json::json!({
            "protected_worst_permille": o.protected_worst,
            "background_mean_permille": o.background_mean,
        })
    })
}

fn objective_from_json(value: &serde_json::Value) -> Option<Objective> {
    Some(Objective {
        protected_worst: value
            .get("protected_worst_permille")
            .and_then(serde_json::Value::as_u64)?,
        background_mean: value
            .get("background_mean_permille")
            .and_then(serde_json::Value::as_u64)?,
    })
}

/// Das Ergebnis als JSON, fuer Bericht und Zustand.
pub(crate) fn to_json(tuning: &Tuning) -> serde_json::Value {
    let candidates: Vec<serde_json::Value> = tuning
        .candidates
        .iter()
        .map(|c| {
            serde_json::json!({
                "number": c.number,
                "change": c.change,
                "settings": c.settings,
                "objective": objective_json(c.objective),
                "decision": c.decision.label(),
                "reason": c.reason,
            })
        })
        .collect();
    serde_json::json!({
        "search": "coordinate descent, one pass over the settings in a fixed order",
        "settings_order": Knob::ORDER.map(Knob::field),
        "noise_threshold": {
            "protected_min_gain_permille": PROTECTED_MIN_GAIN_PERMILLE,
            "background_min_gain_permille": BACKGROUND_MIN_GAIN_PERMILLE,
            "relative_divisor": RELATIVE_GAIN_DIVISOR,
        },
        "untuned": objective_json(Some(tuning.untuned)),
        "tuned": objective_json(Some(tuning.tuned)),
        "improved": tuning.improved(),
        "applied": tuning.applied,
        "withheld": tuning.withheld,
        "candidates": candidates,
        "not_tried": tuning.not_tried,
        "confirmation": tuning.confirmation.as_ref().map(|c| {
            serde_json::json!({
                "pairs_required": CONFIRMATION_PAIRS,
                "held": c.held(),
                "pairs": c.pairs.iter().map(|p| serde_json::json!({
                    "pair": p.pair,
                    "untuned": objective_json(p.untuned),
                    "tuned": objective_json(p.tuned),
                    "held": p.held,
                    "reason": p.reason,
                })).collect::<Vec<_>>(),
            })
        }),
    })
}

/// Die Bestaetigung aus dem Zustand; `None`, wo sie fehlt oder unlesbar ist.
fn confirmation_from_json(value: &serde_json::Value) -> Option<Confirmation> {
    let mut pairs = Vec::new();
    for entry in value.get("pairs")?.as_array()? {
        pairs.push(ConfirmationPair {
            pair: entry.get("pair")?.as_u64()?,
            untuned: entry.get("untuned").and_then(objective_from_json),
            tuned: entry.get("tuned").and_then(objective_from_json),
            held: entry.get("held")?.as_bool()?,
            reason: entry.get("reason")?.as_str()?.to_owned(),
        });
    }
    Some(Confirmation { pairs })
}

/// Liest das Ergebnis aus dem Zustand zurueck; `None`, wo es unvollstaendig
/// ist — dann laeuft der Schritt eben noch einmal.
pub(crate) fn from_json(value: &serde_json::Value) -> Option<Tuning> {
    let strings = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut candidates = Vec::new();
    for entry in value.get("candidates")?.as_array()? {
        candidates.push(Candidate {
            number: entry.get("number")?.as_u64()?,
            change: entry.get("change")?.as_str()?.to_owned(),
            settings: entry
                .get("settings")?
                .as_array()?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect(),
            objective: entry.get("objective").and_then(objective_from_json),
            decision: Decision::from_label(entry.get("decision")?.as_str()?)?,
            reason: entry.get("reason")?.as_str()?.to_owned(),
        });
    }
    Some(Tuning {
        untuned: objective_from_json(value.get("untuned")?)?,
        tuned: objective_from_json(value.get("tuned")?)?,
        candidates,
        not_tried: strings("not_tried"),
        applied: value.get("applied")?.as_bool()?,
        withheld: value
            .get("withheld")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        confirmation: value.get("confirmation").and_then(confirmation_from_json),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{Evaluation, LoadPoint, StreamMiss};

    /// Ein unerfuellbarer nachrangiger Strom darf keine Verbesserung verdecken.
    ///
    /// Der Laptoplauf vom 15.09.: Der 95-ms-Block stand in jeder Fassung bei
    /// 1000 ‰. Als schlechtester nachrangiger Strom gezaehlt, war eine Fassung,
    /// die `pose` von 200 auf 0 ‰ bringt, von der ungetunten nicht zu
    /// unterscheiden.
    #[test]
    fn an_unservable_lower_priority_stream_does_not_hide_an_improvement() {
        let point = |pose: u64| LoadPoint {
            load_percent: 110,
            streams: vec![
                StreamMiss {
                    stream: "detector".to_owned(),
                    protected: true,
                    governed_permille: 0,
                },
                StreamMiss {
                    stream: "pose".to_owned(),
                    protected: false,
                    governed_permille: pose,
                },
                StreamMiss {
                    stream: "vlm".to_owned(),
                    protected: false,
                    governed_permille: 1000,
                },
            ],
        };
        let untuned = Evaluation {
            points: vec![point(200)],
        }
        .objective()
        .unwrap();
        let tuned = Evaluation {
            points: vec![point(0)],
        }
        .objective()
        .unwrap();
        assert_eq!(untuned.background_mean, 600, "(200 + 1000) / 2");
        assert_eq!(tuned.background_mean, 500, "(0 + 1000) / 2");
        assert!(
            tuned.background_mean < untuned.background_mean,
            "die Verbesserung von pose muss in der Zielgroesse ankommen"
        );
    }
}
