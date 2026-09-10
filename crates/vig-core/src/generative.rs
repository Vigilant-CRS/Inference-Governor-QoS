//! Fortschrittskosten zerlegter generativer Auftraege (NV-16).
//!
//! ## Die Luecke, um die es geht
//!
//! ADR-0014 zerlegt einen generativen Auftrag in Quanten, ADR-0015 hat die
//! Zuschneidung um einen festen Sockel ergaenzt. Dessen Kommentar nennt die
//! Ursache des Sockels bereits beim Namen: *„ohne wirksames Prefix-Caching —
//! die erneute Prefill-Berechnung des gewachsenen Prompts"*.
//!
//! **Gewachsen.** Der Sockel ist im bisherigen Modell eine Konstante, die
//! Sache, die er beschreibt, ist es nicht. Faehrt der Zustand im Prompt mit,
//! rechnet jede Fortsetzung den gesamten bisherigen Kontext neu — und der
//! waechst mit jedem erzeugten Token. Ein Auftrag in `n` Quanten leistet damit
//! `n` Prefills wachsender Laenge: der Aufwand ist **quadratisch** in der Zahl
//! der Fortsetzungen, nicht linear.
//!
//! Ein Modell mit konstantem Sockel sieht das nicht. Es plant jedes Quantum
//! gleich teuer, das spaete zieht ueber seine Luecke hinaus, und der
//! Look-ahead hat mit der falschen Zahl gerechnet.
//!
//! ## Was dieses Modul dagegen tut
//!
//! * **Kontextabhaengige Kosten.** `fest + prefill_je_token * kontext +
//!   dekodier_je_token * tokens`. Der Kontext ist Prompt plus alles bisher
//!   Erzeugte.
//! * **Der Preis der Zerlegung wird ausgewiesen.** [`Plan::overhead_permille`]
//!   sagt, wie viel mehr Arbeit eine Zerlegung kostet als der ungeteilte Lauf.
//!   Versteckter Fortschritt ist kein Fortschritt.
//! * **Eine Zerlegung, die sich nicht lohnt, wird nicht empfohlen.** Wenn das
//!   Re-Prefill mehr kostet, als die Unterbrechbarkeit einbringt, sagt
//!   [`Plan::verdict`] das, statt eine Zahl zu liefern und zu schweigen.
//!
//! ## Was hier nicht steht
//!
//! Nichts ueber KV-Caches. Faehrt das Backend einen wirksamen Prefix-Cache,
//! ist `prefill_per_token` klein bis null, und dieses Modell faellt auf das
//! alte zurueck — ohne Sonderfall. Das ist Absicht: ob ein Cache greift, ist
//! eine **Messfrage** und keine Annahme, die der Kern treffen darf.

use crate::time::Duration;

/// Was ein Token kostet, aufgeteilt nach seiner Ursache.
///
/// Gemessen, nicht geraten — wie jede Zahl, auf der geplant wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextCost {
    /// Fester Aufwand je Quantum: Round-Trip, Scheduling im Backend.
    ///
    /// **Ohne** den Prefill-Anteil — der steht unten und haengt vom Kontext ab.
    pub fixed: Duration,
    /// Aufwand je Token **bereits vorhandenen Kontexts**, bei jeder
    /// Fortsetzung erneut.
    ///
    /// Null heisst: das Backend faehrt einen wirksamen Prefix-Cache, oder es
    /// wurde nicht gemessen. Der Unterschied gehoert ins Profilmanifest, nicht
    /// hierher.
    pub prefill_per_token: Duration,
    /// Aufwand je **neu erzeugtem** Token.
    pub decode_per_token: Duration,
}

impl ContextCost {
    /// Was ein Quantum kostet, das `tokens` Token bei `context` Kontext erzeugt.
    #[must_use]
    pub fn quantum(&self, tokens: u32, context: u32) -> Duration {
        let prefill = self
            .prefill_per_token
            .as_nanos()
            .saturating_mul(u64::from(context));
        let decode = self
            .decode_per_token
            .as_nanos()
            .saturating_mul(u64::from(tokens));
        Duration::from_nanos_unbounded(
            self.fixed
                .as_nanos()
                .saturating_add(prefill)
                .saturating_add(decode),
        )
    }

