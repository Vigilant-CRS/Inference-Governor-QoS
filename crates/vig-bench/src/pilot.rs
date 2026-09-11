//! Vig-Edge-Pilot: Aufgabenmetriken gegen Ground Truth (`docs/pilot/edge-pilot.md`).
//!
//! Die uebrigen Benchmarks messen, **ob** ein frisches Ergebnis vorlag. Dieser
//! Pilot misst, **was es fuer die Aufgabe wert war** — auf echten, Bild fuer
//! Bild annotierten Videos:
//!
//! * **Alarmlatenz.** Eine Person betritt das Bild. Wie lange dauert es, bis
//!   eine gelieferte Detektion sie zeigt? Das ist die Zusage des schnellen
//!   Alarmpfads, und sie ist unabhaengig davon, ob die Lieferfenster
//!   formal abgedeckt waren.
//! * **Lagebild-Trefferquote.** In jedem Takt von 100 ms: welcher Anteil der
//!   Personen, die **jetzt** im Bild sind, wird von der zuletzt gelieferten
//!   Detektion getroffen? Eine alte Detektion verfehlt, wer sich bewegt hat.
//!   Daneben steht die Quote, die ein Detektor ohne jede Wartezeit erreicht
//!   haette — die Obergrenze, gegen die beide Seiten gemessen werden.
//!
//! Der Detektor ist auf beiden Seiten derselbe. Was sich unterscheidet, ist
//! allein, **wann** welches Bild gerechnet wurde.
//!
//! ## Was hier bewusst fehlt
//!
//! Keine Tracker-Logik, keine Glaettung, kein Nachfuehren alter Boxen. Das
//! wuerde die Frische verdecken, die gemessen werden soll — und es waere eine
//! Eigenschaft der Anwendung, nicht des Governors.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Metriken rechnen Zeiten und Anteile in Gleitkomma; die Werte sind klein und begrenzt"
)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;
use vig_gateway::cooperative::{SAMPLING_PARAMETERS, TEXT_INPUT};
use vig_protocol_oip::inference::infer_parameter::ParameterChoice;
use vig_protocol_oip::inference::model_infer_request::InferInputTensor;
use vig_protocol_oip::inference::{InferParameter, ModelInferRequest, ModelInferResponse};
use vig_protocol_oip::params::P_AGE_US;
use vig_sim::coverage::{Coverage, CoverageTracker};

use crate::workload::try_connect;

/// Ab dieser Ueberlappung gilt eine Detektion als Treffer (Konvention der
/// MOTChallenge-Auswertung).
pub const IOU_THRESHOLD: f32 = 0.5;
/// Ab dieser Konfidenz zaehlt eine Detektion.
pub const SCORE_THRESHOLD: f32 = 0.5;
/// Annotierte Personen, die zu weniger als der Haelfte sichtbar sind, zaehlen
/// nicht: kein Detektor sieht sie zuverlaessig, und sie wuerden beide Seiten
/// gleich schlecht aussehen lassen, ohne etwas ueber Frische zu sagen.
pub const MIN_VISIBILITY: f32 = 0.5;
/// Mindesthoehe einer zaehlenden Person, relativ zur Bildhoehe (20 von 512 px).
pub const MIN_HEIGHT: f32 = 20.0 / 512.0;
/// Takt der Lagebild-Pruefung.
pub const TICK: Duration = Duration::from_millis(100);
/// Ankuenfte, die kuerzer als das vor Laufende liegen, werden nicht als
/// verpasst gezaehlt — sie hatten keine faire Gelegenheit.
pub const ARRIVAL_GRACE: Duration = Duration::from_secs(2);

/// Eine Box in normierten Koordinaten `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxN {
    /// Linke Kante.
    pub x1: f32,
    /// Obere Kante.
    pub y1: f32,
    /// Rechte Kante.
    pub x2: f32,
    /// Untere Kante.
    pub y2: f32,
}

impl BoxN {
    /// Aus Mittelpunkt, Breite und Hoehe — die Form, in der RF-DETR ausgibt.
    #[must_use]
    pub fn from_cxcywh(cx: f32, cy: f32, w: f32, h: f32) -> Self {
        Self {
            x1: cx - w / 2.0,
            y1: cy - h / 2.0,
            x2: cx + w / 2.0,
            y2: cy + h / 2.0,
        }
    }

    /// Die Flaeche; null fuer entartete Boxen.
    #[must_use]
    pub fn area(&self) -> f32 {
        (self.x2 - self.x1).max(0.0) * (self.y2 - self.y1).max(0.0)
    }

    /// Die Hoehe.
    #[must_use]
    pub fn height(&self) -> f32 {
        (self.y2 - self.y1).max(0.0)
    }

    /// Intersection over Union.
    #[must_use]
    pub fn iou(&self, other: &Self) -> f32 {
        let w = (self.x2.min(other.x2) - self.x1.max(other.x1)).max(0.0);
        let h = (self.y2.min(other.y2) - self.y1.max(other.y1)).max(0.0);
        let intersection = w * h;
        let union = self.area() + other.area() - intersection;
        if union <= f32::EPSILON {
            0.0
        } else {
            intersection / union
        }
    }
}

/// Eine annotierte Person in einem Frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GtObject {
    /// Die Track-Kennung aus der Annotation.
    pub id: u32,
    /// Wo sie ist.
    pub bbox: BoxN,
    /// Sichtbarer Anteil laut Annotation.
    pub visibility: f32,
}

impl GtObject {
    /// Ob diese Annotation als Person zaehlt ([`GtFilter::PERSONS`]).
    #[must_use]
    pub fn counts(&self) -> bool {
        GtFilter::PERSONS.keeps(self)
    }
}

/// Welche Annotationen zaehlen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GtFilter {
    /// Mindestanteil sichtbar.
    pub min_visibility: f32,
    /// Mindesthoehe relativ zur Bildhoehe.
    pub min_height: f32,
}

impl GtFilter {
    /// Personen (MOT): halb sichtbar, mindestens 20 von 512 px hoch.
    pub const PERSONS: Self = Self {
        min_visibility: MIN_VISIBILITY,
        min_height: MIN_HEIGHT,
    };
    /// Jede annotierte Box. Fuer Alarmobjekte: sie sind klein, und ein
    /// Hoehenfilter wuerde genau die Faelle entfernen, um die es geht.
    pub const ALL: Self = Self {
        min_visibility: 0.0,
        min_height: 0.0,
    };

    /// Ob eine Annotation zaehlt.
    #[must_use]
    pub fn keeps(&self, object: &GtObject) -> bool {
        object.visibility >= self.min_visibility && object.bbox.height() >= self.min_height
    }
}

