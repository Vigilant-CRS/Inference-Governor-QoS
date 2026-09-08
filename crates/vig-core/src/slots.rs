//! Backend-Execution-Slots und Belegungsplanung (ADR-0004, ADR-0002).
//!
//! Die Spezifikation modelliert das Backend an mehreren Stellen implizit als
//! **eine** serielle Ressource: die Slack-Formel in 10.4, das Idle-Beispiel in
//! 10.7 und das Blocking-Modell in 10.10. Real ist es das nicht — Triton fuehrt
//! mit `instance_group { count: N }` nebenlaeufig aus, und die Interferenzmatrix
//! in 13.4 setzt genau diese Parallelitaet voraus. ADR-0004 loest den
//! Widerspruch auf: das Backend ist eine endliche Menge von **Slots**, und
//! Feasibility ist eine Aussage ueber eine Slot-Belegung, nicht ueber eine
//! skalare Restzeit. Mit `N = 1` faellt das Modell exakt auf das serielle
//! Modell der Spec zurueck.
//!
//! ## In-Flight-Kontrolle
//!
//! ADR-0002 macht daraus zugleich das Kreditkonto gegenueber dem Backend.
//! Wuerde Vigilant mehr Arbeit weiterreichen, als das Backend gleichzeitig
//! ausfuehren kann, entstuende hinter dem Governor eine zweite, unsichtbare
//! Queue — und mit ihr Umordnung, unbegruendete Fertigstellungsprognosen und
//! wirkungslose Supersession. Die Tiefe je Slot ist deshalb hart begrenzt:
//!
//! ```text
//! depth(slot) <= 1 + pipelining_depth
//! ```
//!
//! `pipelining_depth = 0` haelt die Backend-Queue exakt leer und kostet je
//! Fertigstellung eine Round-Trip-Luecke. `pipelining_depth = 1` (Default)
//! haelt das Backend warm und die Backend-Queue bei hoechstens eins.

use crate::arrayvec::ArrayVec;
use crate::ids::{MAX_MODELS, MAX_SLOTS, ModelIdx, RequestId, SlotIdx};
use crate::time::{Duration, Instant};

/// Groesste zulaessige Tiefe je Slot, inklusive des laufenden Requests.
pub const MAX_SLOT_DEPTH: usize = 4;

/// Eine Bitmaske ueber logische Modelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct ModelMask(u32);

impl ModelMask {
    /// Die leere Maske.
    pub const NONE: Self = Self(0);
    /// Die Maske aller Modelle.
    pub const ALL: Self = Self(u32::MAX);

    /// Fuegt ein Modell hinzu.
    #[must_use]
    pub const fn with(self, m: ModelIdx) -> Self {
        if m.get() >= MAX_MODELS {
            return self;
        }
        Self(self.0 | (1_u32 << m.0))
    }

    /// Die Vereinigung zweier Masken.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Entfernt ein Modell.
    #[must_use]
    pub const fn without(self, m: ModelIdx) -> Self {
        if m.get() >= MAX_MODELS {
            return self;
        }
        Self(self.0 & !(1_u32 << m.0))
    }

    /// Wahr, wenn das Modell enthalten ist.
    #[must_use]
    pub const fn contains(self, m: ModelIdx) -> bool {
        if m.get() >= MAX_MODELS {
            return false;
        }
        self.0 & (1_u32 << m.0) != 0
    }

    /// Wahr, wenn beide Masken ein gemeinsames Modell enthalten.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Wahr, wenn die Maske leer ist.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Ein an das Backend uebergebener, noch nicht abgeschlossener Request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InFlight {
    /// Die Requestkennung.
    pub request: RequestId,
    /// Das ausfuehrende logische Modell.
    pub model: ModelIdx,
    /// Der Zeitpunkt, zu dem der Request weitergereicht wurde.
    pub dispatched_at: Instant,
    /// Der konservativ prognostizierte Fertigstellungszeitpunkt.
    ///
    /// Grundlage der Belegungsplanung. Weicht die Realitaet systematisch ab,
    /// ist das ein Profilfehler und Sache des Online Estimators (Spec 13.2),
    /// nicht der Slot-Verwaltung.
    pub expected_finish: Instant,
}

