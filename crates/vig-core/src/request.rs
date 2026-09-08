//! Requestklassifikation und Requestzustandsmaschine.

use crate::ids::{ModelIdx, PayloadRef, RequestId, SupersessionKey, VariantIdx};
use crate::time::{Duration, Instant};

/// Wichtigkeitsklasse eines Requests (Spec L-011).
///
/// Die Varianten sind **aufsteigend** nach Wichtigkeit deklariert, damit die
/// abgeleitete Ordnung `Protected > High > Normal > BestEffort` ergibt und
/// direkt als Sortierschluessel verwendbar ist.
///
/// Es handelt sich um eine produktinterne Klasse ohne Safety-Zertifizierung
/// (Spec Anhang B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Criticality {
    /// Darf freie Reserve nutzen, aber Protected-Ziele nicht wissentlich gefaehrden.
    BestEffort,
    /// Regulaere Arbeit ohne besonderen Schutz.
    #[default]
    Normal,
    /// Erhoehte Wichtigkeit unterhalb von Protected.
    High,
    /// Hoechste MVP-Schutzklasse fuer zeitkritische Wahrnehmung.
    Protected,
}

impl Criticality {
    /// Wahr, wenn diese Klasse durch Admission Control geschuetzt wird.
    ///
    /// Grundlage der lexikographischen Zielordnung aus Spec 10.6: Protected-
    /// und High-Misses werden vor allem anderen minimiert.
    #[must_use]
    pub const fn is_guarded(self) -> bool {
        matches!(self, Self::Protected | Self::High)
    }
}

/// Verhalten einer Queue gegenueber neu eintreffender Arbeit (Spec 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QueuePolicy {
    /// Nur der juengste noch nicht gestartete Request des Modells bleibt erhalten.
    #[default]
    Latest,
    /// Nur der juengste noch nicht gestartete Request je Key bleibt erhalten.
    LatestPerKey,
    /// Reihenfolgeerhaltend, nicht supersedierbar.
    Fifo,
    /// Nicht durch Freshness-Regeln verwerfbar; Ueberlauf erzeugt Backpressure.
    NeverDrop,
}

impl QueuePolicy {
    /// Wahr, wenn ein neuerer Request einen wartenden verdraengen darf.
    #[must_use]
    pub const fn allows_supersession(self) -> bool {
        matches!(self, Self::Latest | Self::LatestPerKey)
    }

    /// Wahr, wenn die Entnahme die Ankunftsreihenfolge einhalten muss.
    ///
    /// `FIFO` heisst first-in-first-out; `NEVER_DROP` ist die noch striktere
    /// Zusage, dass gar nichts verloren geht. Beide duerfen deshalb nicht
    /// modellintern nach Deadline umsortiert werden — bei einer
    /// zustandsbehafteten Sequenz waere die Umsortierung sogar fachlich falsch
    /// (Spec 12.5, G-011): das Backend bekaeme die Frames in einer Reihenfolge,
    /// die zu seinem Zustand nicht passt.
    ///
    /// Ueber Modellgrenzen hinweg bleibt EDF plus Kritikalitaet massgeblich —
    /// die Zusage gilt innerhalb einer Queue, nicht zwischen Queues.
    #[must_use]
    pub const fn preserves_arrival_order(self) -> bool {
        matches!(self, Self::Fifo | Self::NeverDrop)
    }

    /// Wahr, wenn ein wartender Request wegen Ueberalterung verworfen werden darf.
    ///
    /// `NEVER_DROP` ist ausgenommen (Spec 11.4): solche Requests werden auch
    /// dann nicht still entfernt, wenn sie ihre Deadline nicht mehr halten. Sie
    /// erhalten einen expliziten terminalen Zustand.
    #[must_use]
    pub const fn allows_stale_drop(self) -> bool {
        !matches!(self, Self::NeverDrop)
    }
}

/// Verhalten bei vollem Queue-Kontingent (Spec 11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OverflowPolicy {
    /// Der neue Request wird explizit abgelehnt.
    #[default]
    RejectNew,
    /// Der aelteste nicht geschuetzte wartende Request wird abgelehnt.
    RejectOldestNonProtected,
    /// Der Client wird gebremst; kein Request wird verworfen.
    BackpressureClient,
}

