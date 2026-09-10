//! Langsame, optionale Aktuation — und warum sie fast nie eingeschaltet wird
//! (NV-13).
//!
//! ## Der Widerspruch zu ADR-0021, ausgesprochen
//!
//! ADR-0021 sagt: die Hardware wird gelesen, nie gestellt. Ein Governor, der
//! Takte verstellt, braucht Rechte, die ein Governor nicht haben sollte, und
//! macht jede Messung des Betreibers zu einer Messung des Governors.
//!
//! Dieses Modul stellt. Es hebt ADR-0021 nicht auf, sondern grenzt eine
//! Ausnahme ein:
//!
//! * **Ausdrueckliches Opt-in.** Ohne [`Actuation::enable`] ist der Regler ein
//!   teurer `NoOp`. Die Voreinstellung ist [`NoActuator`], und der lehnt jede
//!   Anforderung ab.
//! * **Getrennt vom Rest.** Der lesende Pfad (NV-04) braucht kein Root und
//!   bleibt davon unberuehrt. Wer die Aktuation nicht einschaltet, hat einen
//!   Governor ohne jede Stellbefugnis.
//! * **Beobachtet statt angenommen.** Eine Anforderung gilt erst als wirksam,
//!   wenn ein **Lesen** sie bestaetigt. Ein `nvidia-smi -lgc`, das mit Code 0
//!   zurueckkehrt, hat nichts bewiesen: der Treiber kann die Vorgabe
//!   stillschweigend beschneiden, ein anderer Prozess kann sie ueberschreiben,
//!   und ein thermisches Limit sticht ohnehin.
//!
//! ## Warum ein Regler und keine Stellschraube
//!
//! Der Takt ist eine **globale** Stellgroesse. Er wirkt auf jede Arbeit auf
//! der Karte, auch auf die, die dieser Governor nicht kennt. Drei Vorkehrungen
//! folgen daraus:
//!
//! * **Vorlauf.** Eine Aenderung wird angefordert, bevor sie gebraucht wird,
//!   nicht wenn es knapp ist. Takte brauchen Zeit, und ein Regler, der erst
//!   bei Bedarf stellt, kommt immer zu spaet.
//! * **Verweildauer und Hysterese.** Ohne sie pendelt der Regler, und
//!   Pendeln kostet mehr als der schlechtere Betriebspunkt.
//! * **Zugesagte Betriebsbereiche sind tabu.** Ein Profil wurde bei einem
//!   bestimmten Takt gemessen; eine Zusage, die darauf beruht, darf nicht
//!   durch eine Taktsenkung gebrochen werden. Wer sie senken will, muss
//!   vorher die Zusage aufgeben — nicht umgekehrt.

use crate::snapshot::HardwareSnapshot;

/// Ein Takt in MHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockMhz(pub u32);

/// Warum eine Aktuation nicht stattfand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActuationError {
    /// Die Aktuation ist nicht eingeschaltet.
    ///
    /// Die Voreinstellung, und in den allermeisten Installationen der
    /// richtige Zustand.
    Disabled,
    /// Der Betreiber hat keine Rechte fuer diese Stellgroesse.
    NotPermitted {
        /// Was das System gemeldet hat.
        detail: String,
    },
    /// Der Zieltakt liegt ausserhalb dessen, was die Plattform zulaesst.
    OutOfPlatformRange {
        /// Der geforderte Takt.
        requested: ClockMhz,
        /// Die Untergrenze.
        min: ClockMhz,
        /// Die Obergrenze.
        max: ClockMhz,
    },
    /// Der Zieltakt wuerde einen bereits zugesagten Betriebsbereich brechen.
    ///
    /// Wer den Takt darunter senken will, muss vorher die Zusage aufgeben.
    /// Andersherum waere es eine stille Vertragsverletzung.
    WouldBreakPromise {
        /// Der geforderte Takt.
        requested: ClockMhz,
        /// Der niedrigste Takt, bei dem noch alle Zusagen gelten.
        promised_floor: ClockMhz,
    },
    /// Die Verweildauer seit der letzten Aenderung ist noch nicht um.
    Dwelling {
        /// Wie lange noch, in Millisekunden.
        remaining_ms: u64,
    },
    /// Der **beobachtete** Takt liegt unter dem zugesagten Boden.
    ///
    /// Die Toleranz federt ab, dass Karten auf ihre eigenen Taktstufen runden.
    /// Sie darf keine Zusage abfedern: 1470 MHz beobachtet bei 1500 MHz
    /// zugesagtem Boden ist innerhalb von 50 MHz Toleranz — und trotzdem ein
    /// Betriebspunkt, fuer den kein Profil gemessen wurde (Review R07).
    ///
    /// Der Unterschied zu [`Self::WouldBreakPromise`]: dort war schon die
    /// **Anforderung** unzulaessig, hier war sie in Ordnung und die Karte
    /// liefert etwas anderes.
    ObservedBelowPromise {
        /// Was beobachtet wurde.
        observed: ClockMhz,
        /// Der niedrigste Takt, bei dem noch alle Zusagen gelten.
        promised_floor: ClockMhz,
    },
    /// Ein anderer Prozess haelt die Stellbefugnis.
    NotExclusive {
        /// Was beobachtet wurde.
        detail: String,
    },
    /// Die Anforderung wurde abgesetzt, aber nicht beobachtet.
    ///
    /// Der gefaehrlichste Fall: das Kommando kam mit Erfolg zurueck, und die
    /// Karte laeuft trotzdem anders. Ein Regler, der das nicht bemerkt, plant
    /// auf einem Betriebspunkt, den es nicht gibt.
    NotObserved {
        /// Was angefordert wurde.
        requested: ClockMhz,
        /// Was tatsaechlich anliegt.
        observed: Option<ClockMhz>,
    },
}

