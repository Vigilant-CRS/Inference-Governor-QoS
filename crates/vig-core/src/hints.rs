//! Anwendungshinweise innerhalb freigegebener Grenzen (NV-18).
//!
//! ## Worum es geht
//!
//! Die Anwendung weiss Dinge, die der Governor nicht wissen kann. Ein Roboter,
//! der geradeaus faehrt, braucht die Detektion dringender als einer, der
//! steht. Ein Greifvorgang hat einen Aktionshorizont: bis er abgeschlossen
//! ist, aendert eine neue Wahrnehmung nichts mehr an der Entscheidung.
//!
//! Diese Kenntnis in die Planung zu lassen ist wertvoll und gefaehrlich. Der
//! gefaehrliche Teil ist nicht der Fehler in der Anwendung — den gibt es
//! immer — sondern die Moeglichkeit, dass ein Hinweis eine Zusage
//! **lockert**, die der Betreiber gegeben hat.
//!
//! ## Die Regel dieses Moduls
//!
//! **Ein Hinweis darf verschaerfen, nie lockern.** „Ich brauche es dringender"
//! wird angenommen. „Ich brauche es weniger dringend" wird abgelehnt — es sei
//! denn, der Betreiber hat genau diesen Moduswechsel vorher freigegeben.
//!
//! **Ein Hinweis ohne gueltige Herkunft ist kein Hinweis.** Er traegt eine
//! Berechtigung, und wer sie nicht hat, wird nicht gehoert.
//!
//! **Ein Hinweis ohne Frist ist kein Hinweis.** Jeder traegt eine TTL. Nach
//! ihrem Ablauf gilt wieder der Grundvertrag — nicht der letzte bekannte
//! Zustand. Ein Sensor, der ausfaellt, waehrend er „alles ruhig" gemeldet hat,
//! darf nicht dauerhaft Ruhe bedeuten.
//!
//! **Widerspruch heisst Grundvertrag.** Zwei gueltige Hinweise mit
//! unvereinbarem Inhalt heben sich auf. Der Governor entscheidet nicht, welcher
//! der beiden recht hat — er hat dafuer keine Grundlage.
//!
//! ## Was hier ausdruecklich nicht passiert
//!
//! Der Governor **interpretiert die Welt nicht**. Er liest keine Sensordaten,
//! leitet keinen Zustand ab und trifft keine Sicherheitsentscheidung. Er nimmt
//! entgegen, was eine berechtigte Stelle ihm sagt, prueft es gegen die
//! Freigaben des Betreibers und wendet es an oder nicht. Eine „Confidence" aus
//! einem Modell ist hier kein Eingabewert; sie waere eine Zahl, deren
//! Zustandekommen der Governor nicht beurteilen kann.

use crate::ids::{MAX_MODELS, ModelIdx};
use crate::time::{Duration, Instant};

/// Wie viele Hinweise gleichzeitig gehalten werden: einer je Modell.
///
/// Mehr waere eine Warteschlange, und eine Queue fuer Hinweise waere die
/// falsche Struktur — ein Hinweis ist eine Aussage ueber jetzt, kein Auftrag.
pub const MAX_HINTS: usize = MAX_MODELS;

/// Wer einen Hinweis geben darf.
///
/// Bewusst eine undurchsichtige Kennung und kein Name: der Kern prueft
/// Gleichheit gegen die Freigabeliste des Betreibers, mehr nicht. Woher die
/// Kennung kommt und wie sie belegt wird — Token, mTLS-Zertifikat, Unix-Peer —
/// entscheidet die Schicht darueber (`gateway/auth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Authority(pub u64);

/// Was ein Hinweis aussagt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    /// Die Anwendung braucht diesen Strom fuer diese Dauer **nicht** frischer
    /// als angegeben.
    ///
    /// Der Aktionshorizont: bis der laufende Vorgang abgeschlossen ist, aendert
    /// eine neue Wahrnehmung nichts mehr. Das **lockert** und braucht deshalb
    /// eine Freigabe.
    ActionHorizon {
        /// Wie lange die aktuelle Entscheidung noch traegt.
        holds_for: Duration,
    },
    /// Die Anwendung braucht diesen Strom dringender.
    ///
    /// Verschaerft und ist deshalb ohne Moduswechsel-Freigabe zulaessig — die
    /// Zusage des Betreibers wird dadurch nicht schwaecher, nur teurer.
    Elevated {
        /// Das geforderte Hoechstalter, kuerzer als das vertragliche.
        max_age: Duration,
    },
    /// Ein vom Betreiber benannter Betriebsmodus.
    Mode {
        /// Die Kennung des Modus.
        id: u32,
    },
}

