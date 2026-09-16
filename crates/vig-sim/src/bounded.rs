//! NV-23: eine begrenzte, pruefbare Aussage ueber den Scheduler.
//!
//! Kein universeller Echtzeitbeweis, sondern eine Teilfrage mit festen
//! Annahmen: **ein** Slot, **ein** geschuetzter periodischer Strom, beliebige
//! nicht zerlegbare Hintergrundarbeit darunter, Laufzeiten nicht ueber ihrem
//! Profil. Die Herleitung steht in `docs/analysis/nv23-bounded-claim.md`; hier
//! stehen die Schranken als Rechnung und ein Lauf, der den **echten**
//! [`Scheduler`] faehrt, damit die Rechnung gegen das Programm geprueft werden
//! kann und nicht gegen ein Modell davon.
//!
//! ## Warum eine eigene Ereignisschleife
//!
//! [`crate::harness`] zieht Laufzeiten aus einer Lognormalverteilung, die bis
//! zum Dreifachen des p99 reicht — ueber dem Plan. Eine Aussage, die „Laufzeit
//! hoechstens Plan" voraussetzt, laesst sich damit nicht pruefen. Hier sind die
//! Laufzeiten gesetzt, die Aufnahmezeitpunkte folgen einem gewaehlten
//! Jittermuster, und Weckrufe kommen genau dann, wenn der Scheduler sie
//! verlangt — wie im Gateway-Actor.
//!
//! Alle Zeiten in Mikrosekunden: das Gitter der Suche ist in ganzen
//! Mikrosekunden gesetzt, und die Schranken sollen auf der Mikrosekunde
//! stimmen, nicht ungefaehr.

use crate::coverage::CoverageTracker;
use crate::event::{SimClock, SimEvent};
use crate::rng::Pcg32;
use std::collections::{HashMap, HashSet};
use vig_core::arrayvec::ArrayVec;
use vig_core::contract_ext::ContractExtension;
use vig_core::ids::MAX_MODELS;
use vig_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use vig_core::overload::{OverloadConfig, OverloadController};
use vig_core::profile::{RuntimeProfile, SafetyMargin, VariantProfile};
use vig_core::queue::QueueConfig;
use vig_core::request::{Criticality, OverflowPolicy, QueuePolicy, RequestState};
use vig_core::scheduler::{Action, ActionSink, Event, Scheduler};
use vig_core::slots::SlotSet;
use vig_core::{
    Duration, Instant, ModelIdx, PayloadRef, RequestDescriptor, RequestId, SupersessionKey,
};

/// Wie die Aufnahmezeitpunkte des geschuetzten Stroms um ihr Raster streuen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitterPattern {
    /// Genau auf dem Raster.
    None,
    /// Abwechselnd `+J` und `−J`: jeder zweite Frame kommt `2J` frueher, als
    /// die Prognose aus der vorigen Ankunft erwartet. Der schaerfste Fall fuer
    /// die Wartezeit eines Frames.
    Alternating,
    /// `+J, 0, −J` im Wechsel: Frame `n−1` spaet, Frame `n+1` frueh. Der
    /// schaerfste Fall fuer die Verdraengung — der Nachfolger trifft so frueh
    /// ein, wie es die Prognose seines Vorgaengers zulaesst.
    Descending,
    /// Gleichverteilt in `[−J, J]`, aus einem festen Seed.
    Seeded(u64),
}

/// Ein Hintergrundstrom: periodische, nicht zerlegbare Arbeit unterhalb der
/// geschuetzten Klasse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Background {
    /// Abstand zwischen zwei Ankuenften.
    pub period_us: u64,
    /// Erste Ankunft.
    pub phase_us: u64,
    /// Profilmedian.
    pub p50_us: u64,
    /// Profil-p99.
    pub p99_us: u64,
    /// Tatsaechliche Laufzeit jedes Auftrags.
    pub runtime_us: u64,
}

