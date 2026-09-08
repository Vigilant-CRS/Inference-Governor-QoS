//! Zaehler des Scheduling-Kerns (Spec 18).
//!
//! Bewusst nur Zaehler und Summen, keine Histogramme: der Kern soll zaehlen,
//! nicht aggregieren. Quantile, gleitende Fenster und die Coverage-Metrik aus
//! ADR-0005 gehoeren in den Exporter bzw. in das Benchmark-Harness, wo sie
//! ueber einen definierten Messzeitraum gebildet werden.

use crate::ids::{MAX_MODELS, MAX_VARIANTS};

/// Die Zaehler eines Scheduler-Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metrics {
    /// Am Gateway angenommene Requests.
    pub received: u64,
    /// An das Backend weitergereichte Requests.
    pub forwarded: u64,
    /// Durch einen juengeren Request ersetzte Requests.
    pub superseded: u64,
    /// Wegen Ueberalterung verworfene Requests.
    pub stale: u64,
    /// Als nicht mehr rechtzeitig machbar abgelehnte Requests.
    pub rejected_infeasible: u64,
    /// Wegen erschoepfter Queue-Kapazitaet abgelehnte Requests.
    pub rejected_capacity: u64,
    /// Fertiggestellt und bei Fertigstellung noch aktuell.
    pub completed_valid: u64,
    /// Fertiggestellt, aber bei Fertigstellung bereits obsolet.
    pub completed_obsolete: u64,
    /// Backendfehler.
    pub backend_failures: u64,
    /// Backendaufrufe, die das Timeout ueberschritten haben.
    ///
    /// Jeder davon hat einen Client freigegeben und einen Slotkredit
    /// zurueckgehalten. Steigt der Zaehler, antwortet das Backend nicht mehr.
    pub backend_timeouts: u64,
    /// Slotkredite, die derzeit wegen eines Timeouts gehalten werden.
    ///
    /// Ein Messwert, kein Zaehler: er faellt wieder, wenn das Backend doch
    /// noch antwortet. Erreicht er die Slotzahl, kann nichts mehr starten —
    /// das ist der Zustand, den die Bereitschaftspruefung melden muss.
    pub quarantined: u64,
    /// Vom Client zurueckgezogene Requests, die noch warteten.
    ///
    /// Eine Gesundheitsgroesse, keine Fehlerzahl: steigt sie, laufen Clients
    /// in ihre eigenen Timeouts, waehrend Vigilant ihre Arbeit noch plant.
    pub cancelled: u64,
    /// Verletzte Deadlines ueber alle Klassen.
    pub deadline_misses: u64,
    /// Verletzte Deadlines geschuetzter Klassen (Protected/High).
    pub protected_deadline_misses: u64,
    /// Backendzeit, die in bei Fertigstellung bereits obsolete Ergebnisse floss.
    ///
    /// Die zentrale Metrik der Garbage-Collector-Idee (Spec 18.2): sie zeigt
    /// den konkreten Ressourcenwert vermiedener Arbeit.
    pub stale_compute_nanos: u64,
    /// Gesamte verbrauchte Backendzeit.
    pub total_compute_nanos: u64,
    /// Wie oft welche Variante gewaehlt wurde.
    pub variant_selected: [u64; MAX_VARIANTS],
    /// Wie viele der Modellslots tatsaechlich belegt sind.
    ///
    /// Ohne diese Zahl exportiert der Endpunkt alle `MAX_MODELS` Slots, also
    /// ueberwiegend Nullen. Eine Zeitreihe je unbenutztem Slot ist kein
    /// harmloser Ballast: sie kostet in Prometheus dauerhaft Speicher und
    /// macht jede Abfrage unleserlich.
    pub models: usize,
    /// Die Anzahl konfigurierter Ausfuehrungsslots.
    ///
    /// Steht hier, damit die Bereitschaftspruefung „alle Slots in Quarantaene"
    /// beantworten kann, ohne die Konfiguration zu kennen.
    pub slots: u64,
    /// Der beobachtete Ankunftsabstand je Modell, in Mikrosekunden.
    ///
    /// Null, solange zu wenig gemessen wurde. Zusammen mit
    /// [`Self::contract_period_us`] beantwortet diese Reihe die Frage, ob die
    /// Konfiguration noch zur Wirklichkeit passt — eine Kamera, die dauerhaft
    /// schneller liefert als vereinbart, laesst die Abdeckung fallen, ohne
    /// dass sonst irgendetwas davon berichtet.
    pub arrival_period_us: [u32; MAX_MODELS],
    /// Die konfigurierte Periode je Modell, in Mikrosekunden.
    ///
    /// Steht daneben, damit das Verhaeltnis in Prometheus ohne Kenntnis der
    /// Konfigurationsdatei berechenbar ist.
    pub contract_period_us: [u32; MAX_MODELS],
    /// Die aktuell wirksame Sicherheitsmarge je Modell, in Prozent.
    ///
    /// Kein Zaehler, sondern ein Zustand: der Estimator zieht sie nach oben,
    /// wenn er sich verschaetzt hat (ADR-0013), und ein unbestaetigtes Profil
    /// startet sie erhoeht (ADR-0016). Ueber Stunden gelesen zeigt diese Reihe,
    /// ob das System zur Ruhe kommt oder langsam immer vorsichtiger wird —
    /// und das sieht man an keinem Zaehler.
    pub margin_percent: [u32; MAX_MODELS],
    /// Best-Effort-Requests, die terminal wurden, ohne je gelaufen zu sein.
    ///
    /// ADR-0012: Aushungerung ist ein Befund, kein Nebeneffekt. Ohne diesen
    /// Zaehler waere eine nie ausgefuehrte Hintergrundlast nur an ausbleibenden
    /// Antworten zu erkennen — also praktisch gar nicht.
    pub best_effort_starved: u64,
    /// Requests, die bewusst trotz verfehlbarer Deadline **gestartet** wurden.
    ///
    /// Nach ADR-0009 ist eine verspaetete, aber frische Inferenz besser als
    /// keine. Der Zaehler macht sichtbar, wie oft das noetig war.
    pub dispatched_late: u64,
    /// Wie oft ein Start zugunsten erwarteter geschuetzter Arbeit verschoben wurde.
    ///
    /// Der Zaehler des absichtlichen Idle (Spec 10.7). Er zaehlt **Veto-
    /// Ereignisse**, nicht Requests: derselbe Kandidat kann bei jeder
    /// Planungsrunde erneut zurueckgestellt werden. Er gehoert zu den
    /// wichtigsten Diagnosewerten: ist er null, wirkt der Look-ahead nicht;
    /// ist er sehr hoch, ist die Konfiguration ueberzeichnet.
    pub deferred_for_protected: u64,
}