/// Lebenszyklus eines Requests (Spec 9.3).
///
/// Jeder Request erreicht genau einen terminalen Zustand (Spec 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestState {
    /// Am Gateway angekommen, noch nicht klassifiziert.
    Received,
    /// Wartet auf eine Dispatch-Entscheidung.
    Queued,
    /// Durch einen neueren Request desselben Scopes ersetzt.
    Superseded,
    /// Fachlich zu alt, um noch sinnvoll ausgefuehrt zu werden.
    Stale,
    /// Nach konservativer Planung nicht mehr rechtzeitig machbar.
    RejectedInfeasible,
    /// Zum Dispatch freigegeben; ein Slot-Kredit ist reserviert (ADR-0002).
    Admitted,
    /// An das Backend uebergeben. Ab hier nicht mehr zuverlaessig zurueckholbar.
    Forwarded,
    /// Fertiggestellt und bei Fertigstellung noch aktuell.
    CompletedValid,
    /// Fertiggestellt, aber bei Fertigstellung bereits obsolet (Spec 10.3 Stufe C).
    CompletedObsolete,
    /// Backendfehler oder Verbindungsverlust.
    Failed,
    /// Das Backend hat innerhalb des Timeouts nicht geantwortet.
    ///
    /// Der Client wird freigegeben, die Recheneinheit **nicht**: sie ist
    /// womoeglich noch belegt. Das ist kein Backendfehler im engeren Sinn —
    /// es ist ein unbekanntes Ausfuehrungsende.
    BackendTimeout,
    /// Der Client hat den Request zurueckgezogen, bevor er startete.
    ///
    /// Arbeit fuer einen Empfaenger, den es nicht mehr gibt, ist der
    /// teuerste Leerlauf im System: sie belegt genau die Kapazitaet, um die
    /// noch wartende Stroeme konkurrieren. Nach dem Dispatch ist der Zustand
    /// nicht mehr erreichbar — ein laufender Backendaufruf laesst sich nicht
    /// zuverlaessig zurueckholen (siehe [`Self::Forwarded`]).
    Cancelled,
}

impl RequestState {
    /// Wahr, wenn der Zustand terminal ist.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Superseded
                | Self::Stale
                | Self::RejectedInfeasible
                | Self::CompletedValid
                | Self::CompletedObsolete
                | Self::Failed
                | Self::Cancelled
                | Self::BackendTimeout
        )
    }

    /// Wahr, wenn fuer diesen Zustand Backend-Rechenzeit verbraucht wurde.
    ///
    /// Grundlage der Metrik `stale_compute_seconds_total` (Spec 18.2): nur
    /// Arbeit, die das Backend tatsaechlich erreicht hat, zaehlt als
    /// verbrauchte Rechenzeit.
    #[must_use]
    pub const fn consumed_compute(self) -> bool {
        matches!(
            self,
            Self::CompletedValid | Self::CompletedObsolete | Self::Failed
        )
    }

    /// Wahr, wenn der Uebergang `self -> next` zulaessig ist.
    ///
    /// Die Tabelle kodiert unter anderem Golden Test G-002: ein bereits
    /// `Forwarded`er Request kann nicht mehr `Superseded` werden, weil eine
    /// laufende GPU-Inferenz nicht zuverlaessig zurueckgeholt werden kann
    /// (Spec 3.1, 11.1).
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Received | Self::Queued, Self::Admitted)
                | (
                    Self::Received,
                    Self::Queued | Self::Stale | Self::RejectedInfeasible
                )
                | (
                    Self::Queued,
                    Self::Superseded | Self::Stale | Self::RejectedInfeasible
                )
                | (Self::Admitted, Self::Forwarded | Self::Failed)
                | (
                    Self::Forwarded,
                    Self::CompletedValid | Self::CompletedObsolete | Self::Failed
                )
        )
    }
}

