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
//! den Endpunkt oeffnet, schaltet eines von beiden ein — `vig serve`
//! verweigert sonst den Start ausserhalb von Loopback, und `vig doctor` sagt
//! es vorher.
//!
//! ## Benannte Token
//!
//! Eine Zeile der Tokendatei ist entweder ein Token oder `name:token`. Der
//! Name ist die Identitaet, die im Log erscheinen darf; das Token ist das
//! Geheimnis, das nie erscheint — auch nicht als Hash. Die Kennung fuer
//! Anwendungshinweise (NV-18) wird aus dem **Namen** abgeleitet. Die fruehere
//! Fassung leitete sie mit FNV-1a aus dem Token ab und schrieb sie ins Log:
//! wer das Log hatte, konnte schwache Token offline mit voller
//! Hash-Geschwindigkeit durchprobieren (Security-Review N1).
//!
//! Token sind mindestens [`MIN_TOKEN_LEN`] Zeichen lang. Ein kurzes Token ist
//! auch ohne Log durchprobierbar.
//!
//! ## Administration
//!
//! Endpunkte, die den Zustand des Backends aendern — Modelle laden und
//! entladen, Tracing, Loglevel, CUDA-Shared-Memory —, verlangen ein Token aus
//! einer **eigenen** Datei (`backend.security.admin_token_file`). Ohne sie
//! sind diese Endpunkte gesperrt, auch im offenen Modus (Security-Review H3).
//!
//! ## Vor dem Dekodieren
//!
//! [`Gate`] prueft als tonic-Interceptor, **bevor** die Nachricht dekodiert
//! wird. Ein nicht authentifizierter Client kostet damit keine 64-MiB-
//! Dekodierung (Security-Review M1). Der Dienst prueft zusaetzlich selbst:
//! er wird in Tests auch ohne Interceptor aufgerufen, und eine Pruefung, die
//! nur an einer Stelle haengt, faellt beim naechsten Umbau weg.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use tonic::{Request, Status};
use vig_core::hints::Authority;

/// Der Metadatenschluessel, in dem das Token reist.
pub const AUTH_HEADER: &str = "authorization";

/// Die Mindestlaenge eines Tokens in Zeichen.
///
/// 16 zufaellige Zeichen aus Base64 sind 96 Bit — nicht durchprobierbar. Ein
/// Wort wie `s3cret` ist es in Sekunden.
pub const MIN_TOKEN_LEN: usize = 16;

/// Die Hoechstlaenge eines Tokennamens.
pub const MAX_LABEL_LEN: usize = 64;

/// Wer eine Anfrage stellt, soweit der Governor das weiss.
///
/// Dient der Zuordnung von Besitz — Shared-Memory-Regionen, Knoten im
/// Abhaengigkeitsgraphen —, nicht der Anzeige. Ohne hinterlegte Token sind
/// alle Aufrufer derselbe: [`Identity::ANONYMOUS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identity(pub u64);

impl Identity {
    /// Die Identitaet aller Aufrufer, wenn niemand geprueft wird.
    pub const ANONYMOUS: Self = Self(0);

    /// Ob diese Identitaet aus einer Pruefung stammt.
    #[must_use]
    pub const fn is_authenticated(self) -> bool {
        self.0 != 0
    }
}

/// Ein hinterlegtes Token.
#[derive(Debug, Clone)]
struct Entry {
    /// Der Name, falls die Zeile einen trug.
    label: Option<String>,
    /// Die Identitaet dieses Tokens.
    identity: Identity,
}

/// Die erlaubten Bearer-Token.
///
/// Eine `HashMap` und kein Vec: die Pruefung liegt im Hot Path jedes Requests,
/// und eine lineare Suche ueber eine Tokenliste waere eine Groesse, die mit der
/// Zahl der Clients waechst. Der Hash der Map ist SipHash mit zufaelligem
/// Schluessel; einen verwertbaren Zeitkanal gibt der Vergleich damit nicht her.
#[derive(Debug, Clone, Default)]
pub struct Tokens {
    allowed: HashMap<String, Entry>,
}