/// Die Parameter eines Laufs.
///
/// Die Buchstaben in den Kommentaren sind die der Analyse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    /// `T` — Periode des geschuetzten Stroms, zugleich Vertragsperiode.
    pub period_us: u64,
    /// `J` — groesste Abweichung einer Aufnahme von ihrem Raster.
    pub jitter_us: u64,
    /// Das Jittermuster.
    pub jitter: JitterPattern,
    /// `δ` — Transportzeit von der Aufnahme bis zum Gateway.
    pub transport_us: u64,
    /// `D` — relative Deadline ab Aufnahme.
    pub deadline_us: u64,
    /// `A` — Hoechstalter ab Aufnahme.
    pub max_age_us: u64,
    /// Profilmedian des geschuetzten Stroms.
    pub p50_us: u64,
    /// Profil-p99 des geschuetzten Stroms.
    pub p99_us: u64,
    /// Tatsaechliche Laufzeiten, abwechselnd fuer gerade und ungerade Frames.
    pub runtime_us: [u64; 2],
    /// Die konfigurierte Sicherheitsmarge in Prozent.
    pub margin_percent: u32,
    /// `J_E` — die im Vertrag erklaerte Jitterhuelle
    /// (`release_jitter_envelope`), falls eine erklaert ist.
    pub jitter_envelope_us: Option<u64>,
    /// Die Hintergrundstroeme.
    pub background: Vec<Background>,
    /// Laenge des Laufs.
    pub duration_us: u64,
}

impl Params {
    /// Die Aufnahme des `n`-ten geschuetzten Frames.
    ///
    /// Das Raster beginnt bei `J`, damit auch ein fruehes erstes Bild nicht
    /// vor null liegt.
    #[must_use]
    pub fn capture_us(&self, n: u64, rng: &mut Pcg32) -> u64 {
        let base = self.jitter_us + n * self.period_us;
        let j = self.jitter_us;
        match self.jitter {
            JitterPattern::None => base,
            JitterPattern::Alternating => {
                if n.is_multiple_of(2) {
                    base + j
                } else {
                    base - j
                }
            }
            JitterPattern::Descending => match n % 3 {
                0 => base + j,
                1 => base,
                _ => base - j,
            },
            JitterPattern::Seeded(_) => {
                if j == 0 {
                    return base;
                }
                let span = u32::try_from(2 * j).unwrap_or(u32::MAX);
                base - j + u64::from(rng.next_bounded(span.saturating_add(1)))
            }
        }
    }

    /// Die erste Ankunft des geschuetzten Stroms, `a_0`.
    #[must_use]
    pub fn first_arrival_us(&self) -> u64 {
        let mut rng = self.jitter_rng();
        self.capture_us(0, &mut rng) + self.transport_us
    }

    fn jitter_rng(&self) -> Pcg32 {
        let seed = match self.jitter {
            JitterPattern::Seeded(seed) => seed,
            JitterPattern::None | JitterPattern::Alternating | JitterPattern::Descending => 0,
        };
        Pcg32::new(seed, 23)
    }
}

/// Die geplante Laufzeit, mit der der Scheduler rechnet: `p99 × Marge`.
///
/// Dieselbe ganzzahlige Rechnung wie im Kern ([`SafetyMargin::apply`]). Das
/// Ergebnis gilt, solange keine Laufzeit ueber dem p99 liegt: dann bleibt die
/// Online-Schaetzung darunter, und der Margenregler steht auf seinem Boden.
#[must_use]
pub fn planned_us(p99_us: u64, margin_percent: u32) -> Option<u64> {
    SafetyMargin::from_percent(margin_percent)?
        .apply(Duration::from_nanos_unbounded(p99_us * 1_000))
        .map(|d| d.as_nanos() / 1_000)
}

/// Welche Annahme der Aussage ein Parametersatz verletzt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    /// Eine Zeitangabe ist null oder die Marge ungueltig.
    Degenerate,
    /// (A3) Eine tatsaechliche Laufzeit liegt ueber ihrem Profil-p99.
    RuntimeAboveProfile,
    /// (A4) `Ĉ + δ + max(0, 2J − W) > D` mit `W = max(δ, 2·J_E)`: ein
    /// verspaeteter Frame gilt dem Look-ahead als unrettbar, und er gibt ihn
    /// auf. Mit einer Huelle `J_E ≥ J` bleibt davon `Ĉ + δ ≤ D` — der Frame
    /// haelt seine Deadline allein.
    NoRoomForLateArrival,
    /// (A5) `Ĉ + 2J > T`: ein Frame kann noch laufen, wenn der naechste faellig ist.
    NoRoomInPeriod,
    /// (A7) `D + 2J ≥ T + Ĉ + δ`: ein Frame kann noch warten, wenn der
    /// naechste eintrifft, und wird verdraengt.
    MaySupersede,
    /// (A8) Hintergrundarbeit trifft vor der ersten geschuetzten Ankunft ein.
    BackgroundBeforeProtected,
    /// (A9) `Δ > A`: die Schranke liegt ueber dem Hoechstalter.
    BoundAboveMaxAge,
}

