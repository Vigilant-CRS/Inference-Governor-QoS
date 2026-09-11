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
    /// Wie viele Backendendpunkte konfiguriert sind (Review R11).
    pub backends: u64,
    /// Wie viele davon die letzte aktive Probe beantwortet haben.
    ///
    /// **Nicht** aus dem Verkehr abgeleitet: eine Bereitschaftsaussage, die
    /// nur eine erfolgreiche Inferenz zuruecksetzen kann, erholt sich nie,
    /// wenn ein Loadbalancer daraufhin den Verkehr wegnimmt. Und ein Erfolg
    /// an einem Backend sagt nichts ueber ein anderes.
    ///
    /// Vor der ersten Probe steht hier null. Das heisst „ungeprueft", nicht
    /// „nicht erreichbar" — und beides ist ein Grund, keinen Verkehr zu
    /// schicken.
    pub backends_reachable: u64,
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
    /// Wie viele Misses das laufende Fenster noch vertraegt, je Modell (NV-24).
    ///
    /// Nicht „wie viele waren es", sondern „wie viele darf es noch geben" —
    /// die Groesse, an der eine Policy entscheidet und ein Betreiber sieht,
    /// wie eng es zugeht.
    pub weakly_hard_misses_left: [u32; MAX_MODELS],
    /// Wie oft die zustandsabhaengige Prognose mit dem bisherigen Weg
    /// verglichen wurde (NV-06).
    pub predictor_comparisons: u64,
    /// Wie oft sie keine Aussage hatte.
    pub predictor_fallbacks: u64,
    /// Wie oft sie mehr Zeit veranschlagte als der bisherige Weg.
    ///
    /// Zusammen mit `predictor_more_optimistic` die Antwort auf die Frage,
    /// die vor jeder Umstellung steht: schlaegt die Prognose den alten Weg,
    /// oder lehnt sie nur mehr ab? Eine Policy, die alles ablehnt, haelt
    /// jede Zusage ein und ist trotzdem wertlos.
    pub predictor_more_conservative: u64,
    /// Wie oft sie weniger Zeit veranschlagte.
    pub predictor_more_optimistic: u64,
    /// 1, wenn die Prognose scharf geschaltet ist; sonst 0.
    pub predictor_active: u64,
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
    /// Backendmodelle ohne Abgleichs-Basislinie (NV-20).
    ///
    /// Die Basislinie ist der Statistikzaehler des Backends zum Startzeitpunkt
    /// des Governors. Ohne sie ist kein zaehlerbasierter Endnachweis moeglich:
    /// Tritons Zaehler laeuft ueber die Lebensdauer des Triton-Prozesses, und
    /// der ueberlebt den Governor gewoehnlich — „Backend meldet mindestens so
    /// viele Abschluesse wie wir ausgeliefert haben" waere nach einem Neustart
    /// sofort wahr.
    ///
    /// Ist dieser Wert dauerhaft groesser als null, haelt ein abgebrochener
    /// Aufruf seinen Slotkredit, bis das Backend nachweislich neu startet. Das
    /// ist die sichere Antwort und ein Grund, den Governor bei erreichbarem
    /// Backend neu zu starten.
    pub reconcile_baseline_missing: u64,
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
    ///
    /// Ueber **alle** Modelle summiert: Index 0 ist die beste Variante jedes
    /// Modells, gleichgueltig welches. Sobald mehr als ein Modell laeuft, ist
    /// der Qualitaetsmix eines einzelnen daraus nicht mehr ablesbar. Eine
    /// Aufschluesselung je Modell und Variante kostete `MAX_MODELS` mal
    /// `MAX_VARIANTS` Zaehler in jedem Metrikabzug — 2 KB, kopiert bei jeder
    /// Abfrage, fuer eine Frage, die nur ein Benchmark stellt. Die
    /// Frontier-Messung (Spec 19.7) faehrt deshalb den Detektor allein.
    pub variant_selected: [u64; MAX_VARIANTS],
    /// Wie oft ein Modell auf eine hoeherwertige Variante gewechselt hat.
    ///
    /// Je Modell, anders als [`Self::variant_selected`]: ein Wechsel ist eine
    /// Eigenschaft der Folge **eines** Stroms, und eine Summe ueber Stroeme
    /// macht aus zwei ruhigen Modellen ein pendelndes. Die erste Wahl eines
    /// Modells ist kein Wechsel — vorher gab es keine.
    pub variant_upgrades: [u32; MAX_MODELS],
    /// Wie oft ein Modell auf eine geringerwertige Variante gewechselt hat.
    ///
    /// Getrennt von den Aufwertungen, weil die Hysterese nach Spec 12.4
    /// asymmetrisch ist: Abwertungen wirken sofort, Aufwertungen erst nach
    /// der Verweildauer. Eine Gesamtzahl verdeckte genau das.
    pub variant_downgrades: [u32; MAX_MODELS],
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

    /// Arbeit, die in wiederholte Prefill-Berechnungen ging, in Mikrosekunden
    /// (NV-16).
    ///
    /// Ein Re-Prefill erzeugt **kein einziges Token**. Er entsteht allein
    /// daraus, dass ein zerlegter Auftrag seinen Zustand im Prompt mitfuehrt
    /// und jedes Quantum ihn erneut ins Backend traegt. Ihn als Fortschritt zu
    /// buchen hiesse, dieselbe Arbeit zweimal zu verkaufen — deshalb steht er
    /// getrennt.
    ///
    /// Null heisst nicht „kostenlos", sondern „nicht gemessen": ohne
    /// `prefill_per_token_us` im Vertrag kann dieser Zaehler nichts wissen.
    ///
    /// Gezaehlt werden **abgeschlossene** Quanten. Ein Quantum, das im
    /// Backend gescheitert ist oder dessen Ergebnis veraltet ankam, hat
    /// gerechnet — aber wie lange, ist nicht bekannt. Dafuer einen Modellwert
    /// zu buchen hiesse, Arbeit zu erfinden; die Gesamtauslastung steht
    /// ohnehin in den Zaehlern, die dafuer da sind.
    pub generative_prefill_us: u64,
    /// Arbeit, die tatsaechlich Token erzeugt hat, in Mikrosekunden (NV-16).
    ///
    /// Die Gegengroesse zu [`Self::generative_prefill_us`]. Erst das
    /// Verhaeltnis der beiden sagt, ob sich die Zerlegung noch lohnt.
    pub generative_decode_us: u64,
    /// Feste Kosten der Quanten: Round-Trip und Scheduling im Backend, in
    /// Mikrosekunden (NV-16).
    ///
    /// Auf der Messmaschine der **groesste** Einzelterm der Zerlegung — 18 ms
    /// je Quantum bei rund 4 ms je Token. Er steht neben Prefill und
    /// Dekodierung, weil das Verhaeltnis sonst den dominierenden Kostenanteil
    /// auslaesst.
    pub generative_fixed_us: u64,
    /// Der laengste Kontext, den eine Fortsetzung getragen hat, in Token
    /// (NV-16).
    ///
    /// Die Groesse, an der die Zuschneidung des naechsten Quantums haengt. Sie
    /// waechst ueber die Lebensdauer eines Auftrags und faellt nie — steigt
    /// sie im Betrieb weit ueber das, womit kalibriert wurde, ist die
    /// Zuschneidung nicht mehr belegt.
    pub generative_context_tokens: u32,
    /// Auftraege, die ungeteilt liefen, weil die Zerlegung zu teuer war
    /// (NV-16).
    ///
    /// Der Rueckfall aus ADR-0014, gezaehlt. Ist er null, obwohl eine Grenze
    /// gesetzt ist, greift sie nie; ist er hoch, ist die Zerlegung fuer diese
    /// Vertraege die falsche Betriebsart.
    pub decomposition_refused: u64,
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

    /// Zaehlt einen Variantenwechsel eines Modells (Spec 19.7).
    ///
    /// `previous` ist die bis hierher gehaltene Variante. Ohne sie gab es
    /// keine Wahl, und eine erste Wahl ist kein Wechsel. Ein kleinerer Index
    /// bedeutet hoehere Qualitaet — dieselbe Ordnung, nach der die
    /// Hysterese Auf- und Abwertung unterscheidet.
    ///
    /// Aufgerufen beim Dispatch und nicht in der Auswahl: gezaehlt wird, was
    /// lief, nicht was erwogen wurde. Ein Kandidat kann mehrfach geplant und
    /// wieder zurueckgestellt werden.
    pub fn count_switch(
        &mut self,
        model: crate::ids::ModelIdx,
        previous: Option<crate::ids::VariantIdx>,
        chosen: crate::ids::VariantIdx,
    ) {
        let Some(previous) = previous else {
            return;
        };
        let counters = match chosen.cmp(&previous) {
            core::cmp::Ordering::Less => &mut self.variant_upgrades,
            core::cmp::Ordering::Greater => &mut self.variant_downgrades,
            core::cmp::Ordering::Equal => return,
        };
        if let Some(slot) = counters.get_mut(model.get()) {
            *slot = slot.saturating_add(1);
        }
    }
}