impl core::fmt::Display for ActuationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Disabled => write!(f, "Aktuation ist nicht eingeschaltet"),
            Self::NotPermitted { detail } => write!(f, "keine Stellbefugnis: {detail}"),
            Self::OutOfPlatformRange {
                requested,
                min,
                max,
            } => write!(
                f,
                "Zieltakt {} MHz ausserhalb von {}..{} MHz",
                requested.0, min.0, max.0
            ),
            Self::WouldBreakPromise {
                requested,
                promised_floor,
            } => write!(
                f,
                "Zieltakt {} MHz unter dem zugesagten Boden {} MHz; erst die \
                 Zusage aufgeben, dann senken",
                requested.0, promised_floor.0
            ),
            Self::ObservedBelowPromise {
                observed,
                promised_floor,
            } => write!(
                f,
                "beobachtet {} MHz, unter dem zugesagten Boden {} MHz; die \
                 Toleranz federt Rundung ab, keine Zusage",
                observed.0, promised_floor.0
            ),
            Self::Dwelling { remaining_ms } => {
                write!(f, "Verweildauer laeuft noch {remaining_ms} ms")
            }
            Self::NotExclusive { detail } => {
                write!(f, "Stellbefugnis nicht exklusiv: {detail}")
            }
            Self::NotObserved {
                requested,
                observed,
            } => write!(
                f,
                "angefordert {} MHz, beobachtet {}; die Anforderung gilt als \
                 nicht wirksam",
                requested.0,
                observed.map_or_else(|| "nichts".to_owned(), |c| format!("{} MHz", c.0))
            ),
        }
    }
}

impl core::error::Error for ActuationError {}

/// Etwas, das den Takt einer Karte stellen kann.
///
/// Bewusst schmal: eine Stellgroesse, ein Zuruecksetzen. Jede weitere waere
/// eine weitere Befugnis, und Befugnisse sind hier der teure Teil.
pub trait Actuator: core::fmt::Debug + Send {
    /// Fordert einen Takt an.
    ///
    /// **Ein Erfolg hier ist kein Nachweis.** Ob die Karte den Takt annimmt,
    /// entscheidet erst das Lesen.
    ///
    /// # Errors
    ///
    /// Wenn das Kommando fehlschlaegt oder die Rechte fehlen.
    fn request(&mut self, clock: ClockMhz) -> Result<(), ActuationError>;

    /// Nimmt die eigene Vorgabe zurueck.
    ///
    /// # Errors
    ///
    /// Wenn das Kommando fehlschlaegt.
    fn restore(&mut self) -> Result<(), ActuationError>;
}

/// Der Regler ohne Stellbefugnis — die Voreinstellung.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoActuator;

impl Actuator for NoActuator {
    fn request(&mut self, _clock: ClockMhz) -> Result<(), ActuationError> {
        Err(ActuationError::Disabled)
    }

    fn restore(&mut self) -> Result<(), ActuationError> {
        // Nichts gestellt, nichts zurueckzunehmen. Kein Fehler.
        Ok(())
    }
}

/// Die Grenzen, innerhalb derer der Regler arbeiten darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActuationPolicy {
    /// Der niedrigste Takt, den die Plattform zulaesst.
    pub platform_min: ClockMhz,
    /// Der hoechste.
    pub platform_max: ClockMhz,
    /// Der niedrigste Takt, bei dem alle bereits gegebenen Zusagen gelten.
    ///
    /// Kommt aus den Profilmanifesten (NV-03): wo ein Profil bei 1830 MHz
    /// gemessen wurde und ein Vertrag darauf beruht, ist 1830 der Boden.
    pub promised_floor: ClockMhz,
    /// Wie lange nach einer Aenderung nicht wieder gestellt werden darf.
    pub dwell_ms: u64,
    /// Wie weit der beobachtete Takt vom angeforderten abweichen darf, bis er
    /// als wirksam gilt, in MHz.
    ///
    /// Nicht null: Karten runden auf ihre eigenen Taktstufen, und eine
    /// Gleichheitspruefung wuerde jede Anforderung als unwirksam melden.
    pub tolerance_mhz: u32,
    /// Wie lange nach einer Anforderung auf die Bestaetigung gewartet wird.
    pub settle_ms: u64,
}