impl Tokens {
    /// Laedt die Tokenliste aus einer Datei, ein Token je Zeile.
    ///
    /// Leerzeilen und `#`-Kommentare werden ignoriert; eine Zeile ist
    /// `token` oder `name:token`.
    ///
    /// # Errors
    ///
    /// Wenn die Datei nicht lesbar ist, kein Token enthaelt, ein Token kuerzer
    /// als [`MIN_TOKEN_LEN`] ist oder ein Name oder Token doppelt vorkommt.
    /// Eine leere Tokendatei ist kein leerer Schutz, sondern ein
    /// Konfigurationsfehler: sie wuerde jede Anfrage ablehnen und sieht dabei
    /// aus wie eine funktionierende Einrichtung.
    pub fn load(path: &Path) -> Result<Self, std::io::Error> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Liest eine Tokenliste aus Text.
    ///
    /// # Errors
    ///
    /// Siehe [`Tokens::load`].
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut allowed = HashMap::new();
        let mut labels = HashSet::new();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let number = index.saturating_add(1);
            let (label, token) = split_label(line);
            if token.chars().count() < MIN_TOKEN_LEN {
                return Err(format!(
                    "Zeile {number}: das Token ist kuerzer als {MIN_TOKEN_LEN} Zeichen; \
                     ein kurzes Token laesst sich durchprobieren"
                ));
            }
            if let Some(name) = &label
                && !labels.insert(name.clone())
            {
                return Err(format!(
                    "Zeile {number}: der Name {name} ist doppelt vergeben"
                ));
            }
            let identity = label.as_deref().map_or_else(
                || Identity(nonzero(fnv1a64_parts(b"token\0", token.as_bytes()))),
                |name| Identity(nonzero(fnv1a64_parts(b"label\0", name.as_bytes()))),
            );
            if allowed
                .insert(token.to_owned(), Entry { label, identity })
                .is_some()
            {
                return Err(format!(
                    "Zeile {number}: dasselbe Token steht zweimal in der Datei"
                ));
            }
        }
        if allowed.is_empty() {
            return Err("die Tokendatei enthaelt kein Token".to_owned());
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

    /// Wie viele Token keinen Namen tragen.
    ///
    /// Sie authentifizieren, aber sie koennen keine Anwendungshinweise
    /// geben: eine Hinweis-Kennung ohne Namen muesste aus dem Token selbst
    /// kommen.
    #[must_use]
    pub fn unlabeled(&self) -> usize {
        self.allowed.values().filter(|e| e.label.is_none()).count()
    }

    fn entry<T>(&self, request: &Request<T>) -> Option<&Entry> {
        presented(request).and_then(|token| self.allowed.get(token))
    }

    /// Ob die Anfrage ein hinterlegtes Token traegt.
    #[must_use]
    pub fn contains<T>(&self, request: &Request<T>) -> bool {
        self.entry(request).is_some()
    }

    /// Prueft den Authorization-Header einer Anfrage.
    ///
    /// # Errors
    ///
    /// `Unauthenticated`, wenn der Header fehlt, unlesbar ist oder ein
    /// unbekanntes Token traegt. Der Fehlertext unterscheidet die Faelle nicht:
    /// ob ein Token existiert, ist selbst eine Auskunft.
    pub fn check<T>(&self, request: &Request<T>) -> Result<(), Status> {
        if self.contains(request) {
            Ok(())
        } else {
            Err(unauthenticated())
        }
    }

    /// Die Identitaet des Aufrufers, falls sein Token bekannt ist.
    #[must_use]
    pub fn identity_of<T>(&self, request: &Request<T>) -> Option<Identity> {
        self.entry(request).map(|e| e.identity)
    }

    /// Die belegte Hinweis-Kennung des Aufrufers (NV-18).
    ///
    /// ADR-0029 verlangt, dass die Zugangsschicht die Kennung **belegt** und
    /// der Aufrufer sie nicht behauptet. Sie kommt deshalb aus dem Namen
    /// seines Tokens und steht in keinem Requestfeld: ein Client, der seine
    /// eigene Berechtigung mitschickt, hat keine.
    ///
    /// Ein Token ohne Namen belegt keine Kennung — und gibt damit keine
    /// Hinweise.
    #[must_use]
    pub fn authority_of<T>(&self, request: &Request<T>) -> Option<Authority> {
        self.entry(request)
            .and_then(|e| e.label.as_deref())
            .map(authority_for_label)
    }

    /// Die Namen und Kennungen aller benannten Token.
    ///
    /// Damit der Betreiber weiss, welche Zahl er in `hints.authority`
    /// eintragen muss. Name und Kennung stehen beim Start im Log; aus beiden
    /// laesst sich kein Token zurueckgewinnen.
    #[must_use]
    pub fn authorities(&self) -> Vec<(String, Authority)> {
        let mut out: Vec<(String, Authority)> = self
            .allowed
            .values()
            .filter_map(|e| e.label.as_ref())
            .map(|name| (name.clone(), authority_for_label(name)))
            .collect();
        out.sort();
        out
    }
}

