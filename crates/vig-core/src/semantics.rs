//! Was eine Variante fachlich liefert — und wann zwei Varianten austauschbar
//! sind (NV-10).
//!
//! ## Der Fehler, um den es geht
//!
//! Der Governor waehlt die Variante je Request und sagt es dem Client nicht.
//! Diese Freiheit setzt voraus, dass alle Varianten dasselbe **bedeuten**.
//! Bisher wurde das an der I/O-Signatur geprueft: Namen, Datentypen, Formen.
//! Das faengt den Fall, in dem ein Variantenwechsel den Client garantiert
//! bricht — und genau nicht den schlimmeren:
//!
//! Zwei Detektoren, beide `[1, 300, 6]` in `FP32`, beide mit Ausgabe `boxes`.
//! Der eine liefert `xyxy` in Pixeln und COCO-Labels in ihrer
//! Standardreihenfolge, der andere `cxcywh` normiert und dieselben Labels in
//! einer anderen Reihenfolge. Die Signatur ist identisch. Der Client bekommt
//! Zahlen, die aussehen wie erwartet und etwas anderes heissen — und das
//! faellt erst auf, wenn ein Roboter danach greift.
//!
//! ## Die Regel dieses Moduls
//!
//! **Gleiche Form ist notwendig, nicht hinreichend.** Zwei Varianten sind nur
//! austauschbar, wenn Ausgabeart, Labelmenge **samt Reihenfolge**,
//! Koordinatenkonvention, Einheit und Layout uebereinstimmen — und wenn ihre
//! Eingabevertraege dasselbe verlangen.
//!
//! **Fehlende Angaben machen nicht austauschbar.** Eine Variante ohne
//! Semantikangabe ist eine Variante, deren Bedeutung niemand aufgeschrieben
//! hat. Sie automatisch zu waehlen hiesse, auf eine Vermutung umzuschalten.
//! Der Rueckfall ist die freigegebene feste Variante (NV-02), nicht die
//! naechstbeste.
//!
//! **Aus einem Score folgt keine Freigabe.** Dieses Modul vergleicht
//! Bedeutungen. Ob eine Variante fachlich zugelassen ist, sagt die
//! Freigabeliste im Vertragszusatz — nicht ihre Genauigkeit auf einem
//! Datensatz, den jemand einmal gemessen hat.

use crate::arrayvec::ArrayVec;

/// Wie viele Ausgaben je Variante beschrieben werden koennen.
pub const MAX_OUTPUTS: usize = 8;

/// Was eine Ausgabe fachlich ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputKind {
    /// Nicht beschrieben.
    ///
    /// Die Voreinstellung, und ausdruecklich **nicht** austauschbar mit sich
    /// selbst: zwei Varianten, deren Bedeutung niemand aufgeschrieben hat,
    /// sind nicht deshalb gleich, weil beide schweigen.
    #[default]
    Unspecified,
    /// Objektdetektionen.
    Detections,
    /// Schluesselpunkte.
    Keypoints,
    /// Ein Tiefenbild.
    Depth,
    /// Eine Klassenverteilung.
    Classification,
    /// Segmentierungsmasken.
    Segmentation,
    /// Text.
    Text,
    /// Etwas, das der Betreiber ausdruecklich als undurchsichtig erklaert hat.
    ///
    /// Anders als [`OutputKind::Unspecified`]: hier hat jemand hingesehen und
    /// entschieden, dass die Bedeutung ausserhalb des Governors liegt. Zwei
    /// `Opaque`-Ausgaben gelten als gleich, wenn ihr Layout uebereinstimmt.
    Opaque,
}

/// In welchem Bezugssystem Koordinaten stehen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoordinateConvention {
    /// Nicht beschrieben.
    #[default]
    Unspecified,
    /// Auf die Eingabegroesse normiert, 0 bis 1.
    Normalized,
    /// Pixel der Modelleingabe.
    InputPixels,
    /// Pixel des Originalbildes.
    SourcePixels,
    /// Metrisch, in Metern.
    Meters,
}

/// Die Einheit eines Wertes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Unit {
    /// Nicht beschrieben.
    #[default]
    Unspecified,
    /// Dimensionslos.
    None,
    /// Wahrscheinlichkeit, 0 bis 1.
    Probability,
    /// Logits.
    Logits,
    /// Meter.
    Meters,
    /// Millimeter.
    Millimeters,
    /// Reziproke Tiefe.
    InverseDepth,
}