/// Wann ein Track als Ankunft zaehlt, auf die ein Alarm antworten muss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrivalRule {
    /// Nicht schon im ersten Bild und mindestens eine halbe Sekunde lang
    /// sichtbar. Fuer eine durchgehende Aufnahme (MOT): wer von Anfang an
    /// dasteht, betritt das Bild nicht.
    AfterStart,
    /// Die erste Sichtbarkeit jedes Tracks, auch am Beginn eines Clips. Fuer
    /// eine Wiedergabeliste aus Clips: ein Clipwechsel ist ein Szenenwechsel,
    /// und eine Alarmobjekt, die dort schon zu sehen ist, ist trotzdem neu.
    FirstAppearance,
}

/// Die Bereiche einer Wiedergabeliste ohne Zielobjekt, aus `clips.csv`
/// (`start,frames,kind,clip`, `start` 0-basiert). Negativ ist, was
/// `kind == negative` traegt.
///
/// # Errors
///
/// Bei einer unlesbaren Zeile.
pub fn negative_ranges(clips_csv: &str) -> Result<Vec<(usize, usize)>, String> {
    let mut ranges = Vec::new();
    for (number, line) in clips_csv.lines().enumerate().skip(1) {
        let fields: Vec<&str> = line.split(',').collect();
        let (Some(start), Some(frames), Some(kind)) =
            (fields.first(), fields.get(1), fields.get(2))
        else {
            continue;
        };
        if kind.trim() != "negative" {
            continue;
        }
        let parse = |v: &str| {
            v.trim()
                .parse::<usize>()
                .map_err(|e| format!("clips.csv Zeile {number}: {e}"))
        };
        let start = parse(start)?;
        ranges.push((start, start.saturating_add(parse(frames)?)));
    }
    Ok(ranges)
}

/// Eine annotierte Sequenz — eine „Kamera".
#[derive(Debug, Clone)]
pub struct Sequence {
    /// Name, z. B. `MOT16-02`.
    pub name: String,
    /// Native Bildrate.
    pub fps: u32,
    /// Die zaehlenden Annotationen je Frame, Index = Frame (0-basiert).
    gt: Vec<Vec<GtObject>>,
    /// Das erste zaehlende Frame je Track, der im Lauf der Sequenz **ankommt**.
    arrivals: HashMap<u32, usize>,
    /// Frames, auf denen sicher kein Zielobjekt ist — die Fehlalarmseite.
    negative: Vec<bool>,
}

impl Sequence {
    /// Liest die Annotation im Format von `tools/pilot/prepare-*.sh`.
    ///
    /// `frame,id,x1,y1,x2,y2,visibility`, Frames 1-basiert, eine Kopfzeile.
    ///
    /// # Errors
    ///
    /// Bei einer unlesbaren Zeile oder einem Frame ausserhalb der Sequenz.
    pub fn from_csv(
        name: &str,
        fps: u32,
        frames: usize,
        csv: &str,
        filter: GtFilter,
        rule: ArrivalRule,
    ) -> Result<Self, String> {
        let mut gt: Vec<Vec<GtObject>> = vec![Vec::new(); frames];
        for (number, line) in csv.lines().enumerate().skip(1) {
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split(',').collect();
            let field = |i: usize| -> Result<&str, String> {
                fields
                    .get(i)
                    .copied()
                    .ok_or_else(|| format!("{name}: Zeile {number} hat zu wenige Felder"))
            };
            let float = |i: usize| -> Result<f32, String> {
                field(i)?
                    .trim()
                    .parse::<f32>()
                    .map_err(|e| format!("{name}: Zeile {number}: {e}"))
            };
            let frame: usize = field(0)?
                .trim()
                .parse()
                .map_err(|e| format!("{name}: Zeile {number}: {e}"))?;
            let id: u32 = field(1)?
                .trim()
                .parse()
                .map_err(|e| format!("{name}: Zeile {number}: {e}"))?;
            let object = GtObject {
                id,
                bbox: BoxN {
                    x1: float(2)?,
                    y1: float(3)?,
                    x2: float(4)?,
                    y2: float(5)?,
                },
                visibility: float(6)?,
            };
            let slot = frame
                .checked_sub(1)
                .and_then(|index| gt.get_mut(index))
                .ok_or_else(|| format!("{name}: Frame {frame} ausserhalb von 1..={frames}"))?;
            if filter.keeps(&object) {
                slot.push(object);
            }
        }
        Ok(Self::from_frames_with(name, fps, gt, rule))
    }

    /// Baut eine Sequenz aus fertigen, bereits gefilterten Annotationen, mit
    /// der Ankunftsregel einer durchgehenden Aufnahme.
    #[must_use]
    pub fn from_frames(name: &str, fps: u32, gt: Vec<Vec<GtObject>>) -> Self {
        Self::from_frames_with(name, fps, gt, ArrivalRule::AfterStart)
    }

    /// Baut eine Sequenz aus fertigen, bereits gefilterten Annotationen.
    ///
    /// Eine **Ankunft** ist ein Track, der insgesamt mindestens eine halbe
    /// Sekunde lang zaehlt — ein Track, der nur wenige Frames aufblitzt, ist
    /// kein Ereignis, auf das ein Alarm antworten muss. Ob ein Track, der
    /// schon im ersten Frame zaehlt, ankommt, sagt die [`ArrivalRule`].
    #[must_use]
    pub fn from_frames_with(
        name: &str,
        fps: u32,
        gt: Vec<Vec<GtObject>>,
        rule: ArrivalRule,
    ) -> Self {
        let mut first: HashMap<u32, usize> = HashMap::new();
        let mut count: HashMap<u32, usize> = HashMap::new();
        for (frame, objects) in gt.iter().enumerate() {
            for object in objects {
                first.entry(object.id).or_insert(frame);
                let c = count.entry(object.id).or_insert(0);
                *c = c.saturating_add(1);
            }
        }
        let present_at_start: HashSet<u32> = match rule {
            ArrivalRule::AfterStart => gt
                .first()
                .map(|objects| objects.iter().map(|o| o.id).collect())
                .unwrap_or_default(),
            ArrivalRule::FirstAppearance => HashSet::new(),
        };
        let min_frames = (fps as usize).checked_div(2).unwrap_or(1).max(1);
        let arrivals = first
            .into_iter()
            .filter(|(id, _)| !present_at_start.contains(id))
            .filter(|(id, _)| count.get(id).copied().unwrap_or(0) >= min_frames)
            .collect();
        let negative = vec![false; gt.len()];
        Self {
            name: name.to_owned(),
            fps: fps.max(1),
            gt,
            arrivals,
            negative,
        }
    }

    /// Markiert Bereiche `[start, end)` als sicher ohne Zielobjekt.
    #[must_use]
    pub fn with_negative(mut self, ranges: &[(usize, usize)]) -> Self {
        for &(start, end) in ranges {
            for frame in start..end {
                if let Some(flag) = self.negative.get_mut(frame) {
                    *flag = true;
                }
            }
        }
        self
    }

    /// Ob auf diesem Frame sicher kein Zielobjekt ist.
    #[must_use]
    pub fn is_negative(&self, frame: usize) -> bool {
        self.negative.get(frame).copied().unwrap_or(false)
    }

