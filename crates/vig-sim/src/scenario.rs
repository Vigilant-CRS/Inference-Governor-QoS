//! Lastszenarien fuer Gate S (Spec 19.4 bis 19.6).
//!
//! ADR-0001 verpflichtet Gate S darauf, alle Parameter offenzulegen und sie
//! **nicht** nachtraeglich zugunsten des Ergebnisses anzupassen. Deshalb stehen
//! sie hier zentral, benannt und versioniert — nicht verstreut im Testcode.

use vig_core::Duration;
use vig_core::arrayvec::ArrayVec;
use vig_core::ids::MAX_MODELS;
use vig_core::model::{ModelContract, Quality, QualitySource, QualityValue, Variant};
use vig_core::profile::{RuntimeProfile, VariantProfile};
use vig_core::queue::QueueConfig;
use vig_core::request::{Criticality, OverflowPolicy, QueuePolicy};

/// Eine physische Variante im Szenario.
#[derive(Debug, Clone, Copy)]
pub struct VariantSpec {
    /// Relative Qualitaet in Tausendsteln.
    pub quality_milli: u16,
    /// Median der Backendlaufzeit.
    pub p50: Duration,
    /// 99-%-Quantil der Backendlaufzeit.
    pub p99: Duration,
}

/// Ein periodischer Modellstrom im Szenario.
#[derive(Debug, Clone)]
pub struct StreamSpec {
    /// Sprechender Name fuer den Report.
    pub name: &'static str,
    /// Wichtigkeitsklasse.
    pub criticality: Criticality,
    /// Queue-Policy unter Vigilant. Die Baseline nutzt immer FIFO.
    pub policy: QueuePolicy,
    /// Nominale Periode zwischen zwei Captures.
    pub period: Duration,
    /// Maximale symmetrische Periodenabweichung.
    pub jitter: Duration,
    /// Verzoegerung zwischen Capture und Ankunft am Gateway.
    pub transport: Duration,
    /// Versatz des ersten Captures.
    ///
    /// Ohne Versatz treffen Stroeme mit verwandten Perioden kuenstlich immer
    /// gleichzeitig ein, was einen Sonderfall statt des Regelfalls misst.
    pub phase: Duration,
    /// Relative Deadline ab Generation Time.
    pub deadline: Duration,
    /// Fachliches Hoechstalter.
    pub max_age: Duration,
    /// Queue-Kapazitaet unter Vigilant.
    pub capacity: usize,
    /// Varianten, absteigend nach Qualitaet.
    pub variants: Vec<VariantSpec>,
}

impl StreamSpec {
    /// Der Ankunftsprozess dieses Stroms.
    #[must_use]
    pub const fn capture_spec(&self) -> crate::workload::PeriodicStream {
        crate::workload::PeriodicStream {
            period: self.period,
            jitter: self.jitter,
            transport: self.transport,
            phase: self.phase,
        }
    }

    /// Der Auslastungsbeitrag dieses Stroms in Promille: `C / T` mit `C` als
    /// Median der besten Variante (Spec 10.9).
    #[must_use]
    pub fn utilization_permille(&self) -> u64 {
        let Some(best) = self.variants.first() else {
            return 0;
        };
        best.p50.as_nanos().saturating_mul(1_000) / self.period.as_nanos().max(1)
    }

    fn scaled(&self, num: u64, den: u64) -> Self {
        let scale = |d: Duration| -> Duration {
            Duration::from_nanos_unbounded(d.as_nanos().saturating_mul(num.max(1)) / den.max(1))
        };
        Self {
            variants: self
                .variants
                .iter()
                .map(|v| VariantSpec {
                    quality_milli: v.quality_milli,
                    p50: scale(v.p50),
                    p99: scale(v.p99),
                })
                .collect(),
            ..self.clone()
        }
    }