/// Eine Labelmenge, reihenfolgenempfindlich zusammengefasst.
///
/// Zwei Modelle mit denselben Klassen in anderer Reihenfolge liefern
/// dieselben Zahlen mit anderer Bedeutung. Genau dieser Fall soll auffallen,
/// deshalb geht die **Reihenfolge** in den Fingerabdruck ein.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LabelSet {
    /// Wie viele Labels.
    pub count: u32,
    /// Ein reihenfolgenempfindlicher Fingerabdruck der Labelnamen.
    ///
    /// Null heisst „nicht angegeben". Nicht angegeben ist kein Beleg fuer
    /// Gleichheit.
    pub fingerprint: u64,
}

impl LabelSet {
    /// Bildet den Fingerabdruck aus einer geordneten Labelliste.
    ///
    /// FNV-1a mit Feldtrennern, wie beim Umgebungsfingerabdruck: von Hand,
    /// damit das Ergebnis nicht von der Rust-Version abhaengt, und mit
    /// Trenner, damit `["ab","c"]` und `["a","bc"]` verschieden hashen.
    #[must_use]
    pub fn of<'a>(labels: impl IntoIterator<Item = &'a str>) -> Self {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = OFFSET;
        let mut count = 0_u32;
        for label in labels {
            for byte in label.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(PRIME);
            }
            hash ^= 0xff;
            hash = hash.wrapping_mul(PRIME);
            count = count.saturating_add(1);
        }
        Self {
            count,
            // Ein leerer Fingerabdruck darf nicht zufaellig null sein: null
            // ist der Wert fuer „nicht angegeben".
            fingerprint: if count == 0 { 0 } else { hash | 1 },
        }
    }

    /// Ob die Menge ueberhaupt angegeben ist.
    #[must_use]
    pub const fn is_specified(self) -> bool {
        self.fingerprint != 0
    }
}

/// Was eine Ausgabe bedeutet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputSemantics {
    /// Die fachliche Art.
    pub kind: OutputKind,
    /// Die Labels, sofern die Art sie braucht.
    pub labels: LabelSet,
    /// Das Bezugssystem der Koordinaten.
    pub coordinates: CoordinateConvention,
    /// Die Einheit der Werte.
    pub unit: Unit,
    /// Das Layout, reihenfolgenempfindlich zusammengefasst.
    ///
    /// Etwa `xyxy` gegen `cxcywh`: dieselbe Form, andere Bedeutung. Null
    /// heisst „nicht angegeben".
    pub layout: u64,
}

impl OutputSemantics {
    /// Ob diese Ausgabe vollstaendig genug beschrieben ist, um sie mit einer
    /// anderen zu vergleichen.
    ///
    /// Die Art muss stehen. Was darueber hinaus noetig ist, haengt von der Art
    /// ab: eine Detektion ohne Koordinatenkonvention ist nicht vergleichbar,
    /// eine Klassenverteilung braucht keine.
    #[must_use]
    pub const fn is_comparable(&self) -> bool {
        match self.kind {
            OutputKind::Unspecified => false,
            OutputKind::Detections | OutputKind::Keypoints => {
                !matches!(self.coordinates, CoordinateConvention::Unspecified) && self.layout != 0
            }
            OutputKind::Depth => !matches!(self.unit, Unit::Unspecified),
            OutputKind::Classification => self.labels.is_specified(),
            OutputKind::Segmentation => self.labels.is_specified() && self.layout != 0,
            OutputKind::Text | OutputKind::Opaque => true,
        }
    }
}

/// Was eine Variante an ihrer Eingabe verlangt.
///
/// Eine Variante, die eine andere Aufloesung oder eine andere Normierung
/// braucht, ist kein Ersatz — sie ist eine andere Vorverarbeitung. Und die
/// kostet Zeit, die in die Planung gehoert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputContract {
    /// Das Tensorlayout, etwa `nchw`, reihenfolgenempfindlich zusammengefasst.
    pub layout: u64,
    /// Der Farbraum, zusammengefasst.
    pub color_space: u64,
    /// Die Normierung, zusammengefasst.
    pub normalization: u64,
    /// Die erwartete Breite in Pixeln, 0 heisst „nicht angegeben".
    pub width: u32,
    /// Die erwartete Hoehe in Pixeln.
    pub height: u32,
}

impl InputContract {
    /// Ob der Vertrag ueberhaupt etwas aussagt.
    #[must_use]
    pub const fn is_specified(&self) -> bool {
        self.layout != 0 || self.width != 0 || self.height != 0
    }
}

/// Die fachliche Beschreibung einer Variante.
#[derive(Debug, Clone, Default)]
pub struct VariantSemantics {
    /// Was die Variante an ihrer Eingabe verlangt.
    pub input: InputContract,
    /// Was sie ausgibt, in Ausgabereihenfolge.
    pub outputs: ArrayVec<OutputSemantics, MAX_OUTPUTS>,
}