    /// Ob ueberhaupt etwas gemessen wurde.
    #[must_use]
    pub const fn is_measured(&self) -> bool {
        self.decode_per_token.as_nanos() > 0
    }
}

/// Was ein Auftrag bisher gekostet hat.
///
/// Getrennt nach Prefill und Dekodierung, weil nur das eine Fortschritt ist.
/// Ein Re-Prefill erzeugt kein einziges Token; ihn als Fortschritt zu buchen
/// hiesse, dieselbe Arbeit zweimal zu verkaufen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    /// Wie lang der urspruengliche Prompt war.
    pub prompt_tokens: u32,
    /// Wie viele Token bisher erzeugt wurden.
    pub generated_tokens: u32,
    /// Wie viele Quanten dafuer noetig waren.
    pub quanta: u32,
    /// Aufwand, der in Prefill floss.
    pub prefill_work: Duration,
    /// Aufwand, der in Dekodierung floss.
    pub decode_work: Duration,
    /// Fester Aufwand je Quantum, aufsummiert.
    pub fixed_work: Duration,
}

impl Progress {
    /// Ein Auftrag am Anfang.
    #[must_use]
    pub const fn new(prompt_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            generated_tokens: 0,
            quanta: 0,
            prefill_work: Duration::from_nanos_unbounded(0),
            decode_work: Duration::from_nanos_unbounded(0),
            fixed_work: Duration::from_nanos_unbounded(0),
        }
    }

    /// Der Kontext, den die naechste Fortsetzung neu rechnen muss.
    #[must_use]
    pub const fn context(&self) -> u32 {
        self.prompt_tokens.saturating_add(self.generated_tokens)
    }

    /// Alles, was dieser Auftrag bisher gekostet hat.
    ///
    /// Sockel plus Prefill plus Dekodierung. Die Gegenprobe zu
    /// [`Plan::project`]: die geschlossene Formel dort ersetzt genau die
    /// Schleife, die diese Summe aufbaut, und
    /// `the_closed_form_matches_a_step_by_step_run` stellt beide gegeneinander.
    /// Ohne diese Summe waere die Formel nur behauptet.
    #[must_use]
    pub const fn total_work(&self) -> Duration {
        Duration::from_nanos_unbounded(
            self.fixed_work
                .as_nanos()
                .saturating_add(self.prefill_work.as_nanos())
                .saturating_add(self.decode_work.as_nanos()),
        )
    }

    /// Bucht ein abgeschlossenes Quantum.
    pub fn record(&mut self, tokens: u32, cost: ContextCost) {
        let context = self.context();
        self.prefill_work = Duration::from_nanos_unbounded(
            self.prefill_work.as_nanos().saturating_add(
                cost.prefill_per_token
                    .as_nanos()
                    .saturating_mul(u64::from(context)),
            ),
        );
        self.decode_work = Duration::from_nanos_unbounded(
            self.decode_work.as_nanos().saturating_add(
                cost.decode_per_token
                    .as_nanos()
                    .saturating_mul(u64::from(tokens)),
            ),
        );
        self.fixed_work = Duration::from_nanos_unbounded(
            self.fixed_work
                .as_nanos()
                .saturating_add(cost.fixed.as_nanos()),
        );
        self.generated_tokens = self.generated_tokens.saturating_add(tokens);
        self.quanta = self.quanta.saturating_add(1);
    }
}

/// Ob sich eine Zerlegung lohnt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Zerlegen. Der Aufschlag bleibt unter der Grenze des Betreibers.
    Decompose,
    /// Nicht zerlegen: der Aufschlag ueberschreitet die Grenze.
    ///
    /// Der Auftrag laeuft dann ungeteilt und blockiert seine Zeit am Stueck —
    /// was ADR-0012 beschreibt. Das ist eine Entscheidung mit Preis, und der
    /// Preis steht daneben.
    TooExpensive {
        /// Wie viel mehr Arbeit, in Promille.
        overhead_permille: u32,
        /// Was der Betreiber zulaesst.
        limit_permille: u32,
    },
    /// Es wurde nichts gemessen; eine Aussage waere geraten.
    Unmeasured,
}