    fn contract(&self, policy: QueuePolicy, capacity: usize) -> ModelContract {
        let mut variants = ArrayVec::new();
        for v in &self.variants {
            let _ = variants.push(Variant {
                quality: QualityValue {
                    value: Quality::from_milli(v.quality_milli).unwrap_or(Quality::FULL),
                    // Im Simulator sind die Qualitaetswerte gesetzt, nicht
                    // gemessen. Sie als `Measured` zu deklarieren waere im
                    // Report eine Falschaussage; `UserDeclared` erlaubt die
                    // automatische Wahl, ohne Evidenz zu behaupten.
                    source: QualitySource::UserDeclared,
                },
                // Das volle Profil, nicht nur p99: ADR-0010 braucht p50 als
                // optimistische Schaetzung fuer die Verwerfensentscheidung.
                // Wuerde hier `exact(p99)` stehen, waeren beide Schaetzer
                // identisch und die Asymmetrie waere wirkungslos.
                profile: VariantProfile::solo(
                    RuntimeProfile::new(
                        v.p50,
                        Duration::from_nanos_unbounded(u64::midpoint(
                            v.p50.as_nanos(),
                            v.p99.as_nanos(),
                        )),
                        v.p99,
                        RuntimeProfile::MIN_SAMPLES,
                    )
                    .unwrap_or(RuntimeProfile::exact(v.p99)),
                ),
                // Der Simulator beschreibt Lastszenarien; fachliche
                // Bedeutungen und Vorverarbeitungskosten hat er nicht (NV-10).
                semantics: vig_core::semantics::VariantSemantics::default(),
                preprocess: Duration::from_nanos_unbounded(0),
            });
        }
        ModelContract {
            // Bis das Backend etwas anderes sagt.
            variants_interchangeable: true,
            criticality: self.criticality,
            queue: QueueConfig {
                policy,
                capacity,
                overflow: OverflowPolicy::RejectNew,
            },
            period: Some(self.period),
            deadline: self.deadline,
            max_age: Some(self.max_age),
            stateful: false,
            min_quality: None,
            variant_dwell: Duration::from_nanos_unbounded(100_000_000),
            variants,
            cooperative: None,
            // Der Simulator beschreibt Lastszenarien, keine Vertragszusaetze.
            // Ein Zusatz haette hier keine Beobachtungsquelle (NV-02).
            extension: None,
        }
    }
}

/// Ein vollstaendiges Lastszenario.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// Name fuer den Report.
    pub name: &'static str,
    /// Messdauer.
    pub duration: Duration,
    /// Anzahl Backend-Execution-Slots.
    pub slots: usize,
    /// Zusaetzliche Kredite je Slot (ADR-0002).
    pub pipelining: usize,
    /// Die Modellstroeme.
    pub streams: Vec<StreamSpec>,
}

impl Scenario {
    /// Die Gesamtauslastung in Promille bei den aktuellen Laufzeiten.
    #[must_use]
    pub fn utilization_permille(&self) -> u64 {
        let sum: u64 = self
            .streams
            .iter()
            .map(StreamSpec::utilization_permille)
            .sum();
        sum / u64::try_from(self.slots.max(1)).unwrap_or(1)
    }

    /// Skaliert alle Laufzeiten so, dass die Gesamtauslastung `target_permille`
    /// betraegt.
    ///
    /// Die Perioden bleiben unveraendert — die Kamera liefert weiter 30 FPS,
    /// nur die Inferenz wird teurer. Das ist der realistische Fall: nicht der
    /// Sensor wird schneller, das Modell wird groesser oder die Hardware
    /// langsamer.
    #[must_use]
    pub fn at_load(&self, target_permille: u64) -> Self {
        let base = self.utilization_permille().max(1);
        Self {
            streams: self
                .streams
                .iter()
                .map(|s| s.scaled(target_permille, base))
                .collect(),
            ..self.clone()
        }
    }

    /// Die Modellvertraege fuer Vigilant: je Strom die konfigurierte Policy.
    #[must_use]
    pub fn vig_contracts(&self) -> ArrayVec<ModelContract, MAX_MODELS> {
        let mut out = ArrayVec::new();
        for s in &self.streams {
            let _ = out.push(s.contract(s.policy, s.capacity));
        }
        out
    }