/// Prueft die Annahmen der Aussage.
///
/// # Errors
///
/// Die erste verletzte Annahme.
pub fn check(p: &Params) -> Result<(), Violation> {
    if p.period_us == 0 || p.deadline_us == 0 || p.p99_us == 0 || p.p50_us > p.p99_us {
        return Err(Violation::Degenerate);
    }
    let c = planned_us(p.p99_us, p.margin_percent).ok_or(Violation::Degenerate)?;
    if p.runtime_us.iter().any(|r| *r > p.p99_us)
        || p.background.iter().any(|b| b.runtime_us > b.p99_us)
    {
        return Err(Violation::RuntimeAboveProfile);
    }
    let j2 = 2 * p.jitter_us;
    if late_room_us(p, c) > p.deadline_us {
        return Err(Violation::NoRoomForLateArrival);
    }
    if c + j2 > p.period_us {
        return Err(Violation::NoRoomInPeriod);
    }
    if p.deadline_us + j2 >= p.period_us + c + p.transport_us {
        return Err(Violation::MaySupersede);
    }
    let first = p.first_arrival_us();
    if p.background.iter().any(|b| b.phase_us < first) {
        return Err(Violation::BackgroundBeforeProtected);
    }
    if delivery_bound_us(p) > p.max_age_us {
        return Err(Violation::BoundAboveMaxAge);
    }
    Ok(())
}

/// Wie viel Deadline (A4) verlangt: `Ĉ + δ + max(0, 2J − W)`.
///
/// `W = max(δ, 2·J_E)` ist, wie weit der Look-ahead die Frist einer
/// ueberfaelligen Ankunft mitwandern laesst (ADR-0036). Reicht `W` ueber die
/// ganze moegliche Verspaetung `2J`, bleibt `Ĉ + δ`: der Frame muss seine
/// Deadline allein halten koennen, sonst ist die Frage eine andere.
const fn late_room_us(p: &Params, planned: u64) -> u64 {
    late_room(planned, p.transport_us, p.jitter_us, p.jitter_envelope_us)
}

const fn late_room(
    planned: u64,
    transport_us: u64,
    jitter_us: u64,
    envelope_us: Option<u64>,
) -> u64 {
    let envelope = match envelope_us {
        Some(j) => 2 * j,
        None => 0,
    };
    let window = if transport_us > envelope {
        transport_us
    } else {
        envelope
    };
    planned + transport_us + (2 * jitter_us).saturating_sub(window)
}

/// `Δ = 2J + D` — die Schranke fuer das Alter jedes geschuetzten Ergebnisses
/// bei seiner Auslieferung.
///
/// Bis ADR-0036 `δ + 2J + D`: der Look-ahead rechnete die Deadline ab der
/// erwarteten Ankunft statt ab der Aufnahme.
#[must_use]
pub const fn delivery_bound_us(p: &Params) -> u64 {
    2 * p.jitter_us + p.deadline_us
}

/// `max(0, T + 2J + Δ − A) = max(0, T + D + 4J − A)` — die Schranke fuer die
/// laengste Versorgungsluecke nach der ersten Auslieferung.
///
/// `None`, wenn `Δ ≥ A`: dann macht die Aussage **keine** Aussage ueber die
/// Luecke. Ein Frame, der mit Alter genau `A` ankommt, ist gueltig
/// ausgeliefert, aber keinen Augenblick brauchbar — und die Produktregel
/// zaehlt ihn zu Recht als Luecke. Die Suche hat genau diesen Rand gefunden.
#[must_use]
pub const fn gap_bound_us(p: &Params) -> Option<u64> {
    let delta = delivery_bound_us(p);
    if delta >= p.max_age_us {
        return None;
    }
    Some((p.period_us + 2 * p.jitter_us + delta).saturating_sub(p.max_age_us))
}

