//! Ein begrenzter, gueltigkeitsbewusster Abhaengigkeitsgraph (NV-17).
//!
//! ## Was hier gerechnet wird — und was nicht
//!
//! **Metadaten, keine Tensoren.** Dieser Graph weiss, welches Ergebnis zu
//! welcher Aufnahme gehoert, wer es noch braucht und wann es niemand mehr
//! braucht. Er beruehrt keine Nutzlast (ADR-0003) und fuehrt keine Berechnung
//! aus.
//!
//! ## Der Fehler, um den es geht
//!
//! Ein Roboter kombiniert Ergebnisse: Detektionen aus einem Kamerabild mit
//! einer Tiefenkarte aus demselben Bild. Kommt die Tiefe langsamer als die
//! Detektion, liegt irgendwann eine frische Detektion neben einer alten
//! Tiefenkarte — und beides zusammen ergibt eine Szene, die es nie gegeben
//! hat. Objekte stehen an Stellen, an denen sie vor zwei Bildern standen.
//!
//! Frische allein faengt das nicht: **beide** Ergebnisse koennen unter ihrem
//! Hoechstalter liegen und trotzdem aus verschiedenen Aufnahmen stammen. Was
//! fehlt, ist die Aufnahme selbst als Begriff — die [`CaptureId`].
//!
//! ## Drei Regeln
//!
//! **Eine Zusammenfuehrung braucht eine gemeinsame Aufnahme.** Ein Knoten,
//! dessen Eltern zu verschiedenen [`CaptureId`]s gehoeren, wird abgelehnt —
//! beim Einfuegen und nicht erst beim Kombinieren. Wer wirklich ueber
//! Aufnahmen hinweg rechnen will, sagt das ausdruecklich
//! ([`Graph::insert_across_captures`]).
//!
//! **Ein Ergebnis bleibt gueltig, bis der letzte Verbraucher es freigibt.**
//! Referenzgezaehlt, nicht nach Frist. Zwei Verbraucher an einem Elternteil
//! heissen zwei Freigaben; die erste beendet nichts.
//!
//! **Laufende Arbeit wird nicht als abgebrochen erfunden.** Faellt ein
//! Elternteil aus, werden nur die Kinder abgebrochen, die **noch nicht
//! gestartet** sind. Ein Kind, das schon rechnet, laeuft zu Ende und meldet
//! sein Ergebnis; es dann als storniert zu buchen waere eine Behauptung ueber
//! die GPU, die niemand belegen kann (dasselbe Argument wie in NV-00).

use crate::arrayvec::ArrayVec;
use crate::ids::ModelIdx;

/// Wie viele Knoten der Graph fasst.
///
/// Ein Graph, der mit der Laufzeit waechst, ist ein unbeschraenkter Puffer
/// (Spec L-003). 256 Knoten sind bei vier Modellen und einer Aufnahme je
/// 33 ms rund zwei Sekunden Historie — mehr, als eine Zusammenfuehrung
/// sinnvoll ueberbrueckt.
pub const MAX_NODES: usize = 256;

/// Wie viele Eltern ein Knoten haben kann.
pub const MAX_PARENTS: usize = 8;

/// Eine Sensoraufnahme.
///
/// Der Begriff, der in einer reinen Frischebetrachtung fehlt: zwei Ergebnisse
/// koennen beide frisch und trotzdem aus verschiedenen Aufnahmen sein.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureId(pub u64);

/// Die Vertrags- und Konfigurationsepoche.
///
/// Ein Ergebnis aus der Zeit vor einem Vertragswechsel gehoert nicht in eine
/// Zusammenfuehrung nach dem Wechsel — die Ausgabesemantik kann sich geaendert
/// haben (NV-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct EpochId(pub u32);