impl ActuationPolicy {
    /// Ob dieser Takt innerhalb aller Grenzen liegt.
    ///
    /// # Errors
    ///
    /// [`ActuationError::OutOfPlatformRange`] oder
    /// [`ActuationError::WouldBreakPromise`].
    pub const fn admits(&self, clock: ClockMhz) -> Result<(), ActuationError> {
        if clock.0 < self.platform_min.0 || clock.0 > self.platform_max.0 {
            return Err(ActuationError::OutOfPlatformRange {
                requested: clock,
                min: self.platform_min,
                max: self.platform_max,
            });
        }
        if clock.0 < self.promised_floor.0 {
            return Err(ActuationError::WouldBreakPromise {
                requested: clock,
                promised_floor: self.promised_floor,
            });
        }
        Ok(())
    }
}

/// Was der Regler gerade weiss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActuationState {
    /// Nichts gestellt; es gilt, was die Plattform von sich aus tut.
    Untouched,
    /// Eine Vorgabe wurde beobachtet bestaetigt.
    Holding {
        /// Der bestaetigte Takt.
        clock: ClockMhz,
        /// Seit wann, als Unix-Zeit in Millisekunden.
        since_ms: u64,
    },
    /// Eine Anforderung lief, wurde aber nicht bestaetigt.
    ///
    /// Der Regler plant dann **nicht** auf dem angeforderten Punkt. Er weiss,
    /// dass er nicht weiss, was gilt.
    Unconfirmed {
        /// Was angefordert wurde.
        requested: ClockMhz,
    },
}

/// Der langsame Regler.
#[derive(Debug)]
pub struct Actuation {
    actuator: Box<dyn Actuator>,
    policy: ActuationPolicy,
    state: ActuationState,
    enabled: bool,
    requests: u64,
    confirmed: u64,
    refused: u64,
}

impl Actuation {
    /// Ein Regler ohne Stellbefugnis.
    ///
    /// Die Voreinstellung fuer jeden Governor.
    #[must_use]
    pub fn disabled(policy: ActuationPolicy) -> Self {
        Self {
            actuator: Box::new(NoActuator),
            policy,
            state: ActuationState::Untouched,
            enabled: false,
            requests: 0,
            confirmed: 0,
            refused: 0,
        }
    }

    /// Schaltet die Aktuation mit einem konkreten Stellglied ein.
    ///
    /// Ausdruecklich und nicht als Nebenwirkung einer Konfiguration: wer
    /// einem Governor Stellbefugnis gibt, soll es getan haben.
    pub fn enable(&mut self, actuator: Box<dyn Actuator>) {
        self.actuator = actuator;
        self.enabled = true;
    }

    /// Ob gestellt werden darf.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Der Wissensstand des Reglers.
    #[must_use]
    pub const fn state(&self) -> ActuationState {
        self.state
    }

    /// Die geltenden Grenzen.
    #[must_use]
    pub const fn policy(&self) -> &ActuationPolicy {
        &self.policy
    }

    /// Wie lange der Aufrufer zwischen Anforderung und Beobachtung warten
    /// soll.
    ///
    /// Der Regler schlaeft nicht selbst — er hat keine Uhr und soll keine
    /// haben. Er sagt nur, wie lange. Wer zu frueh liest, bekommt
    /// [`ActuationError::NotObserved`], und das ist richtig so: eine
    /// Taktaenderung, die noch nicht angekommen ist, ist noch nicht wirksam.
    #[must_use]
    pub const fn settle_time(&self) -> core::time::Duration {
        core::time::Duration::from_millis(self.policy.settle_ms)
    }

    /// Setzt die Grenzen neu.
    ///
    /// Ein enger gewordener Boden wirkt sofort: eine bereits gehaltene Vorgabe
    /// darunter wird beim naechsten Zug zurueckgenommen, nicht stillschweigend
    /// weitergefuehrt.
    pub const fn set_policy(&mut self, policy: ActuationPolicy) {
        self.policy = policy;
    }

    /// Wie viele Anforderungen abgesetzt wurden.
    #[must_use]
    pub const fn requests(&self) -> u64 {
        self.requests
    }

    /// Wie viele davon beobachtet bestaetigt wurden.
    #[must_use]
    pub const fn confirmed(&self) -> u64 {
        self.confirmed
    }

    /// Wie viele vor dem Absetzen abgelehnt wurden.
    #[must_use]
    pub const fn refused(&self) -> u64 {
        self.refused
    }

