# Dauerlauf — funktioniert es noch nach acht Stunden?

Stand: 2026-09-02 · RTX 3070, Triton 2.70.0 · 8 h, 480 Fenster à 60 s

## Warum

Alle anderen Messungen dieses Projekts sind zwölf bis dreißig Sekunden lang.
Sie beantworten, **ob** der Governor funktioniert, und schweigen zu der Frage,
die für ein Gerät auf einem Roboter zählt: ob er es nach acht Stunden noch tut.

Drei Dinge kann ein kurzer Lauf grundsätzlich nicht sehen:

- **Ein Speicherleck.** Wenige Kilobyte je Sekunde liegen in fünfzehn Sekunden
  unter jeder Nachweisgrenze und legen ein Gerät nach zwei Tagen lahm.
- **Margendrift.** Der Estimator zieht die Sicherheitsmarge nach einer
  Unterprognose schnell hoch und nur langsam wieder herunter (ADR-0013). Ob
  sie sich einpendelt oder monoton steigt, ist in einer Minute nicht zu
  unterscheiden.
- **Lastspitzen.** Die [Rampe](load-ramp.md) fährt stationäre Punkte. Der
  realistische Fall ist Grundlast mit Spitzen.

## Aufbau

Zyklus aus fünf Fenstern Grundlast (90 %) und einem Fenster Spitze (150 %),
je 60 Sekunden. Ein Gateway für den gesamten Lauf — genau das ist der Punkt.
Nach jedem Fenster werden Ströme, kompletter Metrikabzug, Speicherverbrauch
und Systemlast fortgeschrieben.

**Der Governor ist durchgehend für 90 % konfiguriert.** Die Spitze trifft ihn
unangekündigt, so wie eine Kamera ihre Bildrate nicht mit dem Scheduler
aushandelt. Das unterscheidet diesen Lauf von der Rampe, wo zu jedem Lastpunkt
die passenden Verträge galten.

## Stabilität: bestanden

| | erste Stunde | letzte Stunde |
|---|---:|---:|
| Detektor unabgedeckt | 7 ‰ | 7 ‰ |
| Pose unabgedeckt | 8 ‰ | 7 ‰ |
| Tiefe unabgedeckt | 57 ‰ | 44 ‰ |
| AoI p95 (Detektor) | 25 ms | 25 ms |
| abgewiesen (Detektor) | 83 | 0 |

**Keine Drift.** Der geschützte Strom liefert in der achten Stunde exakt so
zuverlässig wie in der ersten, bei identischer Aktualität. Die schwächeren
Ströme werden im Verlauf sogar leicht besser — der Estimator lernt die realen
Laufzeiten und plant weniger vorsichtig.

## Über den ganzen Lauf

| | |
|---|---:|
| angenommene Requests | 3 190 798 |
| weitergereicht | 2 945 822 |
| durch jüngere ersetzt | 244 975 |
| Backendfehler | **0** |
| Deadline-Misses | **19** (0,0006 %) |
| verspätet gestartet | 113 |
| als unmachbar abgelehnt | 0 |
| wegen Kapazität abgelehnt | 0 |
| Best-Effort ausgehungert | 0 |
| stale verworfen | 1 |
| Nutzquote | 1,000 → 0,999 |

Dreieinhalb Millionen Requests, kein einziger Backendfehler, neunzehn
verpasste Deadlines. Die 244 975 Ersetzungen sind kein Verlust, sondern die
Arbeitsweise: das sind die Frames, die beim Start schon überholt gewesen
wären.

## Speicher: kein Leck

```text
11 448 kB  ->  14 112 kB   (+2 664 kB in 8 h)
nach der ersten Stunde:  +109 kB/h
```

Der Zuwachs steckt fast vollständig in der ersten Stunde — Allokator-Arenen,
Verbindungspuffer, Profile, die der Estimator anlegt. Danach 109 kB je Stunde,
also unter einem Megabyte über den Rest des Laufs. Für ein Gerät, das Wochen
läuft, ist das unbedenklich.

## Die Marge hat gearbeitet und ist zurückgekommen

| Modell | Start → Ende | Höchstwert | erhöhte Ablesungen |
|---|---:|---:|---:|
| Detektor | 110 % → 110 % | **124 %** | 4 von 480 |
| Pose | 110 % → 110 % | 120 % | 1 von 480 |
| Tiefe | 110 % → 110 % | 120 % | 3 von 480 |

Das ist das befriedigendste Einzelergebnis des Laufs. Der Regler hat viermal
angezogen, weil die Prognose danebenlag, und ist jedes Mal wieder auf den
konfigurierten Wert zurückgefallen. Genau so ist ADR-0013 gedacht: schnell
nach oben, langsam nach unten, und kein dauerhaftes Verharren im
Vorsichtsmodus.

Die erste Fassung der Auswertung hat das übersehen, weil sie nur Anfang und
Ende verglich — beide 110 %. Ein Regler, der ausschlägt und zurückkommt, sieht
an den Endpunkten aus wie einer, der nie etwas getan hat.

## Der Befund: Spitzen sind teurer als Dauerüberlast

Nur die Spitzenfenster, über alle acht Stunden:

| Strom | unabgedeckt | AoI p95 |
|---|---:|---:|
| Detektor | **327 ‰** | 30 ms |
| Pose | 683 ‰ | 30 ms |
| Tiefe | 946 ‰ | 60 ms |

Zum Vergleich: in der stationären Rampe liegt der Detektor bei 150 % Last bei
**24 ‰**. Hier sind es 327 ‰ — der dreizehnfache Wert.