    /// Anzahl Frames.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.gt.len()
    }

    /// Die zaehlenden Annotationen eines Frames (0-basiert).
    #[must_use]
    pub fn objects(&self, frame: usize) -> &[GtObject] {
        self.gt.get(frame).map_or(&[], Vec::as_slice)
    }

    /// Die Ankuenfte: Track und erstes zaehlendes Frame.
    #[must_use]
    pub fn arrivals(&self) -> &HashMap<u32, usize> {
        &self.arrivals
    }

    /// Das laufende Bild zur Zeit `elapsed` seit Laufbeginn, als globaler
    /// Index ueber alle Wiederholungen der Sequenz.
    #[must_use]
    pub fn global_frame_at(&self, elapsed: Duration) -> u64 {
        (elapsed.as_secs_f64() * f64::from(self.fps)).floor() as u64
    }

    /// Wiederholung und Frame innerhalb der Sequenz.
    #[must_use]
    pub fn split(&self, global: u64) -> (u64, usize) {
        let frames = self.frames().max(1) as u64;
        let lap = global.checked_div(frames).unwrap_or(0);
        let frame = global.checked_rem(frames).unwrap_or(0) as usize;
        (lap, frame)
    }

    /// Wann ein globales Frame aufgenommen wurde, relativ zum Laufbeginn.
    #[must_use]
    pub fn capture_offset(&self, global: u64) -> Duration {
        Duration::from_secs_f64(global as f64 / f64::from(self.fps))
    }
}

/// Eine Detektion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    /// Wo.
    pub bbox: BoxN,
    /// Klassenindex im Ausgabetensor.
    pub class: usize,
    /// Konfidenz nach der Sigmoid.
    pub score: f32,
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Dekodiert die RF-DETR-Ausgabe: `dets` `[N,4]` als cx, cy, w, h normiert,
/// `labels` `[N,C]` als Logits.
///
/// Je Anfrage die staerkste Klasse; was unter `threshold` liegt, faellt weg.
/// Der letzte Klassenindex kann das No-Object-Signal sein — ob, entscheidet
/// der Aufrufer ueber die Klasse, die er auswertet.
#[must_use]
pub fn decode_rfdetr(
    dets: &[f32],
    logits: &[f32],
    classes: usize,
    threshold: f32,
) -> Vec<Detection> {
    if classes == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (boxes, row) in dets
        .as_chunks::<4>()
        .0
        .iter()
        .zip(logits.chunks_exact(classes))
    {
        let mut best = (0_usize, f32::NEG_INFINITY);
        for (class, &logit) in row.iter().enumerate() {
            if logit > best.1 {
                best = (class, logit);
            }
        }
        let score = sigmoid(best.1);
        if score < threshold {
            continue;
        }
        let [cx, cy, w, h] = *boxes;
        out.push(Detection {
            bbox: BoxN::from_cxcywh(cx, cy, w, h),
            class: best.0,
            score,
        });
    }
    out
}

/// Ordnet Detektionen den Annotationen zu, gierig nach groesster Ueberlappung.
///
/// Jede Detektion trifft hoechstens eine Person und umgekehrt. Ergebnis: je
/// Annotation, ob sie getroffen wurde.
#[must_use]
pub fn match_boxes(truth: &[BoxN], detections: &[BoxN], threshold: f32) -> Vec<bool> {
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
    for (t, gt) in truth.iter().enumerate() {
        for (d, det) in detections.iter().enumerate() {
            let iou = gt.iou(det);
            if iou >= threshold {
                pairs.push((iou, t, d));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut matched = vec![false; truth.len()];
    let mut used = vec![false; detections.len()];
    for (_, t, d) in pairs {
        let free_truth = matched.get(t).is_some_and(|m| !m);
        let free_det = used.get(d).is_some_and(|u| !u);
        if free_truth && free_det {
            if let Some(m) = matched.get_mut(t) {
                *m = true;
            }
            if let Some(u) = used.get_mut(d) {
                *u = true;
            }
        }
    }
    matched
}

/// Das Perzentil einer Reihe; null fuer eine leere.
#[must_use]
pub fn percentile(values: &[u64], percent: u32) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len().saturating_sub(1))
        .saturating_mul(percent as usize)
        .checked_div(100)
        .unwrap_or(0);
    sorted.get(index).copied().unwrap_or(0)
}

/// Die Alarmlatenzen einer Kamera.
#[derive(Debug, Clone)]
pub struct AlarmBook {
    sequence: Arc<Sequence>,
    /// Ab dieser Ueberlappung trifft eine Detektion.
    threshold: f32,
    /// Die erste Alarmlatenz je (Wiederholung, Track).
    alarmed: HashMap<(u64, u32), Duration>,
}

/// Die Zusammenfassung der Alarmlatenzen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlarmSummary {
    /// Ankuenfte mit fairer Gelegenheit.
    pub events: u64,
    /// Davon nie alarmiert.
    pub missed: u64,
    /// Latenzen der alarmierten, in Millisekunden.
    pub latencies_ms: Vec<u64>,
}

impl AlarmSummary {
    /// Median in Millisekunden.
    #[must_use]
    pub fn p50(&self) -> u64 {
        percentile(&self.latencies_ms, 50)
    }
    /// p95 in Millisekunden.
    #[must_use]
    pub fn p95(&self) -> u64 {
        percentile(&self.latencies_ms, 95)
    }
    /// Groesste Latenz.
    #[must_use]
    pub fn max(&self) -> u64 {
        self.latencies_ms.iter().copied().max().unwrap_or(0)
    }
    /// Anteil nie alarmierter Ankuenfte in Promille.
    #[must_use]
    pub fn missed_permille(&self) -> u64 {
        self.missed
            .saturating_mul(1_000)
            .checked_div(self.events)
            .unwrap_or(0)
    }
}

impl AlarmBook {
    /// Ein leeres Buch fuer eine Sequenz, mit der IoU-Schwelle eines Treffers.
    #[must_use]
    pub fn new(sequence: Arc<Sequence>, threshold: f32) -> Self {
        Self {
            sequence,
            threshold,
            alarmed: HashMap::new(),
        }
    }

    /// Nimmt eine gelieferte Detektion auf: gerechnet auf dem globalen Frame
    /// `global`, geliefert `delivered_at` nach Laufbeginn.
    ///
    /// Gezaehlt wird gegen die Annotation **des gerechneten Frames**: eine
    /// Detektion alarmiert, wenn sie die Person in dem Bild findet, auf dem
    /// sie gerechnet wurde. Wann dieses Bild aufgenommen wurde, entscheidet
    /// darueber nicht — die Latenz zaehlt ab der Ankunft der Person.
    pub fn on_delivery(&mut self, global: u64, delivered_at: Duration, detections: &[BoxN]) {
        let (lap, frame) = self.sequence.split(global);
        let objects = self.sequence.objects(frame);
        let truth: Vec<BoxN> = objects.iter().map(|o| o.bbox).collect();
        let hits = match_boxes(&truth, detections, self.threshold);
        let frames = self.sequence.frames() as u64;
        for (object, hit) in objects.iter().zip(hits) {
            if !hit {
                continue;
            }
            let Some(&arrival) = self.sequence.arrivals().get(&object.id) else {
                continue;
            };
            if frame < arrival {
                continue;
            }
            let arrived = self
                .sequence
                .capture_offset(lap.saturating_mul(frames).saturating_add(arrival as u64));
            self.alarmed
                .entry((lap, object.id))
                .or_insert_with(|| delivered_at.saturating_sub(arrived));
        }
    }

