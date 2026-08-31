# ADR-0007: Herkunft der Varianten-Qualitätswerte

**Status:** Akzeptiert · 2026-08-31
**Betrifft:** Spec §12.2 (Qualitätswert), §12.3 (Auswahlregel), §7.1 L-020, §19.8

## Kontext

§12.2 legt richtig fest, dass OneTimer Qualität nicht automatisch erfinden darf,
und verlangt einen „durch den Nutzer oder Benchmark ermittelten relativen Score".
§12.3 macht diesen Score zum **alleinigen** Sortierkriterium der Variantenwahl.

Risiko: Der Kunde hat diese Zahl nicht. Er kennt vielleicht die mAP seines großen
Modells auf einem öffentlichen Datensatz, aber nicht den relativen Nutzenverlust
der kleinen Variante **in seiner Anwendung**. Ein geratener Wert führt entweder
zu unnötiger Qualitätsdegradation oder dazu, dass eine für die Aufgabe
unbrauchbare Variante als „feasible" ausgewählt wird — im zweiten Fall
verschlechtert OneTimer die Wahrnehmung, während seine eigenen Metriken grün
bleiben.

Das ist eine direkte Instanz des Kill-Kriteriums aus §19.8: „erforderliche
Konfiguration pro Kunde so individuell, dass ein Engineer wochenlang manuell
Profile/Regeln schreiben muss."

## Entscheidung

1. **Kein stiller Default.** `quality` bleibt im MVP ein explizit vom Nutzer
   gesetzter, deklarierter Wert. `onetimer doctor` verweigert Varianten ohne
   expliziten Wert — konsistent mit L-020 (Fail-safe Configuration).

2. **Die Herkunft wird mitgeführt.** Das Config-Schema trägt die Provenienz:

   ```yaml
   variants:
     - id: small
       backend_model: detector_small
       quality:
         value: 0.93
         source: user_declared    # user_declared | measured | unknown
         measured_on: null        # Datensatz-/Sample-ID bei source: measured
   ```

   `source: unknown` ist zulässig, **deaktiviert aber die automatische
   Variantenwahl** für dieses Modell. Es bleibt nur manuelle und
   Overload-getriebene Degradation (§14.2 DEGRADED), bei der die Alternative
   ohnehin „gar kein rechtzeitiges Ergebnis" ist.

3. **Ein Ableitungswerkzeug ist post-MVP.** `onetimer eval-variants` — Agreement
   bzw. Metrikdelta zwischen Varianten auf einem Kundensample — wird nicht jetzt
   gebaut, aber das Schema präjudiziert es nicht weg.

## Begründung der Reihenfolge

Das Ableitungswerkzeug ist nur sinnvoll, wenn die Kernhypothese trägt. Das Schema
jetzt richtig zu schneiden kostet fast nichts; das Werkzeug jetzt zu bauen kostet
viel. Die Unterscheidung `user_declared` vs. `measured` ist zudem der Punkt, an
dem später ein ehrlicher Produktclaim möglich wird — ohne sie ließe sich nicht
sagen, ob eine Variantenentscheidung auf Evidenz oder auf einer Schätzung beruht.

## Konsequenzen

- Ein zusätzlicher `doctor`-Check und ein etwas reicheres Config-Schema.
- Der Variant Benchmark (WP19) misst die Qualitäts-/Deadline-Frontier nur dort
  belastbar, wo `source: measured` vorliegt. Für den Benchmark selbst werden die
  Werte daher gemessen, nicht deklariert — sonst misst WP19 die eigene Annahme.