/// Ein einzelner Execution Slot.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SlotState {
    allowed: ModelMask,
    inflight: ArrayVec<InFlight, MAX_SLOT_DEPTH>,
    /// Rein hypothetische Belegung durch bereits eingeplante erwartete Arbeit.
    ///
    /// Nur der Look-ahead setzt das, und nur auf einem [`SlotSet::snapshot`].
    /// Getrennt von `inflight` gehalten, weil eine Reservierung kein laufender
    /// Request ist: sie belegt Zeit, verbraucht aber kein Pipelining-Kontingent
    /// und darf nie mit echter Arbeit verwechselt werden.
    reserved_until: Option<Instant>,
}

impl SlotState {
    /// Der Zeitpunkt, zu dem der Slot voraussichtlich frei wird.
    fn free_at(&self, now: Instant) -> Instant {
        self.inflight
            .iter()
            .map(|f| f.expected_finish)
            .chain(self.reserved_until)
            .max()
            .map_or(now, |finish| finish.max(now))
    }
}

/// Warum eine Slot-Konfiguration oder ein Dispatch unzulaessig ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotError {
    /// Keine Slots konfiguriert — das Backend haette keine Kapazitaet.
    NoSlots,
    /// Mehr Slots gefordert als [`MAX_SLOTS`].
    TooManySlots {
        /// Die geforderte Anzahl.
        requested: usize,
    },
    /// Die Pipelining-Tiefe uebersteigt [`MAX_SLOT_DEPTH`].
    PipeliningTooDeep {
        /// Die geforderte Tiefe.
        requested: usize,
    },
    /// Der Slot existiert nicht.
    UnknownSlot(SlotIdx),
    /// Der Slot hat kein Kreditkontingent mehr frei.
    NoCredit(SlotIdx),
    /// Der Slot fuehrt dieses Modell nicht aus.
    ModelNotAllowed(SlotIdx),
}

impl core::fmt::Display for SlotError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoSlots => write!(f, "keine Execution Slots konfiguriert"),
            Self::TooManySlots { requested } => {
                write!(f, "{requested} Slots gefordert, Maximum {MAX_SLOTS}")
            }
            Self::PipeliningTooDeep { requested } => {
                write!(
                    f,
                    "Pipelining-Tiefe {requested} ueber Maximum {MAX_SLOT_DEPTH}"
                )
            }
            Self::UnknownSlot(s) => write!(f, "unbekannter Slot {s}"),
            Self::NoCredit(s) => write!(f, "Slot {s} hat kein Kreditkontingent frei"),
            Self::ModelNotAllowed(s) => write!(f, "Slot {s} fuehrt dieses Modell nicht aus"),
        }
    }
}

impl core::error::Error for SlotError {}

/// Die Menge der Backend-Execution-Slots samt Belegungszustand.
#[derive(Debug, Clone)]
pub struct SlotSet {
    slots: ArrayVec<SlotState, MAX_SLOTS>,
    /// Zusaetzliche Kredite je Slot ueber den laufenden Request hinaus.
    pipelining_depth: usize,
    /// Je Modell die Maske der Modelle, mit denen es nicht koexistieren darf.
    no_corun: [ModelMask; MAX_MODELS],
}

impl SlotSet {
    /// Erzeugt `count` homogene Slots, die jedes Modell ausfuehren duerfen.
    ///
    /// # Errors
    ///
    /// [`SlotError::NoSlots`], [`SlotError::TooManySlots`] oder
    /// [`SlotError::PipeliningTooDeep`].
    pub fn homogeneous(count: usize, pipelining_depth: usize) -> Result<Self, SlotError> {
        if count == 0 {
            return Err(SlotError::NoSlots);
        }
        if count > MAX_SLOTS {
            return Err(SlotError::TooManySlots { requested: count });
        }
        if pipelining_depth.saturating_add(1) > MAX_SLOT_DEPTH {
            return Err(SlotError::PipeliningTooDeep {
                requested: pipelining_depth,
            });
        }
        let mut slots = ArrayVec::new();
        for _ in 0..count {
            slots
                .push(SlotState {
                    allowed: ModelMask::ALL,
                    inflight: ArrayVec::new(),
                    reserved_until: None,
                })
                .map_err(|_| SlotError::TooManySlots { requested: count })?;
        }
        Ok(Self {
            slots,
            pipelining_depth,
            no_corun: [ModelMask::NONE; MAX_MODELS],
        })
    }