Zwei Gründe, und sie sind unterschiedlich wichtig:

1. **Der Vertrag passt nicht zur Last.** Der Governor plant durchgehend gegen
   die 90-%-Konfiguration, während die Messung während der Spitze die
   Frische-Anforderung der 150-%-Konfiguration anlegt. Das ist der realistische
   Fall — Verträge stehen fest, die Last schwankt — aber es ist nicht dasselbe
   Experiment wie in der Rampe.
2. **Der Übergang kostet.** Beim Sprung von 90 auf 150 % laufen Queues voll,
   die für die Grundlast bemessen waren.

Wichtig für die Einordnung: dabei entstehen **keine** verpassten Deadlines und
keine Ablehnungen wegen Unmachbarkeit. Der Governor verwirft Frames, die er
nicht mehr frisch bedienen kann — er versagt nicht, er verzichtet. Das ist die
konstruierte Antwort auf Überlast und kein Defekt.

**Trotzdem ist die Zahl die ehrlichere.** Wer aus der Rampe abliest, das
System halte den Detektor bei 150 % Last auf 97,6 %, wird bei burstiger Last
enttäuscht. Die belastbare Aussage lautet: **im eingeschwungenen Zustand
97,6 %, bei unangekündigten Spitzen gegen einen für die Grundlast
konfigurierten Governor 67 %.**

## Was offen bleibt

- ~~**Der Vertrag kennt die Last nicht.**~~ **Geschlossen.** Der Governor führt
  jetzt den beobachteten Ankunftsabstand je Modell mit, stellt ihn neben der
  vertraglichen Periode in `/metrics` und warnt, wenn die Rate dauerhaft mehr
  als 20 % darüber liegt ([ADR-0017](../adr/0017-load-that-breaks-the-contract-is-a-finding.md)).
  Er ändert dabei nichts: welche der beiden Zahlen falsch ist, weiß nur der
  Betreiber.
- **Eine Nacht, eine Maschine.** Acht Stunden sind kein Wochenlauf, und 109 kB
  je Stunde könnten über sieben Tage ein anderes Bild ergeben.
- **Kein Backendausfall im Lauf.** Die Wiederanlauffähigkeit ist implementiert
  und getestet, aber in diesen acht Stunden ist Triton nicht einmal gestolpert.
  Sie ist damit nicht im Feld erprobt.

## Reproduzieren

```bash
SOAK_HOURS=8 SOAK_OUT=<verzeichnis> taskset -c 8-15 target/release/soak
python3 tools/soak-report.py <verzeichnis>
```

Das Ausgabeverzeichnis gehört auf ein **fest eingebautes** Laufwerk. Ein per
USB angebundener Datenträger, in den acht Stunden lang jede Minute ein paar
Zeilen geschrieben werden, ist genau der Kandidat für eine
Energiesparabschaltung — der erste Lauf überlebte das nur, weil die Platte
sich erst nach dem Ende aushängte.

## Zweiter Lauf, 11./12.09.2026: der Stand mit den Review-Fixes

Acht Stunden, 479 Fenster, 19:37 bis 03:37, auf dem eingefrorenen Binary von
`64c9d06` (Hash und Treiber im Manifest des Laufs). Der Stand enthält den
Security-Review, den NV-17-Fix, NV-22, `TCP_NODELAY` in den Werkzeugen und
die Korrektur der Bereitschaftsprüfung.

**Dieser Lauf ist ein Stabilitätslauf, keine Latenzqualifikation.** Parallel
liefen die Builds der laufenden Korrekturen auf den Kernen 0–7, der Lauf
selbst auf 8–15; der Wächter markierte 38 seiner rund 480 Proben mit
Fremdlast. Die Abdeckungszahlen unten sind deshalb Hinweise. Was er zeigen
soll — Speicher, Fehler, Drift — hängt daran nicht.

| | erste Stunde | letzte Stunde |
|---|---:|---:|
| Detektor, Verbrauchersicht unabgedeckt | 0,1 ‰ | 0,1 ‰ |
| Pose, Verbrauchersicht | 59,9 ‰ | 34,4 ‰ |
| Tiefe, Verbrauchersicht | 158,8 ‰ | 155,0 ‰ |
| längste Detektorlücke | 39 ms | 21 ms |
| längste Poselücke | 1083 ms | 107 ms |
| längste Tiefenlücke | 29,3 s | 5,5 s |

- **Kein Speicherwachstum.** RSS zwischen 10,9 und 15,3 MB, am Ende 11,9 MB —
  niedriger als am Anfang (13,4 MB). Über acht Stunden ist kein Trend zu
  sehen. „Kein Leck" folgt daraus weiterhin nicht.
- **Keine Drift.** Die Werte der letzten Stunde sind nicht schlechter als die
  der ersten; Pose und die längsten Lücken werden besser, was zu den
  eingeschwungenen Margen passt.
- **Der Preis steht daneben.** Von 3 190 800 angebotenen Aufträgen wurden
  2 962 026 geliefert und 228 589 abgelehnt (7,2 %) — fast alle in den
  Spitzenfenstern, und fast alle aus dem Tiefenstrom, der in der ersten
  Stunde bis zu 29 Sekunden ohne frisches Ergebnis blieb. Der Governor hält
  den Detektor und bezahlt mit dem Strom, der es am ehesten verträgt.
- **Kein Fehler, kein Absturz, kein Backendausfall** in acht Stunden.

Rohdaten: `InferenceQoS-runtime/soak-2026-09-11/` (Fensterprotokoll,
Metrikabzug, Taktmitschnitt, Wächterprotokoll, Manifest).
