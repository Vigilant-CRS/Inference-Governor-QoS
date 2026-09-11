//! Datenpfadbudgets: was der Governor je Request hoechstens kosten darf
//! (NV-20).
//!
//! Die Roadmap nennt „Datenpfadbudgets" als Abnahmekriterium einer Freigabe
//! und definiert sie nirgends. Hier stehen sie als Zahlen, gegen die zwei
//! Pruefungen messen — und nicht als Absatz, den niemand prueft:
//!
//! * `datapath_budgets_hold` in `tests/end_to_end.rs`, gegen ein
//!   gRPC-Mock-Backend, ohne GPU, als Releasequalifikation mit `--ignored`;
//! * `shm-latency` gegen echten Triton, auf der Freigabemaschine.
//!
//! Beide lesen dieselbe Tabelle und dieselbe Quantildefinition. Zwei Tabellen
//! waeren zwei Budgets, und nach dem ersten Nachziehen nur noch eines davon
//! richtig.
//!
//! ## Was ein Budget ist
//!
//! Der **Zusatzaufwand** des Governors gegenueber einem direkten Aufruf
//! desselben Backends, je Pfad und Nutzlastgroesse: p50 und p99 in
//! Mikrosekunden, und der Anteil des p50-Zusatzes am direkten Aufruf.
//! Verglichen werden die Quantile zweier Messreihen, nicht Differenzen
//! gepaarter Requests — ein Request laeuft entweder direkt oder ueber den
//! Governor, nie beides.
//!
//! ## Woher die Zahlen kommen
//!
//! Aus `docs/benchmark/data-plane.md` (Mittelwerte ueber 40 Runden, 5 ms
//! Backendzeit) und aus Spec 4.4 und 19.8: weniger als 3–5 %
//! End-to-End-Regression ist das Produktgate, mehr als 5 % ein
//! Kill-Kriterium. Ein p99 war dort nicht gemessen. Die p99-Grenzen sind
//! deshalb eine **Annahme**, die die erste Qualifikationsmessung bestaetigen
//! oder korrigieren muss — und das steht hier, damit niemand sie fuer
//! gemessen haelt.
//!
//! Die Zahlen gelten fuer diese Maschinenklasse: ein x86-Laptop,
//! Loopback-Transport, Governor und Backend auf demselben Host. Auf einem
//! Jetson sind sie neu zu messen, nicht zu uebernehmen.

use core::fmt;

/// Der Weg, auf dem die Tensordaten reisen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// Im Request reist nur eine Referenz auf System Shared Memory
    /// (ADR-0003). Der Governor beruehrt die Tensordaten nie.
    SharedMemory,
    /// Die Nutzlast reist im Request. Der Governor deserialisiert und
    /// serialisiert sie je Hop.
    GrpcCopy,
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SharedMemory => "Shm-Referenz",
            Self::GrpcCopy => "gRPC-Kopie",
        })
    }
}

/// Was fuer einen Messpunkt gilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Ein Budget. Jede der drei Grenzen muss fuer sich halten.
    Budgeted {
        /// Hoechster Zusatzaufwand im Median, in Mikrosekunden.
        p50_us: u64,
        /// Hoechster Zusatzaufwand im p99, in Mikrosekunden.
        p99_us: u64,
        /// Hoechster Anteil des Median-Zusatzes am direkten Aufruf, in
        /// Prozent. Bewertet nur ab [`REFERENCE_CALL_US`].
        relative_percent: u64,
    },
    /// Kein Budget, weil der Pfad fuer diese Nutzlast nicht unterstuetzt ist.
    ///
    /// Bewusst keine grosszuegige Grenze: ein Budget, das 89 % Aufschlag
    /// zulaesst, waere die Aussage, dass 89 % in Ordnung sind. Gemessen und
    /// berichtet wird trotzdem — ein nicht unterstuetzter Pfad soll nicht
    /// unbeobachtet schlechter werden.
    NotSupported {
        /// Warum, in einem Satz.
        reason: &'static str,
    },
}

/// Ein Messpunkt der Tabelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    /// Der Transportweg.
    pub path: Path,
    /// Die Nutzlast je Request, in Bytes.
    pub payload_bytes: u64,
    /// Wie der Punkt im Bericht heisst.
    pub label: &'static str,
    /// Was gilt.
    pub limit: Limit,
}

