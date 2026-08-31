# ADR-0010: Pessimistisch versprechen, optimistisch verwerfen

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec 10.3 (Stufe B), 13.2 (konservative Prognose)
**Ausloeser:** Gate-S-Lauf nach ADR-0009, Szenario A weiterhin ohne Ausgabe

## Kontext

ADR-0009 stellte die Verwerfensentscheidung von der Deadline auf die Frische
um. Szenario A lieferte danach **weiterhin** null Ergebnisse bei 125 % Last.

Die Ursache liegt eine Ebene tiefer. Die Frischepruefung verwendete dieselbe
konservative Prognose wie die Machbarkeitsrechnung:

```text
predicted_finish = start + p99 * 1.10
verwerfen, wenn predicted_finish - generation_time > max_age
```

In Szenario A bei 125 % Last: `p99 * 1.10 = 68 ms`, Transportverzoegerung
3 ms, `max_age = 66 ms`. Schon ohne jede Wartezeit ergibt das 71 ms — jeder
Frame galt als wertlos, bevor er startete. Die FIFO-Baseline lieferte im selben
Lauf 282 brauchbare Ergebnisse, weil die **tatsaechliche** Laufzeit meist bei
p50 ≈ 41 ms lag und damit gut innerhalb der Frischegrenze.

OneTimer warf also Arbeit weg, die in der Realitaet ueberwiegend nuetzlich
gewesen waere — auf Grundlage einer Annahme, die absichtlich pessimistisch ist.

## Entscheidung

Konservatismus wird richtungsabhaengig angewandt.

| Entscheidung | Schaetzer | Warum |
|---|---|---|
| Machbarkeit, Variantenwahl, Look-ahead-Veto, Slot-Belegung | `p99 * Sicherheitsmarge` | Hier wird etwas zugesagt. Wer optimistisch plant, verspricht, was er nicht halten kann. |
| **Verwerfen wegen Ueberalterung** | `p50` | Hier wird etwas vernichtet. Wer pessimistisch verwirft, wirft weg, was ueberwiegend noch gut gewesen waere. |

Ein Request wird vor dem Dispatch nur verworfen, wenn er **selbst im
guenstigen Fall** wertlos waere:

```text
optimistic_finish = earliest_start + p50
verwerfen, wenn optimistic_finish - generation_time > max_age
```

Die Machbarkeitsrechnung bleibt unveraendert konservativ. Ein Request, dessen
p99-Prognose die Deadline reisst, dessen p50-Prognose aber innerhalb von
`max_age` liegt, wird mit der schnellsten Variante gestartet und bei
Fertigstellung nach Stufe C bewertet.

## Begruendung

Die beiden Entscheidungen haben unterschiedliche Fehlerkosten.

Eine zu optimistische **Zusage** kostet eine verpasste Deadline: das System
haette warten oder degradieren koennen und hat es nicht getan.

Ein zu pessimistisches **Verwerfen** kostet ein Ergebnis, das es nie geben
wird — und im Grenzfall, wie Szenario A zeigt, die gesamte Funktion. Es gibt
keine Korrekturmoeglichkeit: verworfene Arbeit kommt nicht wieder.

Dieselbe Asymmetrie liegt bereits der Variantenhysterese zugrunde (Spec 12.4):
Abwertung sofort, Aufwertung erst nach stabiler Reserve. In beiden Faellen
wird der Konservatismus dorthin gelegt, wo er den geringeren Schaden anrichtet.

## Konsequenzen

- `VariantProfile` liefert neben der konservativen auch eine optimistische
  Laufzeitschaetzung. Beide stammen aus demselben Profil; es entsteht keine
  zweite Datenquelle.
- Mehr Requests werden gestartet, mehr davon werden bei Fertigstellung als
  `CompletedObsolete` bewertet. Das ist gewollt: die Verschwendung wird
  gemessen und ausgewiesen, statt sie durch praeventives Verwerfen unsichtbar
  zu machen.
- Fuer den Report heisst das: `stale_compute` von OneTimer wird nicht mehr
  automatisch null sein. Ein Wert von exakt null ist ab sofort ein
  Warnsignal — er bedeutet meist, dass zu viel praeventiv verworfen wurde.
- Szenario A bleibt auch danach eine ueberzeichnete Konfiguration
  (`max_age` in der Groessenordnung der Laufzeit). Genau solche Vertraege muss
  `onetimer doctor` vor dem Start melden (Spec 10.9), statt sie zur Laufzeit
  auszubaden.