impl PartialEq for VariantSemantics {
    /// Feldweise, weil [`ArrayVec`] keine Gleichheit ableitet.
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input
            && self.outputs.len() == other.outputs.len()
            && self
                .outputs
                .iter()
                .zip(other.outputs.iter())
                .all(|(a, b)| a == b)
    }
}

impl Eq for VariantSemantics {}

impl VariantSemantics {
    /// Ob diese Variante beschrieben ist.
    #[must_use]
    pub fn is_specified(&self) -> bool {
        !self.outputs.is_empty()
    }

    /// Ob sie vollstaendig genug beschrieben ist, um verglichen zu werden.
    #[must_use]
    pub fn is_comparable(&self) -> bool {
        self.is_specified() && self.outputs.iter().all(OutputSemantics::is_comparable)
    }
}

/// Warum zwei Varianten nicht austauschbar sind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticConflict {
    /// Mindestens eine der beiden ist nicht beschrieben.
    NotDescribed,
    /// Eine Beschreibung ist unvollstaendig.
    Incomplete,
    /// Verschieden viele Ausgaben.
    OutputCount {
        /// Die eine Anzahl.
        left: usize,
        /// Die andere.
        right: usize,
    },
    /// Verschiedene Ausgabearten.
    Kind {
        /// Der Index der Ausgabe.
        output: usize,
    },
    /// Verschiedene Labels oder verschiedene Labelreihenfolge.
    ///
    /// Der Fall, den eine Signaturpruefung nicht sieht: dieselbe Form,
    /// dieselben Klassen, andere Reihenfolge.
    Labels {
        /// Der Index der Ausgabe.
        output: usize,
    },
    /// Verschiedene Koordinatenkonventionen.
    Coordinates {
        /// Der Index der Ausgabe.
        output: usize,
    },
    /// Verschiedene Einheiten.
    Unit {
        /// Der Index der Ausgabe.
        output: usize,
    },
    /// Verschiedenes Layout.
    Layout {
        /// Der Index der Ausgabe.
        output: usize,
    },
    /// Verschiedene Eingabevertraege.
    ///
    /// Eine andere Aufloesung oder Normierung ist kein Ersatz, sondern eine
    /// andere Vorverarbeitung.
    Input,
}

impl core::fmt::Display for SemanticConflict {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotDescribed => write!(
                f,
                "mindestens eine Variante hat keine Semantikangabe; \
                 nicht beschrieben ist kein Beleg fuer Gleichheit"
            ),
            Self::Incomplete => write!(
                f,
                "eine Semantikangabe ist unvollstaendig und damit nicht vergleichbar"
            ),
            Self::OutputCount { left, right } => {
                write!(f, "{left} Ausgaben gegen {right}")
            }
            Self::Kind { output } => write!(f, "Ausgabe {output}: verschiedene Art"),
            Self::Labels { output } => write!(
                f,
                "Ausgabe {output}: verschiedene Labels oder verschiedene \
                 Labelreihenfolge — gleiche Form, andere Bedeutung"
            ),
            Self::Coordinates { output } => {
                write!(f, "Ausgabe {output}: verschiedene Koordinatenkonvention")
            }
            Self::Unit { output } => write!(f, "Ausgabe {output}: verschiedene Einheit"),
            Self::Layout { output } => write!(f, "Ausgabe {output}: verschiedenes Layout"),
            Self::Input => write!(
                f,
                "verschiedene Eingabevertraege; eine andere Vorverarbeitung \
                 ist kein Ersatz"
            ),
        }
    }
}

impl core::error::Error for SemanticConflict {}