    /// Fordert einen Takt an und prueft, ob er wirksam wurde.
    ///
    /// `observe` liefert den Zustand **nach** der Anforderung; der Aufrufer
    /// wartet dazwischen [`ActuationPolicy::settle_ms`] ab. Der Regler selbst
    /// schlaeft nicht und liest nicht — er bekommt beides gesagt, wie der Kern
    /// die Zeit gesagt bekommt.
    ///
    /// # Errors
    ///
    /// Siehe [`ActuationError`].
    pub fn request(
        &mut self,
        clock: ClockMhz,
        now_ms: u64,
        observe: &dyn Fn() -> Option<HardwareSnapshot>,
        gpu_index: u32,
    ) -> Result<ClockMhz, ActuationError> {
        if !self.enabled {
            self.refused = self.refused.saturating_add(1);
            return Err(ActuationError::Disabled);
        }
        if let Err(e) = self.policy.admits(clock) {
            self.refused = self.refused.saturating_add(1);
            return Err(e);
        }
        if let ActuationState::Holding { since_ms, .. } = self.state {
            let elapsed = now_ms.saturating_sub(since_ms);
            if elapsed < self.policy.dwell_ms {
                self.refused = self.refused.saturating_add(1);
                return Err(ActuationError::Dwelling {
                    remaining_ms: self.policy.dwell_ms.saturating_sub(elapsed),
                });
            }
        }

        self.requests = self.requests.saturating_add(1);
        self.actuator.request(clock)?;

        // Beobachten statt annehmen. Ein Kommando, das mit Erfolg
        // zurueckkehrt, hat nichts bewiesen.
        let observed = observe().and_then(|snap| {
            snap.gpu(gpu_index)
                .and_then(|gpu| gpu.clock_sm_mhz.value().copied())
                .map(ClockMhz)
        });
        let Some(actual) = observed else {
            self.state = ActuationState::Unconfirmed { requested: clock };
            return Err(ActuationError::NotObserved {
                requested: clock,
                observed: None,
            });
        };
        let delta = actual.0.abs_diff(clock.0);
        if delta > self.policy.tolerance_mhz {
            self.state = ActuationState::Unconfirmed { requested: clock };
            return Err(ActuationError::NotObserved {
                requested: clock,
                observed: Some(actual),
            });
        }
        // Die Toleranz gilt fuer die Rundung der Karte, nicht fuer die
        // Zusage. Ein beobachteter Takt unter dem Boden ist ein Betriebspunkt,
        // fuer den kein Profil gemessen wurde — ihn als bestaetigt zu fuehren
        // hiesse, auf einem Punkt zu planen, den niemand vermessen hat
        // (Review R07). Geprueft wird deshalb, was **dasteht**, nicht was
        // angefordert war.
        if let Err(broken) = self.policy.admits(actual) {
            self.state = ActuationState::Unconfirmed { requested: clock };
            return Err(match broken {
                ActuationError::WouldBreakPromise { .. } => ActuationError::ObservedBelowPromise {
                    observed: actual,
                    promised_floor: self.policy.promised_floor,
                },
                other => other,
            });
        }

        self.confirmed = self.confirmed.saturating_add(1);
        self.state = ActuationState::Holding {
            clock: actual,
            since_ms: now_ms,
        };
        Ok(actual)
    }

    /// Nimmt die eigene Vorgabe zurueck.
    ///
    /// Beim Herunterfahren Pflicht: eine Taktvorgabe, die einen Prozess
    /// ueberlebt, ist eine Aenderung an fremder Hardware, von der niemand
    /// mehr weiss, wer sie gemacht hat.
    ///
    /// # Errors
    ///
    /// Wenn das Kommando fehlschlaegt. Der Zustand gilt danach als
    /// unbestimmt, nicht als zurueckgesetzt.
    pub fn restore(&mut self) -> Result<(), ActuationError> {
        let result = self.actuator.restore();
        if result.is_ok() {
            self.state = ActuationState::Untouched;
        }
        result
    }

    /// Der Betriebspunkt, auf dem geplant werden darf.
    ///
    /// `None` heisst: der Regler weiss es nicht, und der Aufrufer plant wie
    /// ohne Aktuation. Ein unbestaetigter Wunsch ist kein Betriebspunkt.
    #[must_use]
    pub const fn planning_clock(&self) -> Option<ClockMhz> {
        match self.state {
            ActuationState::Holding { clock, .. } => Some(clock),
            ActuationState::Untouched | ActuationState::Unconfirmed { .. } => None,
        }
    }
}

/// Der Pfad der Sperrdatei, die die Stellbefugnis exklusiv macht.
///
/// Eine **beratende** Sperre: sie haelt einen zweiten Vigilant-Prozess ab,
/// nicht einen Betreiber mit `nvidia-smi` in der Hand. Mehr ist von aussen
/// nicht zu erreichen — der Treiber kennt keinen Besitzer einer Taktvorgabe.
/// Genau deshalb prueft der Regler zusaetzlich durch Lesen (siehe
/// [`Actuation::request`]): die Sperre verhindert das Versehen, die
/// Beobachtung faengt den Rest.
pub const LOCK_PATH: &str = "/tmp/vigilant-actuation.lock";