/// Was ein Lauf ueber den geschuetzten Strom ergeben hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Outcome {
    /// Erzeugte geschuetzte Frames.
    pub frames: u64,
    /// Ausgelieferte geschuetzte Ergebnisse.
    pub delivered: u64,
    /// Geschuetzte Frames, die ohne Ergebnis endeten (verdraengt, verworfen,
    /// abgelehnt).
    pub lost: u64,
    /// Das groesste Alter bei Auslieferung, `max(f_n − c_n)`.
    pub worst_age_us: u64,
    /// Der Frame, bei dem es auftrat.
    pub worst_frame: Option<u64>,
    /// Die laengste Versorgungsluecke zwischen erster und letzter
    /// Auslieferung, nach derselben Regel wie der Benchmark
    /// ([`CoverageTracker`]).
    pub longest_gap_us: u64,
    /// Die geplante Laufzeit, die der Scheduler fuer den geschuetzten Strom
    /// tatsaechlich verwendet hat — zum Abgleich mit [`planned_us`].
    pub planned_seen_us: Option<u64>,
    /// Ob der Scheduler je eine andere geplante Laufzeit verwendet hat.
    pub planned_varied: bool,
}

impl Outcome {
    /// Haelt dieser Lauf die Aussage ein?
    ///
    /// Die Begruendung im Fehlerfall ist fuer einen Menschen geschrieben: sie
    /// ist das Gegenbeispiel.
    ///
    /// # Errors
    ///
    /// Die erste verletzte Folgerung.
    pub fn satisfies(&self, p: &Params) -> Result<(), String> {
        let delta = delivery_bound_us(p);
        if self.lost > 0 {
            return Err(format!("{} geschuetzte Frames ohne Ergebnis", self.lost));
        }
        if self.delivered != self.frames {
            return Err(format!(
                "{} von {} Frames ausgeliefert",
                self.delivered, self.frames
            ));
        }
        if self.worst_age_us > delta {
            return Err(format!(
                "Alter bei Auslieferung {} us > Δ = {delta} us (Frame {:?})",
                self.worst_age_us, self.worst_frame
            ));
        }
        if let Some(gap) = gap_bound_us(p)
            && self.longest_gap_us > gap
        {
            return Err(format!(
                "Versorgungsluecke {} us > Schranke {gap} us",
                self.longest_gap_us
            ));
        }
        Ok(())
    }
}

/// Sammelt die Aktionen des Schedulers.
#[derive(Debug, Default)]
struct Sink(Vec<Action>);

impl ActionSink for Sink {
    fn emit(&mut self, action: Action) {
        self.0.push(action);
    }
}

fn us(v: u64) -> Duration {
    Duration::from_nanos_unbounded(v * 1_000)
}

fn at_us(v: u64) -> Instant {
    Instant::from_nanos(v * 1_000)
}

fn variant(p50_us: u64, p99_us: u64) -> Option<Variant> {
    let p95_us = u64::midpoint(p50_us, p99_us);
    let profile = RuntimeProfile::new(
        us(p50_us),
        us(p95_us),
        us(p99_us),
        RuntimeProfile::MIN_SAMPLES,
    )
    .ok()?;
    Some(Variant {
        quality: QualityValue {
            value: Quality::FULL,
            source: QualitySource::UserDeclared,
        },
        profile: VariantProfile::solo(profile),
        semantics: vig_core::semantics::VariantSemantics::default(),
        preprocess: Duration::ZERO,
    })
}

fn contract(
    criticality: Criticality,
    policy: QueuePolicy,
    capacity: usize,
    period: Option<Duration>,
    deadline: Duration,
    max_age: Duration,
    v: Variant,
) -> ModelContract {
    let mut variants = ArrayVec::new();
    let _ = variants.push(v);
    ModelContract {
        variants_interchangeable: true,
        criticality,
        queue: QueueConfig {
            policy,
            capacity,
            overflow: OverflowPolicy::RejectNew,
        },
        period,
        deadline,
        max_age: Some(max_age),
        stateful: false,
        min_quality: None,
        variant_dwell: Duration::ZERO,
        variants,
        cooperative: None,
        extension: None,
        min_runtime: None,
        objective: None,
    }
}

/// Ein Auftrag im Lauf: welcher Strom, welche Aufnahme, welcher Frame.
#[derive(Debug, Clone, Copy)]
struct Meta {
    model: usize,
    capture_us: u64,
    frame: u64,
}

