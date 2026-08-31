//! Queue-Policies und Stale Work Collector (Spec 11, WP2).
//!
//! Dies ist der Kern der Produktidee: nicht jede erzeugte Arbeit hat noch
//! denselben Wert, wenn sie spaeter ausgefuehrt wird. Der Stale Work Collector
//! entfernt Requests, deren Ergebnis aufgrund neuerer Information keinen
//! ausreichenden Nutzen mehr hat, **bevor** sie Backendzeit verbrauchen
//! (Spec 1.2).
//!
//! ## Zustaendigkeit
//!
//! Diese Queue entscheidet ueber **Frische und Kapazitaet** — Stufe A
//! (Ingress Supersession) und Stufe B (Pre-dispatch Freshness) aus Spec 10.3.
//! Sie entscheidet **nicht** ueber Machbarkeit: dafuer braucht es
//! Laufzeitprognose und Slot-Belegung, und das ist Sache des Schedulers.
//! Die Trennung ist absichtlich — sie haelt die Queue frei von Annahmen ueber
//! das Backend und damit unabhaengig testbar (Spec 8.4).
//!
//! ## Was hier niemals passiert
//!
//! Kein Request verlaesst diese Queue unbemerkt. Jede Entfernung erzeugt eine
//! [`Eviction`] mit explizitem terminalem Zustand und Begruendung (Spec 8.2).
//! Ein stiller Drop waere fuer `NEVER_DROP` ein Produktfehler (Spec 11.4) und
//! fuer jede andere Policy ein Beobachtbarkeitsloch.

use crate::arrayvec::ArrayVec;
use crate::ids::{RequestId, SupersessionKey};
use crate::request::{OverflowPolicy, QueuePolicy, RequestDescriptor, RequestState};
use crate::time::Instant;

/// Hoechstkapazitaet einer einzelnen Queue.
///
/// Harte Obergrenze unabhaengig von der Konfiguration: der Speicherbedarf des
/// Schedulers ist damit zur Kompilierzeit beschraenkt, auch wenn eine
/// Konfiguration eine unsinnig grosse Kapazitaet fordert (Spec L-003, 8.3).
pub const MAX_QUEUE_CAPACITY: usize = 64;

/// Der Grund, aus dem ein Request die Queue verlassen hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropReason {
    /// Durch einen juengeren Request desselben Freshness-Scopes ersetzt.
    Superseded,
    /// Ein juengerer Request war bereits eingereiht; dieser kam zu spaet an.
    ///
    /// Der symmetrische Fall zu [`DropReason::Superseded`]: bei
    /// Netzwerkumordnung kann ein **aelterer** Frame nach einem juengeren
    /// eintreffen. Er wird nicht eingereiht, denn er wuerde die Frische
    /// verschlechtern.
    ArrivedOutOfOrder,
    /// Das fachliche Hoechstalter ist ueberschritten.
    OverAge,
    /// Die Queue ist voll und die Overflow-Policy lehnt den neuen Request ab.
    QueueFull,
    /// Die Queue ist voll; der Client soll gebremst werden statt Arbeit zu verlieren.
    Backpressure,
}

impl DropReason {
    /// Der terminale Requestzustand, der zu diesem Grund gehoert.
    #[must_use]
    pub const fn terminal_state(self) -> RequestState {
        match self {
            Self::Superseded | Self::ArrivedOutOfOrder => RequestState::Superseded,
            Self::OverAge => RequestState::Stale,
            Self::QueueFull | Self::Backpressure => RequestState::RejectedInfeasible,
        }
    }

    /// Wahr, wenn dieser Grund eine Freshness-Entscheidung ist.
    ///
    /// `NEVER_DROP` ist gegen genau diese Gruende immun (Spec L-007, 11.4);
    /// gegen Kapazitaetsgrenzen ist niemand immun, dort gibt es stattdessen
    /// explizite Backpressure.
    #[must_use]
    pub const fn is_freshness_decision(self) -> bool {
        matches!(
            self,
            Self::Superseded | Self::ArrivedOutOfOrder | Self::OverAge
        )
    }
}

/// Ein Request, der die Queue mit terminalem Zustand verlassen hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eviction {
    /// Der betroffene Request.
    pub descriptor: RequestDescriptor,
    /// Der Grund.
    pub reason: DropReason,
}

impl Eviction {
    /// Die Kennung des betroffenen Requests.
    #[must_use]
    pub const fn id(&self) -> RequestId {
        self.descriptor.id
    }

    /// Der terminale Zustand.
    #[must_use]
    pub const fn state(&self) -> RequestState {
        self.reason.terminal_state()
    }
}

/// Sammelbehaelter fuer Evictions eines einzelnen Aufrufs.
pub type Evictions = ArrayVec<Eviction, MAX_QUEUE_CAPACITY>;

/// Konfiguration einer Queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    /// Das Verhalten gegenueber neuer Arbeit.
    pub policy: QueuePolicy,
    /// Die maximale Anzahl wartender Requests.
    pub capacity: usize,
    /// Das Verhalten bei erschoepfter Kapazitaet.
    pub overflow: OverflowPolicy,
}

