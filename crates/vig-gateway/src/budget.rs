//! Das Nutzlastbudget und seine Reservierungen.
//!
//! Ein eigenes Modul, weil die Reservierung zwei Schichten weit reist: der
//! Dienst nimmt sie, der Actor haelt sie, und freigegeben wird sie erst, wenn
//! die **Ausfuehrung** nachweislich vorbei ist. Lag sie in `model_infer`,
//! endete sie mit dem Client — und nach einem Timeout rechnete das Backend
//! weiter, hielt die Nutzlast, und dieselbe Zahl Bytes war ein zweites Mal zu
//! haben (Review R04).
//!
//! Dieselbe Klasse Fehler wie ein zu frueh zurueckgegebener Slotkredit, nur in
//! einer anderen Waehrung.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Ein Budget fuer gleichzeitig gehaltene Requestnutzlast.
///
/// Zaehlt Bytes, nicht Requests. Der Ereigniskanal des Actors begrenzt bereits
/// die Anzahl; die kostet aber je nach Tensorgroesse zwischen einem Kilobyte
/// und zig Megabyte. Eine Grenze, die beides nicht unterscheidet, ist entweder
/// zu eng fuer Bilder oder zu weit fuer Speicher.
#[derive(Debug)]
pub struct PayloadBudget {
    /// Die Obergrenze in Bytes.
    limit: u64,
    /// Was gerade reserviert ist.
    used: Arc<AtomicU64>,
}

/// Gibt die reservierten Bytes zurueck, sobald die **Ausfuehrung** vorbei ist.
///
/// Als Guard und nicht als Aufruf am Ende: jeder fruehe Rueckgabepfad — und
/// davon gibt es in `model_infer` mehrere — wuerde das Budget sonst dauerhaft
/// verkleinern, bis das Gateway ohne erkennbaren Grund alles ablehnt.
///
/// Der Guard reist mit dem Request **in den Actor** und stirbt dort, wenn der
/// Slotkredit endet. Blieb er in `model_infer`, endete das Budget mit dem
/// Client: nach einem Timeout gab es die Bytes frei, waehrend das Backend
/// weiterrechnete und die Nutzlast weiter hielt. Zwei 16-Byte-Auftraege
/// liefen so bei einem Budget von 16 Bytes (Review R04).
#[derive(Debug)]
pub struct PayloadPermit {
    /// Der gemeinsame Zaehler.
    used: Arc<AtomicU64>,
    /// Was diese Reservierung haelt.
    bytes: u64,
}

impl Drop for PayloadPermit {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl PayloadBudget {
    /// Ein Budget mit dieser Obergrenze.
    #[must_use]
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            used: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Reserviert `bytes`, wenn das Budget reicht.
    ///
    /// `None` heisst: es reicht nicht. Der Aufrufer lehnt dann ab, statt die
    /// Nutzlast trotzdem zu halten.
    #[must_use]
    pub fn try_reserve(&self, bytes: u64) -> Option<PayloadPermit> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(bytes);
            if next > self.limit {
                return None;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(PayloadPermit {
                        used: Arc::clone(&self.used),
                        bytes,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

impl PayloadPermit {
    /// Eine Reservierung, die nichts zaehlt.
    ///
    /// Fuer Aufrufer, die `Handle::submit` direkt benutzen und kein Budget
    /// fuehren — Tests und Werkzeuge. Bewusst benannt statt als `Option`
    /// getarnt: wer sie nimmt, sagt damit, dass er die Nutzlast selbst
    /// begrenzt.
    #[must_use]
    pub fn untracked() -> Self {
        Self {
            used: Arc::new(AtomicU64::new(0)),
            bytes: 0,
        }
    }
}