/// Faehrt einen Lauf mit dem echten [`Scheduler`].
///
/// Gibt `None` zurueck, wenn sich aus den Parametern kein gueltiger
/// Scheduler bauen laesst (ungueltiges Profil, ungueltige Marge).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn run(p: &Params) -> Option<Outcome> {
    let margin = SafetyMargin::from_percent(p.margin_percent)?;
    let mut contracts: ArrayVec<ModelContract, MAX_MODELS> = ArrayVec::new();
    let mut protected = contract(
        Criticality::Protected,
        QueuePolicy::Latest,
        1,
        Some(us(p.period_us)),
        us(p.deadline_us),
        us(p.max_age_us),
        variant(p.p50_us, p.p99_us)?,
    );
    if let Some(envelope) = p.jitter_envelope_us {
        protected.extension = Some(ContractExtension {
            release_jitter_envelope: Some(us(envelope)),
            ..ContractExtension::default()
        });
    }
    contracts.push(protected).ok()?;
    for b in &p.background {
        // Grosszuegige Grenzen: der Hintergrund soll durch nichts anderes
        // verschwinden als durch die Entscheidungen, um die es geht.
        contracts
            .push(contract(
                Criticality::BestEffort,
                QueuePolicy::Fifo,
                8,
                None,
                us(10_000_000),
                us(20_000_000),
                variant(b.p50_us, b.p99_us)?,
            ))
            .ok()?;
    }
    let slots = SlotSet::homogeneous(1, 0).ok()?;
    let overload = OverloadController::new(OverloadConfig::default(), Instant::ZERO).ok()?;
    let mut scheduler = Scheduler::new(contracts.clone(), slots, overload, margin).ok()?;

    let mut clock = SimClock::new();
    let mut pending: HashMap<u64, RequestDescriptor> = HashMap::new();
    let mut meta: HashMap<u64, Meta> = HashMap::new();
    let mut next_id = 0_u64;
    let mut enqueue = |model: usize,
                       capture_us: u64,
                       arrival_us: u64,
                       frame: u64,
                       clock: &mut SimClock,
                       pending: &mut HashMap<u64, RequestDescriptor>,
                       meta: &mut HashMap<u64, Meta>|
     -> Option<()> {
        let c = contracts.get(model)?;
        next_id += 1;
        let capture = at_us(capture_us);
        let descriptor = RequestDescriptor {
            id: RequestId(next_id),
            logical_model: ModelIdx(u16::try_from(model).ok()?),
            supersession_key: SupersessionKey::DEFAULT,
            generation_time: capture,
            arrival_time: at_us(arrival_us),
            absolute_deadline: capture.checked_add(c.deadline),
            max_age: c.max_age,
            criticality: c.criticality,
            queue_policy: c.queue.policy,
            stateful: false,
            variant: None,
            payload: PayloadRef(next_id),
            context_tokens: 0,
            decomposable: false,
        };
        pending.insert(next_id, descriptor);
        meta.insert(
            next_id,
            Meta {
                model,
                capture_us,
                frame,
            },
        );
        let _ = clock.schedule(
            at_us(arrival_us),
            SimEvent::Arrival {
                model: descriptor.logical_model,
                key: SupersessionKey(next_id),
                generation: capture,
            },
        );
        Some(())
    };

    // Die geschuetzten Ankuenfte zuerst: bei gleicher Zeit gewinnt die
    // niedrigere Einfuegenummer, und eine geschuetzte Ankunft zeitgleich mit
    // Hintergrundarbeit soll nicht vom Zufall der Reihenfolge abhaengen.
    let mut rng = p.jitter_rng();
    let mut frames = 0_u64;
    loop {
        let capture = p.capture_us(frames, &mut rng);
        if capture >= p.duration_us {
            break;
        }
        enqueue(
            0,
            capture,
            capture + p.transport_us,
            frames,
            &mut clock,
            &mut pending,
            &mut meta,
        )?;
        frames += 1;
    }
    for (i, b) in p.background.iter().enumerate() {
        let mut t = b.phase_us;
        let mut k = 0_u64;
        while t < p.duration_us && b.period_us > 0 {
            enqueue(i + 1, t, t, k, &mut clock, &mut pending, &mut meta)?;
            t += b.period_us;
            k += 1;
        }
    }

    let mut outcome = Outcome {
        frames,
        ..Outcome::default()
    };
    let mut deliveries: Vec<(u64, u64)> = Vec::new();
    let mut wakes: HashSet<u64> = HashSet::new();
    let wake_limit = p.duration_us + 1_000_000;

    while let Some((now, event)) = clock.advance() {
        let mut sink = Sink::default();
        match event {
            SimEvent::Arrival { key, .. } => {
                if let Some(descriptor) = pending.remove(&key.0) {
                    scheduler.on_event(now, Event::Arrival(descriptor), &mut sink);
                }
            }
            SimEvent::Completion { request, slot } => {
                scheduler.on_event(now, Event::Completion { request, slot }, &mut sink);
            }
            SimEvent::BackendFailure { request, slot } => {
                scheduler.on_event(now, Event::BackendFailure { request, slot }, &mut sink);
            }
            SimEvent::Tick | SimEvent::EndOfRun => {
                scheduler.on_event(now, Event::Tick, &mut sink);
            }
        }
        let now_us = now.as_nanos() / 1_000;
        for action in &sink.0 {
            match *action {
                Action::Dispatch {
                    request,
                    slot,
                    predicted_runtime,
                    ..
                } => {
                    let Some(m) = meta.get(&request.0).copied() else {
                        continue;
                    };
                    let runtime_us = if m.model == 0 {
                        let planned = predicted_runtime.as_nanos() / 1_000;
                        match outcome.planned_seen_us {
                            None => outcome.planned_seen_us = Some(planned),
                            Some(seen) if seen != planned => outcome.planned_varied = true,
                            Some(_) => {}
                        }
                        let parity = usize::try_from(m.frame % 2).unwrap_or(0);
                        p.runtime_us.get(parity).copied().unwrap_or(0)
                    } else {
                        p.background.get(m.model - 1).map_or(0, |b| b.runtime_us)
                    };
                    let _ = clock.schedule(
                        at_us(now_us + runtime_us),
                        SimEvent::Completion { request, slot },
                    );
                }
                Action::Terminate { request, state } => {
                    let Some(m) = meta.get(&request.0).copied() else {
                        continue;
                    };
                    if m.model != 0 {
                        continue;
                    }
                    match state {
                        RequestState::CompletedValid | RequestState::CompletedObsolete => {
                            outcome.delivered += 1;
                            deliveries.push((now_us, m.capture_us));
                            let age = now_us.saturating_sub(m.capture_us);
                            if outcome.worst_frame.is_none() || age > outcome.worst_age_us {
                                outcome.worst_age_us = age;
                                outcome.worst_frame = Some(m.frame);
                            }
                        }
                        _ => outcome.lost += 1,
                    }
                }
                Action::WakeAt(t) => {
                    let wake_us = t.as_nanos() / 1_000;
                    if wake_us > now_us && wake_us <= wake_limit && wakes.insert(wake_us) {
                        let _ = clock.schedule(at_us(wake_us), SimEvent::Tick);
                    }
                }
                Action::ObservedRuntime { .. } => {}
            }
        }
    }

    outcome.longest_gap_us = longest_gap_us(&deliveries, p);
    Some(outcome)
}