/// Ob zwei Varianten fachlich austauschbar sind.
///
/// # Errors
///
/// Der erste gefundene Widerspruch. Der erste und nicht alle: fuer die
/// Entscheidung genuegt einer, und die Meldung soll den Betreiber auf ein
/// Feld zeigen, nicht auf eine Liste.
pub fn interchangeable(
    left: &VariantSemantics,
    right: &VariantSemantics,
) -> Result<(), SemanticConflict> {
    if !left.is_specified() || !right.is_specified() {
        return Err(SemanticConflict::NotDescribed);
    }
    if !left.is_comparable() || !right.is_comparable() {
        return Err(SemanticConflict::Incomplete);
    }
    if left.outputs.len() != right.outputs.len() {
        return Err(SemanticConflict::OutputCount {
            left: left.outputs.len(),
            right: right.outputs.len(),
        });
    }
    // Eine Eingabeangabe, die nur auf einer Seite steht, ist kein Beleg.
    if left.input != right.input {
        return Err(SemanticConflict::Input);
    }
    for index in 0..left.outputs.len() {
        let (Some(a), Some(b)) = (left.outputs.get(index), right.outputs.get(index)) else {
            return Err(SemanticConflict::OutputCount {
                left: left.outputs.len(),
                right: right.outputs.len(),
            });
        };
        if a.kind != b.kind {
            return Err(SemanticConflict::Kind { output: index });
        }
        if a.labels != b.labels {
            return Err(SemanticConflict::Labels { output: index });
        }
        if a.coordinates != b.coordinates {
            return Err(SemanticConflict::Coordinates { output: index });
        }
        if a.unit != b.unit {
            return Err(SemanticConflict::Unit { output: index });
        }
        if a.layout != b.layout {
            return Err(SemanticConflict::Layout { output: index });
        }
    }
    Ok(())
}