    /// Die Zusammenfassung ueber einen Lauf der Laenge `duration`.
    #[must_use]
    pub fn finish(&self, duration: Duration) -> AlarmSummary {
        let frames = self.sequence.frames().max(1) as u64;
        let laps = self
            .sequence
            .global_frame_at(duration)
            .checked_div(frames)
            .unwrap_or(0);
        let deadline = duration.saturating_sub(ARRIVAL_GRACE);
        let mut summary = AlarmSummary::default();
        for lap in 0..=laps {
            for (&id, &arrival) in self.sequence.arrivals() {
                let arrived = self
                    .sequence
                    .capture_offset(lap.saturating_mul(frames).saturating_add(arrival as u64));
                if arrived > deadline {
                    continue;
                }
                summary.events = summary.events.saturating_add(1);
                match self.alarmed.get(&(lap, id)) {
                    Some(latency) => summary
                        .latencies_ms
                        .push(u64::try_from(latency.as_millis()).unwrap_or(u64::MAX)),
                    None => summary.missed = summary.missed.saturating_add(1),
                }
            }
        }
        summary
    }
}

/// Die Lagebild-Trefferquote einer Kamera.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickRecall {
    /// Zaehlende Personen ueber alle Takte.
    pub total: u64,
    /// Davon von der zuletzt gelieferten Detektion getroffen.
    pub hits: u64,
    /// Davon von einer Detektion **des laufenden Bildes** getroffen — die
    /// Obergrenze ohne jede Wartezeit.
    pub ideal: u64,
}

impl TickRecall {
    /// Nimmt einen Takt auf.
    pub fn tick(
        &mut self,
        truth: &[BoxN],
        latest: Option<&[BoxN]>,
        ideal: Option<&[BoxN]>,
        threshold: f32,
    ) {
        let n = truth.len() as u64;
        self.total = self.total.saturating_add(n);
        let count = |dets: Option<&[BoxN]>| {
            dets.map_or(0, |d| {
                match_boxes(truth, d, threshold)
                    .into_iter()
                    .filter(|&m| m)
                    .count() as u64
            })
        };
        self.hits = self.hits.saturating_add(count(latest));
        self.ideal = self.ideal.saturating_add(count(ideal));
    }

    /// Trefferquote in Promille.
    #[must_use]
    pub fn permille(&self) -> u64 {
        self.hits
            .saturating_mul(1_000)
            .checked_div(self.total)
            .unwrap_or(0)
    }

    /// Ideale Trefferquote in Promille.
    #[must_use]
    pub fn ideal_permille(&self) -> u64 {
        self.ideal
            .saturating_mul(1_000)
            .checked_div(self.total)
            .unwrap_or(0)
    }
}

/// Die Detektionen je Frame einer Sequenz, ohne jede Wartezeit gerechnet.
///
/// Entsteht in einem Vorlauf, in dem jedes Bild einzeln und ohne Konkurrenz
/// gerechnet wird. Gegen sie misst die Trefferquote, was ueberhaupt
/// erreichbar war.
pub type IdealTable = Vec<Vec<BoxN>>;

/// Wandelt ein RGB24-Bild (HWC, `u8`) in den RF-DETR-Eingang: FP32, CHW,
/// normiert mit den ImageNet-Konstanten, little-endian.
#[must_use]
pub fn frame_tensor(rgb: &[u8], width: usize, height: usize) -> Vec<u8> {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let plane = width.saturating_mul(height);
    let mut out = vec![0_u8; plane.saturating_mul(3).saturating_mul(4)];
    for (pixel, rgb) in rgb.as_chunks::<3>().0.iter().take(plane).enumerate() {
        for (channel, &value) in rgb.iter().enumerate() {
            let (Some(&mean), Some(&std)) = (MEAN.get(channel), STD.get(channel)) else {
                continue;
            };
            let normed = (f32::from(value) / 255.0 - mean) / std;
            let offset = channel
                .saturating_mul(plane)
                .saturating_add(pixel)
                .saturating_mul(4);
            if let Some(target) = out.get_mut(offset..offset.saturating_add(4)) {
                target.copy_from_slice(&normed.to_le_bytes());
            }
        }
    }
    out
}

/// Liest einen FP32-Ausgabetensor aus einer Antwort.
#[must_use]
pub fn f32_output(response: &ModelInferResponse, name: &str) -> Option<Vec<f32>> {
    let index = response.outputs.iter().position(|o| o.name == name)?;
    let raw = response.raw_output_contents.get(index)?;
    Some(
        raw.as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect(),
    )
}

/// Eine Kamera im Pilotlauf.
#[derive(Debug, Clone)]
pub struct CameraDef {
    /// Anzeigename.
    pub name: String,
    /// Der Modellname, den der Client anfragt.
    pub model: String,
    /// Die Annotation.
    pub sequence: Arc<Sequence>,
    /// Alle Frames als RGB24, hintereinander.
    pub frames: Arc<Vec<u8>>,
    /// Kantenlaenge der Frames.
    pub size: usize,
    /// Wie oft die Kamera sendet.
    pub rate_hz: f64,
    /// Registrierte Shared-Memory-Regionen, eine je gleichzeitig offenem
    /// Auftrag. Leer heisst Kopierpfad.
    pub regions: Vec<(String, std::path::PathBuf)>,
    /// Name, Datentyp und Form des Eingangs.
    pub input: (String, String, Vec<i64>),
    /// Die Klassenindizes, die als Zielobjekt zaehlen — Person, oder
    /// Alarmobjekt/Klasse A/Klasse B.
    pub target_classes: Vec<usize>,
    /// Ab dieser Ueberlappung trifft eine Detektion.
    pub iou_threshold: f32,
    /// Anzahl Klassen im Logit-Tensor.
    pub classes: usize,
    /// Die Detektionen ohne Wartezeit, falls ein Vorlauf sie erzeugt hat.
    pub ideal: Option<Arc<IdealTable>>,
}

/// Der Sprachmodellpfad — der langsame, semantische Lagebericht.
#[derive(Debug, Clone)]
pub struct LlmDef {
    /// Wohin: direkt zum vLLM-Triton oder zum Gateway.
    pub endpoint: String,
    /// Der Modellname.
    pub model: String,
    /// Wie viele Token ein Bericht haben darf.
    pub max_tokens: u32,
    /// Mindestabstand zwischen zwei Berichten.
    pub min_interval: Duration,
}

