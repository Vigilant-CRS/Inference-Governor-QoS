# 04 — Zeitmodell, Verträge und Grenzen der Zusagen

Status: Spezifikationsentwurf. Formeln definieren Zielsemantik und Annahmen;
sie sind kein Beweis, dass der heutige Scheduler diese Eigenschaften garantiert.

## 1. Vier Fragen auseinanderhalten

1. **Darf** eine Ausführung stattfinden? Identität, Rechte, Qualität, Budget.
2. **Passt sie voraussichtlich** in den Vertrag? Gültiges Modell der Ausführung.
3. **Was ist wirklich passiert?** Verbraucherzustand und Ausführungsnachweis.
4. **Wie stark ist die Zusage?** Beobachtung, qualifiziertes SLO oder Beweis.

„Predicted admissible“, „im Test eingehalten“ und „für alle zulässigen Fälle
garantiert“ sind verschiedene Aussagen. Eine konservative Ablehnung beweist
nicht, dass keine andere Policy den Auftrag ausführen könnte.

## 2. Zeitpunkte und Messgrenzen

Für einen Auftrag i:

```text
g_i  Aufnahme-/Generationszeit der Information
a_i  Eingang am Governor
s_i  tatsächlicher Ausführungsbeginn
e_i  bestätigtes Ende der Berechnung
f_i  Verfügbarkeit beim vereinbarten Verbraucher
d_i  absolute Deadline
A_i  maximal zulässiges Informationsalter
```

`f_i - g_i` ist Antwortalter; `e_i - s_i` ist Ausführungszeit.
Gateway-, GPU- und Verbraucherzeiten werden getrennt gemessen. Ein
CUDA-Event allein beobachtet nicht alle Netzwerk- und Verbraucherwartezeiten.

Bei einem Vertrag, der sowohl Deadline als auch Frische am Verbraucher verlangt,
ist der effektive späteste Lieferzeitpunkt `min(d_i, g_i + A_i)`. Fehlende
Grenzen werden als nicht spezifiziert behandelt, nicht als Null.
Das ist nicht automatisch die Drop-Regel aller Vertragsklassen: FIFO-/Event-
und NeverDrop-Verträge können eine explizite verspätete Zustellung verlangen.

Uhrendomänen werden benannt. Bei unbekannter Synchronisation wird kein
präzises End-to-End-Alter behauptet. Eine bekannte Zeitunsicherheit epsilon
wird konservativ als Altersaufschlag bzw. Budgetabzug behandelt und protokolliert.

## 3. Consumer-AoI und Versorgung

Für einen Datenstrom sei `u(t)` die jüngste Generationszeit eines zum Zeitpunkt t
bereits verfügbaren und für den Verbraucher semantisch gültigen Ergebnisses:

```text
Delta(t) = t - u(t)
```