/// Das Stellglied, das `nvidia-smi` aufruft.
///
/// Braucht Rechte. Ohne sie meldet es [`ActuationError::NotPermitted`] — und
/// das ist auf einer gewoehnlichen Installation der Normalfall.
#[derive(Debug)]
pub struct NvidiaSmiActuator {
    binary: String,
    gpu_index: u32,
    lock: Option<std::path::PathBuf>,
}

impl NvidiaSmiActuator {
    /// Nimmt die Stellbefugnis fuer diese GPU in Anspruch.
    ///
    /// # Errors
    ///
    /// [`ActuationError::NotExclusive`], wenn ein anderer lebender Prozess die
    /// Sperre haelt. Eine Sperre eines toten Prozesses wird uebernommen — ein
    /// abgestuerzter Governor soll die Karte nicht dauerhaft blockieren.
    pub fn acquire(gpu_index: u32) -> Result<Self, ActuationError> {
        Self::acquire_at(gpu_index, std::path::Path::new(LOCK_PATH), "nvidia-smi")
    }

    /// Wie [`Self::acquire`], mit eigenem Sperrpfad und Binary.
    ///
    /// # Errors
    ///
    /// Siehe [`Self::acquire`].
    pub fn acquire_at(
        gpu_index: u32,
        lock: &std::path::Path,
        binary: &str,
    ) -> Result<Self, ActuationError> {
        use std::io::Write as _;

        // Eine bestehende Sperre eines toten Prozesses uebernehmen: ein
        // abgestuerzter Governor soll die Karte nicht dauerhaft blockieren.
        if let Ok(text) = std::fs::read_to_string(lock)
            && let Ok(pid) = text.trim().parse::<i32>()
            && std::path::Path::new(&format!("/proc/{pid}")).exists()
        {
            return Err(ActuationError::NotExclusive {
                detail: format!("Prozess {pid} haelt {}", lock.display()),
            });
        }

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(lock)
            .map_err(|e| ActuationError::NotPermitted {
                detail: format!("{}: {e}", lock.display()),
            })?;
        write!(file, "{}", std::process::id()).map_err(|e| ActuationError::NotPermitted {
            detail: e.to_string(),
        })?;

        Ok(Self {
            binary: binary.to_owned(),
            gpu_index,
            lock: Some(lock.to_path_buf()),
        })
    }

    fn run(&self, args: &[String]) -> Result<(), ActuationError> {
        let output = std::process::Command::new(&self.binary)
            .args(args)
            .output()
            .map_err(|e| ActuationError::NotPermitted {
                detail: format!("{} nicht ausfuehrbar: {e}", self.binary),
            })?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail: String = if stderr.trim().is_empty() {
            stdout.trim().chars().take(200).collect()
        } else {
            stderr.trim().chars().take(200).collect()
        };
        Err(ActuationError::NotPermitted { detail })
    }
}

impl Drop for NvidiaSmiActuator {
    /// Gibt die Sperre zurueck.
    ///
    /// Die **Taktvorgabe** wird hier ausdruecklich nicht zurueckgenommen:
    /// `restore` gehoert in den geordneten Ablauf, wo sein Fehlschlag noch
    /// gemeldet werden kann. Ein `Drop`, der scheitert, schweigt.
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take() {
            let _ = std::fs::remove_file(lock);
        }
    }
}

impl Actuator for NvidiaSmiActuator {
    fn request(&mut self, clock: ClockMhz) -> Result<(), ActuationError> {
        // `-lgc min,max` mit gleichem Ober- und Untergrenzwert: die Karte soll
        // genau dort laufen und nicht in einem Bereich schwanken.
        self.run(&[
            "-i".to_owned(),
            self.gpu_index.to_string(),
            "-lgc".to_owned(),
            format!("{},{}", clock.0, clock.0),
        ])
    }

