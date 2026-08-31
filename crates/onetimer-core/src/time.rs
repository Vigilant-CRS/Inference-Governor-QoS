//! Monotone Zeitarithmetik fuer den Scheduler.
//!
//! Zwei Anforderungen der Spezifikation bestimmen dieses Modul:
//!
//! * **L-019** — Scheduling darf nicht auf einer durch NTP korrigierbaren
//!   Wall-Clock basieren. Deshalb existiert hier kein Bezug zur Kalenderzeit;
//!   [`Instant`] ist ein Offset gegen eine beliebige, aber feste Epoche.
//! * **G-012** — Extremwerte aus Clientparametern duerfen keinen Integer-Overflow
//!   und keine negative Deadline erzeugen. Deshalb ist jede Konstruktion aus
//!   fremd kontrollierten Werten fehlbar (`Option`), und jede Addition, die
//!   ueberlaufen kann, ist `checked_*`.
//!
//! Der Scheduler-Core ruft **niemals** selbst eine Uhr ab. `now` wird an jedem
//! Eintrittspunkt uebergeben. Damit ist ein Live-Trace im Simulator exakt
//! reproduzierbar (Spec 30.2).

// Dieses Modul ist die Grenzschicht, an der Zeitarithmetik ueberhaupt
// stattfindet. Genau deshalb werden die Arithmetiklints des Workspace hier
// lokal ausgesetzt: die Invarianten, die sie erzwingen sollen, werden hier per
// Konstruktion hergestellt und per Test abgesichert.
//
//   * Division nur durch von null verschiedene Konstanten (1_000, 1_000_000)
//     oder nach expliziter Nullpruefung.
//   * Multiplikation nur in `u128`/`i128` mit anschliessender Bereichspruefung
//     vor dem Rueckcast.
//   * Subtraktion nur saettigend oder in `i128` mit Klemmung.
//
// Ausserhalb dieses Moduls bleiben die Lints in voller Schaerfe aktiv; dort
// darf nur ueber die hier bereitgestellten geprueften Operationen gerechnet
// werden.
#![allow(clippy::integer_division, clippy::arithmetic_side_effects)]

use core::fmt;

/// Obergrenze fuer jede aus Konfiguration oder Clientparametern abgeleitete
/// Zeitspanne: eine Stunde.
///
/// Ein Inferenzvertrag mit laengerer Deadline ist im Physical-AI-Kontext
/// mit Sicherheit ein Konfigurationsfehler. Die Grenze existiert, damit ein
/// boesartiger oder fehlerhafter Client keine Zeitspanne konstruieren kann,
/// die spaetere Additionen zum Ueberlauf bringt (Spec 8.3, G-012).
pub const MAX_CONTRACT_NANOS: u64 = 3_600 * 1_000_000_000;

/// Eine nicht-negative Zeitspanne in Nanosekunden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Duration(u64);

impl Duration {
    /// Zeitspanne null.
    pub const ZERO: Self = Self(0);

    /// Groesste zulaessige Vertragsdauer, siehe [`MAX_CONTRACT_NANOS`].
    pub const MAX_CONTRACT: Self = Self(MAX_CONTRACT_NANOS);

    /// Konstruiert eine Zeitspanne aus Nanosekunden.
    ///
    /// Gibt `None` zurueck, wenn der Wert [`MAX_CONTRACT_NANOS`] ueberschreitet.
    /// Fuer intern erzeugte Messwerte (z. B. beobachtete Backend-Laufzeiten)
    /// ist [`Duration::from_nanos_unchecked_span`] zu verwenden.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Option<Self> {
        if nanos > MAX_CONTRACT_NANOS {
            None
        } else {
            Some(Self(nanos))
        }
    }

    /// Konstruiert eine Zeitspanne aus Mikrosekunden.
    #[must_use]
    pub const fn from_micros(micros: u64) -> Option<Self> {
        match micros.checked_mul(1_000) {
            Some(nanos) => Self::from_nanos(nanos),
            None => None,
        }
    }

    /// Konstruiert eine Zeitspanne aus Millisekunden.
    #[must_use]
    pub const fn from_millis(millis: u64) -> Option<Self> {
        match millis.checked_mul(1_000_000) {
            Some(nanos) => Self::from_nanos(nanos),
            None => None,
        }
    }

    /// Konstruiert eine Zeitspanne ohne Vertragsobergrenze.
    ///
    /// Nur fuer intern erzeugte Spannen (Messwerte, Simulationshorizonte), die
    /// nicht aus fremd kontrollierten Eingaben stammen. Der Wert bleibt durch
    /// `u64` begrenzt und kann keinen Ueberlauf erzeugen.
    #[must_use]
    pub const fn from_nanos_unbounded(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Die Zeitspanne in Nanosekunden.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Die Zeitspanne in Mikrosekunden, abgerundet.
    #[must_use]
    pub const fn as_micros(self) -> u64 {
        self.0 / 1_000
    }

    /// Die Zeitspanne in Millisekunden, abgerundet.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0 / 1_000_000
    }

    /// Addiert zwei Zeitspannen, `None` bei Ueberlauf.
    #[must_use]
    pub const fn checked_add(self, other: Self) -> Option<Self> {
        match self.0.checked_add(other.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Subtrahiert `other`, saettigend bei null.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    /// Skaliert die Zeitspanne mit dem Bruch `num / den`.
    ///
    /// Bewusst ganzzahlig statt mit `f64`: der Sicherheitsmargen-Faktor aus
    /// Spec 13.2 (z. B. 1,10) wird als `110 / 100` ausgedrueckt. Damit bleibt
    /// der Hot Path frei von Gleitkomma und exakt reproduzierbar.
    ///
    /// Gibt `None` bei `den == 0` oder Ueberlauf zurueck.
    #[must_use]
    pub fn checked_scale(self, num: u32, den: u32) -> Option<Self> {
        if den == 0 {
            return None;
        }
        // u128-Zwischenrechnung: u64 * u32 kann darin nicht ueberlaufen.
        let scaled = u128::from(self.0) * u128::from(num) / u128::from(den);
        u64::try_from(scaled).ok().map(Self)
    }

    /// Das Maximum zweier Zeitspannen.
    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        if self.0 >= other.0 { self } else { other }
    }

    /// Das Minimum zweier Zeitspannen.
    #[must_use]
    pub const fn min(self, other: Self) -> Self {
        if self.0 <= other.0 { self } else { other }
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 >= 1_000_000 {
            write!(
                f,
                "{}.{:03}ms",
                self.0 / 1_000_000,
                (self.0 / 1_000) % 1_000
            )
        } else if self.0 >= 1_000 {
            write!(f, "{}.{:03}us", self.0 / 1_000, self.0 % 1_000)
        } else {
            write!(f, "{}ns", self.0)
        }
    }
}