Das ist eine Verbrauchersicht auf die
[Age-of-Information-Grundidee](https://arxiv.org/abs/2007.08564).
Sie altert auch ohne neue Antworten. Vor dem ersten gültigen Ergebnis gilt
`no_result`; dieser Zustand wird nicht durch ein fiktives Capture bei Testbeginn
in gute Frische umgerechnet. Zeit ohne Ergebnis wird separat ausgewiesen.

Ein später eintreffendes älteres Ergebnis darf `u(t)` nicht zurücksetzen.
Bei einem Variantenwechsel zählt es nur, wenn es dieselbe freigegebene
Ausgabesemantik besitzt. Beim DAG zählen zusätzlich Abhängigkeiten und Epochen.

Drei Metriken bleiben getrennt:

- **Fresh delivery windows:** Fenster mit mindestens einer frischen Lieferung
  (Legacy-Vergleichsgröße).
- **Consumer coverage:** Anteil vereinbarter Verbraucher-Abtastzeitpunkte,
  an denen ein gültiges, ausreichend frisches Resultat vorhanden ist.
- **Time-weighted AoI / Peak-AoI:** zeitgewichtetes Alter bzw. Spitzenalter;
  zusätzlich `no_result_duration` und längste Versorgungslücke.

Teilfenster, Startzustand, Endpunkt `[start,end)` und Initial-Warm-up werden
vor dem Lauf festgelegt. Warm-up wird nicht nach einem schlechten Lauf neu
definiert. Metriken aus unterschiedlichen Definitionen erhalten eigene Namen.

## 4. Weakly-hard-Bedingungen

Der Vertrag definiert zuerst einen logischen Verbraucherzyklus n mit
Abtastzeit `r_n`. Sei `b_n = 1`, wenn an diesem Zeitpunkt keine vertragsgemäße
Versorgung besteht, sonst 0. Der Takt kommt aus dem vereinbarten Vertrag,
nicht aus der Zahl der tatsächlich angenommenen Requests.

Für maximal M Misses in jedem Fenster aus K Zyklen:

```text
für jedes vollständige Fenster j:
sum(b_n, n=j ... j+K-1) <= M
```

Für maximal L aufeinanderfolgende Misses darf keine Einsenfolge länger als L
werden. „Höchstens zwei Misses in 100 Zyklen und niemals zwei hintereinander“
entspricht `M=2, K=100, L=1`, nicht `L=2`.

Regeln:

- `0 <= M < K`, `K > 0`, begrenzte maximale Fenstergröße; Sonderfälle explizit.
- Sensorstillstand während eines aktiven Vertrags verschwindet nicht aus dem
  Nenner. Er wird als fehlende Versorgung mit eigener Ursache gezählt.
- Supersession zählt nicht automatisch als Verbrauchermiss: ein neuerer Frame
  kann denselben Zyklus versorgen. Umgekehrt macht Ablehnen aller Requests
  den Vertrag nicht erfolgreich.
- Ein noch gültiges Bestandsresultat kann mehrere Verbraucherzyklen abdecken,
  falls der Vertrag keinen zwingend neuen Messwert pro Zyklus verlangt.
- Vertragswechsel tragen Version und Aktivierungszeit; Zähler werden nicht
  heimlich bei Überlast zurückgesetzt.

Eine begrenzte Ringstruktur genügt zur Beobachtung. Durchsetzung erfordert
dagegen genügend zukünftige Kapazität und beherrschte Störungen. Nach einem
Miss kann der nächste Zyklus dringlicher werden; Vorrang allein erzwingt
aber weder verfügbare GPU-Zeit noch ein korrektes Ergebnis.

Deshalb zuerst Monitor, danach experimentelle Policy, erst nach eigener
Qualifikation eine Zusage. Bestehende `Protected`-Priorität ist nicht schon
ein weakly-hard-Vertrag.

## 5. Vertragsschema des Maximalausbaus

Kein parallel eingeführtes zweites Auftragsmodell. Der bestehende
`ModelContract` wird über ein versioniertes Zusatzobjekt erweitert:

```text
ContractExtension {
  consumer_period, phase, release_jitter_envelope,
  delivery_boundary,
  delivery_semantics: latest_state | every_event | stateful_sequence,
  require_new_sample_each_cycle,
  observation_window,
  miss_budget: optional {max_misses, window_cycles, max_consecutive},
  minimum_background_progress: optional,
  approved_quality_set, validity_envelope,
  evidence_required,
  contract_version
}
```

Die bestehende Deadline, `max_age`, Kritikalität und Mindestqualität bleiben
die Ausgangswerte. Energie ist eine nachrangige Policy, keine Erlaubnis,
Verträge zu brechen. Anforderungen stammen vom Betreiber, Messwerte vom
Profiler. Wenn kein Betriebspunkt passt, wird das gemeldet und nicht die
Deadline passend verlängert.

## 6. Zulassung unter Zustand und Interferenz

Für Variante m, Ressourcenlayout r und Zustand z wird eine Laufzeitprognose
`C(i,m,r,z)` benötigt. z enthält nur relevante und beobachtbare Merkmale;
die genaue Auswahl wird gegen Ablationen geprüft.

Schematische Lieferprognose, **ohne überlappende Messanteile doppelt zu zählen**:

```text
predicted_finish = now
                 + wartende/noch blockierende Arbeit
                 + nötige Stell-/Warm-up-Verzögerung
                 + Eingabe- und Ausführungszeit
                 + Ausgabe-/Verbraucherpfad
                 + begründete Unsicherheitsreserve
```

Ein bereits End-to-End gemessenes Profil darf nicht nochmals um dieselben
Transportkosten erweitert werden. Parallel überlappende Kopien und Rechenzeit
werden als Ressourcenablauf modelliert, nicht beliebig summiert.

Prüfreihenfolge:

1. Identität, Rechte und Version stimmen; Payload ist vollständig und gültig.
2. Variante hat freigegebene Semantik und ausreichende Qualität.
3. Profil deckt Ausführungspfad, Zustand und Ressourcenlayout ab.
4. Physische Leases und Speicherbudgets passen; unbekannte Belegung bleibt belegt.
5. Geplante Lieferung passt in die vereinbarten Zeitgrenzen.
6. Gemeinsam betrachtete geschützte Zukunftsarbeit bleibt im Modell machbar.
7. Mindestfortschritt anderer zugesagter Verbraucher bleibt berücksichtigt.

Der vorhandene Look-ahead ist eine begrenzte Prognose. Er wird nicht durch
Umbenennen zur allgemeinen Schedulability-Analyse. Bei Jitter, mehrteiligen
Jobs oder unkontrollierter Fremdlast muss die verwendete Annahme sichtbar sein.

## 7. Nichtpräemptives Blocking und Reservierung

Im vereinfachten seriellen Modell ist bei beliebiger Ankunftsphase eines
kritischen Jobs eine notwendige Bedingung:

```text
maximale Restblockierzeit B + kritische Ausführungszeit C <= relative Deadline D
```

Zusätzliche Jobs, Jitter und Transport können die Bedingung verschärfen.
Erwartete Perioden allein ersetzen keine Grenze für unerwartete Ankünfte.
Ein häufiger Scheduler-Tick verkürzt B nicht. Eine kürzere Backend-Quantumgrenze
oder wirksame Ressourcenaufteilung kann B verändern und muss vermessen werden.

Eine SM-Reservierung kann Recheninterferenz verändern, ersetzt aber keinen
Nachweis für gemeinsam benutzte Speicher-, Host- und Transferressourcen.
Latenzprofile einer ganzen GPU gelten nicht unverändert für eine kleine Partition.

## 8. Interferenz ist gerichtet und nicht allgemein additiv

`I(A | B)` beschreibt die Veränderung von A während B läuft. Im Allgemeinen
ist `I(A | B) != I(B | A)`. Für drei Modelle folgt aus zwei Paarmessungen
keine allgemeine Laufzeitgrenze.

Der Profiler erfasst daher absolute Laufzeiten und Verteilungen je Richtung,
Überlappungsphase, Ausführungsform und gegebenenfalls Ressourcenlayout.
Paarweise Daten sind eine Kandidatenheuristik. Die tatsächliche N-Wege-
Konfiguration muss vor stärkerer Zulassung separat validiert werden.

Das Gegenbeispiel zu der bisherigen 2x-Schwelle steht in Dokument 02.
Auch eine positive Durchsatzbilanz macht Parallelität nicht automatisch
deadlineverträglich. Umgekehrt kann Parallelität trotz schlechterer A-Latenz
die Versorgung von B erst ermöglichen. Vertragsziele entscheiden.

## 9. Quantile, Risiko und Stichproben

- Ein Stichproben-p99 aus 100 Werten ist eine Ordnungsstatistik mit hoher
  Unsicherheit, kein belastbares Zertifikat für ein seltenes Ereignis.
- Worst observed ist kein Worst Case. Auch eine Sicherheitsmarge liefert
  ohne weitere Annahmen keine garantierte Verletzungswahrscheinlichkeit.
- Einzelkomponenten-p99 addieren sich nicht automatisch zu einem End-to-End-p99.
  Falls echte obere Tail-Schranken vorliegen, kann eine aufgeteilte
  Risikobudgetierung mit Union Bound verwendet werden; die Schranken müssen
  ihrerseits validiert und die Messanteile korrekt definiert sein.
- Bei zeitlicher Abhängigkeit ist die Zahl der Zyklen nicht gleich der Zahl
  unabhängiger Experimente. Wiederholte Starts, Phasen und Betriebsbedingungen
  werden getrennt ausgewertet. Kein naiver Binomialbeweis aus korrelierten Tokens.
- Kalibrierung, Policy-Auswahl und Endtest werden getrennt. Nachjustierte
  Parameter verlangen einen neuen, unbenutzten Bestätigungssatz.
- Eine Policy beobachtet überwiegend die von ihr ausgewählten Jobs. Dieses
  Selektionsproblem verbietet unkontrolliertes Online-Lernen über nie getestete
  Varianten. Exploration erfolgt außerhalb geschützter Produktivverträge.

Extremwertmodelle, Conformal Prediction und lernende Predictor sind mögliche
Experimente, keine Standardlösung. Voraussetzungen wie Stationarität oder
Exchangeability müssen belegt werden; unbekannte Zustände bleiben unbekannt.

## 10. Nachweisstufen als Produkteigenschaft

| Stufe | Zulässige Aussage | Nicht zulässige Folgerung |
|---|---|---|
| E0: synthetisch | Entscheidungslogik im Modell getestet | reale GPU-Latenz |
| E1: beobachtet | Ergebnisse auf benannter Maschine und Last | andere Hardware oder beliebige Störungen |
| E2: qualifiziertes SLO | empirische Zielerreichung in festgelegter Betriebsdomäne mit unabhängiger Prüfung | harte oder funktionale Safety-Garantie |
| E3: analytisch begrenzt | Eigenschaft folgt unter ausdrücklich geprüften Grenzen/Annahmen | Gültigkeit bei Annahmeverletzung |
| Funktionale Sicherheit | eigener Safety-Lifecycle und Systemnachweis erforderlich | durch `Protected`, Rust oder E3 automatisch vorhanden |

Admission trägt seine Nachweisstufe. Bei verlorener Profilgültigkeit wird sie
herabgesetzt und nach Policy neu zugelassen, konservativ ausgeführt oder
abgelehnt. Silent Downgrade ist verboten.

## 11. Semantische Gültigkeit ohne eigenmächtige Anforderungsänderung

Die Anwendung darf beispielsweise `valid_until`, Aktionsresthorizont oder
einen engeren Qualitätsbedarf melden. Hinweise tragen Identität, Version,
Timestamp und TTL. Der Governor prüft sie gegen freigegebene Grenzen.

Strenger werden darf ein autorisierter Vertrag kurzfristig; lockerer werden
darf er nur innerhalb vorher freigegebener Betriebsmodi. Ein vermeintlich
ruhiges Bild beweist nicht, dass ein alter Sensorwert ungefährlich ist.
Bei unbekanntem Anwendungszustand gilt der konservative Grundvertrag.

Ein Folgejob im DAG wird nur dann obsolet, wenn kein gültiger Verbraucher
mehr von ihm abhängt. Gleiche Kamera und neuere Frame-ID genügen nicht,
um Ereignisbelege oder zustandsbehaftete Verarbeitung zu löschen.

## 12. Optimierungsziel

Vorrang haben Rechte, Ressourcen- und Sicherheitsgrenzen sowie die vereinbarte
Prioritäts-/Versorgungsordnung. Innerhalb dieser Grenzen können Qualität,
Aktualität, Hintergrundfortschritt und Energie optimiert werden.

Eine skalare Nutzensumme darf diese Grenzen nicht durch viele billige
Hintergrundresultate aufwiegen. Ein erreichter Varianten-Qualitätsscore ist
zudem keine Garantie für die Korrektheit einer einzelnen Vorhersage.
Bei Widersprüchen zwischen Verträgen wird Unmachbarkeit sichtbar gemacht;
der Governor entscheidet nicht selbst, welche menschliche Anforderung entfällt.