/// Unterhalb dieser Dauer des direkten Aufrufs wird der Anteil nicht
/// bewertet, in Mikrosekunden.
///
/// Spec 19.8 spricht von der End-to-End-Regression einer Pipeline, nicht vom
/// Anteil an einem einzelnen, sehr kurzen Aufruf. Bei einem 2-ms-Modell sind
/// 160 us schon 8 % — eine Aussage ueber die Modellwahl, nicht ueber eine
/// Regression des Governors. Bei 6 ms fallen die absolute Grenze des
/// Shm-Pfads (300 us) und die 5 % aus Spec 19.8 zusammen; darunter entscheidet
/// die absolute Grenze allein.
pub const REFERENCE_CALL_US: u64 = 6_000;

/// Das Budget des Shm-Pfads — fuer jede Nutzlastgroesse dasselbe.
///
/// Gemessen: +160 us bei 6,2 MB, 2 % eines 6,2-ms-Aufrufs
/// (`data-plane.md`). 300 us sind knapp das Doppelte davon und genau 5 % von
/// [`REFERENCE_CALL_US`].
///
/// **Dieselbe Grenze fuer jede Groesse ist die eigentliche Pruefung.** Die
/// Zusage von ADR-0003 ist, dass der Aufwand nicht mit der Tensorgroesse
/// waechst. Ein Governor, der doch kopiert, kostet bei 1,2 MB rund 2,5 ms und
/// bei 6,2 MB rund 11,7 ms und faellt hier weit durch. Bei 150 KB kostete
/// eine Kopie nur rund 240 us und bliebe unter der Grenze — dort faengt sie
/// nicht die Zeitmessung, sondern die strukturelle Pruefung
/// `the_shm_path_never_carries_the_payload`, die in jedem Testlauf laeuft.
///
/// p99: 1 000 us, eine Annahme (siehe Moduldoku).
const SHM_LIMIT: Limit = Limit::Budgeted {
    p50_us: 300,
    p99_us: 1_000,
    relative_percent: 5,
};

/// Die Budgettabelle. Die einzige.
pub const POINTS: [Point; 6] = [
    Point {
        path: Path::SharedMemory,
        payload_bytes: 224 * 224 * 3,
        label: "150 KB",
        limit: SHM_LIMIT,
    },
    Point {
        path: Path::SharedMemory,
        payload_bytes: 640 * 640 * 3,
        label: "1,2 MB",
        limit: SHM_LIMIT,
    },
    Point {
        path: Path::SharedMemory,
        payload_bytes: 1920 * 1080 * 3,
        label: "6,2 MB",
        limit: SHM_LIMIT,
    },
    // Gemessen: +238 us, 2 % eines 8,1-ms-Aufrufs. Eine
    // Klassifikationseingabe, fuer die der Copy-Pfad unterstuetzt ist.
    // 500 us sind rund das Doppelte; p99 1 500 us ist eine Annahme.
    Point {
        path: Path::GrpcCopy,
        payload_bytes: 224 * 224 * 3,
        label: "150 KB",
        limit: Limit::Budgeted {
            p50_us: 500,
            p99_us: 1_500,
            relative_percent: 5,
        },
    },
    // Gemessen: +2 559 us, 26 % — das Fuenffache des Kill-Kriteriums.
    Point {
        path: Path::GrpcCopy,
        payload_bytes: 640 * 640 * 3,
        label: "1,2 MB",
        limit: Limit::NotSupported {
            reason: "Kameraframes gehoeren auf den Shm-Pfad (ADR-0003, Spec 19.8)",
        },
    },
    // Gemessen: +11 692 us, 89 % — das Achtzehnfache des Kill-Kriteriums.
    Point {
        path: Path::GrpcCopy,
        payload_bytes: 1920 * 1080 * 3,
        label: "6,2 MB",
        limit: Limit::NotSupported {
            reason: "Kameraframes gehoeren auf den Shm-Pfad (ADR-0003, Spec 19.8)",
        },
    },
];

/// Das Budget des Shm-Pfads, fuer eine Messung gegen echten Triton.
///
/// Dort ist die Nutzlast die Eingabe des gewaehlten Modells und keine der
/// Tabellengroessen; das Budget ist fuer jede Groesse dasselbe.
#[must_use]
pub const fn shared_memory() -> Limit {
    SHM_LIMIT
}

/// Median und p99 einer Messreihe, in Mikrosekunden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Latency {
    /// Der Median.
    pub p50_us: u64,
    /// Das 99. Perzentil.
    pub p99_us: u64,
}