/// Die Hinweis-Kennung eines Tokennamens.
///
/// Kein Geheimnis und keins noetig: die Kennung ist eine Gleichheitspruefung
/// gegen die Freigabeliste des Betreibers. Das Geheimnis ist das Token, und
/// von ihm wird hier nichts abgeleitet.
#[must_use]
pub fn authority_for_label(label: &str) -> Authority {
    Authority(fnv1a64_parts(b"", label.as_bytes()))
}

/// Die Zugangspruefung, einmal fuer alle Endpunkte.
///
/// Laeuft als tonic-Interceptor vor dem Dekodieren und wird vom Dienst
/// zusaetzlich selbst befragt.
#[derive(Debug, Clone, Default)]
pub struct Gate {
    tokens: Option<Arc<Tokens>>,
    admin: Option<Arc<Tokens>>,
}

impl Gate {
    /// Eine Pruefung mit den gegebenen Tokenlisten.
    #[must_use]
    pub fn new(tokens: Option<Tokens>, admin: Option<Tokens>) -> Self {
        Self {
            tokens: tokens.map(Arc::new),
            admin: admin.map(Arc::new),
        }
    }

    /// Dieselbe Pruefung mit dieser Tokenliste.
    #[must_use]
    pub fn with_tokens(mut self, tokens: Tokens) -> Self {
        self.tokens = Some(Arc::new(tokens));
        self
    }

    /// Dieselbe Pruefung mit dieser Administrationsliste.
    #[must_use]
    pub fn with_admin(mut self, admin: Tokens) -> Self {
        self.admin = Some(Arc::new(admin));
        self
    }

    /// Ob ueberhaupt Bearer-Token geprueft werden.
    #[must_use]
    pub const fn checks_tokens(&self) -> bool {
        self.tokens.is_some()
    }

    /// Laesst eine Anfrage durch oder weist sie ab.
    ///
    /// Ohne Tokenliste kommt jeder durch — die Grenze ist dann Loopback oder
    /// mTLS. Mit Tokenliste nur ein bekanntes Token; ein Administrationstoken
    /// gilt dabei auch fuer gewoehnliche Anfragen.
    ///
    /// # Errors
    ///
    /// `Unauthenticated`.
    pub fn admit<T>(&self, request: &Request<T>) -> Result<(), Status> {
        let Some(tokens) = &self.tokens else {
            return Ok(());
        };
        if tokens.contains(request) || self.is_admin(request) {
            Ok(())
        } else {
            Err(unauthenticated())
        }
    }

    /// Ob die Anfrage ein Administrationstoken traegt.
    ///
    /// Ohne Administrationsdatei nie: dann sind die Endpunkte gesperrt.
    #[must_use]
    pub fn is_admin<T>(&self, request: &Request<T>) -> bool {
        self.admin
            .as_ref()
            .is_some_and(|admin| admin.contains(request))
    }

    /// Die Identitaet des Aufrufers.
    ///
    /// [`Identity::ANONYMOUS`], wenn keine Token geprueft werden.
    #[must_use]
    pub fn identity_of<T>(&self, request: &Request<T>) -> Identity {
        self.tokens
            .as_ref()
            .and_then(|t| t.identity_of(request))
            .or_else(|| self.admin.as_ref().and_then(|a| a.identity_of(request)))
            .unwrap_or(Identity::ANONYMOUS)
    }