/// Die laengste Versorgungsluecke zwischen erster und letzter Auslieferung.
///
/// Dieselbe Regel wie im Benchmark: gemessen wird ab dem Ablauf des letzten
/// brauchbaren Ergebnisses, und eine bereits veraltete Lieferung schliesst
/// nichts. Anlauf und Auslauf gehoeren nicht zur Aussage.
fn longest_gap_us(deliveries: &[(u64, u64)], p: &Params) -> u64 {
    let (Some(first), Some(last)) = (
        deliveries.iter().map(|d| d.0).min(),
        deliveries.iter().map(|d| d.0).max(),
    ) else {
        return 0;
    };
    let mut tracker = CoverageTracker::new(
        us(p.period_us),
        us(p.max_age_us),
        at_us(first),
        us(last - first),
    );
    for (completion, capture) in deliveries {
        tracker.record_delivery(at_us(*completion), at_us(*capture));
    }
    tracker.finish().longest_gap_ns / 1_000
}

/// Wie gross das Suchgitter sein soll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridSize {
    /// Fuer den normalen Testlauf.
    Small,
    /// Fuer den ausfuehrlichen, ignorierten Test.
    Full,
}

/// Die Achsen eines Suchgitters.
#[derive(Debug, Clone, Copy)]
struct Axes {
    periods: &'static [u64],
    jitters: &'static [u64],
    transports: &'static [u64],
    p99s: &'static [u64],
    margins: &'static [u32],
    background_p99s: &'static [u64],
    background_periods: &'static [u64],
    duration_us: u64,
    /// Auch zufaelliger Jitter, nicht nur die beiden schaerfsten Muster.
    seeded: bool,
    /// Auch abwechselnd kurze und lange geschuetzte Laufzeiten, nicht nur
    /// jede genau auf dem Plan.
    mixed_runtime: bool,
}

