# ADR-0011: Die Erzeugungszeit kommt aus einer fremden Uhr

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec 16.2 (`onetimer_generation_ns`), 10.2, L-008, L-019

## Kontext

Die Generation Time ist der wertvollste Wert, den ein Client liefern kann: an
ihr haengen Deadline, Alter und die gesamte Freshness-Semantik (Spec 10.2).

Sie ist zugleich der einzige Scheduling-Eingabewert, der **nicht aus unserer
Zeitbasis stammt**. Spec 16.2 nennt nur `onetimer_generation_ns` und laesst
offen, welche Uhr das ist. Damit sind drei Fehlerfaelle moeglich, die alle
still ablaufen:

1. **Andere Epoche.** `CLOCK_MONOTONIC` zaehlt auf jedem Host ab dem jeweiligen
   Bootzeitpunkt. Ein Wert von einem anderen Rechner ergibt ein Alter von
   Stunden oder eine Deadline in der Zukunft — beides ohne Fehlermeldung.
2. **Wall-Clock.** Ein Client, der `CLOCK_REALTIME` sendet, liefert eine Zahl,
   die durch NTP springen kann. Spec L-019 verbietet ausdruecklich, Scheduling
   darauf zu gruenden.
3. **Uhrenversatz.** Selbst auf demselben Host kann ein Zeitstempel ein paar
   Mikrosekunden in der Zukunft liegen. Naiv verrechnet ergibt das ein
   negatives Alter.

Ohne Behandlung sind alle drei Faelle unsichtbar: der Scheduler rechnet
weiter, nur mit sinnlosen Zahlen.

## Entscheidung

**1. Zwei Wege, der Client waehlt nach seiner Topologie.**

| Parameter | Bedeutung | Gueltig |
|---|---|---|
| `onetimer_generation_ns` | absolute monotone Zeit des Clients | nur bei geteilter Uhr, also auf demselben Host |
| `onetimer_age_us` | Alter des Sensordatums beim Senden | ueber Hostgrenzen hinweg |

Beide gleichzeitig zu setzen ist ein Fehler und wird abgelehnt. Sie
stillschweigend gegeneinander abzuwaegen hiesse, sich fuer eine zu entscheiden,
ohne es zu sagen (Spec L-020).

Der Regelfall auf einem Roboter — ROS-Node und Governor auf demselben SoC —
bleibt der genaue Pfad. Der Alterspfad kostet die Uebertragungszeit an
Genauigkeit und ist dafuer topologieunabhaengig.

**2. Plausibilitaetspruefung statt blindem Vertrauen.**

Ein absoluter Zeitstempel wird nur uebernommen, wenn er

- hoechstens `MAX_CLOCK_SKEW` (10 ms) in der Zukunft liegt und
- nicht aelter als das konfigurierte plausible Hoechstalter ist.

Andernfalls faellt OneTimer auf die Ankunftszeit zurueck — der Compatibility
Mode aus Spec 16.3 — und **meldet das**. Ein leicht zukuenftiger Zeitstempel
wird auf die Ankunftszeit geklemmt; negative Alter gibt es nicht.

**3. Der Fallback ist ein Messwert, kein Notbehelf.**

`GenerationSource` unterscheidet, ob die Erzeugungszeit vom Client kam oder aus
dem Fallback stammt, und im zweiten Fall, ob ein Clientwert verworfen wurde.
Als Metrik ausgewiesen (`onetimer_generation_fallback_total{reason=...}`) macht
das den haeufigsten Integrationsfehler sichtbar: der Kunde setzt den Parameter,
er wird verworfen, und OneTimer arbeitet ohne die Semantik, fuer die er
gekauft wurde.

## Konsequenzen

- Ein neuer Parameter `onetimer_age_us` ergaenzt die Liste aus Spec 16.2.
- `onetimer doctor` sollte spaeter eine Stichprobe echter Requests bewerten und
  melden, wenn ein nennenswerter Anteil in den Fallback laeuft.
- Ein unbekannter Parameter mit `onetimer_`-Prefix wird abgelehnt statt
  ignoriert. Ein Tippfehler in `onetimer_deadline_us` wuerde sonst dazu
  fuehren, dass der Request ohne Deadline laeuft — technisch fehlerfrei und
  fachlich falsch.