/// Ein Hinweis der Anwendung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hint {
    /// Fuer welchen Strom.
    pub model: ModelIdx,
    /// Wer ihn gegeben hat.
    pub authority: Authority,
    /// Was er aussagt.
    pub kind: HintKind,
    /// Wann er gegeben wurde.
    pub issued_at: Instant,
    /// Wie lange er gilt.
    ///
    /// Ohne Frist gibt es keinen Hinweis. Nach ihrem Ablauf gilt der
    /// Grundvertrag und nicht der letzte bekannte Zustand.
    pub ttl: Duration,
}

impl Hint {
    /// Wann dieser Hinweis verfaellt.
    ///
    /// `None`, wenn die Rechnung ueberlaeuft — ein Hinweis, dessen Ende sich
    /// nicht ausrechnen laesst, gilt als abgelaufen.
    #[must_use]
    pub fn expires_at(&self) -> Option<Instant> {
        self.issued_at.checked_add(self.ttl)
    }

    /// Ob er zu diesem Zeitpunkt noch gilt.
    #[must_use]
    pub fn is_fresh(&self, now: Instant) -> bool {
        // Ein Hinweis aus der Zukunft ist kein frischer Hinweis, sondern eine
        // Uhr, die nicht stimmt.
        if self.issued_at.as_nanos() > now.as_nanos() {
            return false;
        }
        self.expires_at()
            .is_some_and(|end| now.as_nanos() <= end.as_nanos())
    }

    /// Ob dieser Hinweis eine Zusage lockern wuerde.
    #[must_use]
    pub const fn loosens(&self) -> bool {
        // Weniger Frische zu verlangen macht die Zusage schwaecher. Ein
        // Moduswechsel kann beides und wird deshalb ebenfalls immer gegen die
        // Freigabeliste geprueft. Nur das Verschaerfen ist fuer sich sicher.
        !matches!(self.kind, HintKind::Elevated { .. })
    }
}

/// Was der Betreiber erlaubt hat.
///
/// Die Freigabe ist eine Aussage des Betreibers und wird nie aus Messwerten
/// oder aus dem Verhalten der Anwendung abgeleitet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HintPolicy {
    /// Wer ueberhaupt gehoert wird. `None` heisst: niemand.
    pub authority: Option<Authority>,
    /// Ob lockernde Hinweise angenommen werden duerfen.
    pub allow_loosening: bool,
    /// Die freigegebenen Modus-Kennungen, als Maske.
    ///
    /// Ein Modus ausserhalb dieser Maske wird abgelehnt, auch von einer
    /// berechtigten Stelle.
    pub approved_modes: u32,
    /// Das kuerzeste Hoechstalter, das ein Hinweis fordern darf.
    ///
    /// Ohne Untergrenze koennte eine Anwendung durch immer schaerfere
    /// Forderungen die gesamte Kapazitaet auf sich ziehen — verschaerfen ist
    /// sicher fuer die **Zusage** und nicht fuer die **Nachbarn**.
    pub min_max_age: Option<Duration>,
    /// Die laengste Dauer, die ein Aktionshorizont beanspruchen darf.
    pub max_action_horizon: Option<Duration>,
}

impl HintPolicy {
    /// Eine Policy, die nichts annimmt.
    ///
    /// Die Voreinstellung. Wer Hinweise zulassen will, sagt es.
    #[must_use]
    pub const fn closed() -> Self {
        Self {
            authority: None,
            allow_loosening: false,
            approved_modes: 0,
            min_max_age: None,
            max_action_horizon: None,
        }
    }

    /// Ob diese Modus-Kennung freigegeben ist.
    #[must_use]
    pub const fn mode_approved(&self, id: u32) -> bool {
        if id >= 32 {
            return false;
        }
        self.approved_modes & (1_u32 << id) != 0
    }
}