impl Axes {
    /// Das kleine Gitter haelt die schaerfsten Faelle — Muster, die einen
    /// Frame `2J` zu frueh bringen, Laufzeiten genau auf dem Plan, alle drei
    /// Raender von Deadline und Hoechstalter — und laesst weg, was nur Breite
    /// bringt. Es muss in einem Debug-Testlauf unter 30 s bleiben.
    const fn of(size: GridSize) -> Self {
        match size {
            GridSize::Small => Self {
                periods: &[20_000, 33_000],
                jitters: &[0, 1_000, 3_000],
                transports: &[2_000],
                p99s: &[4_000, 9_000],
                margins: &[110],
                background_p99s: &[12_000, 46_000, 90_000],
                background_periods: &[5_000, 61_000],
                duration_us: 500_000,
                seeded: false,
                mixed_runtime: false,
            },
            GridSize::Full => Self {
                periods: &[16_000, 20_000, 33_000, 50_000, 120_000],
                jitters: &[0, 1_000, 3_000, 5_000],
                transports: &[0, 5_000],
                p99s: &[3_000, 9_000, 15_000],
                margins: &[100, 150],
                background_p99s: &[2_000, 46_000, 140_000],
                background_periods: &[3_000, 250_000],
                duration_us: 1_500_000,
                seeded: true,
                mixed_runtime: true,
            },
        }
    }
}