impl Metrics {
    /// Der Anteil gueltiger an allen fertiggestellten Inferenzen in Promille
    /// (Spec 18.1, Useful Inference Ratio).
    #[must_use]
    pub fn useful_inference_permille(&self) -> u16 {
        let total = self.completed_valid.saturating_add(self.completed_obsolete);
        if total == 0 {
            return 0;
        }
        let permille = self
            .completed_valid
            .saturating_mul(1_000)
            .checked_div(total)
            .unwrap_or(0);
        u16::try_from(permille).unwrap_or(u16::MAX)
    }

    /// Der Anteil verschwendeter an der gesamten Backendzeit in Promille
    /// (Spec 18.2, Stale Compute Waste).
    #[must_use]
    pub fn stale_compute_permille(&self) -> u16 {
        if self.total_compute_nanos == 0 {
            return 0;
        }
        let permille = self
            .stale_compute_nanos
            .saturating_mul(1_000)
            .checked_div(self.total_compute_nanos)
            .unwrap_or(0);
        u16::try_from(permille).unwrap_or(u16::MAX)
    }

    /// Zaehlt eine Variantenwahl.
    pub fn count_variant(&mut self, variant: crate::ids::VariantIdx) {
        if let Some(slot) = self.variant_selected.get_mut(variant.get()) {
            *slot = slot.saturating_add(1);
        }
    }
}