/// Ein Knoten im Graphen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// Woran ein Knoten gerade ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    /// Eingefuegt, noch nicht gestartet.
    Pending,
    /// Laeuft.
    Running,
    /// Fertig, Ergebnis gueltig.
    Completed,
    /// Fehlgeschlagen.
    Failed,
    /// Vor dem Start abgebrochen.
    ///
    /// Nur aus [`NodeState::Pending`] erreichbar. Laufende Arbeit wird nicht
    /// als abgebrochen gebucht.
    Cancelled,
    /// Fertig geworden, aber nachtraeglich als unzustaendig erkannt.
    ///
    /// Etwa weil ein Elternteil ausfiel, waehrend dieses Kind schon rechnete.
    /// Das Ergebnis existiert, es ist nur fuer die Zusammenfuehrung nicht mehr
    /// zu gebrauchen — ein anderer Zustand als „abgebrochen".
    Superseded,
}

impl NodeState {
    /// Ob der Knoten noch etwas werden kann.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }

    /// Ob ein Ergebnis vorliegt, das verwendet werden darf.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Completed)
    }
}

/// Warum eine Graphoperation abgelehnt wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphError {
    /// Der Graph ist voll.
    Full {
        /// Die Obergrenze.
        capacity: usize,
    },
    /// Mehr Eltern als [`MAX_PARENTS`].
    TooManyParents {
        /// Die geforderte Anzahl.
        requested: usize,
    },
    /// Ein Elternteil existiert nicht.
    UnknownParent {
        /// Die genannte Kennung.
        parent: NodeId,
    },
    /// Die Eltern gehoeren zu verschiedenen Aufnahmen.
    ///
    /// Der Fehler, um den es geht: eine frische Detektion neben einer alten
    /// Tiefenkarte ergibt eine Szene, die es nie gegeben hat.
    MixedCaptures {
        /// Die eine Aufnahme.
        left: CaptureId,
        /// Die andere.
        right: CaptureId,
    },
    /// Die Eltern gehoeren zu verschiedenen Epochen.
    MixedEpochs {
        /// Die eine Epoche.
        left: EpochId,
        /// Die andere.
        right: EpochId,
    },
    /// Der Knoten existiert nicht.
    UnknownNode {
        /// Die genannte Kennung.
        node: NodeId,
    },
    /// Der Zustandswechsel ist nicht zulaessig.
    InvalidTransition {
        /// Woher.
        from: NodeState,
        /// Wohin.
        to: NodeState,
    },
    /// Es gibt mehr Freigaben als Verbraucher.
    ///
    /// Ein Zaehler, der unter null faellt, wuerde ein Ergebnis freigeben, das
    /// noch jemand haelt.
    ReleaseWithoutHold {
        /// Der Knoten.
        node: NodeId,
    },
}

impl core::fmt::Display for GraphError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Full { capacity } => write!(f, "der Graph fasst hoechstens {capacity} Knoten"),
            Self::TooManyParents { requested } => {
                write!(f, "{requested} Eltern, Maximum {MAX_PARENTS}")
            }
            Self::UnknownParent { parent } => write!(f, "unbekanntes Elternteil {parent:?}"),
            Self::MixedCaptures { left, right } => write!(
                f,
                "Eltern aus verschiedenen Aufnahmen ({left:?} und {right:?}); \
                 zusammengefuehrt ergaebe das eine Szene, die es nie gab"
            ),
            Self::MixedEpochs { left, right } => write!(
                f,
                "Eltern aus verschiedenen Epochen ({left:?} und {right:?})"
            ),
            Self::UnknownNode { node } => write!(f, "unbekannter Knoten {node:?}"),
            Self::InvalidTransition { from, to } => {
                write!(f, "Zustandswechsel {from:?} -> {to:?} ist unzulaessig")
            }
            Self::ReleaseWithoutHold { node } => write!(
                f,
                "Freigabe ohne Halter fuer {node:?}; ein Zaehler unter null \
                 gaebe ein Ergebnis frei, das noch jemand haelt"
            ),
        }
    }
}

impl core::error::Error for GraphError {}

/// Ein Knoten samt Buchhaltung.
#[derive(Debug, Clone)]
struct Node {
    id: NodeId,
    model: ModelIdx,
    capture: CaptureId,
    epoch: EpochId,
    state: NodeState,
    parents: ArrayVec<NodeId, MAX_PARENTS>,
    /// Wie viele Verbraucher dieses Ergebnis noch brauchen.
    holds: u32,
    /// Ob ausdruecklich ueber Aufnahmegrenzen hinweg gerechnet wird.
    across_captures: bool,
}