/// Die Scheduling-Metadaten eines Requests.
///
/// Bewusst klein und `Copy`-nah gehalten und strikt von der Tensor-Payload
/// getrennt (Spec 9.3): der Scheduler kopiert Descriptoren, niemals Tensoren.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestDescriptor {
    /// Eindeutige Kennung.
    pub id: RequestId,
    /// Das angefragte logische Modell.
    pub logical_model: ModelIdx,
    /// Der Freshness-Scope fuer Supersession.
    pub supersession_key: SupersessionKey,

    /// Erzeugungs-/Capture-Zeit, Basis von Alter und Deadline (Spec L-008).
    pub generation_time: Instant,
    /// Ankunftszeit am Gateway.
    pub arrival_time: Instant,
    /// Absolute Deadline, abgeleitet aus `generation_time` (Spec L-010).
    pub absolute_deadline: Option<Instant>,
    /// Hoechstalter, ab dem das Ergebnis fachlich wertlos ist (Spec L-009).
    pub max_age: Option<Duration>,

    /// Wichtigkeitsklasse.
    pub criticality: Criticality,
    /// Queue-Verhalten.
    pub queue_policy: QueuePolicy,
    /// Wahr, wenn der Request zu einer zustandsbehafteten Sequenz gehoert.
    ///
    /// Stateful-Requests duerfen weder supersediert noch zwischen Varianten
    /// umgeschaltet werden (Spec 12.5, G-011).
    pub stateful: bool,
    /// Die vom Resolver gewaehlte Variante, sobald entschieden.
    pub variant: Option<VariantIdx>,

    /// Verweis auf die Payload; der Core dereferenziert ihn nie.
    pub payload: PayloadRef,
}

impl RequestDescriptor {
    /// Das fachliche Alter des Requests zum Zeitpunkt `now`.
    ///
    /// Basiert auf der Generation Time, nicht auf der Ankunftszeit — ein Frame,
    /// der bereits alt am Gateway ankommt, ist alt (Spec 10.2).
    #[must_use]
    pub const fn age_at(&self, now: Instant) -> Duration {
        now.saturating_since(self.generation_time)
    }

    /// Wahr, wenn der Request zum Zeitpunkt `now` sein Hoechstalter ueberschritten hat.
    ///
    /// Ohne konfiguriertes `max_age` altert ein Request nie — die Deadline
    /// bleibt dann das einzige Zeitkriterium.
    #[must_use]
    pub const fn is_over_age(&self, now: Instant) -> bool {
        match self.max_age {
            Some(limit) => self.age_at(now).as_nanos() > limit.as_nanos(),
            None => false,
        }
    }

    /// Wahr, wenn dieser Request von `other` ueberholt werden darf — **ohne**
    /// Scopevergleich.
    ///
    /// Setzt voraus: Policy-Erlaubnis, gleiches Modell, kein Stateful-Request
    /// und ein tatsaechlich juengerer Herausforderer. Die Generationszeit
    /// entscheidet, nicht die Ankunftszeit — ein verzoegert eingetroffener
    /// aelterer Frame ueberholt nichts.
    ///
    /// Ob die beiden ueberhaupt im selben Wettbewerb stehen, entscheidet die
    /// Queue: nur sie kennt die Policy und damit den wirksamen Scope. Bei
    /// `LATEST` ist er modellweit, bei `LATEST_PER_KEY` der Key. Wuerde diese
    /// Methode den Key zusaetzlich selbst pruefen, waere modellweites Latest
    /// durch unterschiedliche Clientkeys abschaltbar — obwohl die Queue den
    /// Scope bereits korrekt normalisiert hat.
    #[must_use]
    pub const fn is_outdated_by(&self, other: &Self) -> bool {
        self.queue_policy.allows_supersession()
            && !self.stateful
            && !other.stateful
            && self.logical_model.0 == other.logical_model.0
            && other.generation_time.as_nanos() > self.generation_time.as_nanos()
    }

    /// Wahr, wenn dieser Request von `other` **im selben Key** ueberholt wird.
    ///
    /// Die per-Key-Variante von [`Self::is_outdated_by`].
    #[must_use]
    pub const fn is_superseded_by(&self, other: &Self) -> bool {
        self.is_outdated_by(other) && self.supersession_key.0 == other.supersession_key.0
    }
}