/// Ein monotoner Zeitpunkt in Nanosekunden seit einer beliebigen festen Epoche.
///
/// Traegt keine Kalenderbedeutung. Zeitpunkte aus verschiedenen Prozessen oder
/// Maschinen sind nicht vergleichbar; die Generation Time eines Clients muss am
/// Ingress in die lokale monotone Zeitbasis ueberfuehrt werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Instant(u64);

impl Instant {
    /// Der Nullpunkt der Zeitbasis. Startpunkt jeder Simulation.
    pub const ZERO: Self = Self(0);

    /// Konstruiert einen Zeitpunkt aus Nanosekunden seit Epoche.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Nanosekunden seit Epoche.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Addiert eine Zeitspanne, `None` bei Ueberlauf.
    ///
    /// Dies ist der Pfad, ueber den absolute Deadlines entstehen
    /// (`generation_time + relative_deadline`, Spec L-010). Ein Ueberlauf ist
    /// hier ein Eingabefehler und muss den Request ablehnen, nicht wrappen
    /// (Spec G-012).
    #[must_use]
    pub const fn checked_add(self, d: Duration) -> Option<Self> {
        match self.0.checked_add(d.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Subtrahiert eine Zeitspanne, saettigend bei [`Instant::ZERO`].
    #[must_use]
    pub const fn saturating_sub(self, d: Duration) -> Self {
        Self(self.0.saturating_sub(d.0))
    }

    /// Die verstrichene Zeitspanne seit `earlier`, saettigend bei null.
    ///
    /// Saettigung statt Fehler ist hier korrekt: ein Request, dessen
    /// Generation Time in der Zukunft liegt (Uhrenversatz beim Client), hat
    /// aus Sicht des Schedulers das Alter null, nicht ein negatives Alter.
    #[must_use]
    pub const fn saturating_since(self, earlier: Self) -> Duration {
        Duration(self.0.saturating_sub(earlier.0))
    }

    /// Die vorzeichenbehaftete Differenz `self - other`.
    ///
    /// Basis der Slack-Rechnung (Spec 10.4), die ausdruecklich negativ werden
    /// koennen muss. Saettigt an den `i64`-Grenzen statt zu wrappen.
    #[must_use]
    pub fn signed_since(self, other: Self) -> Slack {
        let d = i128::from(self.0) - i128::from(other.0);
        Slack(i64::try_from(d).unwrap_or(if d.is_negative() { i64::MIN } else { i64::MAX }))
    }

    /// Der spaetere zweier Zeitpunkte.
    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        if self.0 >= other.0 { self } else { other }
    }

    /// Der fruehere zweier Zeitpunkte.
    #[must_use]
    pub const fn min(self, other: Self) -> Self {
        if self.0 <= other.0 { self } else { other }
    }
}

impl fmt::Display for Instant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t={}.{:06}ms", self.0 / 1_000_000, self.0 % 1_000_000)
    }
}

/// Eine vorzeichenbehaftete Zeitspanne in Nanosekunden.
///
/// Ergebnis der Slack-/Laxity-Rechnung aus Spec 10.4:
/// `slack(j,v) = deadline(j) - now - predicted_runtime(j,v)`.
/// Negativer Slack bedeutet: mit dieser Variante nicht mehr rechtzeitig machbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Slack(i64);