/// Ein reihenfolgenempfindlicher Fingerabdruck eines kurzen Bezeichners.
///
/// Fuer Layout, Farbraum und Normierung: der Kern soll keine Zeichenketten
/// halten, aber `xyxy` und `cxcywh` unterscheiden koennen.
#[must_use]
pub fn tag(value: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    if value.is_empty() {
        return 0;
    }
    let mut hash = OFFSET;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    // Null bleibt „nicht angegeben".
    hash | 1
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn detections(labels: &[&str], layout: &str, coords: CoordinateConvention) -> VariantSemantics {
        let mut outputs = ArrayVec::new();
        let _ = outputs.push(OutputSemantics {
            kind: OutputKind::Detections,
            labels: LabelSet::of(labels.iter().copied()),
            coordinates: coords,
            unit: Unit::Probability,
            layout: tag(layout),
        });
        VariantSemantics {
            input: InputContract {
                layout: tag("nchw"),
                color_space: tag("rgb"),
                normalization: tag("imagenet"),
                width: 512,
                height: 512,
            },
            outputs,
        }
    }

    const COCO3: [&str; 3] = ["person", "car", "dog"];

    // -- Der Fall, um den es geht -----------------------------------------

    #[test]
    fn the_same_shape_with_a_permuted_label_order_is_rejected() {
        let a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let b = detections(
            &["car", "person", "dog"],
            "xyxy",
            CoordinateConvention::InputPixels,
        );
        assert_eq!(
            interchangeable(&a, &b),
            Err(SemanticConflict::Labels { output: 0 }),
            "dieselben Klassen in anderer Reihenfolge sind dieselben Zahlen \
             mit anderer Bedeutung"
        );
    }

    #[test]
    fn the_same_labels_in_the_same_order_are_interchangeable() {
        let a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let b = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        assert_eq!(interchangeable(&a, &b), Ok(()));
    }

    #[test]
    fn a_different_box_layout_is_rejected() {
        let a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let b = detections(&COCO3, "cxcywh", CoordinateConvention::InputPixels);
        assert_eq!(
            interchangeable(&a, &b),
            Err(SemanticConflict::Layout { output: 0 })
        );
    }

    #[test]
    fn a_different_coordinate_convention_is_rejected() {
        let a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let b = detections(&COCO3, "xyxy", CoordinateConvention::Normalized);
        assert_eq!(
            interchangeable(&a, &b),
            Err(SemanticConflict::Coordinates { output: 0 })
        );
    }

    #[test]
    fn a_different_class_count_is_rejected() {
        let a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let b = detections(
            &["person", "car"],
            "xyxy",
            CoordinateConvention::InputPixels,
        );
        assert_eq!(
            interchangeable(&a, &b),
            Err(SemanticConflict::Labels { output: 0 })
        );
    }

    #[test]
    fn a_different_input_resolution_is_rejected() {
        // Die RF-DETR-Varianten des Messaufbaus unterscheiden sich genau
        // darin. Eine andere Vorverarbeitung ist kein Ersatz.
        let a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let mut b = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        b.input.width = 640;
        b.input.height = 640;
        assert_eq!(interchangeable(&a, &b), Err(SemanticConflict::Input));
    }

    #[test]
    fn a_different_normalization_is_rejected() {
        let a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let mut b = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        b.input.normalization = tag("none");
        assert_eq!(interchangeable(&a, &b), Err(SemanticConflict::Input));
    }

    // -- Schweigen ist kein Beleg -----------------------------------------

    #[test]
    fn two_undescribed_variants_are_not_interchangeable() {
        let empty = VariantSemantics::default();
        assert_eq!(
            interchangeable(&empty, &empty),
            Err(SemanticConflict::NotDescribed),
            "zwei Varianten, deren Bedeutung niemand aufgeschrieben hat, \
             sind nicht deshalb gleich, weil beide schweigen"
        );
    }

    #[test]
    fn one_described_and_one_silent_is_not_interchangeable() {
        let a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        assert_eq!(
            interchangeable(&a, &VariantSemantics::default()),
            Err(SemanticConflict::NotDescribed)
        );
    }

    #[test]
    fn an_incomplete_description_is_not_a_comparison() {
        let mut a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let b = a.clone();
        // Eine Detektion ohne Koordinatenkonvention laesst sich nicht
        // vergleichen — auch nicht mit sich selbst.
        if let Some(first) = a.outputs.get_mut(0) {
            first.coordinates = CoordinateConvention::Unspecified;
        }
        assert_eq!(interchangeable(&a, &b), Err(SemanticConflict::Incomplete));
    }

    #[test]
    fn an_unspecified_kind_is_never_comparable() {
        let output = OutputSemantics::default();
        assert!(!output.is_comparable());
    }

    // -- Ausdrueckliche Undurchsichtigkeit ---------------------------------

    #[test]
    fn deliberately_opaque_outputs_compare_by_layout() {
        // Anders als „nicht beschrieben": hier hat jemand hingesehen und
        // entschieden, dass die Bedeutung ausserhalb des Governors liegt.
        let make = |layout: &str| {
            let mut outputs = ArrayVec::new();
            let _ = outputs.push(OutputSemantics {
                kind: OutputKind::Opaque,
                layout: tag(layout),
                ..OutputSemantics::default()
            });
            VariantSemantics {
                input: InputContract::default(),
                outputs,
            }
        };
        assert_eq!(
            interchangeable(&make("embedding_768"), &make("embedding_768")),
            Ok(())
        );
        assert_eq!(
            interchangeable(&make("embedding_768"), &make("embedding_512")),
            Err(SemanticConflict::Layout { output: 0 })
        );
    }

    // -- Mehrere Ausgaben --------------------------------------------------

    #[test]
    fn a_different_output_count_is_rejected() {
        let a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let mut b = a.clone();
        let _ = b.outputs.push(OutputSemantics {
            kind: OutputKind::Depth,
            unit: Unit::Meters,
            ..OutputSemantics::default()
        });
        assert_eq!(
            interchangeable(&a, &b),
            Err(SemanticConflict::OutputCount { left: 1, right: 2 })
        );
    }

    #[test]
    fn the_conflict_names_the_output_it_found() {
        let mut a = detections(&COCO3, "xyxy", CoordinateConvention::InputPixels);
        let _ = a.outputs.push(OutputSemantics {
            kind: OutputKind::Depth,
            unit: Unit::Meters,
            ..OutputSemantics::default()
        });
        let mut b = a.clone();
        if let Some(second) = b.outputs.get_mut(1) {
            second.unit = Unit::Millimeters;
        }
        assert_eq!(
            interchangeable(&a, &b),
            Err(SemanticConflict::Unit { output: 1 })
        );
    }

    // -- Labelfingerabdruck ------------------------------------------------

    #[test]
    fn the_label_fingerprint_is_order_sensitive() {
        let forward = LabelSet::of(["a", "b"]);
        let backward = LabelSet::of(["b", "a"]);
        assert_eq!(forward.count, backward.count);
        assert_ne!(forward.fingerprint, backward.fingerprint);
    }

    #[test]
    fn a_shifted_label_boundary_does_not_collide() {
        assert_ne!(
            LabelSet::of(["ab", "c"]).fingerprint,
            LabelSet::of(["a", "bc"]).fingerprint
        );
    }

    #[test]
    fn an_empty_label_set_is_unspecified() {
        let empty = LabelSet::of(core::iter::empty::<&str>());
        assert_eq!(empty.count, 0);
        assert!(!empty.is_specified());
    }

    #[test]
    fn a_label_fingerprint_is_never_accidentally_unspecified() {
        // Der Hash darf nicht zufaellig null werden — null heisst
        // „nicht angegeben".
        for name in ["a", "person", "", "sehr langer labelname mit leerzeichen"] {
            let set = LabelSet::of([name]);
            assert!(set.is_specified(), "{name:?}");
        }
    }

    #[test]
    fn a_tag_is_never_accidentally_unspecified() {
        assert_eq!(tag(""), 0);
        for value in ["xyxy", "cxcywh", "nchw", "rgb"] {
            assert_ne!(tag(value), 0, "{value}");
        }
        assert_ne!(tag("xyxy"), tag("cxcywh"));
    }
}