/// Die vorausberechnete Zerlegung eines Auftrags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Wie viele Quanten es werden.
    pub quanta: u32,
    /// Was der ungeteilte Lauf kosten wuerde.
    pub undivided: Duration,
    /// Was die Zerlegung kostet.
    pub divided: Duration,
    /// Ob dem Plan ein gemessenes Kostenmodell zugrunde lag.
    ///
    /// Steht hier, nicht als zweites Argument an [`Plan::verdict`]: sonst
    /// koennte ein Plan aus einem Modell gegen die Messlage eines anderen
    /// geprueft werden, und niemand haette es gemerkt.
    pub measured: bool,
}

impl Plan {
    /// Rechnet die Zerlegung eines Auftrags voraus.
    ///
    /// `quantum_tokens` ist die geplante Groesse je Quantum; die letzte kann
    /// kleiner ausfallen.
    #[must_use]
    pub fn project(
        cost: ContextCost,
        prompt_tokens: u32,
        total_tokens: u32,
        quantum_tokens: u32,
    ) -> Self {
        // Ungeteilt: ein Prefill ueber den Prompt, dann alles dekodieren.
        let undivided = cost.quantum(total_tokens, prompt_tokens);

        if quantum_tokens == 0 || total_tokens == 0 {
            return Self {
                quanta: 0,
                undivided,
                divided: undivided,
                measured: cost.is_measured(),
            };
        }

        // Geschlossen gerechnet und nicht Quantum fuer Quantum. Eine Schleife
        // ueber die Quanten haette bei `total_tokens = u32::MAX` und
        // Quantengroesse 1 vier Milliarden Durchlaeufe gebraucht — in einer
        // Funktion, die oeffentlich ist und beliebige Eingaben nimmt.
        //
        // Die Prefill-Kontexte sind `prompt`, `prompt + q`, `prompt + 2q`, …
        // bis `prompt + (n-1)q`. Ihre Summe ist
        //
        //     n * prompt + q * n * (n - 1) / 2
        //
        // — und das `n²` darin **ist** der quadratische Aufwand, um den es in
        // diesem Modul geht. Er steht jetzt als Formel da statt als Verhalten
        // einer Schleife.
        let n = u64::from(total_tokens).div_ceil(u64::from(quantum_tokens));
        let q = u64::from(quantum_tokens);
        let context_sum = n.saturating_mul(u64::from(prompt_tokens)).saturating_add(
            // `n * (n - 1)` ist immer gerade; erst halbieren, dann mit `q`
            // multiplizieren saettigt spaeter als andersherum.
            n.saturating_mul(n.saturating_sub(1))
                .checked_div(2)
                .unwrap_or(0)
                .saturating_mul(q),
        );

        let divided = Duration::from_nanos_unbounded(
            cost.fixed
                .as_nanos()
                .saturating_mul(n)
                .saturating_add(
                    cost.prefill_per_token
                        .as_nanos()
                        .saturating_mul(context_sum),
                )
                .saturating_add(
                    cost.decode_per_token
                        .as_nanos()
                        .saturating_mul(u64::from(total_tokens)),
                ),
        );

        Self {
            quanta: u32::try_from(n).unwrap_or(u32::MAX),
            undivided,
            divided,
            measured: cost.is_measured(),
        }
    }

    /// Wie viel mehr Arbeit die Zerlegung kostet, in Promille.
    ///
    /// 0 heisst gleich teuer, 1000 heisst doppelt so teuer.
    #[must_use]
    pub fn overhead_permille(&self) -> u32 {
        let base = self.undivided.as_nanos();
        if base == 0 {
            return 0;
        }
        let extra = self.divided.as_nanos().saturating_sub(base);
        u32::try_from(extra.saturating_mul(1_000).checked_div(base).unwrap_or(0))
            .unwrap_or(u32::MAX)
    }

    /// Ob sich die Zerlegung innerhalb der Grenze des Betreibers lohnt.
    #[must_use]
    pub fn verdict(&self, limit_permille: u32) -> Verdict {
        if !self.measured {
            return Verdict::Unmeasured;
        }
        let overhead = self.overhead_permille();
        if overhead > limit_permille {
            return Verdict::TooExpensive {
                overhead_permille: overhead,
                limit_permille,
            };
        }
        Verdict::Decompose
    }
}