impl Slack {
    /// Slack null — exakt auf der Deadline.
    pub const ZERO: Self = Self(0);

    /// Konstruiert Slack aus Nanosekunden.
    #[must_use]
    pub const fn from_nanos(nanos: i64) -> Self {
        Self(nanos)
    }

    /// Der Slack in Nanosekunden.
    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }

    /// Wahr, wenn die Deadline mit der zugrunde liegenden Annahme nicht mehr
    /// eingehalten werden kann.
    #[must_use]
    pub const fn is_infeasible(self) -> bool {
        self.0 < 0
    }

    /// Subtrahiert eine Zeitspanne, saettigend an den `i64`-Grenzen.
    #[must_use]
    pub fn saturating_sub(self, d: Duration) -> Self {
        let rhs = i64::try_from(d.0).unwrap_or(i64::MAX);
        Self(self.0.saturating_sub(rhs))
    }
}

impl fmt::Display for Slack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let abs = self.0.unsigned_abs();
        let sign = if self.0 < 0 { "-" } else { "+" };
        write!(
            f,
            "{sign}{}.{:03}ms",
            abs / 1_000_000,
            (abs / 1_000) % 1_000
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    #[test]
    fn duration_rejects_absurd_contracts() {
        assert!(Duration::from_millis(30).is_some());
        assert!(
            Duration::from_millis(3_600_000).is_some(),
            "exakt eine Stunde ist zulaessig"
        );
        assert!(
            Duration::from_millis(3_600_001).is_none(),
            "ueber eine Stunde nicht"
        );
        assert!(
            Duration::from_millis(u64::MAX).is_none(),
            "kein Overflow beim Skalieren"
        );
        assert!(Duration::from_micros(u64::MAX).is_none());
    }

    /// Spec G-012: Extremwerte duerfen keinen Overflow und keine negative
    /// Deadline erzeugen.
    #[test]
    fn g012_deadline_arithmetic_cannot_overflow() {
        let late = Instant::from_nanos(u64::MAX - 5);
        let d = Duration::from_millis(30).unwrap();
        assert!(
            late.checked_add(d).is_none(),
            "Overflow muss als None sichtbar sein"
        );

        let ok = Instant::from_nanos(1_000);
        assert_eq!(ok.checked_add(d).unwrap().as_nanos(), 1_000 + 30_000_000);
    }

    #[test]
    fn age_of_a_future_frame_is_zero_not_negative() {
        // Client mit Uhrenversatz: generation_time liegt hinter now.
        let now = Instant::from_nanos(1_000);
        let generated = Instant::from_nanos(5_000);
        assert_eq!(now.saturating_since(generated), Duration::ZERO);
    }

    #[test]
    fn signed_difference_survives_extremes() {
        let hi = Instant::from_nanos(u64::MAX);
        let lo = Instant::ZERO;
        assert_eq!(
            hi.signed_since(lo).as_nanos(),
            i64::MAX,
            "saettigt, wrappt nicht"
        );
        assert_eq!(lo.signed_since(hi).as_nanos(), i64::MIN);
    }

    #[test]
    fn slack_is_negative_when_infeasible() {
        let deadline = Instant::from_nanos(30_000_000);
        let now = Instant::from_nanos(25_000_000);
        let runtime = Duration::from_millis(20).unwrap();

        let slack = deadline.signed_since(now).saturating_sub(runtime);
        assert!(
            slack.is_infeasible(),
            "5ms Rest, 20ms Laufzeit => infeasible"
        );
        assert_eq!(slack.as_nanos(), -15_000_000);
    }

    /// Spec 13.2: `predicted = max(offline_p99, online_p99) * safety_margin`.
    /// Der Margin ist ganzzahlig als Bruch ausgedrueckt, damit der Hot Path
    /// gleitkommafrei und exakt reproduzierbar bleibt.
    #[test]
    fn safety_margin_is_exact_integer_arithmetic() {
        let p99 = Duration::from_micros(10_000).unwrap(); // 10ms
        let with_margin = p99.checked_scale(110, 100).unwrap();
        assert_eq!(with_margin.as_micros(), 11_000);

        assert!(
            p99.checked_scale(1, 0).is_none(),
            "Division durch null wird abgefangen"
        );
        assert_eq!(
            Duration::from_nanos_unbounded(u64::MAX).checked_scale(2, 1),
            None,
            "Overflow beim Skalieren wird abgefangen"
        );
    }

    #[test]
    fn display_is_readable_in_traces() {
        assert_eq!(Duration::from_micros(1_500).unwrap().to_string(), "1.500ms");
        assert_eq!(Duration::from_nanos(1_500).unwrap().to_string(), "1.500us");
        assert_eq!(Slack::from_nanos(-15_000_000).to_string(), "-15.000ms");
        assert_eq!(Slack::from_nanos(2_500_000).to_string(), "+2.500ms");
    }
}