    /// Die Modellvertraege fuer die Baseline: durchgehend FIFO mit der
    /// vorgegebenen Queue-Tiefe.
    ///
    /// Die Baseline bekommt bewusst dieselben Vertraege, Deadlines und
    /// Laufzeitprofile. Der einzige Unterschied ist, wie entschieden wird.
    #[must_use]
    pub fn baseline_contracts(&self, capacity: usize) -> ArrayVec<ModelContract, MAX_MODELS> {
        let mut out = ArrayVec::new();
        for s in &self.streams {
            let _ = out.push(s.contract(QueuePolicy::Fifo, capacity));
        }
        out
    }
}

fn ms(v: u64) -> Duration {
    Duration::from_nanos_unbounded(v.saturating_mul(1_000_000))
}

/// **Kernvergleich A — Freshness** (Spec 19.5).
///
/// Ein 30-FPS-Kamerastrom auf einer GPU, deren Servicekapazitaet absichtlich
/// unter der Bildrate liegt. Genau die Situation aus Spec 1.1: FIFO baut einen
/// Rueckstand auf und rechnet an einer immer aelteren Welt.
#[must_use]
pub fn freshness() -> Scenario {
    Scenario {
        name: "A-freshness",
        duration: ms(30_000),
        slots: 1,
        pipelining: 0,
        streams: vec![StreamSpec {
            name: "detector",
            criticality: Criticality::Protected,
            policy: QueuePolicy::Latest,
            period: ms(33),
            jitter: ms(2),
            transport: ms(3),
            phase: Duration::ZERO,
            deadline: ms(66),
            max_age: ms(66),
            capacity: 1,
            variants: vec![VariantSpec {
                quality_milli: 1_000,
                p50: ms(30),
                p99: ms(45),
            }],
        }],
    }
}

/// **Kernvergleich B — Protected versus Best Effort** (Spec 19.6).
///
/// Drei periodische Wahrnehmungsmodelle plus eine lange, unteilbare
/// Hintergrundlast. Das Beispiel aus Spec 1.3 und 4.3.
#[must_use]
pub fn protected_vs_best_effort() -> Scenario {
    Scenario {
        name: "B-protected-vs-best-effort",
        duration: ms(30_000),
        slots: 1,
        pipelining: 0,
        streams: vec![
            StreamSpec {
                name: "detector",
                criticality: Criticality::Protected,
                policy: QueuePolicy::Latest,
                period: ms(33),
                jitter: ms(2),
                transport: ms(3),
                phase: Duration::ZERO,
                deadline: ms(33),
                max_age: ms(66),
                capacity: 1,
                variants: vec![
                    VariantSpec {
                        quality_milli: 1_000,
                        p50: ms(10),
                        p99: ms(16),
                    },
                    VariantSpec {
                        quality_milli: 930,
                        p50: ms(6),
                        p99: ms(10),
                    },
                ],
            },
            StreamSpec {
                name: "depth",
                criticality: Criticality::High,
                policy: QueuePolicy::Latest,
                period: ms(66),
                jitter: ms(3),
                transport: ms(4),
                phase: ms(7),
                deadline: ms(66),
                max_age: ms(132),
                capacity: 1,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: ms(12),
                    p99: ms(20),
                }],
            },
            StreamSpec {
                name: "pose",
                criticality: Criticality::High,
                policy: QueuePolicy::Latest,
                period: ms(33),
                jitter: ms(2),
                transport: ms(3),
                phase: ms(11),
                deadline: ms(33),
                max_age: ms(66),
                capacity: 1,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: ms(8),
                    p99: ms(14),
                }],
            },
            StreamSpec {
                name: "vlm",
                criticality: Criticality::BestEffort,
                policy: QueuePolicy::Fifo,
                period: ms(500),
                jitter: ms(120),
                transport: ms(2),
                phase: ms(37),
                deadline: ms(800),
                max_age: ms(1_500),
                capacity: 4,
                variants: vec![VariantSpec {
                    quality_milli: 1_000,
                    p50: ms(200),
                    p99: ms(320),
                }],
            },
        ],
    }
}
