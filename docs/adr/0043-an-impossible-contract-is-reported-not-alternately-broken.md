# ADR-0043: Ein unerfuellbarer Vertrag wird gemeldet, nicht abwechselnd gebrochen

**Status:** Akzeptiert · 2026-09-14
**Betrifft:** `cli/doctor` (`check_supply_guard_vs_background`),
`backend.protect_supply` (ADR-0041), `minimum_background_progress_pct`
(Spec 19.6, `core/contract_ext`); ADR-0012, ADR-0014, ADR-0035, ADR-0037
**Ausloeser:** Die offene Frage aus ADR-0041, beantwortet durch die Abnahme
vom 12.09. ([Messung](../benchmark/abnahme-2026-09-12.md)) und den Review
vom 14.09. ([R07](../reviews/2026-09-14/REVIEW.md))

## Kontext

Der Versorgungsschutz (ADR-0041) haelt den geschuetzten Strom, indem er
Hintergrundarbeit zurueckhaelt, die das vorige Ergebnis ablaufen liesse. Auf
der GPU kostet das bei 110 % Last **998 von 1000 Takten** des Hintergrunds.
ADR-0041 liess offen, ob `minimum_background_progress_pct` den Guard
ueberstimmen soll: der Guard laesst dann wieder Arbeit zu, sobald der
Hintergrund unter seine Zusage faellt.

Das waere ein Regler zwischen zwei Zusagen, und genau daran scheitert es.
Ein unteilbarer Hintergrundauftrag der Dauer `B` passt nur dann zwischen zwei
geschuetzte Ankuenfte, wenn die naechste danach noch vor dem Ablauf des
vorigen Ergebnisses fertig wird. Im ungeguenstigsten Fall startet er
unmittelbar vor der Ankunft; mit konservativer Laufzeit `C` des geschuetzten
Stroms und seinem Hoechstalter `A` heisst das:

```
B + C <= A - C        also        B <= A - 2C
```

Ist `B` groesser, gibt es keine Reihenfolge, die beide Zusagen haelt. Ein
Mindestfortschrittsregler wuerde die Arbeit dann trotzdem starten — auf
Kosten des Schutzes — und der Guard wuerde sie beim naechsten Mal wieder
zurueckhalten. Das Ergebnis waere ein System, das abwechselnd beide Zusagen
bricht und keine davon erklaert. Der Betreiber erfaehrt nie, welche gilt.

## Entscheidung

**Der Mindestfortschritt ueberstimmt den Versorgungsschutz nicht.** Beide
Zusagen bleiben, was sie sind. Stattdessen prueft `vig doctor` die
Kombination und **meldet sie als unerfuellbar**, bevor der Governor startet:
`NOT_READY`, mit der Rechnung und dem Namen beider Vertraege.

Wer beides braucht, aendert den Vertrag, und zwar sichtbar:

- die Hintergrundarbeit **zerlegen** (ADR-0014), bis `B` in die Luecke passt,
- ihre **Rate senken** oder das Hoechstalter `A` des geschuetzten Stroms
  anheben, wenn die Anwendung das traegt,
- sie **unterbrechbar** machen (ADR-0035), dann zaehlt die Restblockierung
  statt der vollen Laufzeit,
- oder ihr **eine eigene Ressource** geben (ADR-0037).

## Konsequenzen

- Der Kern aendert sich nicht. `protect_supply` bleibt opt-in und behaelt
  genau das Verhalten aus ADR-0041; es gibt keinen versteckten Regler.
- Eine unerfuellbare Kombination startet nicht mehr unbemerkt. Sie faellt
  beim Start auf, nicht nach zwei Wochen in einer leeren Kennzahl.
- Die Pruefung ist **konservativ und vereinfacht**: Sie rechnet mit der
  laengsten Variante des Hintergrunds, mit `p99 x Marge` als Laufzeit und mit
  dem engsten geschuetzten Fenster. Sie beweist keine Erfuellbarkeit — ein
  gemessenes p99 ist keine obere Schranke, und Jitter, Streuung und fremde
  Last stehen nicht darin. Sie beweist nur **Unerfuellbarkeit**: liegt `B`
  ueber `A - 2C`, gibt es keine Reihenfolge, die beide Zusagen haelt.
- Ohne `protect_supply` aendert sich nichts; ohne
  `minimum_background_progress_pct` ebenso wenig. Die Pruefung schweigt dann.
- Was sie nicht loest: Die Frage, ob der Hintergrund auf dieser Karte
  ueberhaupt genug bekommt, beantwortet weiter nur eine Messung. Der
  Mindestfortschritt bleibt ein Weakly-hard-Kriterium, das der Monitor zaehlt
  (`core/contract_ext::effective_miss_budget`), keine Ausfuehrungszusage.
