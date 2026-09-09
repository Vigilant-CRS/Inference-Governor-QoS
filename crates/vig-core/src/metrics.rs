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
    /// Backendaufrufe, die seit dem letzten Erfolg am **Transport** scheiterten.
    ///
    /// Ein Modellfehler zaehlt hier nicht: der sagt etwas ueber diesen einen
    /// Request, nicht ueber die Erreichbarkeit des Backends. Ein
    /// Verbindungsabbruch dagegen heisst, dass **nichts** mehr laufen wird —
    /// und genau das muss die Bereitschaftspruefung sehen, auch bevor ein
    /// Timeout ueberhaupt ablaufen konnte.
    pub consecutive_transport_failures: u64,
    /// Die laengste Zeit ohne gueltiges Ergebnis, je Modell, in Mikrosekunden.
    ///
    /// Die Groesse, die eine Abdeckungszahl nicht zeigt: zehn verstreute
    /// Ausfaelle und ein Block von zehn ergeben dieselbe Rate und voellig
    /// verschiedene Folgen fuer einen Regler. Im Betrieb ist gerade der Block
    /// das, was auffaellt — und im Mittelwert verschwindet.
    pub longest_gap_us: [u32; MAX_MODELS],
    /// Aufeinanderfolgende Requests ohne gueltiges Ergebnis, je Modell.
    ///
    /// Faellt auf null zurueck, sobald wieder etwas Brauchbares ankommt.
    pub consecutive_misses: [u32; MAX_MODELS],
    /// Fehlversorgte Verbraucherzyklen im laufenden Fenster, je Modell (NV-02).
    ///
    /// Zaehlt ueber den **Vertragstakt**, nicht ueber angenommene Requests:
    /// ein Governor, der alles ablehnt, erzeugt trotzdem Zyklen und faellt
    /// hier auf. Null, wo kein Missbudget vereinbart ist.
    pub weakly_hard_misses: [u32; MAX_MODELS],
    /// 1, wo die Weakly-hard-Bedingung im letzten vollstaendigen Fenster
    /// verletzt ist; sonst 0 (NV-02).
    ///
    /// Waehrend der Aufwaermphase — bevor ein vollstaendiges Fenster
    /// beobachtet wurde — steht hier 0. Das ist keine Zusage, sondern die
    /// Aussage, dass noch nichts feststeht.
    pub weakly_hard_violated: [u32; MAX_MODELS],
    /// Ausfuehrungsenden, die durch Abgleich mit dem Backend belegt wurden.
    ///
    /// Jeder davon ist ein Slotkredit, der ohne Antwort des Backends
    /// zurueckgegeben werden konnte — weil dessen eigene Statistik das Ende
    /// belegt hat, nicht weil eine Frist ablief.
    pub reconciled: u64,
    /// Requests, die wegen vollstaendiger Quarantaene sofort abgewiesen wurden.
    ///
    /// Sie einzureihen waere die schlechtere Antwort: der Client wartete bis in
    /// sein eigenes Timeout, und der Governor hielte Speicher fuer Arbeit, die
    /// nie beginnt.
    pub rejected_quarantined: u64,
    /// Wie oft der Look-ahead ein Profil als nicht mehr zustaendig vorfand.
    ///
    /// Spec 30.3: liegt die Beobachtung weit ueber dem hinterlegten Profil,
    /// ist das Profil nicht falsch, sondern unzustaendig. Steigt dieser
    /// Zaehler, gehoert `vig calibrate` erneut gefahren.
    pub degraded_profiles: u64,
    /// Backendaufrufe, die noch offen sind.
    ///
    /// Zaehlt die tatsaechlich laufenden Aufrufe, nicht die wartenden Clients.
    /// Nach einem Timeout ist der Client beantwortet und die Recheneinheit
    /// womoeglich weiterhin belegt — wer nur die Clients zaehlt, haelt das
    /// System dann faelschlich fuer leer.
    pub outstanding_backend_calls: u64,
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
