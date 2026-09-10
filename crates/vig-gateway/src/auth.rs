//! Zugangspruefung des Inferenzendpunkts.
//!
//! Zwei Verfahren, bewusst getrennt:
//!
//! * **mTLS** — der Transport prueft ein Clientzertifikat gegen eine CA. Das
//!   ist die belastbare Variante: ein privater Schluessel verlaesst das Geraet
//!   nicht, ein Token schon. Sie wird im Server aufgesetzt, nicht hier.
//! * **Bearer-Token** — eine Datei mit erlaubten Token. Die pragmatische
//!   Variante fuer Umgebungen ohne Zertifikatsverwaltung.
//!
//! Beides ist optional und beides ist aus. Der Governor ist fuer ein Geraet
//! mit einem Betreiber gebaut, und dort ist Loopback die richtige Grenze. Wer
//! den Endpunkt oeffnet, schaltet eines von beiden ein — `vig doctor` sagt es
//! ihm.

use std::collections::HashSet;
use std::path::Path;
use tonic::{Request, Status};
use vig_core::hints::Authority;

/// Der Metadatenschluessel, in dem das Token reist.
pub const AUTH_HEADER: &str = "authorization";

/// Die erlaubten Bearer-Token.
///
/// Ein `HashSet` und kein Vec: die Pruefung liegt im Hot Path jedes Requests,
/// und eine lineare Suche ueber eine Tokenliste waere eine Groesse, die mit der
/// Zahl der Clients waechst.
#[derive(Debug, Clone, Default)]
pub struct Tokens {
    allowed: HashSet<String>,
}

impl Tokens {
    /// Laedt die Tokenliste aus einer Datei, ein Token je Zeile.
    ///
    /// Leerzeilen und `#`-Kommentare werden ignoriert.
    ///
    /// # Errors
    ///
    /// Wenn die Datei nicht lesbar ist oder kein einziges Token enthaelt. Eine
    /// leere Tokendatei ist kein leerer Schutz, sondern ein Konfigurations-
    /// fehler: sie wuerde jede Anfrage ablehnen und sieht dabei aus wie eine
    /// funktionierende Einrichtung.
    pub fn load(path: &Path) -> Result<Self, std::io::Error> {
        let text = std::fs::read_to_string(path)?;
        let allowed: HashSet<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(ToOwned::to_owned)
            .collect();
        if allowed.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "die Tokendatei enthaelt kein Token",
            ));
        }
        Ok(Self { allowed })
    }

    /// Wie viele Token hinterlegt sind.
    #[must_use]
    pub fn len(&self) -> usize {
        self.allowed.len()
    }

    /// Wahr, wenn keine Token hinterlegt sind.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    /// Prueft den Authorization-Header einer Anfrage.
    ///
    /// # Errors
    ///
    /// `Unauthenticated`, wenn der Header fehlt, unlesbar ist oder ein
    /// unbekanntes Token traegt. Der Fehlertext unterscheidet die Faelle nicht:
    /// ob ein Token existiert, ist selbst eine Auskunft.
    pub fn check<T>(&self, request: &Request<T>) -> Result<(), Status> {
        let presented = request
            .metadata()
            .get(AUTH_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);

        match presented {
            Some(token) if self.allowed.contains(token) => Ok(()),
            _ => Err(Status::unauthenticated("kein gueltiges Bearer-Token")),
        }
    }

    /// Die belegte Kennung des Aufrufers, falls sein Token bekannt ist
    /// (NV-18).
    ///
    /// ADR-0029 verlangt, dass die Zugangsschicht die Kennung **belegt** und
    /// der Aufrufer sie nicht behauptet. Sie ist deshalb aus dem Token
    /// abgeleitet und steht in keinem Requestfeld: ein Client, der seine
    /// eigene Berechtigung mitschickt, hat keine.
    ///
    /// Ohne hinterlegte Token gibt es keine belegte Kennung — und damit
    /// keinen Hinweis. Ein Governor ohne Zugangssicherung soll seinen Vertrag
    /// nicht von Aufrufern verschieben lassen.
    #[must_use]
    pub fn authority_of<T>(&self, request: &Request<T>) -> Option<Authority> {
        let token = request
            .metadata()
            .get(AUTH_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)?;
        self.allowed
            .contains(token)
            .then(|| Authority(fnv1a64(token.as_bytes())))
    }

    /// Die Kennungen aller hinterlegten Token.
    ///
    /// Damit der Betreiber weiss, welche Zahl er in `hints.authority`
    /// eintragen muss. Sie steht beim Start im Log; die Token selbst stehen
    /// dort nicht.
    #[must_use]
    pub fn authorities(&self) -> Vec<Authority> {
        let mut out: Vec<Authority> = self
            .allowed
            .iter()
            .map(|t| Authority(fnv1a64(t.as_bytes())))
            .collect();
        out.sort_unstable();
        out
    }
}

/// FNV-1a ueber 64 Bit.
///
/// Kein kryptografischer Hash und keiner noetig: die Kennung ist eine
/// Gleichheitspruefung gegen die Freigabeliste des Betreibers, kein Geheimnis
/// und kein Beweis. Das Geheimnis ist das Token, und das bleibt, wo es ist.
/// Bewusst selbst geschrieben statt einer Abhaengigkeit: fuenf Zeilen, deren
/// Ergebnis ueber Versionen hinweg gleich bleiben muss.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::io::Write as _;

    fn file_with(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    fn request_with(header: Option<&str>) -> Request<()> {
        let mut r = Request::new(());
        if let Some(h) = header {
            r.metadata_mut().insert(AUTH_HEADER, h.parse().unwrap());
        }
        r
    }

    #[test]
    fn tokens_are_read_line_by_line_ignoring_comments() {
        let f = file_with("# Kommentar\nalpha\n\n  beta  \n");
        let tokens = Tokens::load(f.path()).unwrap();
        assert_eq!(tokens.len(), 2);
        assert!(tokens.check(&request_with(Some("Bearer alpha"))).is_ok());
        assert!(tokens.check(&request_with(Some("Bearer beta"))).is_ok());
    }

    /// Eine leere Tokendatei sieht aus wie Schutz und ist keiner.
    #[test]
    fn an_empty_token_file_is_a_configuration_error() {
        let f = file_with("# nur Kommentare\n\n");
        assert!(Tokens::load(f.path()).is_err());
    }

    #[test]
    fn anything_but_a_known_token_is_rejected() {
        let f = file_with("alpha\n");
        let tokens = Tokens::load(f.path()).unwrap();
        for header in [
            None,
            Some("alpha"),
            Some("Bearer gamma"),
            Some("Basic alpha"),
        ] {
            let status = tokens
                .check(&request_with(header))
                .expect_err("nur ein bekanntes Bearer-Token kommt durch");
            assert_eq!(status.code(), tonic::Code::Unauthenticated);
        }
    }
}