/// Warum ein Hinweis nicht gilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Der Betreiber hat keine berechtigte Stelle benannt.
    NoAuthorityConfigured,
    /// Die Stelle ist nicht die benannte.
    WrongAuthority {
        /// Wer es versucht hat.
        got: Authority,
    },
    /// Die Frist ist abgelaufen — oder der Hinweis stammt aus der Zukunft.
    Stale,
    /// Er wuerde eine Zusage lockern, und das ist nicht freigegeben.
    WouldLoosen,
    /// Der Modus ist nicht freigegeben.
    ModeNotApproved {
        /// Welcher.
        id: u32,
    },
    /// Die Forderung liegt ausserhalb der vom Betreiber gesetzten Grenzen.
    OutOfBounds,
    /// Zwei gueltige Hinweise widersprechen sich.
    ///
    /// Der Governor entscheidet nicht, welcher recht hat — er hat dafuer keine
    /// Grundlage.
    Conflicting,
    /// Der Hinweis nennt ein Modell, das es nicht gibt.
    ///
    /// Ihn anzunehmen und wirkungslos abzulegen waere die unangenehmere
    /// Variante: der Zaehler saehe gut aus, und niemand faende den Tippfehler.
    UnknownModel {
        /// Die genannte Kennung.
        model: ModelIdx,
    },
}

/// Was aus den Hinweisen fuer einen Strom folgt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Nichts. Der Grundvertrag gilt unveraendert.
    BaseContract,
    /// Das Hoechstalter wird auf diesen Wert **verschaerft**.
    TightenMaxAge {
        /// Der neue, kuerzere Wert.
        max_age: Duration,
    },
    /// Das Hoechstalter darf bis zu diesem Zeitpunkt gelockert bleiben.
    RelaxUntil {
        /// Bis wann.
        until: Instant,
    },
    /// Ein freigegebener Betriebsmodus ist aktiv.
    Mode {
        /// Welcher.
        id: u32,
        /// Bis wann.
        until: Instant,
    },
}

/// Die Hinweise eines Governors, begrenzt und nach Frist verfallend.
#[derive(Debug)]
pub struct Hints {
    policy: HintPolicy,
    /// Je Modell hoechstens ein angenommener Hinweis.
    ///
    /// Ein neuer ersetzt den alten. Zwei gleichzeitig gueltige mit
    /// unvereinbarem Inhalt gaebe es damit gar nicht erst — bis auf den Fall,
    /// den [`Hints::offer`] als [`Rejection::Conflicting`] meldet: derselbe
    /// Zeitpunkt, verschiedene Aussage.
    slots: Vec<Option<Hint>>,
    accepted: u64,
    rejected: u64,
}

impl Default for Hints {
    fn default() -> Self {
        Self::new(HintPolicy::closed())
    }
}

impl Hints {
    /// Ein Hinweisspeicher mit dieser Freigabe.
    #[must_use]
    pub fn new(policy: HintPolicy) -> Self {
        Self {
            policy,
            slots: vec![None; MAX_MODELS],
            accepted: 0,
            rejected: 0,
        }
    }

    /// Die geltende Freigabe.
    #[must_use]
    pub const fn policy(&self) -> &HintPolicy {
        &self.policy
    }

    /// Wie viele Hinweise angenommen wurden.
    #[must_use]
    pub const fn accepted_total(&self) -> u64 {
        self.accepted
    }

    /// Wie viele abgelehnt wurden.
    #[must_use]
    pub const fn rejected_total(&self) -> u64 {
        self.rejected
    }

    /// Bietet einen Hinweis an.
    ///
    /// # Errors
    ///
    /// Siehe [`Rejection`]. Ein abgelehnter Hinweis aendert nichts — auch
    /// nicht den zuvor angenommenen.
    pub fn offer(&mut self, hint: Hint, now: Instant) -> Result<Effect, Rejection> {
        let verdict = self.evaluate(hint, now);
        match verdict {
            Ok(effect) => {
                self.accepted = self.accepted.saturating_add(1);
                if let Some(slot) = self.slots.get_mut(hint.model.get()) {
                    *slot = Some(hint);
                }
                Ok(effect)
            }
            Err(reason) => {
                self.rejected = self.rejected.saturating_add(1);
                Err(reason)
            }
        }
    }