    /// Die belegte Hinweis-Kennung des Aufrufers (NV-18).
    #[must_use]
    pub fn authority_of<T>(&self, request: &Request<T>) -> Option<Authority> {
        self.tokens
            .as_ref()
            .and_then(|t| t.authority_of(request))
            .or_else(|| self.admin.as_ref().and_then(|a| a.authority_of(request)))
    }
}

impl tonic::service::Interceptor for Gate {
    fn call(&mut self, request: Request<()>) -> Result<Request<()>, Status> {
        self.admit(&request)?;
        Ok(request)
    }
}

fn unauthenticated() -> Status {
    Status::unauthenticated("kein gueltiges Bearer-Token")
}

/// Das vorgelegte Token einer Anfrage, ohne `Bearer `.
fn presented<T>(request: &Request<T>) -> Option<&str> {
    request
        .metadata()
        .get(AUTH_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
}

/// Trennt `name:token`. Ohne gueltigen Namen ist die ganze Zeile das Token.
///
/// Ein Name besteht aus Buchstaben, Ziffern, `_`, `-` und `.`. Ein Token,
/// das zufaellig einen Doppelpunkt enthaelt, dessen Vorderteil kein gueltiger
/// Name ist, bleibt damit ein Token.
fn split_label(line: &str) -> (Option<String>, &str) {
    if let Some((label, token)) = line.split_once(':') {
        let label = label.trim();
        let token = token.trim();
        let valid = !label.is_empty()
            && label.len() <= MAX_LABEL_LEN
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            && !token.is_empty();
        if valid {
            return (Some(label.to_owned()), token);
        }
    }
    (None, line)
}

/// Null ist [`Identity::ANONYMOUS`] vorbehalten.
const fn nonzero(value: u64) -> u64 {
    if value == 0 { 1 } else { value }
}

/// FNV-1a ueber 64 Bit, ueber ein Praefix und einen Wert.
///
/// Kein kryptografischer Hash und keiner noetig: er bildet Namen auf
/// Kennungen ab, nicht Geheimnisse auf Beweise. Bewusst selbst geschrieben
/// statt einer Abhaengigkeit: wenige Zeilen, deren Ergebnis ueber Versionen
/// hinweg gleich bleiben muss — die Hinweis-Kennung steht in
/// Betreiberkonfigurationen.
fn fnv1a64_parts(prefix: &[u8], bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in prefix.iter().chain(bytes) {
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

    const ALPHA: &str = "alpha-0123456789abcdef";
    const BETA: &str = "beta-0123456789abcdef";

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

    fn bearer(token: &str) -> Request<()> {
        request_with(Some(&format!("Bearer {token}")))
    }

    #[test]
    fn tokens_are_read_line_by_line_ignoring_comments() {
        let f = file_with(&format!("# Kommentar\n{ALPHA}\n\n  robot:{BETA}  \n"));
        let tokens = Tokens::load(f.path()).unwrap();
        assert_eq!(tokens.len(), 2);
        assert!(tokens.check(&bearer(ALPHA)).is_ok());
        assert!(tokens.check(&bearer(BETA)).is_ok());
        assert_eq!(tokens.unlabeled(), 1);
    }

    /// Eine leere Tokendatei sieht aus wie Schutz und ist keiner.
    #[test]
    fn an_empty_token_file_is_a_configuration_error() {
        let f = file_with("# nur Kommentare\n\n");
        assert!(Tokens::load(f.path()).is_err());
    }

    /// N1: ein kurzes Token ist durchprobierbar und wird nicht geladen.
    #[test]
    fn a_short_token_is_refused() {
        let error = Tokens::parse("s3cret\n").unwrap_err();
        assert!(error.contains("kuerzer"), "{error}");
        assert!(Tokens::parse("robot:s3cret\n").is_err());
    }

    #[test]
    fn duplicate_labels_and_tokens_are_refused() {
        assert!(Tokens::parse(&format!("a:{ALPHA}\na:{BETA}\n")).is_err());
        assert!(Tokens::parse(&format!("{ALPHA}\nb:{ALPHA}\n")).is_err());
    }

    #[test]
    fn anything_but_a_known_token_is_rejected() {
        let tokens = Tokens::parse(&format!("{ALPHA}\n")).unwrap();
        for header in [
            None,
            Some(ALPHA.to_owned()),
            Some("Bearer gamma-0123456789abcdef".to_owned()),
            Some(format!("Basic {ALPHA}")),
        ] {
            let status = tokens
                .check(&request_with(header.as_deref()))
                .expect_err("nur ein bekanntes Bearer-Token kommt durch");
            assert_eq!(status.code(), tonic::Code::Unauthenticated);
        }
    }

    /// N1: die Hinweis-Kennung kommt aus dem Namen, nicht aus dem Token.
    ///
    /// Ein Token ohne Namen belegt keine Kennung; ein benanntes belegt die
    /// seines Namens — dieselbe Zahl, die beim Start im Log steht.
    #[test]
    fn the_hint_authority_comes_from_the_label_not_the_secret() {
        let tokens = Tokens::parse(&format!("{ALPHA}\nrobot:{BETA}\n")).unwrap();
        assert_eq!(tokens.authority_of(&bearer(ALPHA)), None);
        assert_eq!(
            tokens.authority_of(&bearer(BETA)),
            Some(authority_for_label("robot"))
        );
        assert_eq!(
            tokens.authorities(),
            vec![("robot".to_owned(), authority_for_label("robot"))]
        );
        // Der Name bestimmt die Kennung: ein neues Token mit demselben Namen
        // behaelt sie. Wer Token rotiert, muss die Hinweispolicy nicht anfassen.
        let rotated = Tokens::parse("robot:rotated-0123456789abcdef\n").unwrap();
        assert_eq!(
            rotated.authority_of(&bearer("rotated-0123456789abcdef")),
            Some(authority_for_label("robot"))
        );
    }

    /// Ein Doppelpunkt ohne gueltigen Namen davor gehoert zum Token.
    #[test]
    fn a_colon_inside_a_token_does_not_make_a_label() {
        let token = "a b:0123456789abcdef";
        let tokens = Tokens::parse(&format!("{token}\n")).unwrap();
        assert!(tokens.contains(&bearer(token)));
        assert_eq!(tokens.unlabeled(), 1);
    }

    /// Verschiedene Token, verschiedene Identitaeten; keine ist anonym.
    #[test]
    fn identities_differ_per_token_and_are_never_anonymous() {
        let tokens = Tokens::parse(&format!("a:{ALPHA}\n{BETA}\n")).unwrap();
        let a = tokens.identity_of(&bearer(ALPHA)).unwrap();
        let b = tokens.identity_of(&bearer(BETA)).unwrap();
        assert_ne!(a, b);
        assert!(a.is_authenticated() && b.is_authenticated());
    }

    /// M1: der Interceptor weist ab, bevor irgendetwas dekodiert wird.
    #[test]
    fn the_gate_rejects_before_the_service_sees_anything() {
        use tonic::service::Interceptor as _;
        let tokens = Tokens::parse(&format!("{ALPHA}\n")).unwrap();
        let admin = Tokens::parse(&format!("ops:{BETA}\n")).unwrap();
        let mut gate = Gate::new(Some(tokens), Some(admin));

        assert_eq!(
            gate.call(request_with(None)).unwrap_err().code(),
            tonic::Code::Unauthenticated
        );
        assert!(gate.call(bearer(ALPHA)).is_ok());
        // Ein Administrationstoken gilt auch fuer gewoehnliche Anfragen.
        assert!(gate.call(bearer(BETA)).is_ok());
        assert!(gate.is_admin(&bearer(BETA)));
        assert!(!gate.is_admin(&bearer(ALPHA)));
    }

    /// Ohne Tokenliste laesst die Pruefung jeden durch, und alle sind
    /// dieselbe Identitaet.
    #[test]
    fn without_tokens_everyone_is_anonymous() {
        let gate = Gate::default();
        assert!(gate.admit(&request_with(None)).is_ok());
        assert_eq!(gate.identity_of(&request_with(None)), Identity::ANONYMOUS);
        assert!(!gate.is_admin(&request_with(None)));
    }
}