/// Was ein Arm faehrt.
#[derive(Debug, Clone)]
pub struct ArmConfig {
    /// Wohin die Kameras senden.
    pub endpoint: String,
    /// Ob der Client die Governor-Parameter mitschickt.
    pub via_governor: bool,
    /// Die Kameras.
    pub cameras: Vec<CameraDef>,
    /// Hoechstens so viele offene Auftraege je Kamera.
    pub in_flight_cap: usize,
    /// Laufdauer.
    pub duration: Duration,
    /// Der Berichtspfad, falls einer faehrt.
    pub llm: Option<LlmDef>,
}

/// Das Ergebnis einer Kamera.
#[derive(Debug, Clone, Default)]
pub struct CameraReport {
    /// Anzeigename.
    pub name: String,
    /// Alarmlatenzen.
    pub alarm: AlarmSummary,
    /// Lagebild-Trefferquote.
    pub recall: TickRecall,
    /// Klassische Abdeckung und Age of Information.
    pub coverage: Coverage,
    /// Gesendete Auftraege.
    pub sent: u64,
    /// Beantwortete Auftraege.
    pub delivered: u64,
    /// Abgewiesene oder fehlgeschlagene Auftraege.
    pub rejected: u64,
    /// Gelieferte Detektionen auf Frames ohne Zielobjekt.
    pub negative_deliveries: u64,
    /// Davon mit mindestens einem Zielobjekt — Fehlalarme.
    pub false_alarms: u64,
}

/// Das Ergebnis des Berichtspfads.
#[derive(Debug, Clone, Default)]
pub struct LlmReport {
    /// Fertige Berichte.
    pub reports: u64,
    /// Fehlgeschlagene Berichte.
    pub failed: u64,
    /// Dauer je Bericht, in Millisekunden.
    pub latencies_ms: Vec<u64>,
    /// Alter der juengsten Detektion, auf der ein Bericht beruhte, bei
    /// seiner Fertigstellung, in Millisekunden.
    pub basis_age_ms: Vec<u64>,
}

/// Das Ergebnis eines Arms.
#[derive(Debug, Clone, Default)]
pub struct ArmReport {
    /// Je Kamera.
    pub cameras: Vec<CameraReport>,
    /// Der Berichtspfad.
    pub llm: LlmReport,
}

/// Was der Client von einer Kamera zuletzt weiss.
#[derive(Debug, Default)]
struct CameraState {
    /// Das neueste gerechnete Frame und seine Personen-Detektionen.
    latest: Option<(u64, Vec<BoxN>)>,
    /// Wann das neueste gerechnete Frame aufgenommen wurde.
    latest_capture: Option<Instant>,
    alarm: Option<AlarmBook>,
    coverage: Option<CoverageTracker>,
    sent: u64,
    delivered: u64,
    rejected: u64,
    negative_deliveries: u64,
    false_alarms: u64,
}

fn to_core(d: Duration) -> vig_core::Duration {
    vig_core::Duration::from_nanos_unbounded(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

fn core_instant(d: Duration) -> vig_core::Instant {
    vig_core::Instant::from_nanos(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Baut den Detektorauftrag fuer ein Frame.
fn detector_request(
    camera: &CameraDef,
    id: &str,
    slot: Option<&str>,
    payload: Option<Vec<u8>>,
    age: Duration,
    via_governor: bool,
) -> ModelInferRequest {
    let (name, datatype, shape) = camera.input.clone();
    let byte_size = i64::try_from(camera.size.saturating_mul(camera.size).saturating_mul(12))
        .unwrap_or(i64::MAX);
    let mut tensor_params = std::collections::HashMap::new();
    if let Some(region) = slot {
        tensor_params.insert(
            "shared_memory_region".to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::StringParam(region.to_owned())),
            },
        );
        tensor_params.insert(
            "shared_memory_byte_size".to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::Int64Param(byte_size)),
            },
        );
    }
    let mut parameters = std::collections::HashMap::new();
    if via_governor {
        parameters.insert(
            P_AGE_US.to_owned(),
            InferParameter {
                parameter_choice: Some(ParameterChoice::Int64Param(
                    i64::try_from(age.as_micros()).unwrap_or(i64::MAX),
                )),
            },
        );
    }
    ModelInferRequest {
        model_name: camera.model.clone(),
        model_version: String::new(),
        id: id.to_owned(),
        parameters,
        inputs: vec![InferInputTensor {
            name,
            datatype,
            shape,
            parameters: tensor_params,
            contents: None,
        }],
        outputs: Vec::new(),
        raw_input_contents: payload.into_iter().collect(),
    }
}

/// Der Textauftrag fuer einen Lagebericht, im Format des vLLM-Backends.
#[must_use]
pub fn report_request(model: &str, prompt: &str, max_tokens: u32) -> ModelInferRequest {
    let params = format!("{{\"max_tokens\": {max_tokens}, \"temperature\": 0.0}}");
    let prefixed = |value: &str| {
        let bytes = value.as_bytes();
        let mut out = Vec::with_capacity(bytes.len().saturating_add(4));
        out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(bytes);
        out
    };
    let tensor = |name: &str| InferInputTensor {
        name: name.to_owned(),
        datatype: "BYTES".to_owned(),
        shape: vec![1],
        parameters: std::collections::HashMap::new(),
        contents: None,
    };
    ModelInferRequest {
        model_name: model.to_owned(),
        model_version: String::new(),
        id: "lagebericht".to_owned(),
        parameters: std::collections::HashMap::new(),
        inputs: vec![tensor(TEXT_INPUT), tensor(SAMPLING_PARAMETERS)],
        outputs: Vec::new(),
        raw_input_contents: vec![prefixed(prompt), prefixed(&params)],
    }
}

/// Der Prompt aus dem, was die Kameras zuletzt geliefert haben.
#[must_use]
pub fn report_prompt(counts: &[(String, Option<usize>)]) -> String {
    use std::fmt::Write as _;
    let mut prompt =
        String::from("Lagebericht fuer die Einsatzleitung. Aktuelle Detektionen je Kamera:\n");
    for (name, count) in counts {
        let _ = match count {
            Some(n) => writeln!(prompt, "- {name}: {n} Zielobjekte erkannt"),
            None => writeln!(prompt, "- {name}: keine aktuelle Detektion"),
        };
    }
    prompt.push_str("Fasse die Lage in zwei Saetzen zusammen.");
    prompt
}