/// Warum eine Queue-Konfiguration unzulaessig ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueConfigError {
    /// Kapazitaet null — die Queue koennte nichts aufnehmen.
    ZeroCapacity,
    /// Kapazitaet ueber [`MAX_QUEUE_CAPACITY`].
    CapacityTooLarge {
        /// Die geforderte Kapazitaet.
        requested: usize,
    },
    /// Ein zustandsbehaftetes Modell mit supersedierender Policy.
    ///
    /// Spec 12.5 und Golden Test G-011: eine Sequenz darf nicht mittendrin
    /// Frames verlieren, sonst ist der Backendzustand inkonsistent.
    StatefulCannotSupersede {
        /// Die unzulaessige Policy.
        policy: QueuePolicy,
    },
}

impl core::fmt::Display for QueueConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ZeroCapacity => write!(f, "Queue-Kapazitaet muss groesser als null sein"),
            Self::CapacityTooLarge { requested } => write!(
                f,
                "Queue-Kapazitaet {requested} ueberschreitet das Maximum {MAX_QUEUE_CAPACITY}"
            ),
            Self::StatefulCannotSupersede { policy } => write!(
                f,
                "stateful: true ist mit Queue-Policy {policy:?} unvereinbar; \
                 zulaessig sind fifo oder never_drop"
            ),
        }
    }
}

impl core::error::Error for QueueConfigError {}

impl QueueConfig {
    /// Prueft die Konfiguration.
    ///
    /// # Errors
    ///
    /// Siehe [`QueueConfigError`]. Spec L-020 verlangt, dass eine ungueltige
    /// Konfiguration den Start verhindert, statt still mit riskanten Defaults
    /// weiterzulaufen — deshalb gibt es hier keine Reparatur, nur Ablehnung.
    pub const fn validate(&self, stateful: bool) -> Result<(), QueueConfigError> {
        if self.capacity == 0 {
            return Err(QueueConfigError::ZeroCapacity);
        }
        if self.capacity > MAX_QUEUE_CAPACITY {
            return Err(QueueConfigError::CapacityTooLarge {
                requested: self.capacity,
            });
        }
        if stateful && self.policy.allows_supersession() {
            return Err(QueueConfigError::StatefulCannotSupersede {
                policy: self.policy,
            });
        }
        Ok(())
    }
}

/// Das Ergebnis eines Einreihungsversuchs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    /// `None`, wenn der Request eingereiht wurde; sonst der Ablehnungsgrund.
    pub rejected: Option<DropReason>,
    /// Requests, die durch diesen Aufruf terminal geworden sind.
    pub evicted: Evictions,
}

impl PushOutcome {
    /// Wahr, wenn der Request eingereiht wurde.
    #[must_use]
    pub const fn accepted(&self) -> bool {
        self.rejected.is_none()
    }
}

/// Die Warteschlange eines logischen Modells.
#[derive(Debug, Clone)]
pub struct ModelQueue {
    config: QueueConfig,
    entries: ArrayVec<RequestDescriptor, MAX_QUEUE_CAPACITY>,
}

impl ModelQueue {
    /// Erzeugt eine Queue aus einer bereits geprueften Konfiguration.
    ///
    /// # Errors
    ///
    /// Reicht [`QueueConfig::validate`] durch.
    pub fn new(config: QueueConfig, stateful: bool) -> Result<Self, QueueConfigError> {
        config.validate(stateful)?;
        Ok(Self {
            config,
            entries: ArrayVec::new(),
        })
    }

    /// Die Konfiguration dieser Queue.
    #[must_use]
    pub const fn config(&self) -> QueueConfig {
        self.config
    }

    /// Die Anzahl wartender Requests.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Wahr, wenn kein Request wartet.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iteriert ueber die wartenden Requests in Ankunftsreihenfolge.
    pub fn iter(&self) -> impl Iterator<Item = &RequestDescriptor> {
        self.entries.iter()
    }

    /// Der Freshness-Scope eines Requests unter der Policy dieser Queue.
    ///
    /// Bei `LATEST` ist der Scope das gesamte Modell — alle Requests
    /// konkurrieren miteinander. Bei `LATEST_PER_KEY` ist es der Key, sodass
    /// mehrere Kameras oder Objekt-IDs sich nicht gegenseitig verdraengen
    /// (Spec 11.1, 11.2).
    ///
    /// Die Normalisierung findet hier statt und nicht am Ingress: dann kann
    /// eine falsch konfigurierte oder boesartige Clientangabe die
    /// Scope-Semantik nicht umgehen.
    #[must_use]
    const fn scope_of(&self, desc: &RequestDescriptor) -> SupersessionKey {
        match self.config.policy {
            QueuePolicy::Latest => SupersessionKey::DEFAULT,
            _ => desc.supersession_key,
        }
    }