/// Der Abhaengigkeitsgraph.
#[derive(Debug)]
pub struct Graph {
    nodes: Vec<Node>,
    next_id: u32,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    /// Ein leerer Graph.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(MAX_NODES),
            next_id: 0,
        }
    }

    /// Wie viele Knoten der Graph gerade fuehrt.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Ob der Graph leer ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn find(&self, node: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == node)
    }

    fn find_mut(&mut self, node: NodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == node)
    }

    /// Fuegt einen Knoten ein.
    ///
    /// Alle Eltern muessen zur selben Aufnahme und zur selben Epoche gehoeren
    /// wie der neue Knoten; sonst wird abgelehnt.
    ///
    /// # Errors
    ///
    /// Siehe [`GraphError`].
    pub fn insert(
        &mut self,
        model: ModelIdx,
        capture: CaptureId,
        epoch: EpochId,
        parents: &[NodeId],
    ) -> Result<NodeId, GraphError> {
        self.insert_inner(model, capture, epoch, parents, false)
    }

    /// Fuegt einen Knoten ein, der ausdruecklich ueber Aufnahmegrenzen hinweg
    /// rechnet.
    ///
    /// Fuer Faelle, in denen das fachlich gewollt ist — eine Bewegungsschaetzung
    /// braucht zwei Aufnahmen. Ausdruecklich und benannt, damit es nicht die
    /// bequeme Umgehung der Regel wird.
    ///
    /// # Errors
    ///
    /// Siehe [`GraphError`]. Die Epochenpruefung bleibt: eine geaenderte
    /// Ausgabesemantik ist auch ueber Aufnahmen hinweg ein Problem.
    pub fn insert_across_captures(
        &mut self,
        model: ModelIdx,
        capture: CaptureId,
        epoch: EpochId,
        parents: &[NodeId],
    ) -> Result<NodeId, GraphError> {
        self.insert_inner(model, capture, epoch, parents, true)
    }

    fn insert_inner(
        &mut self,
        model: ModelIdx,
        capture: CaptureId,
        epoch: EpochId,
        parents: &[NodeId],
        across_captures: bool,
    ) -> Result<NodeId, GraphError> {
        if self.nodes.len() >= MAX_NODES {
            return Err(GraphError::Full {
                capacity: MAX_NODES,
            });
        }
        if parents.len() > MAX_PARENTS {
            return Err(GraphError::TooManyParents {
                requested: parents.len(),
            });
        }

        let mut list = ArrayVec::new();
        for parent in parents {
            let found = self
                .find(*parent)
                .ok_or(GraphError::UnknownParent { parent: *parent })?;
            if !across_captures && found.capture != capture {
                return Err(GraphError::MixedCaptures {
                    left: found.capture,
                    right: capture,
                });
            }
            if found.epoch != epoch {
                return Err(GraphError::MixedEpochs {
                    left: found.epoch,
                    right: epoch,
                });
            }
            if list.push(*parent).is_err() {
                return Err(GraphError::TooManyParents {
                    requested: parents.len(),
                });
            }
        }

        let id = NodeId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.nodes.push(Node {
            id,
            model,
            capture,
            epoch,
            state: NodeState::Pending,
            parents: list,
            holds: 0,
            across_captures,
        });
        Ok(id)
    }

    /// Der Zustand eines Knotens.
    #[must_use]
    pub fn state(&self, node: NodeId) -> Option<NodeState> {
        self.find(node).map(|n| n.state)
    }

    /// Die Aufnahme eines Knotens.
    #[must_use]
    pub fn capture(&self, node: NodeId) -> Option<CaptureId> {
        self.find(node).map(|n| n.capture)
    }

    /// Das Modell eines Knotens.
    #[must_use]
    pub fn model(&self, node: NodeId) -> Option<ModelIdx> {
        self.find(node).map(|n| n.model)
    }

    /// Wie viele Verbraucher dieses Ergebnis noch halten.
    #[must_use]
    pub fn holds(&self, node: NodeId) -> u32 {
        self.find(node).map_or(0, |n| n.holds)
    }

    /// Meldet einen Verbraucher an.
    ///
    /// # Errors
    ///
    /// [`GraphError::UnknownNode`], wenn der Knoten nicht existiert.
    pub fn acquire(&mut self, node: NodeId) -> Result<u32, GraphError> {
        let found = self
            .find_mut(node)
            .ok_or(GraphError::UnknownNode { node })?;
        found.holds = found.holds.saturating_add(1);
        Ok(found.holds)
    }

    /// Meldet einen Verbraucher ab.
    ///
    /// # Errors
    ///
    /// [`GraphError::ReleaseWithoutHold`], wenn es keinen Halter gibt. Ein
    /// Zaehler unter null gaebe ein Ergebnis frei, das noch jemand haelt.
    pub fn release(&mut self, node: NodeId) -> Result<u32, GraphError> {
        let found = self
            .find_mut(node)
            .ok_or(GraphError::UnknownNode { node })?;
        if found.holds == 0 {
            return Err(GraphError::ReleaseWithoutHold { node });
        }
        found.holds = found.holds.saturating_sub(1);
        Ok(found.holds)
    }

    /// Ob dieses Ergebnis noch gebraucht wird.
    ///
    /// Referenzgezaehlt und nicht nach Frist: zwei Verbraucher an einem
    /// Elternteil heissen zwei Freigaben, und die erste beendet nichts.
    #[must_use]
    pub fn is_retained(&self, node: NodeId) -> bool {
        self.holds(node) > 0
    }

    /// Wechselt den Zustand eines Knotens.
    ///
    /// # Errors
    ///
    /// [`GraphError::InvalidTransition`] fuer einen unzulaessigen Wechsel —
    /// insbesondere `Running -> Cancelled`: laufende Arbeit wird nicht als
    /// abgebrochen erfunden.
    pub fn transition(&mut self, node: NodeId, to: NodeState) -> Result<(), GraphError> {
        let found = self
            .find_mut(node)
            .ok_or(GraphError::UnknownNode { node })?;
        let from = found.state;
        // Aus `Pending` heraus: starten oder abbrechen. Aus `Running`: nur
        // ein Ende, nie ein Abbruch — laufende Arbeit wird nicht als
        // abgebrochen erfunden. Und ein fertiges Ergebnis kann sich
        // nachtraeglich als unzustaendig erweisen, aber nicht verschwinden.
        let allowed = matches!(
            (from, to),
            (
                NodeState::Pending,
                NodeState::Running | NodeState::Cancelled
            ) | (
                NodeState::Running,
                NodeState::Completed | NodeState::Failed | NodeState::Superseded
            ) | (NodeState::Completed, NodeState::Superseded)
        );
        if !allowed {
            return Err(GraphError::InvalidTransition { from, to });
        }
        found.state = to;
        Ok(())
    }

    /// Ob dieser Knoten fachlich gueltig ist.
    ///
    /// Gueltig heisst: er selbst ist verwendbar, und jedes Elternteil ist
    /// verwendbar und gehoert zur selben Aufnahme und Epoche. Ein Knoten ohne
    /// Eltern ist gueltig, sobald er fertig ist.
    #[must_use]
    pub fn is_valid(&self, node: NodeId) -> bool {
        let Some(found) = self.find(node) else {
            return false;
        };
        if !found.state.is_usable() {
            return false;
        }
        found.parents.iter().all(|parent| {
            self.find(*parent).is_some_and(|p| {
                p.state.is_usable()
                    && p.epoch == found.epoch
                    && (found.across_captures || p.capture == found.capture)
            })
        })
    }

    /// Bricht die noch nicht gestarteten Nachkommen eines Knotens ab.
    ///
    /// Gibt die abgebrochenen Knoten zurueck — und die laufenden, die
    /// **nicht** abgebrochen wurden, als eigene Liste. Ein Kind, das schon
    /// rechnet, laeuft zu Ende: es als storniert zu buchen waere eine
    /// Behauptung ueber die GPU, die niemand belegen kann.
    pub fn cancel_dependents(&mut self, node: NodeId) -> CancellationOutcome {
        let mut cancelled = ArrayVec::new();
        let mut still_running = ArrayVec::new();

        // Breitensuche ueber die Nachkommen, begrenzt durch die Knotenzahl.
        let mut frontier: ArrayVec<NodeId, MAX_NODES> = ArrayVec::new();
        let _ = frontier.push(node);
        let mut visited = 0_usize;
        while visited < frontier.len() && visited < MAX_NODES {
            let Some(current) = frontier.get(visited).copied() else {
                break;
            };
            visited = visited.saturating_add(1);

            let children: ArrayVec<NodeId, MAX_NODES> = self
                .nodes
                .iter()
                .filter(|n| n.parents.iter().any(|p| *p == current))
                .map(|n| n.id)
                .fold(ArrayVec::new(), |mut acc, id| {
                    let _ = acc.push(id);
                    acc
                });

            for child in children.iter() {
                if frontier.iter().any(|f| f == child) {
                    continue;
                }
                let _ = frontier.push(*child);
                let Some(found) = self.find_mut(*child) else {
                    continue;
                };
                match found.state {
                    NodeState::Pending => {
                        found.state = NodeState::Cancelled;
                        let _ = cancelled.push(*child);
                    }
                    NodeState::Running => {
                        // Laeuft weiter. Der Aufrufer erfaehrt es und kann das
                        // Ergebnis spaeter als `Superseded` buchen.
                        let _ = still_running.push(*child);
                    }
                    NodeState::Completed
                    | NodeState::Failed
                    | NodeState::Cancelled
                    | NodeState::Superseded => {}
                }
            }
        }

        CancellationOutcome {
            cancelled,
            still_running,
        }
    }

    /// Entfernt Knoten, die niemand mehr braucht.
    ///
    /// Ein Knoten geht, wenn er abgeschlossen ist, niemand ihn mehr haelt und
    /// kein anderer Knoten ihn als Elternteil nennt. Gibt zurueck, wie viele
    /// entfernt wurden.
    pub fn collect(&mut self) -> usize {
        let before = self.nodes.len();
        loop {
            let removable: Option<NodeId> = self
                .nodes
                .iter()
                .find(|n| {
                    !n.state.is_open()
                        && n.holds == 0
                        && !self
                            .nodes
                            .iter()
                            .any(|other| other.parents.iter().any(|p| *p == n.id))
                })
                .map(|n| n.id);
            match removable {
                Some(id) => self.nodes.retain(|n| n.id != id),
                None => break,
            }
        }
        before.saturating_sub(self.nodes.len())
    }
}

