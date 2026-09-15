//! Das Rohdatenformat eines `BYTES`-Elements.
//!
//! In `raw_input_contents` und `raw_output_contents` steht jedes Element
//! eines `BYTES`-Tensors als vier Bytes Laenge, little endian, gefolgt von
//! den Nutzbytes. Das Format stand zuvor viermal getrennt im Baum — und die
//! Kopien liefen auseinander: zwei saettigten die Laenge bei `u32::MAX`, zwei
//! fielen auf `0` zurueck. Beides ergibt ab 4 GiB einen falschen Rahmen, der
//! nach einem gueltigen aussieht. Ein Drahtformat, das viermal beschrieben
//! wird, hat keinen Besitzer; hier ist er.
//!
//! Beide Richtungen sind **streng**. Was nicht exakt passt, ist `None`, und
//! der Aufrufer muss sagen, was das heisst. Eine erfundene Laenge oder ein
//! stillschweigend gekuerzter Inhalt ist keine Antwort.

/// Die Breite des Laengenpraefixes in Bytes.
pub const LENGTH_PREFIX: usize = 4;

/// Kodiert ein einzelnes `BYTES`-Element: Laenge, dann Nutzbytes.
///
/// `None`, wenn die Laenge nicht in `u32` passt. Es gibt dafuer keinen
/// richtigen Rahmen — weder `u32::MAX` noch `0` beschreibt diese Nutzlast.
#[must_use]
pub fn encode_bytes_element(value: &[u8]) -> Option<Vec<u8>> {
    let header = length_header(value.len())?;
    let mut out = Vec::with_capacity(value.len().checked_add(LENGTH_PREFIX)?);
    out.extend_from_slice(&header);
    out.extend_from_slice(value);
    Some(out)
}

/// Liest genau ein `BYTES`-Element.
///
/// `Some` nur, wenn die Rohdaten aus **exakt einem** Element bestehen: der
/// Praefix nennt genau die Zahl der folgenden Bytes. Eine groessere Angabe
/// hiesse, fehlende Bytes zu erfinden; eine kleinere, dass weitere Elemente
/// folgen — ein Batch mit Form `[2]` etwa —, und die stillschweigend zu
/// verwerfen waere Datenverlust.
#[must_use]
pub fn decode_single_bytes_element(raw: &[u8]) -> Option<&[u8]> {
    let (header, rest) = raw.split_first_chunk::<LENGTH_PREFIX>()?;
    let length = usize::try_from(u32::from_le_bytes(*header)).ok()?;
    (rest.len() == length).then_some(rest)
}

/// Der Laengenpraefix fuer `length` Nutzbytes, falls er darstellbar ist.
fn length_header(length: usize) -> Option<[u8; LENGTH_PREFIX]> {
    u32::try_from(length).ok().map(u32::to_le_bytes)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    #[test]
    fn an_element_round_trips() {
        for value in [&b""[..], b"hallo", "Grüße".as_bytes()] {
            let raw = encode_bytes_element(value).unwrap();
            assert_eq!(raw.len(), value.len() + LENGTH_PREFIX);
            assert_eq!(decode_single_bytes_element(&raw), Some(value));
        }
    }

    #[test]
    fn the_prefix_is_little_endian() {
        let raw = encode_bytes_element(b"abc").unwrap();
        assert_eq!(raw, vec![3, 0, 0, 0, b'a', b'b', b'c']);
    }

    /// Ab 4 GiB gibt es keinen Rahmen — weder gesaettigt noch null.
    ///
    /// Geprueft am Praefix, weil ein Puffer dieser Groesse im Test keinen
    /// Platz hat; `encode_bytes_element` nimmt genau diesen Weg.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn a_length_beyond_u32_has_no_frame() {
        let limit = usize::try_from(u32::MAX).unwrap();
        assert_eq!(length_header(limit), Some([0xFF; 4]));
        assert_eq!(length_header(limit.checked_add(1).unwrap()), None);
        assert_eq!(length_header(usize::MAX), None);
    }

    /// Eine Laenge ueber die vorhandenen Bytes hinaus wird nicht gekuerzt.
    #[test]
    fn an_oversized_declared_length_is_rejected() {
        let mut raw = encode_bytes_element(b"hallo").unwrap();
        raw[0] = 200;
        assert_eq!(decode_single_bytes_element(&raw), None);
        assert_eq!(
            decode_single_bytes_element(&[0xFF, 0xFF, 0xFF, 0xFF, 1]),
            None
        );
    }

    /// Eine kleinere Laenge laesst Bytes uebrig — die gehoeren zu etwas.
    #[test]
    fn an_undersized_declared_length_is_rejected() {
        let mut raw = encode_bytes_element(b"hallo").unwrap();
        raw[0] = 2;
        assert_eq!(decode_single_bytes_element(&raw), None);
    }

    /// Zwei Elemente sind nicht eines, auch wenn das erste gueltig ist.
    #[test]
    fn two_elements_are_not_one() {
        let mut raw = encode_bytes_element(b"eins").unwrap();
        raw.extend(encode_bytes_element(b"zwei").unwrap());
        assert_eq!(decode_single_bytes_element(&raw), None);
    }

    #[test]
    fn a_truncated_prefix_is_rejected() {
        assert_eq!(decode_single_bytes_element(&[]), None);
        assert_eq!(decode_single_bytes_element(&[0, 0, 0]), None);
        assert_eq!(decode_single_bytes_element(&[0, 0, 0, 0]), Some(&b""[..]));
    }
}