    /// Versucht, einen Request einzureihen.
    ///
    /// Fuehrt dabei Stufe A der Stale-Pruefung aus (Spec 10.3): ein juengerer
    /// Request verdraengt aeltere wartende desselben Scopes.
    pub fn push(&mut self, incoming: RequestDescriptor) -> PushOutcome {
        let mut evicted = Evictions::new();

        if self.config.policy.allows_supersession() && !incoming.stateful {
            let scope = self.scope_of(&incoming);

            // Ein aelterer Frame, der nach einem juengeren eintrifft, wuerde die
            // Frische verschlechtern. Er wird nicht eingereiht.
            let outranked = self
                .entries
                .iter()
                .any(|queued| self.scope_of(queued) == scope && incoming.is_superseded_by(queued));
            if outranked {
                return PushOutcome {
                    rejected: Some(DropReason::ArrivedOutOfOrder),
                    evicted,
                };
            }

            let cfg = self.config;
            self.entries.retain_reporting(
                |queued| {
                    let same_scope = match cfg.policy {
                        QueuePolicy::Latest => true,
                        _ => queued.supersession_key == scope,
                    };
                    !(same_scope && queued.is_superseded_by(&incoming))
                },
                |queued| {
                    // Kann nicht ueberlaufen: hoechstens so viele Evictions wie
                    // die Queue Eintraege hat, und beide haben dieselbe Kapazitaet.
                    let _ = evicted.push(Eviction {
                        descriptor: queued,
                        reason: DropReason::Superseded,
                    });
                },
            );
        }

        if self.entries.len() >= self.config.capacity {
            match self.config.overflow {
                OverflowPolicy::RejectNew => {
                    return PushOutcome {
                        rejected: Some(DropReason::QueueFull),
                        evicted,
                    };
                }
                OverflowPolicy::BackpressureClient => {
                    return PushOutcome {
                        rejected: Some(DropReason::Backpressure),
                        evicted,
                    };
                }
                OverflowPolicy::RejectOldestNonProtected => {
                    let victim = self
                        .entries
                        .iter()
                        .position(|queued| !queued.criticality.is_guarded());
                    match victim.and_then(|i| self.entries.remove(i)) {
                        Some(removed) => {
                            let _ = evicted.push(Eviction {
                                descriptor: removed,
                                reason: DropReason::QueueFull,
                            });
                        }
                        None => {
                            // Nur geschuetzte Arbeit wartet. Sie zu verdraengen
                            // waere genau die Inversion, die das Produkt
                            // verhindern soll.
                            return PushOutcome {
                                rejected: Some(DropReason::QueueFull),
                                evicted,
                            };
                        }
                    }
                }
            }
        }

        match self.entries.push(incoming) {
            Ok(()) => PushOutcome {
                rejected: None,
                evicted,
            },
            // Unerreichbar, solange capacity <= MAX_QUEUE_CAPACITY gilt; wird
            // trotzdem als Ablehnung behandelt statt als Panik.
            Err(_) => PushOutcome {
                rejected: Some(DropReason::QueueFull),
                evicted,
            },
        }
    }

    /// Entfernt wartende Requests, die ihr fachliches Hoechstalter
    /// ueberschritten haben (Spec 10.3 Stufe B).
    ///
    /// `NEVER_DROP` ist ausgenommen: solche Requests bleiben eingereiht, auch
    /// wenn sie alt sind. Ihre Verspaetung wird sichtbar gemacht, nicht
    /// verschwiegen (Spec 11.4).
    pub fn collect_stale(&mut self, now: Instant) -> Evictions {
        let mut evicted = Evictions::new();
        if !self.config.policy.allows_stale_drop() {
            return evicted;
        }
        self.entries.retain_reporting(
            |queued| !queued.is_over_age(now),
            |queued| {
                let _ = evicted.push(Eviction {
                    descriptor: queued,
                    reason: DropReason::OverAge,
                });
            },
        );
        evicted
    }

    /// Entnimmt den Request mit der angegebenen Kennung.
    ///
    /// Der Scheduler waehlt ueber alle Queues hinweg aus (EDF plus
    /// Kritikalitaet, Spec 10.5/10.6) und holt den Gewinner hier ab. Die Queue
    /// selbst trifft keine Reihenfolgeentscheidung ueber Modellgrenzen hinweg.
    pub fn take(&mut self, id: RequestId) -> Option<RequestDescriptor> {
        let index = self.entries.iter().position(|queued| queued.id == id)?;
        self.entries.remove(index)
    }

    /// Entnimmt den aeltesten wartenden Request.
    ///
    /// Die FIFO-Entnahme; erhaelt die Ankunftsreihenfolge (Spec G-003).
    pub fn take_front(&mut self) -> Option<RequestDescriptor> {
        self.entries.pop_front()
    }

    /// Der aelteste wartende Request, ohne ihn zu entnehmen.
    #[must_use]
    pub fn front(&self) -> Option<&RequestDescriptor> {
        self.entries.get(0)
    }

    /// Die Anzahl wartender Requests im Scope von `desc`.
    ///
    /// Grundlage der `LATEST`-Invariante aus Spec 11.1: dieser Wert darf für
    /// supersedierende Policies niemals groesser als eins werden.
    #[must_use]
    pub fn scope_depth(&self, desc: &RequestDescriptor) -> usize {
        let scope = self.scope_of(desc);
        self.entries
            .iter()
            .filter(|queued| self.scope_of(queued) == scope)
            .count()
    }
}
