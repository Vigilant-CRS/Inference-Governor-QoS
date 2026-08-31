# ADR-0013: Die Marge korrigiert Prognosefehler, nicht Vertragsverletzungen

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec 13.3 (langsame Margin-Anpassung)
**Ausloeser:** Verdrahtung des Online Estimators (WP11)

## Kontext

Spec 13.3 nennt zwei Ausloeser fuer eine straffere Sicherheitsmarge:

> Nach Deadline-Miss **oder** deutlicher Runtime-Unterprognose Margin
> schrittweise erhoehen.

Die erste Implementierung folgte dem woertlich und straffte nach jeder
Vertragsverletzung — verpasste Deadline oder bei Fertigstellung veraltetes
Ergebnis.

Das ist bei genauerem Hinsehen eine Verwechslung zweier verschiedener Dinge.

Die Sicherheitsmarge beantwortet genau eine Frage: **wie viel laenger als
prognostiziert kann diese Inferenz dauern?** Sie ist eine Aussage ueber die
Laufzeitverteilung des Modells.

Eine verpasste Deadline kann daraus entstehen — oder aus Wartezeit vor einem
belegten Slot, aus einer Ankunftsspitze, aus einem Frame, der schon alt
eintraf. In allen diesen Faellen war die Laufzeitprognose **richtig**, und eine
groessere Marge macht die Lage schlechter statt besser:

```text
Marge steigt
  -> prognostizierte Laufzeit steigt
    -> geplante Slot-Belegung reicht weiter in die Zukunft
      -> prognostizierter Start des naechsten Requests liegt spaeter
        -> mehr Requests gelten als zu alt und werden verworfen
          -> mehr Vertragsverletzungen
            -> Marge steigt
```

Eine Regelung, die auf ein Symptom reagiert, das sie selbst verschlimmert.

## Entscheidung

Ausloeser fuer das Straffen ist allein die **Unterprognose**:

```text
beobachtete Laufzeit > geplante Laufzeit  ->  straffen
sonst                                     ->  langsam entspannen
```

Dafuer wird die geplante Laufzeit beim Dispatch mitgefuehrt und bei der
Fertigstellung mit der beobachteten verglichen. Ohne diesen Vergleichswert
waere „Unterprognose" gar nicht feststellbar — die absolute Laufzeit allein
sagt nichts darueber, ob die Prognose falsch war.

Verspaetung aus Wartezeit bleibt sichtbar, wird aber dort behandelt, wo sie
hingehoert: in der Zulassung, in der Variantenwahl und in der Ueberlast-FSM.
Das sind die Mechanismen, die Kapazitaetsprobleme loesen koennen.

## Ehrlichkeit zur Beweislage

Die Untersuchung begann mit zwei fehlschlagenden Tests nach der Verdrahtung des
Schaetzers. Deren Ursache war am Ende **eine andere**: `SafetyMargin::NONE` war
als `1/1` dargestellt, `as_percent()` lieferte daher 1 statt 100, und der
Margenregler wich auf den Default aus. Nach dessen Korrektur waren die Tests
wieder gruen.

Die hier getroffene Entscheidung steht davon unabhaengig — sie beruht auf dem
Rueckkopplungsargument oben, nicht auf jener Messung. Das wird festgehalten,
damit spaeter niemand eine Evidenz zitiert, die es nicht gibt.

## Konsequenzen

- Bei einem guten Profil bleibt die Marge auf ihrem konfigurierten Startwert.
  Das ist gewollt: sie soll nichts tun, solange nichts zu korrigieren ist.
- Ein systematisch zu optimistisches Profil wird weiterhin erkannt und
  korrigiert, denn dann ist die beobachtete Laufzeit tatsaechlich groesser als
  die geplante.
- Der Circuit Breaker aus Spec 30.3 bleibt die Antwort auf ein Profil, das so
  falsch ist, dass keine Marge es rettet.