    /// Prueft einen Hinweis, ohne ihn anzunehmen.
    ///
    /// # Errors
    ///
    /// Siehe [`Rejection`].
    pub fn evaluate(&self, hint: Hint, now: Instant) -> Result<Effect, Rejection> {
        if hint.model.get() >= self.slots.len() {
            return Err(Rejection::UnknownModel { model: hint.model });
        }
        let Some(expected) = self.policy.authority else {
            return Err(Rejection::NoAuthorityConfigured);
        };
        if hint.authority != expected {
            return Err(Rejection::WrongAuthority {
                got: hint.authority,
            });
        }
        if !hint.is_fresh(now) {
            return Err(Rejection::Stale);
        }
        let Some(until) = hint.expires_at() else {
            return Err(Rejection::Stale);
        };

        // Erst den Hinweis fuer sich pruefen, dann gegen den bestehenden.
        // Andersherum bekaeme ein unfreigegebener Modus die Meldung
        // „Widerspruch" statt seiner eigenen — und der Betreiber suchte den
        // Fehler an der falschen Stelle.
        let effect = self.evaluate_alone(hint, until)?;

        // Ein zweiter, gleich frischer Hinweis mit anderer Aussage ist ein
        // Widerspruch. Der Governor entscheidet nicht, welcher recht hat.
        if let Some(Some(existing)) = self.slots.get(hint.model.get())
            && existing.is_fresh(now)
            && existing.kind != hint.kind
            && existing.issued_at.as_nanos() == hint.issued_at.as_nanos()
        {
            return Err(Rejection::Conflicting);
        }

        Ok(effect)
    }

    /// Prueft einen Hinweis ohne Blick auf die bereits angenommenen.
    fn evaluate_alone(&self, hint: Hint, until: Instant) -> Result<Effect, Rejection> {
        match hint.kind {
            HintKind::Elevated { max_age } => {
                // Verschaerfen ist sicher fuer die Zusage — aber nicht
                // unbegrenzt, sonst zieht ein Strom die ganze Kapazitaet.
                if self
                    .policy
                    .min_max_age
                    .is_some_and(|floor| max_age.as_nanos() < floor.as_nanos())
                {
                    return Err(Rejection::OutOfBounds);
                }
                Ok(Effect::TightenMaxAge { max_age })
            }
            HintKind::ActionHorizon { holds_for } => {
                if !self.policy.allow_loosening {
                    return Err(Rejection::WouldLoosen);
                }
                if self
                    .policy
                    .max_action_horizon
                    .is_some_and(|cap| holds_for.as_nanos() > cap.as_nanos())
                {
                    return Err(Rejection::OutOfBounds);
                }
                // Die Lockerung endet mit dem **frueheren** von Horizont und
                // TTL. Ein Hinweis kann nicht laenger wirken, als er gilt.
                let horizon = hint
                    .issued_at
                    .checked_add(holds_for)
                    .ok_or(Rejection::OutOfBounds)?;
                Ok(Effect::RelaxUntil {
                    until: if horizon.as_nanos() < until.as_nanos() {
                        horizon
                    } else {
                        until
                    },
                })
            }
            HintKind::Mode { id } => {
                if !self.policy.mode_approved(id) {
                    return Err(Rejection::ModeNotApproved { id });
                }
                Ok(Effect::Mode { id, until })
            }
        }
    }

    /// Was fuer diesen Strom jetzt gilt.
    ///
    /// Nach Ablauf der Frist der Grundvertrag — **nicht** der letzte bekannte
    /// Zustand.
    #[must_use]
    pub fn effect(&self, model: ModelIdx, now: Instant) -> Effect {
        let Some(Some(accepted)) = self.slots.get(model.get()) else {
            return Effect::BaseContract;
        };
        if !accepted.is_fresh(now) {
            return Effect::BaseContract;
        }
        self.evaluate(*accepted, now)
            .unwrap_or(Effect::BaseContract)
    }