/// Faehrt einen Arm: alle Kameras und den Berichtspfad gleichzeitig.
///
/// Beide Seiten senden dieselben Bilder zu denselben Zeitpunkten: welches
/// Bild eine Kamera sendet, bestimmt allein die Uhr seit Laufbeginn.
///
/// # Errors
///
/// Wenn keine Verbindung zum Ziel aufgebaut werden kann.
#[expect(
    clippy::too_many_lines,
    reason = "vier eng verzahnte Nebenlaeufe — Kameras, Takt, Bericht, Zusammenfassung — \
              teilen sich Zustand und Ursprung; zerteilt waere der Ablauf schwerer zu pruefen"
)]
pub async fn run_arm(config: ArmConfig) -> Result<ArmReport, String> {
    let origin = Instant::now();
    let states: Vec<Arc<Mutex<CameraState>>> = config
        .cameras
        .iter()
        .map(|camera| {
            let period = Duration::from_secs_f64(1.0 / camera.rate_hz.max(0.1));
            Arc::new(Mutex::new(CameraState {
                alarm: Some(AlarmBook::new(
                    Arc::clone(&camera.sequence),
                    camera.iou_threshold,
                )),
                coverage: Some(CoverageTracker::new(
                    to_core(period),
                    to_core(period.saturating_mul(2)),
                    vig_core::Instant::ZERO,
                    to_core(config.duration),
                )),
                ..CameraState::default()
            }))
        })
        .collect();
    let recalls: Vec<Arc<Mutex<TickRecall>>> = config
        .cameras
        .iter()
        .map(|_| Arc::new(Mutex::new(TickRecall::default())))
        .collect();

    let mut tasks = Vec::new();
    for (camera, state) in config.cameras.iter().cloned().zip(states.iter().cloned()) {
        let client = try_connect(&config.endpoint)
            .await
            .map_err(|e| format!("{}: {e}", config.endpoint))?;
        let permits = Arc::new(Semaphore::new(config.in_flight_cap.max(1)));
        let duration = config.duration;
        let via_governor = config.via_governor;
        tasks.push(tokio::spawn(async move {
            let period = Duration::from_secs_f64(1.0 / camera.rate_hz.max(0.1));
            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut sequence_number = 0_u64;
            let frame_bytes = camera.size.saturating_mul(camera.size).saturating_mul(3);
            loop {
                ticker.tick().await;
                let capture = Instant::now();
                let elapsed = capture.duration_since(origin);
                if elapsed >= duration {
                    break;
                }
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    continue;
                };
                let global = camera.sequence.global_frame_at(elapsed);
                let (_, frame) = camera.sequence.split(global);
                let start = frame.saturating_mul(frame_bytes);
                let Some(rgb) = camera.frames.get(start..start.saturating_add(frame_bytes)) else {
                    continue;
                };
                let tensor = frame_tensor(rgb, camera.size, camera.size);
                let slot_index = usize::try_from(sequence_number)
                    .unwrap_or(0)
                    .checked_rem(camera.regions.len().max(1))
                    .unwrap_or(0);
                sequence_number = sequence_number.saturating_add(1);
                let (slot, payload) = match camera.regions.get(slot_index) {
                    Some((name, path)) => {
                        if std::fs::write(path, &tensor).is_err() {
                            continue;
                        }
                        (Some(name.clone()), None)
                    }
                    None => (None, Some(tensor)),
                };
                if let Ok(mut s) = state.lock() {
                    s.sent = s.sent.saturating_add(1);
                }
                let mut client = client.clone();
                let camera = camera.clone();
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let _permit = permit;
                    let id = format!("{}:{global}", camera.name);
                    let request = detector_request(
                        &camera,
                        &id,
                        slot.as_deref(),
                        payload,
                        capture.elapsed(),
                        via_governor,
                    );
                    let result = client.model_infer(request).await;
                    let delivered = Instant::now().duration_since(origin);
                    let Ok(mut s) = state.lock() else { return };
                    let Ok(response) = result.map(tonic::Response::into_inner) else {
                        s.rejected = s.rejected.saturating_add(1);
                        return;
                    };
                    s.delivered = s.delivered.saturating_add(1);
                    let persons: Vec<BoxN> = match (
                        f32_output(&response, "dets"),
                        f32_output(&response, "labels"),
                    ) {
                        (Some(dets), Some(labels)) => {
                            decode_rfdetr(&dets, &labels, camera.classes, SCORE_THRESHOLD)
                                .into_iter()
                                .filter(|d| camera.target_classes.contains(&d.class))
                                .map(|d| d.bbox)
                                .collect()
                        }
                        _ => Vec::new(),
                    };
                    let (_, computed) = camera.sequence.split(global);
                    if camera.sequence.is_negative(computed) {
                        s.negative_deliveries = s.negative_deliveries.saturating_add(1);
                        if !persons.is_empty() {
                            s.false_alarms = s.false_alarms.saturating_add(1);
                        }
                    }
                    if let Some(book) = s.alarm.as_mut() {
                        book.on_delivery(global, delivered, &persons);
                    }
                    if let Some(tracker) = s.coverage.as_mut() {
                        tracker.record_delivery(core_instant(delivered), core_instant(elapsed));
                    }
                    let newer = s.latest.as_ref().is_none_or(|(g, _)| global > *g);
                    if newer {
                        s.latest = Some((global, persons));
                        s.latest_capture = Some(capture);
                    }
                });
            }
        }));
    }

    // Die Lagebild-Pruefung: im festen Takt, fuer alle Kameras.
    let checker = {
        let cameras = config.cameras.clone();
        let states = states.clone();
        let recalls = recalls.clone();
        let duration = config.duration;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let elapsed = Instant::now().duration_since(origin);
                if elapsed >= duration {
                    break;
                }
                for ((camera, state), recall) in cameras.iter().zip(&states).zip(&recalls) {
                    let (_, frame) = camera
                        .sequence
                        .split(camera.sequence.global_frame_at(elapsed));
                    let truth: Vec<BoxN> = camera
                        .sequence
                        .objects(frame)
                        .iter()
                        .map(|o| o.bbox)
                        .collect();
                    let latest = state.lock().ok().and_then(|s| s.latest.clone());
                    let ideal = camera.ideal.as_ref().and_then(|t| t.get(frame).cloned());
                    if let Ok(mut r) = recall.lock() {
                        r.tick(
                            &truth,
                            latest.as_ref().map(|(_, d)| d.as_slice()),
                            ideal.as_deref(),
                            camera.iou_threshold,
                        );
                    }
                }
            }
        })
    };

    // Der Berichtspfad: ein Bericht nach dem anderen, mit Mindestabstand.
    let reporter = config.llm.clone().map(|llm| {
        let cameras = config.cameras.clone();
        let states = states.clone();
        let duration = config.duration;
        let via_governor = config.via_governor;
        tokio::spawn(async move {
            let mut report = LlmReport::default();
            let direct = vig_backend_triton::TritonClient::new(llm.endpoint.clone());
            let governed = try_connect(&llm.endpoint).await.ok();
            while Instant::now().duration_since(origin) < duration {
                let started = Instant::now();
                let mut counts = Vec::new();
                let mut basis: Option<Instant> = None;
                for (camera, state) in cameras.iter().zip(&states) {
                    let snapshot = state
                        .lock()
                        .ok()
                        .map(|s| (s.latest.as_ref().map(|(_, d)| d.len()), s.latest_capture));
                    let (count, capture) = snapshot.unwrap_or((None, None));
                    counts.push((camera.name.clone(), count));
                    basis = match (basis, capture) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (a, b) => a.or(b),
                    };
                }
                let request = report_request(&llm.model, &report_prompt(&counts), llm.max_tokens);
                let ok = if via_governor {
                    match governed.clone() {
                        Some(mut client) => client.model_infer(request).await.is_ok(),
                        None => false,
                    }
                } else {
                    direct.infer_decoupled(request).await.is_ok()
                };
                if ok {
                    report.reports = report.reports.saturating_add(1);
                    report
                        .latencies_ms
                        .push(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
                    if let Some(capture) = basis {
                        report
                            .basis_age_ms
                            .push(u64::try_from(capture.elapsed().as_millis()).unwrap_or(u64::MAX));
                    }
                } else {
                    report.failed = report.failed.saturating_add(1);
                }
                let wait = llm.min_interval.saturating_sub(started.elapsed());
                if !ok {
                    tokio::time::sleep(Duration::from_millis(200).max(wait)).await;
                } else if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
            }
            report
        })
    });

    for task in tasks {
        let _ = task.await;
    }
    // Offene Auftraege duerfen noch ankommen; was dann fehlt, fehlt.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = checker.await;
    let llm = match reporter {
        Some(task) => task.await.unwrap_or_default(),
        None => LlmReport::default(),
    };

    let mut report = ArmReport {
        cameras: Vec::new(),
        llm,
    };
    for ((camera, state), recall) in config.cameras.iter().zip(&states).zip(&recalls) {
        let Ok(s) = state.lock() else { continue };
        report.cameras.push(CameraReport {
            name: camera.name.clone(),
            alarm: s
                .alarm
                .as_ref()
                .map(|b| b.finish(config.duration))
                .unwrap_or_default(),
            recall: recall.lock().map(|r| *r).unwrap_or_default(),
            coverage: s
                .coverage
                .as_ref()
                .map_or_else(Coverage::default, CoverageTracker::finish),
            sent: s.sent,
            delivered: s.delivered,
            rejected: s.rejected,
            negative_deliveries: s.negative_deliveries,
            false_alarms: s.false_alarms,
        });
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::float_cmp)]

    use super::*;

    fn b(x1: f32, y1: f32, x2: f32, y2: f32) -> BoxN {
        BoxN { x1, y1, x2, y2 }
    }

    fn person(id: u32, bbox: BoxN) -> GtObject {
        GtObject {
            id,
            bbox,
            visibility: 1.0,
        }
    }

    #[test]
    fn iou_of_identical_disjoint_and_half_overlapping_boxes() {
        let a = b(0.0, 0.0, 0.2, 0.2);
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
        assert!(a.iou(&b(0.5, 0.5, 0.6, 0.6)).abs() < 1e-6);
        // Halb verschoben: Schnitt 0,02, Vereinigung 0,06.
        let shifted = b(0.1, 0.0, 0.3, 0.2);
        assert!((a.iou(&shifted) - 1.0 / 3.0).abs() < 1e-5);
    }

    #[test]
    fn a_degenerate_box_overlaps_nothing() {
        let line = b(0.1, 0.1, 0.1, 0.5);
        assert!(line.iou(&line).abs() < 1e-6);
    }

    #[test]
    fn cxcywh_is_converted_to_corners() {
        let r = BoxN::from_cxcywh(0.5, 0.5, 0.2, 0.4);
        assert!((r.x1 - 0.4).abs() < 1e-6 && (r.y2 - 0.7).abs() < 1e-6);
    }

    /// Jede Detektion trifft hoechstens eine Person — auch wenn sie zwei
    /// ueberlappende Personen gleich gut abdeckt.
    #[test]
    fn matching_is_one_to_one_and_prefers_the_best_overlap() {
        let truth = [b(0.0, 0.0, 0.2, 0.4), b(0.05, 0.0, 0.25, 0.4)];
        let one = [b(0.05, 0.0, 0.25, 0.4)];
        let hits = match_boxes(&truth, &one, 0.5);
        assert_eq!(hits, vec![false, true], "die bessere Ueberlappung gewinnt");
        let two = [b(0.05, 0.0, 0.25, 0.4), b(0.0, 0.0, 0.2, 0.4)];
        assert_eq!(match_boxes(&truth, &two, 0.5), vec![true, true]);
    }

    #[test]
    fn rfdetr_output_is_decoded_to_the_strongest_class_above_threshold() {
        // Zwei Anfragen, drei Klassen. Die erste ist sicher Klasse 0, die
        // zweite unter der Schwelle.
        let dets = [0.5, 0.5, 0.2, 0.4, 0.1, 0.1, 0.1, 0.1];
        let logits = [4.0, -2.0, -3.0, -5.0, -4.0, -6.0];
        let out = decode_rfdetr(&dets, &logits, 3, 0.5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].class, 0);
        assert!(out[0].score > 0.98);
        assert!((out[0].bbox.x1 - 0.4).abs() < 1e-6);
    }

    #[test]
    fn the_csv_is_read_and_filtered() {
        let csv = "frame,id,x1,y1,x2,y2,visibility\n\
                   1,1,0.1,0.1,0.2,0.4,1.0\n\
                   1,2,0.5,0.5,0.51,0.51,1.0\n\
                   2,3,0.3,0.3,0.4,0.6,0.2\n";
        let seq = Sequence::from_csv("s", 10, 3, csv, GtFilter::PERSONS, ArrivalRule::AfterStart)
            .unwrap();
        assert_eq!(seq.objects(0).len(), 1, "zu klein faellt heraus");
        assert!(seq.objects(1).is_empty(), "zu wenig sichtbar faellt heraus");
        let all =
            Sequence::from_csv("s", 10, 3, csv, GtFilter::ALL, ArrivalRule::AfterStart).unwrap();
        assert_eq!(all.objects(0).len(), 2, "ohne Filter zaehlt jede Box");
        assert!(
            Sequence::from_csv(
                "s",
                10,
                1,
                "h\n2,1,0,0,1,1,1\n",
                GtFilter::ALL,
                ArrivalRule::AfterStart
            )
            .is_err()
        );
    }

    /// In einer Wiedergabeliste ist eine Alarmobjekt, die beim Clipwechsel schon
    /// sichtbar ist, trotzdem ein neues Ereignis.
    #[test]
    fn first_appearance_counts_objects_visible_from_the_start() {
        let gun = b(0.4, 0.4, 0.45, 0.45);
        let gt = vec![vec![person(21, gun)]; 10];
        let after = Sequence::from_frames_with("s", 10, gt.clone(), ArrivalRule::AfterStart);
        let first = Sequence::from_frames_with("s", 10, gt, ArrivalRule::FirstAppearance);
        assert!(after.arrivals().is_empty());
        assert_eq!(first.arrivals().get(&21), Some(&0));
    }

    #[test]
    fn negative_ranges_come_from_the_no_gun_clips() {
        let clips = "start,frames,kind,clip\n\
                     0,150,klasse_a,A\n\
                     150,175,negative,B\n\
                     325,200,Machine_Gun,C\n";
        assert_eq!(negative_ranges(clips).unwrap(), vec![(150, 325)]);
        let seq = Sequence::from_frames("s", 25, vec![Vec::new(); 400])
            .with_negative(&negative_ranges(clips).unwrap());
        assert!(!seq.is_negative(149));
        assert!(seq.is_negative(150) && seq.is_negative(324));
        assert!(!seq.is_negative(325));
        assert!(!seq.is_negative(10_000), "ausserhalb ist nichts negativ");
    }

    /// Wer von Anfang an im Bild steht, kommt nicht an; wer nur aufblitzt,
    /// auch nicht.
    #[test]
    fn arrivals_exclude_the_initial_scene_and_flickers() {
        let standing = b(0.0, 0.0, 0.1, 0.3);
        let walking = b(0.5, 0.0, 0.6, 0.3);
        let mut gt = vec![vec![person(1, standing)]; 20];
        for frame in gt.iter_mut().skip(5) {
            frame.push(person(2, walking));
        }
        gt[3].push(person(3, walking));
        let seq = Sequence::from_frames("s", 10, gt);
        assert_eq!(seq.arrivals().get(&2), Some(&5));
        assert!(!seq.arrivals().contains_key(&1));
        assert!(
            !seq.arrivals().contains_key(&3),
            "ein Frame ist kein Ereignis"
        );
    }

    fn arriving_sequence() -> Arc<Sequence> {
        // 10 fps, 50 Frames. Person 7 kommt in Frame 20 an (2,0 s).
        let target = b(0.4, 0.2, 0.5, 0.6);
        let mut gt = vec![Vec::new(); 50];
        for frame in gt.iter_mut().skip(20) {
            frame.push(person(7, target));
        }
        Arc::new(Sequence::from_frames("s", 10, gt))
    }

    #[test]
    fn alarm_latency_counts_from_the_arrival_not_from_the_computed_frame() {
        let seq = arriving_sequence();
        let mut book = AlarmBook::new(Arc::clone(&seq), IOU_THRESHOLD);
        let target = [b(0.4, 0.2, 0.5, 0.6)];
        // Ein frueheres Bild ohne die Person alarmiert nicht.
        book.on_delivery(10, Duration::from_millis(1_050), &target);
        // Frame 23 zeigt sie, geliefert bei 2,45 s: 450 ms nach der Ankunft.
        book.on_delivery(23, Duration::from_millis(2_450), &target);
        book.on_delivery(25, Duration::from_millis(2_600), &target);
        let summary = book.finish(Duration::from_secs(5));
        assert_eq!(summary.events, 1);
        assert_eq!(summary.missed, 0);
        assert_eq!(summary.latencies_ms, vec![450]);
    }

    #[test]
    fn an_arrival_that_is_never_detected_is_missed_and_late_arrivals_are_forgiven() {
        let seq = arriving_sequence();
        let book = AlarmBook::new(Arc::clone(&seq), IOU_THRESHOLD);
        // 5 s Lauf: Ankunft bei 2 s, Frist 2 s vor Ende -> zaehlt.
        let summary = book.finish(Duration::from_secs(5));
        assert_eq!((summary.events, summary.missed), (1, 1));
        assert_eq!(summary.missed_permille(), 1_000);
        // 3 s Lauf: die Ankunft liegt in der Frist -> zaehlt nicht.
        assert_eq!(book.finish(Duration::from_secs(3)).events, 0);
    }

    /// Nach dem Ende der Sequenz beginnt sie von vorn; dieselbe Person kommt
    /// ein zweites Mal an und zaehlt ein zweites Mal.
    #[test]
    fn a_looped_sequence_counts_each_lap_separately() {
        let seq = arriving_sequence();
        let mut book = AlarmBook::new(Arc::clone(&seq), IOU_THRESHOLD);
        let target = [b(0.4, 0.2, 0.5, 0.6)];
        book.on_delivery(21, Duration::from_millis(2_200), &target);
        // Runde 2: global 70 = Frame 20, Ankunft bei 7,0 s.
        book.on_delivery(71, Duration::from_millis(7_300), &target);
        let summary = book.finish(Duration::from_secs(10));
        assert_eq!(summary.events, 2);
        let mut lat = summary.latencies_ms.clone();
        lat.sort_unstable();
        assert_eq!(lat, vec![200, 300]);
    }

    #[test]
    fn tick_recall_separates_stale_from_ideal() {
        let now = [b(0.5, 0.2, 0.6, 0.6), b(0.1, 0.1, 0.2, 0.5)];
        let stale = [b(0.4, 0.2, 0.5, 0.6), b(0.1, 0.1, 0.2, 0.5)];
        let mut recall = TickRecall::default();
        recall.tick(&now, Some(&stale), Some(&now), IOU_THRESHOLD);
        recall.tick(&now, None, Some(&now), IOU_THRESHOLD);
        assert_eq!((recall.total, recall.hits, recall.ideal), (4, 1, 4));
        assert_eq!(recall.permille(), 250);
        assert_eq!(recall.ideal_permille(), 1_000);
    }

    #[test]
    fn percentile_is_robust_to_empty_and_small_series() {
        assert_eq!(percentile(&[], 95), 0);
        assert_eq!(percentile(&[7], 95), 7);
        assert_eq!(percentile(&[1, 2, 3, 4, 100], 50), 3);
        assert_eq!(percentile(&[1, 2, 3, 4, 100], 100), 100);
    }

    #[test]
    fn the_frame_tensor_is_chw_normalised_little_endian() {
        // 1x2 Bild: ein weisses und ein schwarzes Pixel.
        let rgb = [255, 255, 255, 0, 0, 0];
        let t = frame_tensor(&rgb, 2, 1);
        assert_eq!(t.len(), 2 * 3 * 4);
        let read = |i: usize| f32::from_le_bytes(t[i * 4..i * 4 + 4].try_into().unwrap());
        // Kanal R: Pixel 0, Pixel 1; dann G, dann B.
        assert!((read(0) - (1.0 - 0.485) / 0.229).abs() < 1e-5);
        assert!((read(1) - (0.0 - 0.485) / 0.229).abs() < 1e-5);
        assert!((read(5) - (0.0 - 0.406) / 0.225).abs() < 1e-5);
    }

    #[test]
    fn the_prompt_names_every_camera() {
        let p = report_prompt(&[("A".into(), Some(3)), ("B".into(), None)]);
        assert!(p.contains("A: 3 Zielobjekte") && p.contains("B: keine aktuelle Detektion"));
    }

    #[test]
    fn global_frames_follow_the_clock_and_wrap() {
        let seq = arriving_sequence();
        assert_eq!(seq.global_frame_at(Duration::from_millis(2_550)), 25);
        assert_eq!(seq.split(73), (1, 23));
        assert_eq!(seq.capture_offset(25), Duration::from_millis(2_500));
    }
}