    fn restore(&mut self) -> Result<(), ActuationError> {
        self.run(&[
            "-i".to_owned(),
            self.gpu_index.to_string(),
            "-rgc".to_owned(),
        ])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::collector::parse_output;
    use std::sync::{Arc, Mutex};

    /// Ein Stellglied, das mitschreibt und auf Kommando scheitert.
    #[derive(Debug, Default)]
    struct FakeActuator {
        requested: Arc<Mutex<Vec<ClockMhz>>>,
        restored: Arc<Mutex<u32>>,
        fail_with: Option<ActuationError>,
    }

    impl Actuator for FakeActuator {
        fn request(&mut self, clock: ClockMhz) -> Result<(), ActuationError> {
            if let Some(e) = &self.fail_with {
                return Err(e.clone());
            }
            self.requested.lock().unwrap().push(clock);
            Ok(())
        }
        fn restore(&mut self) -> Result<(), ActuationError> {
            let mut n = self.restored.lock().unwrap();
            *n = n.saturating_add(1);
            Ok(())
        }
    }

    fn line(clock: u32) -> String {
        format!(
            "0, NVIDIA GeForce RTX 3070 Laptop GPU, 580.173.02, 8.6, 8192, \
             80, {clock}, 2100, 129.55, [N/A], P0, Disabled, 0x0000000000000004"
        )
    }

    fn snapshot_at(clock: u32) -> impl Fn() -> Option<HardwareSnapshot> {
        move || Some(parse_output(&line(clock), 1_000))
    }

    fn policy() -> ActuationPolicy {
        ActuationPolicy {
            platform_min: ClockMhz(300),
            platform_max: ClockMhz(2100),
            promised_floor: ClockMhz(1500),
            dwell_ms: 10_000,
            tolerance_mhz: 50,
            settle_ms: 500,
        }
    }

    // -- Voreinstellung ----------------------------------------------------

    #[test]
    fn the_default_has_no_authority_to_set_anything() {
        let mut a = Actuation::disabled(policy());
        assert!(!a.is_enabled());
        assert_eq!(
            a.request(ClockMhz(1800), 0, &snapshot_at(1800), 0),
            Err(ActuationError::Disabled),
            "wer einem Governor Stellbefugnis gibt, soll es getan haben"
        );
        assert_eq!(a.requests(), 0, "es wurde gar nichts abgesetzt");
        assert_eq!(a.refused(), 1);
    }

    #[test]
    fn restoring_without_having_set_anything_is_not_an_error() {
        let mut a = Actuation::disabled(policy());
        assert!(a.restore().is_ok());
        assert_eq!(a.state(), ActuationState::Untouched);
    }

    // -- Beobachtet statt angenommen ---------------------------------------

    #[test]
    fn a_confirmed_request_becomes_the_planning_point() {
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator::default()));
        assert_eq!(
            a.request(ClockMhz(1800), 1_000, &snapshot_at(1800), 0),
            Ok(ClockMhz(1800))
        );
        assert_eq!(
            a.state(),
            ActuationState::Holding {
                clock: ClockMhz(1800),
                since_ms: 1_000
            }
        );
        assert_eq!(a.planning_clock(), Some(ClockMhz(1800)));
        assert_eq!(a.confirmed(), 1);
    }

    #[test]
    fn a_command_that_succeeds_without_effect_is_not_confirmed() {
        // Der gefaehrlichste Fall: `nvidia-smi` kehrt mit 0 zurueck, und die
        // Karte laeuft trotzdem anders. Ein Regler, der das nicht bemerkt,
        // plant auf einem Betriebspunkt, den es nicht gibt.
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator::default()));
        assert_eq!(
            a.request(ClockMhz(1800), 1_000, &snapshot_at(1200), 0),
            Err(ActuationError::NotObserved {
                requested: ClockMhz(1800),
                observed: Some(ClockMhz(1200)),
            })
        );
        assert_eq!(
            a.state(),
            ActuationState::Unconfirmed {
                requested: ClockMhz(1800)
            }
        );
        assert_eq!(
            a.planning_clock(),
            None,
            "er weiss, dass er nicht weiss, was gilt"
        );
        assert_eq!(a.confirmed(), 0);
    }

    #[test]
    fn a_card_rounding_to_its_own_step_still_counts_as_confirmed() {
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator::default()));
        // 1785 statt 1800 — innerhalb der Toleranz von 50 MHz.
        assert_eq!(
            a.request(ClockMhz(1800), 1_000, &snapshot_at(1785), 0),
            Ok(ClockMhz(1785)),
            "eine Gleichheitspruefung wuerde jede Anforderung als unwirksam melden"
        );
    }

    #[test]
    fn without_any_observation_the_request_is_unconfirmed() {
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator::default()));
        let blind = || None;
        assert_eq!(
            a.request(ClockMhz(1800), 1_000, &blind, 0),
            Err(ActuationError::NotObserved {
                requested: ClockMhz(1800),
                observed: None,
            })
        );
        assert_eq!(a.planning_clock(), None);
    }

    // -- Grenzen -----------------------------------------------------------

    #[test]
    fn a_target_outside_the_platform_range_is_refused_before_the_command() {
        let mut a = Actuation::disabled(policy());
        let fake = FakeActuator::default();
        let seen = Arc::clone(&fake.requested);
        a.enable(Box::new(fake));
        assert!(matches!(
            a.request(ClockMhz(2500), 1_000, &snapshot_at(2500), 0),
            Err(ActuationError::OutOfPlatformRange { .. })
        ));
        assert!(seen.lock().unwrap().is_empty(), "nichts abgesetzt");
    }

    #[test]
    fn a_target_below_the_promised_floor_is_refused() {
        // Ein Profil wurde bei 1830 MHz gemessen; eine Zusage, die darauf
        // beruht, darf nicht durch eine Taktsenkung gebrochen werden.
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator::default()));
        assert_eq!(
            a.request(ClockMhz(1200), 1_000, &snapshot_at(1200), 0),
            Err(ActuationError::WouldBreakPromise {
                requested: ClockMhz(1200),
                promised_floor: ClockMhz(1500),
            })
        );
    }

    #[test]
    fn a_tightened_floor_takes_effect_on_the_next_move() {
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator::default()));
        a.request(ClockMhz(1600), 0, &snapshot_at(1600), 0).unwrap();
        a.set_policy(ActuationPolicy {
            promised_floor: ClockMhz(1800),
            ..policy()
        });
        assert!(matches!(
            a.request(ClockMhz(1600), 100_000, &snapshot_at(1600), 0),
            Err(ActuationError::WouldBreakPromise { .. })
        ));
    }

    // -- Verweildauer ------------------------------------------------------

    #[test]
    fn the_caller_is_told_how_long_to_wait() {
        let a = Actuation::disabled(policy());
        assert_eq!(a.settle_time(), core::time::Duration::from_millis(500));
    }

    #[test]
    fn the_regulator_does_not_oscillate() {
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator::default()));
        a.request(ClockMhz(1800), 1_000, &snapshot_at(1800), 0)
            .unwrap();
        assert_eq!(
            a.request(ClockMhz(1600), 5_000, &snapshot_at(1600), 0),
            Err(ActuationError::Dwelling {
                remaining_ms: 6_000
            }),
            "Pendeln kostet mehr als der schlechtere Betriebspunkt"
        );
        // Nach der Verweildauer geht es.
        assert!(
            a.request(ClockMhz(1600), 11_000, &snapshot_at(1600), 0)
                .is_ok()
        );
    }

    #[test]
    fn an_unconfirmed_state_does_not_start_a_dwell() {
        // Sonst haette ein gescheiterter Versuch den Regler fuer die volle
        // Verweildauer blockiert — obwohl gar nichts gestellt wurde.
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator::default()));
        let _ = a.request(ClockMhz(1800), 1_000, &snapshot_at(1200), 0);
        assert!(
            a.request(ClockMhz(1800), 1_100, &snapshot_at(1800), 0)
                .is_ok(),
            "ein Fehlversuch sperrt nicht"
        );
    }

    // -- Rechte und Konkurrenz ---------------------------------------------

    #[test]
    fn a_refused_command_leaves_the_state_untouched() {
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator {
            fail_with: Some(ActuationError::NotPermitted {
                detail: "Insufficient Permissions".to_owned(),
            }),
            ..FakeActuator::default()
        }));
        assert!(matches!(
            a.request(ClockMhz(1800), 1_000, &snapshot_at(1800), 0),
            Err(ActuationError::NotPermitted { .. })
        ));
        assert_eq!(a.state(), ActuationState::Untouched);
        assert_eq!(a.planning_clock(), None);
    }

    #[test]
    fn a_competing_setter_shows_up_as_an_unobserved_request() {
        // Ein anderer Prozess stellt den Takt zurueck. Der Regler bemerkt es,
        // weil er liest — nicht, weil ihm jemand Bescheid sagt.
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(FakeActuator::default()));
        a.request(ClockMhz(1800), 1_000, &snapshot_at(1800), 0)
            .unwrap();
        assert_eq!(a.planning_clock(), Some(ClockMhz(1800)));

        // Naechster Zug: die Karte laeuft laengst woanders.
        let err = a
            .request(ClockMhz(1800), 20_000, &snapshot_at(900), 0)
            .unwrap_err();
        assert!(matches!(err, ActuationError::NotObserved { .. }));
        assert_eq!(a.planning_clock(), None);
    }

    // -- Zuruecknehmen -----------------------------------------------------

    #[test]
    fn restoring_gives_the_card_back() {
        let mut a = Actuation::disabled(policy());
        let fake = FakeActuator::default();
        let restored = Arc::clone(&fake.restored);
        a.enable(Box::new(fake));
        a.request(ClockMhz(1800), 1_000, &snapshot_at(1800), 0)
            .unwrap();
        a.restore().unwrap();
        assert_eq!(*restored.lock().unwrap(), 1);
        assert_eq!(a.state(), ActuationState::Untouched);
        assert_eq!(a.planning_clock(), None);
    }

    // -- Das echte Stellglied ---------------------------------------------

    fn scratch_lock(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("vig-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn setting_the_clock_without_rights_is_refused_and_says_so() {
        // Auf einer gewoehnlichen Installation ist das der Normalfall, und der
        // Betreiber soll die Meldung des Systems sehen und nicht unsere.
        let lock = scratch_lock("rights");
        let mut act = NvidiaSmiActuator::acquire_at(0, &lock, "nvidia-smi").unwrap();
        match act.request(ClockMhz(1800)) {
            Err(ActuationError::NotPermitted { detail }) => {
                assert!(!detail.is_empty(), "die Meldung des Systems fehlt");
            }
            Err(other) => panic!("unerwarteter Fehler: {other}"),
            Ok(()) => {
                // Laeuft der Test als root, ist der Takt jetzt gesetzt —
                // dann sofort zuruecknehmen.
                act.restore().unwrap();
            }
        }
        drop(act);
        assert!(!lock.exists(), "die Sperre wird beim Ende zurueckgegeben");
    }

    #[test]
    fn a_second_governor_does_not_get_the_setting_rights() {
        let lock = scratch_lock("exclusive");
        let first = NvidiaSmiActuator::acquire_at(0, &lock, "nvidia-smi").unwrap();
        assert!(matches!(
            NvidiaSmiActuator::acquire_at(0, &lock, "nvidia-smi"),
            Err(ActuationError::NotExclusive { .. })
        ));
        drop(first);
        // Nach dem Ende des ersten geht es wieder.
        let second = NvidiaSmiActuator::acquire_at(0, &lock, "nvidia-smi");
        assert!(second.is_ok());
        drop(second);
        let _ = std::fs::remove_file(&lock);
    }

    #[test]
    fn a_lock_from_a_dead_process_is_taken_over() {
        // Ein abgestuerzter Governor soll die Karte nicht dauerhaft
        // blockieren.
        let lock = scratch_lock("stale");
        std::fs::write(&lock, "999999").unwrap();
        let taken = NvidiaSmiActuator::acquire_at(0, &lock, "nvidia-smi");
        assert!(
            taken.is_ok(),
            "eine Sperre ohne lebenden Halter blockiert nicht"
        );
        drop(taken);
        let _ = std::fs::remove_file(&lock);
    }

    #[test]
    fn a_missing_binary_is_a_permission_finding_not_a_crash() {
        let lock = scratch_lock("nobinary");
        let mut act = NvidiaSmiActuator::acquire_at(0, &lock, "/nonexistent-nvidia-smi").unwrap();
        assert!(matches!(
            act.request(ClockMhz(1800)),
            Err(ActuationError::NotPermitted { .. })
        ));
        drop(act);
        let _ = std::fs::remove_file(&lock);
    }

    #[test]
    fn a_failed_restore_does_not_claim_the_card_is_back() {
        #[derive(Debug)]
        struct Stubborn;
        impl Actuator for Stubborn {
            fn request(&mut self, _c: ClockMhz) -> Result<(), ActuationError> {
                Ok(())
            }
            fn restore(&mut self) -> Result<(), ActuationError> {
                Err(ActuationError::NotPermitted {
                    detail: "weg".to_owned(),
                })
            }
        }
        let mut a = Actuation::disabled(policy());
        a.enable(Box::new(Stubborn));
        a.request(ClockMhz(1800), 1_000, &snapshot_at(1800), 0)
            .unwrap();
        assert!(a.restore().is_err());
        assert_ne!(
            a.state(),
            ActuationState::Untouched,
            "der Zustand gilt als unbestimmt, nicht als zurueckgesetzt"
        );
    }

    /// Ein beobachteter Takt unter dem zugesagten Boden ist keine
    /// Bestaetigung (Review R07).
    ///
    /// Die Toleranz gilt fuer die Rundung der Karte auf ihre eigenen
    /// Taktstufen, nicht fuer die Zusage. 1470 MHz bei einem Boden von
    /// 1500 MHz liegt innerhalb von 50 MHz Toleranz — und ist trotzdem ein
    /// Betriebspunkt, fuer den kein Profil gemessen wurde. Ihn als
    /// bestaetigt zu fuehren hiesse, auf einem Punkt zu planen, den niemand
    /// vermessen hat.
    #[test]
    fn an_observed_clock_below_the_promised_floor_is_not_confirmation() {
        let mut actuation = Actuation::disabled(ActuationPolicy {
            platform_min: ClockMhz(300),
            platform_max: ClockMhz(2100),
            promised_floor: ClockMhz(1500),
            dwell_ms: 0,
            tolerance_mhz: 50,
            settle_ms: 0,
        });
        actuation.enable(Box::new(AlwaysOk));

        let at_1470 = || Some(snapshot_with_clock(1470));
        assert!(
            matches!(
                actuation.request(ClockMhz(1500), 1000, &at_1470, 0),
                Err(ActuationError::ObservedBelowPromise { .. })
            ),
            "1470 unter dem Boden 1500, trotz Toleranz"
        );

        // Die Gegenprobe: auf dem Boden selbst wird bestaetigt.
        let at_1500 = || Some(snapshot_with_clock(1500));
        assert!(actuation.request(ClockMhz(1500), 2000, &at_1500, 0).is_ok());
    }

    /// Ein Stellglied, das jede Anforderung annimmt.
    #[derive(Debug)]
    struct AlwaysOk;

    impl Actuator for AlwaysOk {
        fn request(&mut self, _: ClockMhz) -> Result<(), ActuationError> {
            Ok(())
        }
        fn restore(&mut self) -> Result<(), ActuationError> {
            Ok(())
        }
    }

    /// Ein Messwertsatz mit diesem SM-Takt.
    fn snapshot_with_clock(mhz: u32) -> crate::HardwareSnapshot {
        crate::collector::parse_output(
            &format!(
                "0, GPU, 580.173.02, 8.6, 8192, 80, {mhz}, 2100, 129.55, [N/A], P0, Disabled, 0x4"
            ),
            1000,
        )
    }
}