impl Latency {
    /// Aus einer Messreihe; `None` ohne Messwerte.
    ///
    /// Nearest-Rank-Quantile, dieselbe Definition, die `shm-latency` immer
    /// benutzt hat. Beide Pruefungen rechnen damit, damit „p99" in beiden
    /// dasselbe heisst.
    #[must_use]
    pub fn from_samples(samples: &mut [u64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        let last = samples.len().saturating_sub(1);
        let pick = |percent: usize| -> u64 {
            let index = samples
                .len()
                .saturating_mul(percent)
                .checked_div(100)
                .unwrap_or(0)
                .min(last);
            samples.get(index).copied().unwrap_or(0)
        };
        Some(Self {
            p50_us: pick(50),
            p99_us: pick(99),
        })
    }
}

/// Wie ein Messpunkt ausgegangen ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Alle Grenzen halten.
    Pass,
    /// Mindestens eine Grenze ist gerissen; welche, steht einzeln da.
    Fail {
        /// Der Median-Zusatz liegt ueber der Grenze.
        p50: bool,
        /// Der p99-Zusatz liegt ueber der Grenze.
        p99: bool,
        /// Der Anteil am direkten Aufruf liegt ueber der Grenze.
        relative: bool,
    },
    /// Fuer diesen Punkt gibt es kein Budget.
    NotBudgeted {
        /// Warum.
        reason: &'static str,
    },
}

impl Outcome {
    /// Ob der Punkt eine Freigabe verhindert.
    #[must_use]
    pub const fn is_fail(self) -> bool {
        matches!(self, Self::Fail { .. })
    }
}

/// Die Bewertung eines Messpunkts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assessment {
    /// Was galt.
    pub limit: Limit,
    /// Median-Zusatz des Governors, in Mikrosekunden.
    pub overhead_p50_us: u64,
    /// p99-Zusatz des Governors, in Mikrosekunden.
    pub overhead_p99_us: u64,
    /// Anteil des Median-Zusatzes am direkten Aufruf, in Promille.
    ///
    /// `None`, wenn der direkte Aufruf kuerzer als [`REFERENCE_CALL_US`]
    /// war: dann wird der Anteil nicht bewertet, und das soll im Bericht
    /// stehen statt einer Zahl, die nichts entscheidet.
    pub relative_permille: Option<u64>,
    /// Das Ergebnis.
    pub outcome: Outcome,
}

/// Bewertet eine Messung gegen ihr Budget.
///
/// Ist der Governor schneller als der direkte Aufruf — bei Streuung kommt das
/// vor —, zaehlt der Zusatz als null und nicht als Gutschrift.
#[must_use]
pub fn assess(limit: Limit, direct: Latency, governed: Latency) -> Assessment {
    let overhead_p50_us = governed.p50_us.saturating_sub(direct.p50_us);
    let overhead_p99_us = governed.p99_us.saturating_sub(direct.p99_us);
    let relative_permille = (direct.p50_us >= REFERENCE_CALL_US).then(|| {
        overhead_p50_us
            .saturating_mul(1_000)
            .checked_div(direct.p50_us)
            .unwrap_or(0)
    });
    let outcome = match limit {
        Limit::NotSupported { reason } => Outcome::NotBudgeted { reason },
        Limit::Budgeted {
            p50_us,
            p99_us,
            relative_percent,
        } => {
            let p50 = overhead_p50_us > p50_us;
            let p99 = overhead_p99_us > p99_us;
            let relative =
                relative_permille.is_some_and(|r| r > relative_percent.saturating_mul(10));
            if p50 || p99 || relative {
                Outcome::Fail { p50, p99, relative }
            } else {
                Outcome::Pass
            }
        }
    };
    Assessment {
        limit,
        overhead_p50_us,
        overhead_p99_us,
        relative_permille,
        outcome,
    }
}