    /// Das wirksame Hoechstalter fuer einen Strom.
    ///
    /// Der Grundvertrag ist die Obergrenze fuer Lockerung und die Ausgangslage
    /// fuer Verschaerfung. Ein Hinweis kann ihn nie ueberschreiten, ausser die
    /// Lockerung ist freigegeben — und auch dann nur bis zu ihrer Frist.
    #[must_use]
    pub fn effective_max_age(
        &self,
        model: ModelIdx,
        contract_max_age: Option<Duration>,
        now: Instant,
    ) -> Option<Duration> {
        match self.effect(model, now) {
            Effect::BaseContract | Effect::Mode { .. } => contract_max_age,
            Effect::TightenMaxAge { max_age } => Some(match contract_max_age {
                Some(base) => base.min(max_age),
                None => max_age,
            }),
            Effect::RelaxUntil { until } => {
                // Gelockert heisst: das Hoechstalter reicht bis zum Ende des
                // Horizonts — und **nicht**, dass es keines mehr gibt.
                //
                // „Kein Hoechstalter" waere die bequeme Lesart und die falsche:
                // dann wird bis zum Ende des Horizonts nichts mehr als veraltet
                // verworfen, und die GPU rechnet ausgerechnet in dem Fenster am
                // meisten Altlast, in dem niemand hinsieht. Mit einer Obergrenze
                // faellt Arbeit weiterhin heraus, sobald sie aelter ist als der
                // Horizont noch dauert.
                //
                // Nach dem Horizont gilt wieder der Grundvertrag, ohne dass
                // jemand etwas zuruecknehmen muss.
                if now.as_nanos() > until.as_nanos() {
                    return contract_max_age;
                }
                let remaining = until.saturating_since(now);
                Some(match contract_max_age {
                    // Nie kuerzer als der Vertrag: eine Lockerung, die
                    // verschaerft, waere ein Widerspruch in sich.
                    Some(base) => base.max(remaining),
                    None => remaining,
                })
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const APP: Authority = Authority(0x1234);
    const FREMD: Authority = Authority(0x9999);
    const M: ModelIdx = ModelIdx(0);

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    fn at(v: u64) -> Instant {
        Instant::from_nanos(v.saturating_mul(1_000_000))
    }

    fn open_policy() -> HintPolicy {
        HintPolicy {
            authority: Some(APP),
            allow_loosening: true,
            approved_modes: 0b0000_0110, // Modi 1 und 2
            min_max_age: Some(ms(10)),
            max_action_horizon: Some(ms(500)),
        }
    }

    fn hint(kind: HintKind, issued: u64, ttl: u64) -> Hint {
        Hint {
            model: M,
            authority: APP,
            kind,
            issued_at: at(issued),
            ttl: ms(ttl),
        }
    }

    // -- Voreinstellung: nichts wird angenommen ---------------------------

    #[test]
    fn the_default_policy_hears_nobody() {
        let mut h = Hints::default();
        assert_eq!(
            h.offer(hint(HintKind::Elevated { max_age: ms(20) }, 0, 100), at(0)),
            Err(Rejection::NoAuthorityConfigured),
            "wer Hinweise zulassen will, sagt es"
        );
        assert_eq!(h.effect(M, at(0)), Effect::BaseContract);
    }

    #[test]
    fn an_unauthorised_source_is_not_heard() {
        let mut h = Hints::new(open_policy());
        let mut fremd = hint(HintKind::Elevated { max_age: ms(20) }, 0, 100);
        fremd.authority = FREMD;
        assert_eq!(
            h.offer(fremd, at(0)),
            Err(Rejection::WrongAuthority { got: FREMD })
        );
        assert_eq!(h.effect(M, at(0)), Effect::BaseContract);
    }

    // -- Verschaerfen ist erlaubt, lockern nicht --------------------------

    #[test]
    fn tightening_is_accepted_without_a_mode_approval() {
        let mut h = Hints::new(HintPolicy {
            allow_loosening: false,
            ..open_policy()
        });
        assert_eq!(
            h.offer(hint(HintKind::Elevated { max_age: ms(20) }, 0, 100), at(0)),
            Ok(Effect::TightenMaxAge { max_age: ms(20) }),
            "die Zusage wird dadurch nicht schwaecher, nur teurer"
        );
    }

    #[test]
    fn loosening_without_approval_is_refused() {
        let mut h = Hints::new(HintPolicy {
            allow_loosening: false,
            ..open_policy()
        });
        assert_eq!(
            h.offer(
                hint(HintKind::ActionHorizon { holds_for: ms(200) }, 0, 300),
                at(0)
            ),
            Err(Rejection::WouldLoosen)
        );
        assert_eq!(h.effect(M, at(0)), Effect::BaseContract);
    }

    #[test]
    fn loosening_with_approval_is_bounded_by_its_own_deadline() {
        let mut h = Hints::new(open_policy());
        // Horizont 200 ms, TTL 300 ms -> der Horizont ist frueher.
        assert_eq!(
            h.offer(
                hint(HintKind::ActionHorizon { holds_for: ms(200) }, 0, 300),
                at(0)
            ),
            Ok(Effect::RelaxUntil { until: at(200) })
        );
        // Umgekehrt: TTL 50 ms, Horizont 200 ms -> die TTL gewinnt.
        let mut h2 = Hints::new(open_policy());
        assert_eq!(
            h2.offer(
                hint(HintKind::ActionHorizon { holds_for: ms(200) }, 0, 50),
                at(0)
            ),
            Ok(Effect::RelaxUntil { until: at(50) }),
            "ein Hinweis kann nicht laenger wirken, als er gilt"
        );
    }

    // -- Fristen ----------------------------------------------------------

    #[test]
    fn an_expired_hint_yields_the_base_contract_not_the_last_state() {
        let mut h = Hints::new(open_policy());
        h.offer(hint(HintKind::Elevated { max_age: ms(20) }, 0, 100), at(0))
            .unwrap();
        assert_eq!(
            h.effect(M, at(100)),
            Effect::TightenMaxAge { max_age: ms(20) },
            "genau auf der Frist gilt er noch"
        );
        assert_eq!(
            h.effect(M, at(101)),
            Effect::BaseContract,
            "ein Sensor, der ausfaellt, waehrend er Ruhe meldete, darf nicht \
             dauerhaft Ruhe bedeuten"
        );
    }

    #[test]
    fn a_hint_offered_after_its_deadline_is_refused() {
        let mut h = Hints::new(open_policy());
        assert_eq!(
            h.offer(hint(HintKind::Elevated { max_age: ms(20) }, 0, 10), at(50)),
            Err(Rejection::Stale)
        );
    }

    #[test]
    fn a_hint_from_the_future_is_not_fresh() {
        let mut h = Hints::new(open_policy());
        assert_eq!(
            h.offer(
                hint(HintKind::Elevated { max_age: ms(20) }, 1_000, 100),
                at(0)
            ),
            Err(Rejection::Stale),
            "das ist eine Uhr, die nicht stimmt, und kein frischer Hinweis"
        );
    }

    #[test]
    fn a_hint_whose_deadline_overflows_counts_as_stale() {
        let mut h = Hints::new(open_policy());
        let broken = Hint {
            model: M,
            authority: APP,
            kind: HintKind::Elevated { max_age: ms(20) },
            issued_at: Instant::from_nanos(u64::MAX - 5),
            ttl: Duration::from_nanos_unbounded(u64::MAX),
        };
        assert_eq!(
            h.offer(broken, Instant::from_nanos(u64::MAX - 4)),
            Err(Rejection::Stale)
        );
    }

    // -- Grenzen des Betreibers -------------------------------------------

    #[test]
    fn tightening_below_the_operator_floor_is_refused() {
        // Verschaerfen ist sicher fuer die Zusage und nicht fuer die Nachbarn:
        // ohne Untergrenze zieht ein Strom die ganze Kapazitaet auf sich.
        let mut h = Hints::new(open_policy());
        assert_eq!(
            h.offer(hint(HintKind::Elevated { max_age: ms(5) }, 0, 100), at(0)),
            Err(Rejection::OutOfBounds)
        );
        assert_eq!(
            h.offer(hint(HintKind::Elevated { max_age: ms(10) }, 0, 100), at(0)),
            Ok(Effect::TightenMaxAge { max_age: ms(10) }),
            "genau auf der Grenze noch zulaessig"
        );
    }

    #[test]
    fn an_over_long_action_horizon_is_refused() {
        let mut h = Hints::new(open_policy());
        assert_eq!(
            h.offer(
                hint(HintKind::ActionHorizon { holds_for: ms(501) }, 0, 1_000),
                at(0)
            ),
            Err(Rejection::OutOfBounds)
        );
    }

    // -- Modi --------------------------------------------------------------

    #[test]
    fn only_approved_modes_are_accepted() {
        let mut h = Hints::new(open_policy());
        assert_eq!(
            h.offer(hint(HintKind::Mode { id: 1 }, 0, 100), at(0)),
            Ok(Effect::Mode {
                id: 1,
                until: at(100)
            })
        );
        assert_eq!(
            h.offer(hint(HintKind::Mode { id: 3 }, 0, 100), at(0)),
            Err(Rejection::ModeNotApproved { id: 3 })
        );
    }

    #[test]
    fn a_mode_beyond_the_mask_width_is_refused() {
        let mut h = Hints::new(HintPolicy {
            approved_modes: u32::MAX,
            ..open_policy()
        });
        assert_eq!(
            h.offer(hint(HintKind::Mode { id: 32 }, 0, 100), at(0)),
            Err(Rejection::ModeNotApproved { id: 32 })
        );
    }

    #[test]
    fn a_refused_hint_does_not_disturb_the_accepted_one() {
        let mut h = Hints::new(open_policy());
        h.offer(hint(HintKind::Elevated { max_age: ms(20) }, 0, 100), at(0))
            .unwrap();
        let _ = h.offer(hint(HintKind::Mode { id: 7 }, 10, 100), at(10));
        assert_eq!(
            h.effect(M, at(10)),
            Effect::TightenMaxAge { max_age: ms(20) },
            "ein abgelehnter Hinweis aendert nichts, auch nicht den vorigen"
        );
        assert_eq!(h.accepted_total(), 1);
        assert_eq!(h.rejected_total(), 1);
    }

    // -- Widerspruch -------------------------------------------------------

    #[test]
    fn two_contradicting_hints_at_the_same_instant_cancel_out() {
        let mut h = Hints::new(open_policy());
        h.offer(hint(HintKind::Elevated { max_age: ms(20) }, 0, 100), at(0))
            .unwrap();
        assert_eq!(
            h.offer(
                hint(HintKind::ActionHorizon { holds_for: ms(200) }, 0, 300),
                at(0)
            ),
            Err(Rejection::Conflicting),
            "der Governor entscheidet nicht, welcher recht hat"
        );
        assert_eq!(
            h.effect(M, at(0)),
            Effect::TightenMaxAge { max_age: ms(20) }
        );
    }

    #[test]
    fn a_later_hint_replaces_an_earlier_one() {
        let mut h = Hints::new(open_policy());
        h.offer(hint(HintKind::Elevated { max_age: ms(20) }, 0, 100), at(0))
            .unwrap();
        h.offer(
            hint(HintKind::Elevated { max_age: ms(15) }, 10, 100),
            at(10),
        )
        .unwrap();
        assert_eq!(
            h.effect(M, at(10)),
            Effect::TightenMaxAge { max_age: ms(15) }
        );
    }

    // -- Wirksames Hoechstalter -------------------------------------------

    #[test]
    fn the_contract_stays_the_ceiling_for_tightening() {
        let mut h = Hints::new(open_policy());
        h.offer(hint(HintKind::Elevated { max_age: ms(20) }, 0, 100), at(0))
            .unwrap();
        assert_eq!(
            h.effective_max_age(M, Some(ms(66)), at(0)),
            Some(ms(20)),
            "der Hinweis verschaerft"
        );
        // Und er kann den Vertrag nicht ueberschreiten.
        let mut h2 = Hints::new(open_policy());
        h2.offer(hint(HintKind::Elevated { max_age: ms(80) }, 0, 100), at(0))
            .unwrap();
        assert_eq!(
            h2.effective_max_age(M, Some(ms(66)), at(0)),
            Some(ms(66)),
            "ein 'Hoechstalter 80' lockert einen Vertrag mit 66 nicht"
        );
    }

    #[test]
    fn a_relaxation_extends_the_ceiling_and_never_removes_it() {
        let mut h = Hints::new(open_policy());
        h.offer(
            hint(HintKind::ActionHorizon { holds_for: ms(200) }, 0, 300),
            at(0),
        )
        .unwrap();
        // Am Anfang des Horizonts reicht das Hoechstalter bis zu seinem Ende.
        assert_eq!(h.effective_max_age(M, Some(ms(66)), at(0)), Some(ms(200)));
        // Es schrumpft mit ihm — und faellt nie unter den Vertrag.
        assert_eq!(h.effective_max_age(M, Some(ms(66)), at(100)), Some(ms(100)));
        assert_eq!(
            h.effective_max_age(M, Some(ms(66)), at(180)),
            Some(ms(66)),
            "eine Lockerung, die verschaerft, waere ein Widerspruch in sich"
        );
        assert_eq!(
            h.effective_max_age(M, Some(ms(66)), at(201)),
            Some(ms(66)),
            "danach gilt wieder der Grundvertrag"
        );
    }

    #[test]
    fn a_relaxation_never_switches_freshness_off_entirely() {
        // Der bequeme Fehler waere „kein Hoechstalter": dann wird im Fenster,
        // in dem niemand hinsieht, gar nichts mehr verworfen.
        let mut h = Hints::new(open_policy());
        h.offer(
            hint(HintKind::ActionHorizon { holds_for: ms(400) }, 0, 500),
            at(0),
        )
        .unwrap();
        for t in [0, 1, 100, 399] {
            assert!(
                h.effective_max_age(M, Some(ms(66)), at(t)).is_some(),
                "bei t={t} gab es kein Hoechstalter mehr"
            );
        }
    }

    #[test]
    fn a_tightened_policy_invalidates_an_already_accepted_hint() {
        let mut h = Hints::new(open_policy());
        h.offer(hint(HintKind::Mode { id: 1 }, 0, 1_000), at(0))
            .unwrap();
        assert_eq!(
            h.effect(M, at(10)),
            Effect::Mode {
                id: 1,
                until: at(1_000)
            }
        );

        // Der Betreiber nimmt Modus 1 aus der Freigabe.
        let mut h2 = Hints::new(HintPolicy {
            approved_modes: 0b0000_0100,
            ..open_policy()
        });
        h2.slots[M.get()] = Some(hint(HintKind::Mode { id: 1 }, 0, 1_000));
        assert_eq!(
            h2.effect(M, at(10)),
            Effect::BaseContract,
            "eine engere Freigabe wirkt sofort, nicht erst nach Ablauf der TTL"
        );
    }

    #[test]
    fn a_hint_for_a_model_that_does_not_exist_is_refused() {
        let mut h = Hints::new(open_policy());
        let far = ModelIdx(u16::try_from(MAX_MODELS).unwrap_or(0));
        assert_eq!(
            h.offer(
                Hint {
                    model: far,
                    ..hint(HintKind::Elevated { max_age: ms(20) }, 0, 100)
                },
                at(0)
            ),
            Err(Rejection::UnknownModel { model: far }),
            "annehmen und wirkungslos ablegen hiesse, den Tippfehler zu verstecken"
        );
        assert_eq!(h.accepted_total(), 0);
    }

    #[test]
    fn a_mode_does_not_touch_the_max_age_by_itself() {
        let mut h = Hints::new(open_policy());
        h.offer(hint(HintKind::Mode { id: 2 }, 0, 100), at(0))
            .unwrap();
        assert_eq!(
            h.effective_max_age(M, Some(ms(66)), at(0)),
            Some(ms(66)),
            "ein Modus ist eine Ansage, keine Vertragsaenderung"
        );
    }

    #[test]
    fn a_model_without_a_hint_gets_the_base_contract() {
        let h = Hints::new(open_policy());
        assert_eq!(h.effect(ModelIdx(3), at(0)), Effect::BaseContract);
        assert_eq!(
            h.effective_max_age(ModelIdx(3), Some(ms(66)), at(0)),
            Some(ms(66))
        );
    }

    #[test]
    fn hints_for_one_model_do_not_reach_another() {
        let mut h = Hints::new(open_policy());
        h.offer(hint(HintKind::Elevated { max_age: ms(20) }, 0, 100), at(0))
            .unwrap();
        assert_eq!(h.effect(ModelIdx(1), at(0)), Effect::BaseContract);
    }
}