    /// Verbietet die gleichzeitige Ausfuehrung zweier Modelle.
    ///
    /// Das ist die MVP-Fassung der Interferenzsteuerung: ein binaeres Veto
    /// statt einer gemessenen Matrix (ADR-0006). Formal der Grenzfall
    /// `slowdown = unendlich`, sodass die vollstaendige Matrix spaeter ohne
    /// Bruch nachruestbar bleibt.
    pub fn forbid_corun(&mut self, a: ModelIdx, b: ModelIdx) {
        if let Some(mask) = self.no_corun.get_mut(a.get()) {
            *mask = mask.with(b);
        }
        if let Some(mask) = self.no_corun.get_mut(b.get()) {
            *mask = mask.with(a);
        }
    }

    /// Die Anzahl konfigurierter Slots.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.slots.len()
    }

    /// Wahr, wenn keine Slots konfiguriert sind. Kann nach [`Self::homogeneous`]
    /// nicht eintreten.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Die Gesamtzahl an das Backend uebergebener, offener Requests.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.slots.iter().map(|s| s.inflight.len()).sum()
    }

    /// Die Anzahl belegter Slots.
    ///
    /// Der Belegungsgrad, mit dem [`crate::profile::VariantProfile`] die
    /// Laufzeit prognostiziert (ADR-0006).
    #[must_use]
    pub fn occupancy(&self) -> usize {
        self.slots.iter().filter(|s| !s.inflight.is_empty()).count()
    }

    /// Die Maske aller Modelle mit offener Backendarbeit.
    #[must_use]
    pub fn active_models(&self) -> ModelMask {
        let mut mask = ModelMask::NONE;
        for slot in self.slots.iter() {
            for f in slot.inflight.iter() {
                mask = mask.with(f.model);
            }
        }
        mask
    }

    /// Wahr, wenn `model` neben der aktuell laufenden Arbeit starten darf.
    #[must_use]
    pub fn corun_allowed(&self, model: ModelIdx) -> bool {
        let Some(forbidden) = self.no_corun.get(model.get()) else {
            return true;
        };
        // Das Modell darf neben sich selbst laufen; nur fremde Konflikte zaehlen.
        !forbidden.intersects(self.active_models().without(model))
    }

    /// **Planung:** wann koennte `model` fruehestens starten, und auf welchem Slot?
    ///
    /// Beruecksichtigt Slot-Erlaubnis, prognostizierte Belegung und
    /// Co-Run-Veto — aber **nicht** das Kreditkontingent. Das ist die
    /// entscheidende Unterscheidung: ein Kredit ist eine Live-Schranke gegen
    /// das Backend (ADR-0002), keine Aussage darueber, wann Rechenkapazitaet
    /// frei wird. Ein Slot, dessen Arbeit in 40 ms endet, ist in 40 ms
    /// verfuegbar — auch wenn jetzt gerade kein Kredit frei ist.
    ///
    /// Wuerde die Planung Kredite mitpruefen, waere jede voll ausgelastete
    /// Konfiguration „nicht machbar", und der Look-ahead aus Spec 10.7 koennte
    /// nie zwischen „gefaehrdet" und „ohnehin verloren" unterscheiden.
    ///
    /// Gibt `None` nur zurueck, wenn kein Slot dieses Modell je ausfuehren darf.
    #[must_use]
    pub fn projected_start(&self, model: ModelIdx, at: Instant) -> Option<(SlotIdx, Instant)> {
        let corun_block = self.corun_blocked_until(model, at);
        let mut best: Option<(SlotIdx, Instant)> = None;

        for (i, slot) in self.slots.iter().enumerate() {
            if !slot.allowed.contains(model) {
                continue;
            }
            let idx = SlotIdx(u16::try_from(i).unwrap_or(u16::MAX));
            let mut start = slot.free_at(at);
            if let Some(blocked_until) = corun_block {
                start = start.max(blocked_until);
            }
            match best {
                Some((_, t)) if t <= start => {}
                _ => best = Some((idx, start)),
            }
        }
        best
    }

    /// **Live:** ein Slot, auf dem `model` jetzt sofort weitergereicht werden darf.
    ///
    /// Erfordert zusaetzlich zur Planung ein freies Kreditkontingent. Dies ist
    /// die Durchsetzung von Anforderung L-021: hinter dem Governor darf keine
    /// zweite Warteschlange entstehen.
    #[must_use]
    pub fn ready_slot(&self, model: ModelIdx, now: Instant) -> Option<SlotIdx> {
        let max_depth = self.pipelining_depth.saturating_add(1);
        // **Live**, nicht prognostisch: ein Co-Run-Verbot gilt, solange die
        // verbotene Arbeit tatsaechlich laeuft. `expected_finish` ist eine
        // Schaetzung und beendet nichts. Haenge das Verbot allein daran, faellt
        // es genau dann, wenn die Prognose zu optimistisch war — also unter
        // Ueberlast, wo es gebraucht wird. Fuer die *Planung* bleibt die
        // Prognose richtig; fuer die Durchsetzung zaehlt der Ist-Zustand.
        if self.corun_inflight(model) {
            return None;
        }
        let corun_block = self.corun_blocked_until(model, now);
        if corun_block.is_some_and(|until| until > now) {
            return None;
        }
        self.slots.iter().enumerate().find_map(|(i, slot)| {
            let free = slot.allowed.contains(model)
                && slot.inflight.len() < max_depth
                && slot.free_at(now) <= now;
            free.then(|| SlotIdx(u16::try_from(i).unwrap_or(u16::MAX)))
        })
    }

    /// Wahr, wenn irgendein Slot fuer `model` noch Kreditkontingent hat.
    #[must_use]
    pub fn has_credit(&self, model: ModelIdx) -> bool {
        let max_depth = self.pipelining_depth.saturating_add(1);
        self.slots
            .iter()
            .any(|slot| slot.allowed.contains(model) && slot.inflight.len() < max_depth)
    }

    /// Wahr, wenn gerade Arbeit laeuft, die mit `model` nicht zusammen darf.
    ///
    /// Unabhaengig von jeder Laufzeitprognose: entscheidend ist allein, ob der
    /// verbotene Request das Backend noch belegt. Er verlaesst `inflight` erst
    /// bei der gemeldeten Fertigstellung.
    fn corun_inflight(&self, model: ModelIdx) -> bool {
        let Some(forbidden) = self.no_corun.get(model.get()) else {
            return false;
        };
        if forbidden.is_empty() {
            return false;
        }
        self.slots
            .iter()
            .flat_map(|slot| slot.inflight.iter())
            .any(|f| f.model != model && forbidden.contains(f.model))
    }

    /// Bis wann ein Co-Run-Verbot den Start von `model` **voraussichtlich**
    /// blockiert.
    ///
    /// Eine Planungsgroesse fuer den Look-ahead. Die Durchsetzung im Jetzt
    /// leistet [`Self::corun_inflight`].
    fn corun_blocked_until(&self, model: ModelIdx, now: Instant) -> Option<Instant> {
        let forbidden = self.no_corun.get(model.get())?;
        if forbidden.is_empty() {
            return None;
        }
        let mut until: Option<Instant> = None;
        for slot in self.slots.iter() {
            for f in slot.inflight.iter() {
                if f.model != model && forbidden.contains(f.model) && f.expected_finish > now {
                    let finish = f.expected_finish;
                    until = Some(until.map_or(finish, |u: Instant| u.max(finish)));
                }
            }
        }
        until
    }

    /// Wahr, wenn `model` zum Zeitpunkt `now` sofort weitergereicht werden koennte.
    #[must_use]
    pub fn can_start_now(&self, model: ModelIdx, now: Instant) -> bool {
        self.ready_slot(model, now).is_some()
    }

    /// Belegt einen Slot mit einem weitergereichten Request.
    ///
    /// # Errors
    ///
    /// [`SlotError::UnknownSlot`], [`SlotError::ModelNotAllowed`] oder
    /// [`SlotError::NoCredit`]. Die Kreditgrenze ist die Umsetzung von
    /// Anforderung L-021 (ADR-0002) und wird hier durchgesetzt, nicht nur
    /// dokumentiert.
    pub fn dispatch(
        &mut self,
        slot: SlotIdx,
        request: RequestId,
        model: ModelIdx,
        now: Instant,
        predicted_runtime: Duration,
    ) -> Result<InFlight, SlotError> {
        let max_depth = self.pipelining_depth.saturating_add(1);
        let state = self
            .slots
            .get_mut(slot.get())
            .ok_or(SlotError::UnknownSlot(slot))?;
        if !state.allowed.contains(model) {
            return Err(SlotError::ModelNotAllowed(slot));
        }
        if state.inflight.len() >= max_depth {
            return Err(SlotError::NoCredit(slot));
        }
        let start = state.free_at(now);
        let expected_finish = start
            .checked_add(predicted_runtime)
            .unwrap_or(Instant::from_nanos(u64::MAX));
        let entry = InFlight {
            request,
            model,
            dispatched_at: now,
            expected_finish,
        };
        state
            .inflight
            .push(entry)
            .map_err(|_| SlotError::NoCredit(slot))?;
        Ok(entry)
    }

    /// Belegt einen Slot **hypothetisch** bis `until`.
    ///
    /// Fuer den Look-ahead: erwartete Ankuenfte muessen einander sehen, sonst
    /// passen zwei geschuetzte Jobs jeweils einzeln und zusammen doch nicht.
    /// Bewusst kein [`Self::dispatch`]: eine erwartete Ankunft in der Zukunft
    /// laeuft nicht gleichzeitig mit der davor, sie laeuft danach — sie darf
    /// deshalb nicht am Pipelining-Kontingent scheitern, das die
    /// *Gleichzeitigkeit* begrenzt.
    ///
    /// Nur auf einem [`Self::snapshot`] sinnvoll; auf dem echten Zustand wuerde
    /// eine Reservierung Slots blockieren, die niemand belegt.
    pub fn reserve_until(&mut self, slot: SlotIdx, until: Instant) {
        if let Some(state) = self.slots.get_mut(slot.get()) {
            state.reserved_until = Some(state.reserved_until.map_or(until, |u| u.max(until)));
        }
    }

    /// Meldet einen Request auf einem Slot als fertig und gibt den Kredit frei.
    ///
    /// Gibt `None` zurueck, wenn der Request auf diesem Slot nicht offen war —
    /// eine doppelte Fertigstellungsmeldung darf keinen Kredit erfinden.
    pub fn complete(&mut self, slot: SlotIdx, request: RequestId) -> Option<InFlight> {
        let state = self.slots.get_mut(slot.get())?;
        let index = state.inflight.iter().position(|f| f.request == request)?;
        state.inflight.remove(index)
    }

    /// Sucht den Slot, auf dem ein Request offen ist.
    #[must_use]
    pub fn slot_of(&self, request: RequestId) -> Option<SlotIdx> {
        self.slots.iter().enumerate().find_map(|(i, slot)| {
            slot.inflight
                .iter()
                .any(|f| f.request == request)
                .then(|| SlotIdx(u16::try_from(i).unwrap_or(u16::MAX)))
        })
    }

    /// Alle offenen Requests.
    pub fn inflight_iter(&self) -> impl Iterator<Item = &InFlight> {
        self.slots.iter().flat_map(|s| s.inflight.iter())
    }

    /// Eine Kopie des Belegungszustands fuer hypothetische Planung.
    ///
    /// Der Look-ahead braucht die Frage „was waere, wenn ich jetzt X starte" —
    /// und darf sie nicht am echten Zustand beantworten. Der Snapshot ist
    /// allokationsfrei, weil die Slotzahl klein und statisch begrenzt ist.
    #[must_use]
    pub fn snapshot(&self) -> Self {
        self.clone()
    }
}