/// Was ein Abbruch bewirkt hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationOutcome {
    /// Die abgebrochenen Knoten — alle waren noch nicht gestartet.
    pub cancelled: ArrayVec<NodeId, MAX_NODES>,
    /// Die Knoten, die weiterlaufen.
    ///
    /// Sie werden **nicht** abgebrochen. Der Aufrufer kann ihr Ergebnis
    /// spaeter als [`NodeState::Superseded`] buchen — das ist eine Aussage
    /// ueber die Verwendbarkeit, keine ueber die GPU.
    pub still_running: ArrayVec<NodeId, MAX_NODES>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const DETECTOR: ModelIdx = ModelIdx(0);
    const DEPTH: ModelIdx = ModelIdx(1);
    const FUSION: ModelIdx = ModelIdx(2);

    fn capture(n: u64) -> CaptureId {
        CaptureId(n)
    }

    // -- Aufnahmen nicht mischen -------------------------------------------

    #[test]
    fn a_fusion_needs_a_common_capture() {
        // Der Fall, um den es geht: eine frische Detektion neben einer alten
        // Tiefenkarte ergibt eine Szene, die es nie gegeben hat.
        let mut g = Graph::new();
        let old_depth = g.insert(DEPTH, capture(1), EpochId(1), &[]).unwrap();
        let new_detection = g.insert(DETECTOR, capture(2), EpochId(1), &[]).unwrap();

        let error = g
            .insert(FUSION, capture(2), EpochId(1), &[new_detection, old_depth])
            .unwrap_err();
        assert_eq!(
            error,
            GraphError::MixedCaptures {
                left: capture(1),
                right: capture(2),
            }
        );
    }

    #[test]
    fn a_fusion_from_one_capture_is_accepted() {
        let mut g = Graph::new();
        let detection = g.insert(DETECTOR, capture(7), EpochId(1), &[]).unwrap();
        let depth = g.insert(DEPTH, capture(7), EpochId(1), &[]).unwrap();
        let fusion = g
            .insert(FUSION, capture(7), EpochId(1), &[detection, depth])
            .unwrap();
        assert_eq!(g.capture(fusion), Some(capture(7)));
    }

    #[test]
    fn crossing_captures_is_possible_but_must_be_said() {
        // Eine Bewegungsschaetzung braucht zwei Aufnahmen. Ausdruecklich und
        // benannt, damit es nicht die bequeme Umgehung der Regel wird.
        let mut g = Graph::new();
        let first = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        let second = g.insert(DETECTOR, capture(2), EpochId(1), &[]).unwrap();
        assert!(
            g.insert(FUSION, capture(2), EpochId(1), &[first, second])
                .is_err()
        );
        assert!(
            g.insert_across_captures(FUSION, capture(2), EpochId(1), &[first, second])
                .is_ok()
        );
    }

    #[test]
    fn an_epoch_change_is_not_bridged_even_across_captures() {
        // Eine geaenderte Ausgabesemantik ist auch ueber Aufnahmen hinweg ein
        // Problem (NV-10).
        let mut g = Graph::new();
        let old = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        assert_eq!(
            g.insert_across_captures(FUSION, capture(2), EpochId(2), &[old])
                .unwrap_err(),
            GraphError::MixedEpochs {
                left: EpochId(1),
                right: EpochId(2),
            }
        );
    }

    #[test]
    fn an_unknown_parent_is_refused() {
        let mut g = Graph::new();
        assert_eq!(
            g.insert(FUSION, capture(1), EpochId(1), &[NodeId(99)])
                .unwrap_err(),
            GraphError::UnknownParent { parent: NodeId(99) }
        );
    }

    // -- Gueltigkeit -------------------------------------------------------

    #[test]
    fn a_fusion_is_valid_only_when_all_parents_delivered() {
        let mut g = Graph::new();
        let detection = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        let depth = g.insert(DEPTH, capture(1), EpochId(1), &[]).unwrap();
        let fusion = g
            .insert(FUSION, capture(1), EpochId(1), &[detection, depth])
            .unwrap();

        for node in [detection, depth, fusion] {
            g.transition(node, NodeState::Running).unwrap();
        }
        g.transition(detection, NodeState::Completed).unwrap();
        g.transition(fusion, NodeState::Completed).unwrap();
        assert!(
            !g.is_valid(fusion),
            "die Tiefe fehlt noch; das Ergebnis waere halb"
        );

        g.transition(depth, NodeState::Completed).unwrap();
        assert!(g.is_valid(fusion));
    }

    #[test]
    fn a_failed_parent_makes_a_finished_child_invalid() {
        let mut g = Graph::new();
        let depth = g.insert(DEPTH, capture(1), EpochId(1), &[]).unwrap();
        let fusion = g.insert(FUSION, capture(1), EpochId(1), &[depth]).unwrap();
        g.transition(depth, NodeState::Running).unwrap();
        g.transition(fusion, NodeState::Running).unwrap();
        g.transition(fusion, NodeState::Completed).unwrap();
        g.transition(depth, NodeState::Failed).unwrap();
        assert!(!g.is_valid(fusion));
    }

    #[test]
    fn a_node_without_parents_is_valid_when_it_is_done() {
        let mut g = Graph::new();
        let detection = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        assert!(!g.is_valid(detection), "noch nicht gelaufen");
        g.transition(detection, NodeState::Running).unwrap();
        g.transition(detection, NodeState::Completed).unwrap();
        assert!(g.is_valid(detection));
    }

    // -- Referenzzaehlung --------------------------------------------------

    #[test]
    fn one_parent_with_two_consumers_stays_until_the_last_releases() {
        let mut g = Graph::new();
        let detection = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        g.transition(detection, NodeState::Running).unwrap();
        g.transition(detection, NodeState::Completed).unwrap();

        assert_eq!(g.acquire(detection).unwrap(), 1);
        assert_eq!(g.acquire(detection).unwrap(), 2);
        assert!(g.is_retained(detection));

        assert_eq!(g.release(detection).unwrap(), 1);
        assert!(
            g.is_retained(detection),
            "die erste Freigabe beendet nichts"
        );
        assert_eq!(g.release(detection).unwrap(), 0);
        assert!(!g.is_retained(detection));
    }

    #[test]
    fn a_release_without_a_hold_is_refused() {
        let mut g = Graph::new();
        let detection = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        assert_eq!(
            g.release(detection).unwrap_err(),
            GraphError::ReleaseWithoutHold { node: detection }
        );
    }

    #[test]
    fn a_retained_node_is_not_collected() {
        let mut g = Graph::new();
        let detection = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        g.transition(detection, NodeState::Running).unwrap();
        g.transition(detection, NodeState::Completed).unwrap();
        g.acquire(detection).unwrap();
        assert_eq!(g.collect(), 0);
        assert_eq!(g.len(), 1);
        g.release(detection).unwrap();
        assert_eq!(g.collect(), 1);
        assert!(g.is_empty());
    }

    #[test]
    fn a_parent_is_not_collected_while_a_child_names_it() {
        let mut g = Graph::new();
        let detection = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        let fusion = g
            .insert(FUSION, capture(1), EpochId(1), &[detection])
            .unwrap();
        g.transition(detection, NodeState::Running).unwrap();
        g.transition(detection, NodeState::Completed).unwrap();
        assert_eq!(g.collect(), 0, "das Kind ist noch offen");
        g.transition(fusion, NodeState::Running).unwrap();
        g.transition(fusion, NodeState::Completed).unwrap();
        assert_eq!(g.collect(), 2, "erst zusammen gehen beide");
    }

    // -- Laufende Arbeit nicht erfinden ------------------------------------

    #[test]
    fn a_running_child_is_not_cancelled() {
        // Dasselbe Argument wie in NV-00: ein Kind, das schon rechnet, als
        // storniert zu buchen waere eine Behauptung ueber die GPU, die
        // niemand belegen kann.
        let mut g = Graph::new();
        let depth = g.insert(DEPTH, capture(1), EpochId(1), &[]).unwrap();
        let running = g.insert(FUSION, capture(1), EpochId(1), &[depth]).unwrap();
        let pending = g.insert(FUSION, capture(1), EpochId(1), &[depth]).unwrap();
        g.transition(depth, NodeState::Running).unwrap();
        g.transition(running, NodeState::Running).unwrap();

        g.transition(depth, NodeState::Failed).unwrap();
        let outcome = g.cancel_dependents(depth);

        assert_eq!(outcome.cancelled.len(), 1);
        assert_eq!(outcome.cancelled.get(0), Some(&pending));
        assert_eq!(outcome.still_running.len(), 1);
        assert_eq!(outcome.still_running.get(0), Some(&running));
        assert_eq!(g.state(running), Some(NodeState::Running));
        assert_eq!(g.state(pending), Some(NodeState::Cancelled));
    }

    #[test]
    fn a_running_child_can_be_superseded_after_it_finishes() {
        let mut g = Graph::new();
        let depth = g.insert(DEPTH, capture(1), EpochId(1), &[]).unwrap();
        let child = g.insert(FUSION, capture(1), EpochId(1), &[depth]).unwrap();
        g.transition(depth, NodeState::Running).unwrap();
        g.transition(child, NodeState::Running).unwrap();
        g.transition(depth, NodeState::Failed).unwrap();
        let _ = g.cancel_dependents(depth);

        // Das Ergebnis kommt trotzdem und ist nur nicht zu gebrauchen.
        g.transition(child, NodeState::Completed).unwrap();
        assert!(!g.is_valid(child));
        g.transition(child, NodeState::Superseded).unwrap();
        assert_eq!(g.state(child), Some(NodeState::Superseded));
    }

    #[test]
    fn a_running_node_cannot_be_cancelled_directly() {
        let mut g = Graph::new();
        let node = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        g.transition(node, NodeState::Running).unwrap();
        assert_eq!(
            g.transition(node, NodeState::Cancelled).unwrap_err(),
            GraphError::InvalidTransition {
                from: NodeState::Running,
                to: NodeState::Cancelled,
            }
        );
    }

    #[test]
    fn a_completed_node_cannot_go_back_to_running() {
        let mut g = Graph::new();
        let node = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        g.transition(node, NodeState::Running).unwrap();
        g.transition(node, NodeState::Completed).unwrap();
        assert!(g.transition(node, NodeState::Running).is_err());
    }

    #[test]
    fn cancellation_reaches_grandchildren() {
        let mut g = Graph::new();
        let root = g.insert(DEPTH, capture(1), EpochId(1), &[]).unwrap();
        let child = g.insert(FUSION, capture(1), EpochId(1), &[root]).unwrap();
        let grandchild = g.insert(FUSION, capture(1), EpochId(1), &[child]).unwrap();
        g.transition(root, NodeState::Running).unwrap();
        g.transition(root, NodeState::Failed).unwrap();

        let outcome = g.cancel_dependents(root);
        assert_eq!(outcome.cancelled.len(), 2);
        assert_eq!(g.state(child), Some(NodeState::Cancelled));
        assert_eq!(g.state(grandchild), Some(NodeState::Cancelled));
    }

    #[test]
    fn cancellation_does_not_touch_a_sibling_branch() {
        let mut g = Graph::new();
        let failing = g.insert(DEPTH, capture(1), EpochId(1), &[]).unwrap();
        let healthy = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        let a = g
            .insert(FUSION, capture(1), EpochId(1), &[failing])
            .unwrap();
        let b = g
            .insert(FUSION, capture(1), EpochId(1), &[healthy])
            .unwrap();
        g.transition(failing, NodeState::Running).unwrap();
        g.transition(failing, NodeState::Failed).unwrap();

        let outcome = g.cancel_dependents(failing);
        assert_eq!(outcome.cancelled.get(0), Some(&a));
        assert_eq!(g.state(b), Some(NodeState::Pending));
    }

    // -- Grenzen ----------------------------------------------------------

    #[test]
    fn the_graph_is_bounded() {
        let mut g = Graph::new();
        for _ in 0..MAX_NODES {
            g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        }
        assert_eq!(g.len(), MAX_NODES);
        assert_eq!(
            g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap_err(),
            GraphError::Full {
                capacity: MAX_NODES
            }
        );
    }

    #[test]
    fn too_many_parents_are_refused() {
        let mut g = Graph::new();
        let mut parents = Vec::new();
        for _ in 0..=MAX_PARENTS {
            parents.push(g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap());
        }
        assert_eq!(
            g.insert(FUSION, capture(1), EpochId(1), &parents)
                .unwrap_err(),
            GraphError::TooManyParents {
                requested: parents.len()
            }
        );
    }

    #[test]
    fn node_ids_are_not_reused_after_collection() {
        // Sonst koennte ein spaeter Abschluss einen frischen Knoten treffen —
        // dasselbe Fencing-Argument wie bei den Slotkrediten (NV-00).
        let mut g = Graph::new();
        let first = g.insert(DETECTOR, capture(1), EpochId(1), &[]).unwrap();
        g.transition(first, NodeState::Running).unwrap();
        g.transition(first, NodeState::Completed).unwrap();
        assert_eq!(g.collect(), 1);
        let second = g.insert(DETECTOR, capture(2), EpochId(1), &[]).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn a_model_index_is_kept_for_diagnostics() {
        let mut g = Graph::new();
        let node = g.insert(DEPTH, capture(1), EpochId(1), &[]).unwrap();
        assert_eq!(g.model(node), Some(DEPTH));
    }
}