/// Zeit bis zum ersten Token und zwischen Token.
///
/// Die beiden Groessen, an denen ein Nutzer einen generativen Dienst misst.
/// Sie stehen hier getrennt, weil die Zerlegung sie **gegenlaeufig**
/// beeinflusst: kleine Quanten verkuerzen die Wartezeit auf das erste Token
/// und verlaengern den Abstand zwischen den spaeteren.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Latencies {
    /// Zeit bis zum ersten Token.
    pub ttft: Duration,
    /// Groesster beobachteter Abstand zwischen zwei Token.
    pub worst_tbt: Duration,
}

impl Latencies {
    /// Rechnet beide Groessen aus einer geplanten Zerlegung voraus.
    ///
    /// Vereinfachend: innerhalb eines Quantums fliessen die Token gleichmaessig,
    /// zwischen zwei Quanten liegt die Wartezeit auf den naechsten Slot. Die
    /// Wartezeit kennt der Aufrufer, nicht dieses Modul.
    #[must_use]
    pub fn project(
        cost: ContextCost,
        prompt_tokens: u32,
        total_tokens: u32,
        quantum_tokens: u32,
        gap_between_quanta: Duration,
    ) -> Self {
        // Bis zum ersten Token: fester Aufwand, Prefill ueber den Prompt, ein
        // Token dekodieren.
        let ttft = Duration::from_nanos_unbounded(
            cost.fixed
                .as_nanos()
                .saturating_add(
                    cost.prefill_per_token
                        .as_nanos()
                        .saturating_mul(u64::from(prompt_tokens)),
                )
                .saturating_add(cost.decode_per_token.as_nanos()),
        );
        // Der schlimmste Abstand liegt an der **letzten** Quantengrenze: dort
        // ist der Kontext am groessten, und genau dort ist das Re-Prefill am
        // teuersten. Mit dem Prompt allein zu rechnen waere derselbe Fehler,
        // den dieses Modul behebt — nur an einer anderen Stelle.
        //
        // Ohne Grenze gibt es keinen Uebergang: das ist nicht nur
        // `quantum_tokens == 0` (die Kodierung fuer „ungeteilt"), sondern
        // ebenso ein Quantum, das den ganzen Auftrag traegt. Beides ist ein
        // Lauf am Stueck, und dort ist der groesste Abstand die Dekodierzeit
        // eines Tokens.
        let quanta = if quantum_tokens == 0 {
            0
        } else {
            u64::from(total_tokens).div_ceil(u64::from(quantum_tokens))
        };
        let worst_tbt = if quanta <= 1 {
            cost.decode_per_token
        } else {
            // Die letzte Grenze liegt bei `(n-1) * q` erzeugten Token, nicht
            // bei `total - q`. Die beiden fallen nur zusammen, wenn `q` den
            // Auftrag glatt teilt; sonst unterschaetzt die Differenz den
            // Kontext um bis zu `q-1` Token — dieselbe Fehlerrichtung, gegen
            // die der Absatz darueber argumentiert.
            let last_boundary = prompt_tokens.saturating_add(
                u32::try_from(
                    quanta
                        .saturating_sub(1)
                        .saturating_mul(u64::from(quantum_tokens)),
                )
                .unwrap_or(u32::MAX),
            );
            Duration::from_nanos_unbounded(
                gap_between_quanta
                    .as_nanos()
                    .saturating_add(cost.fixed.as_nanos())
                    .saturating_add(
                        cost.prefill_per_token
                            .as_nanos()
                            .saturating_mul(u64::from(last_boundary)),
                    )
                    .saturating_add(cost.decode_per_token.as_nanos()),
            )
        };
        Self { ttft, worst_tbt }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn us(v: u64) -> Duration {
        Duration::from_micros(v).unwrap()
    }

    /// Gemessen auf der Messmaschine: 18 ms Sockel, rund 4 ms je Token.
    /// Der Prefill-Anteil ist der Wert, den NV-16 hinzufuegt.
    fn measured() -> ContextCost {
        ContextCost {
            fixed: us(2_000),
            prefill_per_token: us(20),
            decode_per_token: us(4_000),
        }
    }

    /// Ein Backend mit wirksamem Prefix-Cache: kein Prefill-Anteil.
    fn cached() -> ContextCost {
        ContextCost {
            prefill_per_token: us(0),
            ..measured()
        }
    }

    // -- Kontextabhaengige Kosten -----------------------------------------

    #[test]
    fn a_later_quantum_costs_more_than_an_earlier_one() {
        // Der Kern von NV-16: dasselbe Quantum, mehr Kontext, mehr Kosten.
        let c = measured();
        let early = c.quantum(10, 100);
        let late = c.quantum(10, 900);
        assert!(
            late.as_micros() > early.as_micros(),
            "{} <= {}",
            late.as_micros(),
            early.as_micros()
        );
        // 800 Token mehr Kontext zu 20 us: genau 16 ms mehr.
        assert_eq!(late.as_micros() - early.as_micros(), 16_000);
    }

    #[test]
    fn without_a_prefill_term_the_model_falls_back_to_the_old_one() {
        // Faehrt das Backend einen wirksamen Prefix-Cache, ist der Kontext
        // egal — ohne Sonderfall im Code.
        let c = cached();
        assert_eq!(c.quantum(10, 100), c.quantum(10, 10_000));
    }

    /// Ohne Messung gibt es keine Aussage.
    ///
    /// `is_measured` haengt allein an der Dekodierrate: ohne sie laesst sich
    /// aus einer Dauer keine Tokenzahl ableiten, und ein Urteil ueber den
    /// Preis der Zerlegung waere geraten.
    #[test]
    fn an_unmeasured_cost_model_says_so() {
        assert!(!ContextCost::default().is_measured());
        assert!(measured().is_measured());
        assert!(
            cached().is_measured(),
            "ein wirksamer Prefix-Cache ist eine Messung, kein fehlender Wert"
        );
    }

    // -- Fortschritt und Buchhaltung --------------------------------------

    #[test]
    fn prefill_work_is_booked_separately_from_progress() {
        // Ein Re-Prefill erzeugt kein einziges Token. Ihn als Fortschritt zu
        // buchen hiesse, dieselbe Arbeit zweimal zu verkaufen.
        let c = measured();
        let mut p = Progress::new(500);
        p.record(10, c);
        assert_eq!(p.generated_tokens, 10);
        assert_eq!(p.prefill_work.as_micros(), 500 * 20);
        assert_eq!(p.decode_work.as_micros(), 10 * 4_000);
        assert_eq!(p.context(), 510, "der naechste Prefill ist laenger");
    }

    #[test]
    fn the_context_grows_with_every_quantum() {
        let c = measured();
        let mut p = Progress::new(100);
        for _ in 0..5 {
            p.record(20, c);
        }
        assert_eq!(p.generated_tokens, 100);
        assert_eq!(p.quanta, 5);
        assert_eq!(p.context(), 200);
        // Prefill ueber 100, 120, 140, 160, 180 Token.
        assert_eq!(
            p.prefill_work.as_micros(),
            (100 + 120 + 140 + 160 + 180) * 20
        );
    }

    // -- Der Preis der Zerlegung ------------------------------------------

    #[test]
    fn decomposition_costs_more_and_the_model_says_how_much() {
        let c = measured();
        let plan = Plan::project(c, 500, 200, 20);
        assert_eq!(plan.quanta, 10);
        assert!(
            plan.divided.as_micros() > plan.undivided.as_micros(),
            "versteckter Fortschritt ist kein Fortschritt"
        );
        assert!(
            plan.overhead_permille() > 0,
            "Aufschlag: {}",
            plan.overhead_permille()
        );
    }

    /// Feiner zerlegen kostet linear mehr — die Steigung macht der Cache.
    ///
    /// Der urspruengliche Test hiess „waechst superlinear mit der Zahl der
    /// Quanten" und war auch dann gruen, wenn es den Prefill-Term gar nicht
    /// gab. Nachgerechnet stimmt der Name auch nicht: bei fester Auftragsgroesse
    /// schrumpft das Quantum, waehrend die Zahl der Quanten waechst, und die
    /// Prefill-Summe `q * n * (n-1) / 2` wird damit zu `total * (n-1) / 2` —
    /// **linear** in `n`. Der Aufschlag insgesamt ist
    ///
    ///     (n - 1) * (fixed + prefill * prompt + prefill * total / 2)
    ///
    /// also eine Gerade durch den Ursprung. Was der Prefill-Term aendert, ist
    /// nicht ihre Form, sondern ihre **Steigung** — und die um ein Vielfaches.
    /// Das ist der pruefbare Unterschied, und er steht jetzt hier.
    ///
    /// Wo die Zerlegung wirklich quadratisch wird, zeigt der Test darunter.
    #[test]
    fn a_finer_split_costs_linearly_more_and_the_cache_sets_the_slope() {
        let extra = |c: ContextCost, q: u32| {
            let plan = Plan::project(c, 500, 400, q);
            plan.divided
                .as_nanos()
                .saturating_sub(plan.undivided.as_nanos())
        };

        // Vier mal so viele Quanten (n = 4 -> 16), also `(n-1)` von 3 auf 15:
        // genau fuenf mal so viel Aufschlag. In beiden Faellen.
        let cold = (extra(cached(), 100), extra(cached(), 25));
        let hot = (extra(measured(), 100), extra(measured(), 25));
        assert_eq!(cold.1, cold.0.saturating_mul(5), "linear, mit Cache");
        assert_eq!(hot.1, hot.0.saturating_mul(5), "linear, ohne Cache");

        // Der Unterschied liegt in der Steigung: ohne Cache kostet dieselbe
        // Zerlegung ein Vielfaches.
        assert!(
            hot.0 > cold.0.saturating_mul(5),
            "ohne Cache ist die Gerade viel steiler: {} gegen {}",
            hot.0,
            cold.0
        );
    }

    /// Bei fester Quantengroesse waechst die Prefill-Arbeit quadratisch.
    ///
    /// Hier steckt der eigentliche Befund von NV-16: ein doppelt so langer
    /// Auftrag braucht doppelt so viele Quanten, und **jedes** davon traegt
    /// einen laengeren Kontext. Der Zuwachs steigt deshalb mit jeder
    /// Verdopplung weiter an, statt konstant zu bleiben — er strebt gegen den
    /// Faktor vier.
    ///
    /// Mit wirksamem Prefix-Cache verdoppelt sich der Aufschlag schlicht.
    /// Beides steht hier nebeneinander, weil erst der Vergleich die Aussage
    /// traegt.
    #[test]
    fn a_longer_job_at_the_same_quantum_size_costs_quadratically_more() {
        let extra = |c: ContextCost, total: u32| {
            let plan = Plan::project(c, 500, total, 25);
            plan.divided
                .as_nanos()
                .saturating_sub(plan.undivided.as_nanos())
        };

        let hot: Vec<u64> = [400, 800, 1_600]
            .into_iter()
            .map(|t| extra(measured(), t))
            .collect();
        let growth = |v: &[u64], i: usize| {
            v[i.saturating_add(1)]
                .saturating_mul(1_000)
                .checked_div(v[i])
                .unwrap_or(0)
        };
        assert!(
            growth(&hot, 1) > growth(&hot, 0),
            "der Zuwachs nimmt mit jeder Verdopplung zu: {} dann {} (Promille)",
            growth(&hot, 0),
            growth(&hot, 1)
        );
        assert!(
            growth(&hot, 0) > 2_000,
            "schon die erste Verdopplung kostet mehr als das Doppelte: {}",
            growth(&hot, 0)
        );

        // Die Gegenprobe, und sie ist die eigentliche Aussage: mit Cache ist
        // der Aufschlag `(n-1) * fixed`, und sein Zuwachs **faellt** mit jeder
        // Verdopplung gegen genau zwei. Ohne Cache steigt er. Dieselbe
        // Messreihe, entgegengesetzte Richtung — daran und an nichts anderem
        // haengt das Wort „quadratisch".
        let cold: Vec<u64> = [400, 800, 1_600]
            .into_iter()
            .map(|t| extra(cached(), t))
            .collect();
        assert!(
            growth(&cold, 1) < growth(&cold, 0),
            "mit Cache faellt der Zuwachs: {} dann {}",
            growth(&cold, 0),
            growth(&cold, 1)
        );
        assert!(
            growth(&cold, 1) < 2_050,
            "und er strebt gegen zwei: {}",
            growth(&cold, 1)
        );
    }

    /// Die letzte Quantengrenze liegt bei `(n-1) * q`, nicht bei `total - q`.
    ///
    /// Die beiden fallen nur zusammen, wenn die Quantengroesse den Auftrag
    /// glatt teilt. Genau das taten alle frueheren Testeingaben, und deshalb
    /// blieb der Fehler unsichtbar: bei `total = 45`, `q = 20` liegen die
    /// Grenzen bei 20 und 40 erzeugten Token, die alte Formel rechnete 25.
    #[test]
    fn the_last_boundary_is_right_even_when_the_quantum_does_not_divide_evenly() {
        let c = measured();
        let uneven = Latencies::project(c, 1_000, 45, 20, us(30_000));
        // Letzte Grenze: 1000 Prompt + 40 erzeugte Token.
        let expected = us(30_000)
            .as_nanos()
            .saturating_add(c.fixed.as_nanos())
            .saturating_add(us(1_040 * 20).as_nanos())
            .saturating_add(c.decode_per_token.as_nanos());
        assert_eq!(uneven.worst_tbt.as_nanos(), expected);

        // Und bei glatter Teilung bleibt es beim alten Ergebnis.
        let even = Latencies::project(c, 1_000, 40, 20, us(30_000));
        assert_eq!(
            even.worst_tbt.as_nanos(),
            us(30_000)
                .as_nanos()
                .saturating_add(c.fixed.as_nanos())
                .saturating_add(us(1_020 * 20).as_nanos())
                .saturating_add(c.decode_per_token.as_nanos())
        );
    }

    /// Ein Quantum, das den ganzen Auftrag traegt, hat keine Grenze.
    ///
    /// `quantum_tokens == 0` ist die Kodierung fuer „ungeteilt", aber nicht
    /// der einzige Fall: `q >= total` ist genauso ein Lauf am Stueck. Vorher
    /// meldete die Funktion dafuer eine erfundene Uebergangszeit von 42 ms,
    /// wo es 4 ms sind.
    #[test]
    fn a_quantum_that_carries_the_whole_job_has_no_boundary() {
        let c = measured();
        assert_eq!(
            Latencies::project(c, 500, 200, 200, us(30_000)).worst_tbt,
            c.decode_per_token,
            "n = 1: es gibt keinen Uebergang"
        );
        assert_eq!(
            Latencies::project(c, 500, 200, 500, us(30_000)).worst_tbt,
            c.decode_per_token,
            "ein zu grosses Quantum ebenso"
        );
    }

    #[test]
    fn with_a_prefix_cache_decomposition_is_nearly_free() {
        let c = cached();
        let plan = Plan::project(c, 500, 400, 25);
        assert_eq!(plan.quanta, 16);
        // Nur der feste Sockel je Quantum bleibt.
        assert!(
            plan.overhead_permille() < 200,
            "Aufschlag {} Promille",
            plan.overhead_permille()
        );
    }

    #[test]
    fn an_undivided_job_has_no_overhead() {
        let c = measured();
        let plan = Plan::project(c, 500, 200, 200);
        assert_eq!(plan.quanta, 1);
        assert_eq!(plan.overhead_permille(), 0);
    }

    #[test]
    fn a_last_quantum_smaller_than_the_others_is_counted() {
        let c = cached();
        let plan = Plan::project(c, 100, 45, 20);
        assert_eq!(plan.quanta, 3, "20 + 20 + 5");
    }

    // -- Das Urteil --------------------------------------------------------

    #[test]
    fn a_decomposition_beyond_the_limit_is_refused_with_its_price() {
        let c = measured();
        let plan = Plan::project(c, 2_000, 400, 20);
        match plan.verdict(100) {
            Verdict::TooExpensive {
                overhead_permille,
                limit_permille,
            } => {
                assert!(overhead_permille > 100);
                assert_eq!(limit_permille, 100);
            }
            other => panic!("erwartet TooExpensive, bekam {other:?}"),
        }
    }

    #[test]
    fn a_cheap_decomposition_is_recommended() {
        let c = cached();
        let plan = Plan::project(c, 500, 400, 100);
        assert_eq!(plan.verdict(500), Verdict::Decompose);
    }

    #[test]
    fn without_a_measurement_there_is_no_verdict() {
        let c = ContextCost::default();
        let plan = Plan::project(c, 500, 400, 20);
        assert_eq!(
            plan.verdict(100),
            Verdict::Unmeasured,
            "eine Aussage waere geraten"
        );
    }

    // -- TTFT und TBT ------------------------------------------------------

    #[test]
    fn smaller_quanta_shorten_the_first_token_and_lengthen_the_later_gaps() {
        // Die beiden Groessen laufen gegeneinander, und beide stehen deshalb
        // getrennt da.
        let c = measured();
        let l = Latencies::project(c, 500, 200, 20, us(30_000));
        assert_eq!(
            l.ttft.as_micros(),
            2_000 + 500 * 20 + 4_000,
            "Sockel, Prefill ueber den Prompt, ein Token"
        );
        assert!(
            l.worst_tbt.as_micros() > l.ttft.as_micros(),
            "an der Quantengrenze kommt die Wartezeit auf den Slot dazu"
        );
    }

    #[test]
    fn an_undivided_job_has_no_quantum_boundary() {
        let c = measured();
        let l = Latencies::project(c, 500, 200, 0, us(30_000));
        assert_eq!(
            l.worst_tbt, c.decode_per_token,
            "ohne Zerlegung ist der schlimmste Abstand ein Token"
        );
    }

    // -- Grenzen -----------------------------------------------------------

    #[test]
    fn a_zero_token_job_is_not_a_decomposition() {
        let c = measured();
        let plan = Plan::project(c, 500, 0, 20);
        assert_eq!(plan.quanta, 0);
        assert_eq!(plan.overhead_permille(), 0);
    }

    #[test]
    fn the_projection_terminates_on_extreme_input() {
        let c = cached();
        let plan = Plan::project(c, u32::MAX, 10_000, 1);
        assert_eq!(plan.quanta, 10_000);
        // Und auch dort, wo eine Schleife vier Milliarden Durchlaeufe
        // gebraucht haette.
        let huge = Plan::project(measured(), 1_000, u32::MAX, 1);
        assert_eq!(huge.quanta, u32::MAX);
    }

    #[test]
    fn the_closed_form_matches_a_step_by_step_run() {
        // Die Formel ersetzt eine Schleife; sie muss dasselbe ergeben.
        let c = measured();
        for (prompt, total, q) in [(500, 200, 20), (0, 100, 7), (1_000, 45, 20), (10, 10, 10)] {
            let mut p = Progress::new(prompt);
            let mut remaining = total;
            while remaining > 0 {
                let step = remaining.min(q);
                p.record(step, c);
                remaining -= step;
            }
            let plan = Plan::project(c, prompt, total, q);
            assert_eq!(plan.quanta, p.quanta, "Quanten bei {prompt}/{total}/{q}");
            assert_eq!(
                plan.divided.as_nanos(),
                p.total_work().as_nanos(),
                "Aufwand bei {prompt}/{total}/{q}"
            );
        }
    }

    #[test]
    fn the_worst_gap_uses_the_largest_context_not_the_prompt() {
        // Der schlimmste Abstand liegt an der letzten Quantengrenze. Mit dem
        // Prompt allein zu rechnen waere derselbe Fehler, den dieses Modul
        // behebt — nur an anderer Stelle.
        let c = measured();
        let short = Latencies::project(c, 500, 40, 20, us(0));
        let long = Latencies::project(c, 500, 400, 20, us(0));
        assert!(
            long.worst_tbt.as_micros() > short.worst_tbt.as_micros(),
            "{} <= {}",
            long.worst_tbt.as_micros(),
            short.worst_tbt.as_micros()
        );
        // 380 gegen 20 Token mehr Kontext, je 20 us.
        assert_eq!(
            long.worst_tbt.as_micros() - short.worst_tbt.as_micros(),
            (380 - 20) * 20
        );
    }
}