/// Die Parametersaetze des geschuetzten Stroms, noch ohne Hintergrund.
fn protected_cases(axes: &Axes) -> Vec<Params> {
    let mut out = Vec::new();
    for &period_us in axes.periods {
        for &jitter_us in axes.jitters {
            let patterns: &[JitterPattern] = match (jitter_us, axes.seeded) {
                (0, _) => &[JitterPattern::None],
                (_, false) => &[JitterPattern::Alternating, JitterPattern::Descending],
                (_, true) => &[
                    JitterPattern::Alternating,
                    JitterPattern::Descending,
                    JitterPattern::Seeded(7),
                ],
            };
            for &jitter in patterns {
                for &transport_us in axes.transports {
                    for &p99_us in axes.p99s {
                        for &margin_percent in axes.margins {
                            for jitter_envelope_us in envelopes(jitter_us) {
                                let Some(c) = planned_us(p99_us, margin_percent) else {
                                    continue;
                                };
                                out.extend(deadline_cases(
                                    axes,
                                    &Params {
                                        period_us,
                                        jitter_us,
                                        jitter,
                                        transport_us,
                                        deadline_us: 0,
                                        max_age_us: 0,
                                        p50_us: p99_us * 6 / 10,
                                        p99_us,
                                        runtime_us: [p99_us, p99_us],
                                        margin_percent,
                                        jitter_envelope_us,
                                        background: Vec::new(),
                                        duration_us: axes.duration_us,
                                    },
                                    c,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Ohne Jitter keine Huelle; mit Jitter beides — der Look-ahead mit und
/// ohne das Wissen, wie spaet ein Frame hoechstens kommt (ADR-0036).
fn envelopes(jitter_us: u64) -> Vec<Option<u64>> {
    if jitter_us == 0 {
        vec![None]
    } else {
        vec![None, Some(jitter_us)]
    }
}

/// Deadlines, Hoechstalter und Laufzeitmuster zu einem geschuetzten Strom.
fn deadline_cases(axes: &Axes, template: &Params, c: u64) -> Vec<Params> {
    let mut out = Vec::new();
    // Die kleinste Deadline, die (A4) zulaesst, die Periode, und die
    // groesste, die (A7) zulaesst.
    let lowest = late_room_us(template, c);
    let highest =
        (template.period_us + c + template.transport_us).saturating_sub(2 * template.jitter_us + 1);
    let mut deadlines = vec![lowest, template.period_us, highest];
    deadlines.retain(|d| *d >= lowest);
    deadlines.sort_unstable();
    deadlines.dedup();
    let runtimes: &[[u64; 2]] = if axes.mixed_runtime {
        &[
            [template.p99_us, template.p99_us],
            [template.p50_us, template.p99_us],
        ]
    } else {
        &[[template.p99_us, template.p99_us]]
    };
    for deadline_us in deadlines {
        let delta = 2 * template.jitter_us + deadline_us;
        // Genau auf der Schranke (Auslieferung, keine Luecke behauptet), 1 µs
        // darueber (die engste Luecke), und so weit, dass keine bleibt.
        for max_age_us in [
            delta,
            delta + 1,
            template.period_us + 2 * template.jitter_us + delta,
        ] {
            for &runtime_us in runtimes {
                out.push(Params {
                    deadline_us,
                    max_age_us,
                    runtime_us,
                    ..template.clone()
                });
            }
        }
    }
    out
}

/// Das Suchgitter.
///
/// Dicht an den Raendern der Annahmen: Deadlines bei `Ĉ + 2J` und knapp
/// unter der Grenze von (A7), Hoechstalter genau bei `Δ`, Laufzeiten genau auf
/// dem Plan, Hintergrundarbeit, die 1 µs nach, eine halbe Periode nach und
/// 1 µs vor einer geschuetzten Freigabe eintrifft, und die Jittermuster, die
/// einen Frame `2J` zu frueh oder seinen Nachfolger so frueh wie moeglich
/// bringen. Parameter, die eine Annahme verletzen, bleiben drin: die Suche
/// zaehlt sie getrennt.
#[must_use]
pub fn grid(size: GridSize) -> Vec<Params> {
    let axes = Axes::of(size);
    let mut out = Vec::new();
    for base in protected_cases(&axes) {
        let first = base.first_arrival_us();
        for &bg_p99 in axes.background_p99s {
            for &bg_period in axes.background_periods {
                for offset in [1, base.period_us / 2, base.period_us - 1] {
                    let mut p = base.clone();
                    p.background.push(Background {
                        period_us: bg_period,
                        phase_us: first + offset,
                        p50_us: bg_p99 * 6 / 10,
                        p99_us: bg_p99,
                        runtime_us: bg_p99,
                    });
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Das Ergebnis einer Suche.
#[derive(Debug, Clone, Default)]
pub struct SearchReport {
    /// Laeufe innerhalb der Annahmen.
    pub inside: u64,
    /// Parametersaetze, die eine Annahme verletzen, je Annahme.
    pub outside: Vec<(Violation, u64)>,
    /// Laeufe, deren Szenario sich nicht bauen liess.
    pub unbuildable: u64,
    /// Das erste Gegenbeispiel innerhalb der Annahmen, falls es eines gab.
    pub counterexample: Option<(Params, Outcome, String)>,
    /// Das groesste Verhaeltnis `Alter / Δ` in Promille — wie nah die Laeufe
    /// an die Schranke kamen.
    pub tightest_permille: u64,
    /// Laeufe, in denen der Scheduler mit einer anderen geplanten Laufzeit
    /// rechnete als die Analyse.
    pub plan_mismatch: u64,
}

/// Faehrt alle Parametersaetze und prueft jeden, der die Annahmen erfuellt.
#[must_use]
pub fn search(params: &[Params]) -> SearchReport {
    let mut report = SearchReport::default();
    for p in params {
        if let Err(v) = check(p) {
            match report.outside.iter_mut().find(|(k, _)| *k == v) {
                Some((_, n)) => *n += 1,
                None => report.outside.push((v, 1)),
            }
            continue;
        }
        let Some(outcome) = run(p) else {
            report.unbuildable += 1;
            continue;
        };
        report.inside += 1;
        let expected_plan = planned_us(p.p99_us, p.margin_percent);
        if outcome.planned_varied || outcome.planned_seen_us != expected_plan {
            report.plan_mismatch += 1;
        }
        let delta = delivery_bound_us(p).max(1);
        report.tightest_permille = report
            .tightest_permille
            .max(outcome.worst_age_us * 1_000 / delta);
        if report.counterexample.is_none()
            && let Err(why) = outcome.satisfies(p)
        {
            report.counterexample = Some((p.clone(), outcome, why));
        }
    }
    report
}