impl fmt::Display for Assessment {
    /// Eine Zeile Urteil, mit der gerissenen Grenze und ihrem Messwert.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.outcome, self.limit) {
            (Outcome::Pass, _) => f.write_str("PASS"),
            (Outcome::NotBudgeted { reason }, _) => write!(f, "NICHT BUDGETIERT — {reason}"),
            (
                Outcome::Fail { p50, p99, relative },
                Limit::Budgeted {
                    p50_us,
                    p99_us,
                    relative_percent,
                },
            ) => {
                f.write_str("FAIL (")?;
                let mut first = true;
                let mut separator = |f: &mut fmt::Formatter<'_>| {
                    let text = if first { "" } else { ", " };
                    first = false;
                    f.write_str(text)
                };
                if p50 {
                    separator(f)?;
                    write!(f, "p50 +{} us > {p50_us} us", self.overhead_p50_us)?;
                }
                if p99 {
                    separator(f)?;
                    write!(f, "p99 +{} us > {p99_us} us", self.overhead_p99_us)?;
                }
                if relative {
                    separator(f)?;
                    let permille = self.relative_permille.unwrap_or(0);
                    write!(
                        f,
                        "Anteil {},{} % > {relative_percent} %",
                        permille.checked_div(10).unwrap_or(0),
                        permille.checked_rem(10).unwrap_or(0)
                    )?;
                }
                f.write_str(")")
            }
            // Ein Fail ohne Budget kann `assess` nicht erzeugen.
            (Outcome::Fail { .. }, Limit::NotSupported { reason }) => {
                write!(f, "NICHT BUDGETIERT — {reason}")
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn latency(p50_us: u64, p99_us: u64) -> Latency {
        Latency { p50_us, p99_us }
    }

    #[test]
    fn the_measured_shm_overhead_passes() {
        // data-plane.md: 6229 us direkt, 6389 us ueber den Governor.
        let a = assess(
            shared_memory(),
            latency(6_229, 6_900),
            latency(6_389, 7_100),
        );
        assert_eq!(a.outcome, Outcome::Pass, "{a}");
        assert_eq!(a.relative_permille, Some(25));
    }

    /// Die Regression, fuer die das Budget da ist: der Governor kopiert auf
    /// dem Shm-Pfad doch, und 6,2 MB kosten wieder 11,7 ms.
    #[test]
    fn an_accidental_copy_on_the_shm_path_fails() {
        let a = assess(
            shared_memory(),
            latency(6_229, 6_900),
            latency(17_921, 19_000),
        );
        assert_eq!(
            a.outcome,
            Outcome::Fail {
                p50: true,
                p99: true,
                relative: true
            }
        );
        let text = a.to_string();
        assert!(text.starts_with("FAIL (p50 +11692 us > 300 us"), "{text}");
    }

    /// Ein kurzer Aufruf wird nach der absoluten Grenze beurteilt, nicht nach
    /// dem Anteil.
    #[test]
    fn a_short_call_is_judged_by_the_absolute_budget_only() {
        let a = assess(
            shared_memory(),
            latency(2_000, 2_400),
            latency(2_160, 2_700),
        );
        assert_eq!(a.relative_permille, None);
        assert_eq!(
            a.outcome,
            Outcome::Pass,
            "8 % eines 2-ms-Aufrufs sind kein Befund"
        );
    }

    #[test]
    fn a_tail_regression_alone_fails() {
        let a = assess(
            shared_memory(),
            latency(6_000, 7_000),
            latency(6_100, 9_000),
        );
        assert_eq!(
            a.outcome,
            Outcome::Fail {
                p50: false,
                p99: true,
                relative: false
            }
        );
    }

    #[test]
    fn a_governor_faster_than_the_direct_call_is_no_credit() {
        let a = assess(
            shared_memory(),
            latency(6_300, 7_000),
            latency(6_200, 6_900),
        );
        assert_eq!(a.overhead_p50_us, 0);
        assert_eq!(a.outcome, Outcome::Pass);
    }

    /// Kameraframes auf dem Copy-Pfad bekommen kein Budget — auch kein
    /// grosszuegiges.
    #[test]
    fn camera_frames_on_the_copy_path_are_not_budgeted() {
        for point in POINTS
            .iter()
            .filter(|p| p.path == Path::GrpcCopy && p.payload_bytes >= 640 * 640 * 3)
        {
            let a = assess(point.limit, latency(9_592, 10_000), latency(12_151, 13_000));
            assert!(matches!(a.outcome, Outcome::NotBudgeted { .. }), "{a}");
            assert!(!a.outcome.is_fail());
        }
    }

    /// ADR-0003 sagt: der Aufwand waechst nicht mit der Groesse. Dann muss
    /// auch das Budget fuer jede Groesse dasselbe sein.
    #[test]
    fn every_shm_point_carries_the_same_budget() {
        let shm: Vec<_> = POINTS
            .iter()
            .filter(|p| p.path == Path::SharedMemory)
            .collect();
        assert_eq!(shm.len(), 3);
        assert!(shm.iter().all(|p| p.limit == shared_memory()));
    }

    #[test]
    fn quantiles_are_nearest_rank() {
        let mut samples: Vec<u64> = (1..=100).rev().collect();
        let l = Latency::from_samples(&mut samples).unwrap();
        assert_eq!(l, latency(51, 100));
        assert_eq!(Latency::from_samples(&mut []), None);
    }
}
