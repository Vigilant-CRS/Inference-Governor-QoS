# ADR-0038: Die Planung kalibriert sich an der Karte

**Status:** Akzeptiert, opt-in, auf Hardware noch nicht gemessen · 2026-09-11
**Betrifft:** `core/learning` (neu), `core/estimator`, `core/scheduler`,
`core/profile` (`SafetyMargin::learned`), `backend.margin_learning`,
`vig doctor`, `vig serve`, `load-ramp`; Spec 13.2, 13.3, ADR-0016,
ADR-0023, ADR-0034
**Ausloeser:** Die Lastrampe vom 11.09. (Messkette d) und die Frage, ob sich
das System an seiner Hardware selbst einstellen kann, statt dass
Laptopwerte auf einem Pixel 2 passen muessen

## Kontext

Bei 100 % Angebotslast verliert ein getunter Triton auf der Lastrampe
nichts. Vigilant verwirft 165 ‰ eines `high`-Stroms. Ueber 100 % ist das
gewollt — der geschuetzte Detektor bleibt bei 4–22 ‰, Triton faellt auf
342–500 ‰ —, genau an der Kante ist es Arbeit, die gepasst haette.

Die naheliegende Erklaerung steht in der Konfiguration der Rampe. „100 %" ist
dort die Auslastung aus den Profilmedianen, 99,4 %. Geplant wird mit
`max(offline_p99, online_p95) × Marge`, und die Marge ist 110 %:

```text
je 46 ms:   2 × 17,47 × 1,1  +  2 × 5,51 × 1,1  +  9,53 × 1,1  =  61 ms
                 Detektor           Pose              Tiefe
```

Der Plan sieht 133 % Last, wo 99 % sind. Auch mit 100 % Marge blieben es
121 %: das p99 jedes einzelnen Auftrags ist keine Aussage ueber die Summe.
Und das Online-p95 kann den Plan nur **anheben** — ein Profil, das die
Karte ueberschaetzt, bleibt fuer immer die Untergrenze.

**Die Simulation bestaetigt diese Erklaerung nicht** (Abschnitt „Was die
Simulation zeigt"). Der Kern hat keine Zulassung nach geplanter
Auslastung; der Plan geht nur in den Look-ahead und die Variantenwahl ein,
und an der Kante vetoiert der Look-ahead nichts. Die Kalibrierung ist
trotzdem richtig — sie behebt, dass Profile Zahlen einer anderen Maschine
sind —, aber sie ist nicht die Antwort auf die 165 ‰.

Der Margenregler aus ADR-0034 wuesste es besser. Er steigt nach einer
Ueberziehung um `G·(1−t)` und sinkt sonst um `G·t`; Ueberziehung heisst im
Code `gemessene Laufzeit > Plan` (`Scheduler::on_completion`), und der Plan
ist `Profil × Marge`. Das ist ein Robbins-Monro-Schaetzer: er ruht genau
dort, wo der Anteil `t` der Ausfuehrungen ueberzieht, also beim
`(1−t)`-Quantil von `tatsaechlich / Profil`. `Profil × Marge` verfolgt das
p99 der echten Laufzeit — nur der Boden, die konfigurierte Marge, verbietet
ihm, das auch nach unten zu tun.

Dasselbe ist der Grund, warum Laptopwerte auf anderer Hardware nicht
tragen: die Profile, die Marge und die Datenpfadkosten (auf dem Pixel 2
rund 2 ms je Request, [arm-serve.md](../benchmark/arm-serve.md)) sind
Zahlen einer Maschine.

## Entscheidung

**Opt-in: `backend.margin_learning` laesst die Planung sich an der Karte
kalibrieren.** Ohne den Block ist alles bitgleich wie vorher; ein Test
faehrt dieselbe Simulation mit und ohne ausgeschalteten Lerner und
vergleicht jeden Zaehler.

1. **Der Plan ist `Profil-p99 × gelernter Faktor`.** Der Faktor bezieht
   sich auf das Profil und nur auf das Profil: er ist das gelernte Quantil
   von `tatsaechlich / Profil-p99`. Das Online-p95 als zweite Basis wuerde
   die Groesse, deren Quantil er schaetzt, mit dem Fuellstand einer Zelle
   wechseln lassen. Gemessen wird ab Dispatch bis Fertigstellung — der
   Faktor lernt die Kosten des Datenpfads also mit.
2. **Kein fester Boden.** Der Faktor darf unter 100 % fallen, wenn das
   Profil fuer diese Karte zu pessimistisch ist. Die Grenzen nach unten:
   ein **Boden aus Daten** — die Planung liegt nie unter dem beobachteten
   Median dieser Zelle — und ein harter Konfigurationsboden
   `min_factor_percent` (Voreinstellung 50 %, erlaubt 10–100 %). Nach oben
   `max_factor_percent` (Voreinstellung 1000 %), hoeher als die 300 % einer
   konfigurierten Marge: ein Laptopprofil auf einem fuenfmal langsameren
   Geraet soll konvergieren koennen.
3. **Multiplikative Schritte.** Ein Schritt ist der Pade-Bruch
   `(2 + x) / (2 − x)`, die rationale Naeherung von `e^x`, deren Kehrwert
   exakt der Schritt um `−x` ist; `x = g·(1−t)` nach oben, `g·t` nach
   unten, zusammen `g = 0,1`. Das Verhaeltnis `(1−t) : t` bleibt — es macht
   den Quantilschaetzer aus. Im Log-Bereich braucht ein Faktor 4,5 rund
   fuenfzehn Ueberziehungen, nicht vierzig. Gleitkommafrei, in Millionsteln
   (Spec 18).
4. **Hierarchisch: Geraetefaktor × Rest je Modell.** Beide Ebenen machen je
   den halben Schritt. Der Geraetefaktor lernt aus dem Verkehr aller Modelle
   einer Domaene (NV-22: eine Domaene ist eine GPU), der Rest nur aus dem
   eigenen, begrenzt auf ein Viertel bis das Vierfache. Ein neues Geraet ist
   aus dem gesamten Verkehr schnell kalibriert; ein selten laufendes Modell
   erbt den Geraetefaktor.
5. **Aufwaermen wie bisher.** Der Start ist die konfigurierte Marge, fuer
   ein nicht verifiziertes Profil die erhoehte (ADR-0016). Vorsichtiger wird
   der Faktor sofort, mutiger erst nach `min_observations` Ausfuehrungen
   (Voreinstellung 48, dieselbe Schwelle wie fuer eine mutigere Prognose,
   NV-06) — je Ebene gezaehlt.
6. **An einer Grenze kein Schritt in ihre Richtung.** Er aenderte nichts am
   Plan und saemmelte nur eine Schuld an, die spaeter jede Korrektur
   verzoegert.
7. **Die optimistische Schaetzung ist der gemessene Median**, sobald es ihn
   gibt. Ein Profilmedian darueber verwirft Frames als wertlos, die
   rechtzeitig angekommen waeren (ADR-0010).
8. **Mit `prediction: active` zusammen wird die Konfiguration abgelehnt.**
   Beide lernen den Abstand zwischen Profil und Karte; zwei Regler auf
   demselben Plan jagen einander. Der Schattenvergleich laeuft weiter.

## Was die Prognose aus NV-06 nicht schon abdeckt

Die zustandsabhaengige Prognose ersetzt das Profil durch eine Zelle je
Hardwarezustand. Sie braucht dafuer eine vollstaendige Beobachtung der
Karte (NVIDIA, `nvidia-smi`), sie wirkt nur auf die Variantenwahl und nicht
auf den Look-ahead und die Verwerfensentscheidung, und die Messung auf Gate
M3 zeigte keinen Gewinn ([nv06-ab.md](../benchmark/nv06-ab.md)). Dieser ADR
ist die hardwareunabhaengige, kleinste Form derselben Idee: ein Faktor, der
ueberall wirkt, wo geplant wird, auch auf einem Jetson oder einem Telefon
ohne `nvidia-smi`.

## Was die Simulation zeigt

Derselbe Kern, dieselbe Last wie die Rampe (Perioden, Fristen,
Hoechstalter, Profile), ein Slot, drei Seeds je Punkt; die simulierte
Karte rechnet nach dem Profil mal einem Faktor
(`crates/vig-sim/tests/margin_learning.rs`, `the_edge_matrix`). Stand mit
ADR-0036: der Look-ahead gibt eine geschuetzte Ankunft nicht mehr auf.
Unabgedeckte Perioden in Promille, Detektor / schlechtester `high`-Strom:

| Profil | Last | FIFO | 110 % fest | 100 % fest | gelernt |
|---|---:|---:|---:|---:|---:|
| richtig | 100 % | 52 / 333 | 61 / 361 | 61 / 361 | 61 / 361 |
| richtig | 110 % | 222 / 522 | 304 / 368 | 308 / 368 | 307 / 368 |
| 10 % zu langsam | 100 % | 52 / 333 | 61 / 361 | 61 / 361 | 61 / 361 |
| 10 % zu langsam | 110 % | 222 / 522 | 82 / 567 | 304 / 368 | 301 / 375 |
| 30 % zu schnell | 110 % | 222 / 522 | 245 / 362 | 246 / 362 | 308 / 368 |
| doppelt so langsam | 95 % | 0 / 103 | 0 / 1000 | 0 / 1000 | 0 / 441 |
| doppelt so langsam | 110 % | 222 / 522 | 0 / 1000 | 0 / 1000 | 77 / 770 |
| doppelt so langsam | 125 % | 272 / 973 | 14 / 1000 | 14 / 1000 | 75 / 999 |

1. **An der Kante entscheidet die Marge nichts.** Bis 105 % liefern feste
   110 %, feste 100 % und die gelernte Marge mit richtigem oder 10 % zu
   langsamem Profil dieselbe Abdeckung, Frame fuer Frame: kein Veto, kein
   verworfener Frame haengt am Plan. Die Simulation verliert bei 100 % auch
   unter FIFO, weil die Profilmediane dort 99,4 % ergeben; die echte Karte
   lief schneller, Triton verlor nichts. Was der Simulation fehlt und der
   Rampe nicht, ist der naechste Kandidat: mit `pipelining_depth: 0`
   startet der Governor den naechsten Auftrag erst nach der Antwort auf den
   vorigen, Triton direkt hat bis zu acht in der Schwebe. Die Luecke
   dazwischen kostet an der Kante Durchsatz. Die Rampe hat dafuer
   `VIG_RAMP_PIPELINING`.
2. **Gelernt verhaelt sich wie ein richtiges Profil.** Das ist, was die
   Kalibrierung verspricht. 30 % zu schnell oder 10 % zu langsam: bei 105
   und 110 % liegt gelernt innerhalb von 25 ‰ des richtigen Profils (Test
   `a_learned_plan_behaves_like_a_correct_profile`). Die feste Marge nicht:
   ein 10 % zu langsames Profil schuetzt den Detektor bei 110 % besser (82
   statt 304 ‰) und nimmt es den `high`-Stroemen (567 statt 368 ‰). Ein
   Profilfehler wird so zu einer Prioritaet, die niemand eingestellt hat.
3. **Ein pessimistisches Profil hungert nach ADR-0036 alles andere aus.**
   Doppelt so langsam: mit fester Marge verlieren die `high`-Stroeme bei
   jeder Last 1000 ‰ — laut Plan gefaehrdet jeder Auftrag den Detektor, und
   der Look-ahead gibt ihn nicht mehr auf. Vor ADR-0036 war es umgekehrt:
   der Guard hielt die Ankunft fuer unrettbar, und der Detektor verlor bei
   125 % 454 ‰. Gelernt bekommen die `high`-Stroeme Arbeit zurueck (441–770
   statt 1000 ‰), der Detektor zahlt 0–77 ‰ — weniger, als ihn ein
   richtiges Profil kostet (61–307 ‰), weil der Faktor in 20 s nicht ganz
   ankommt (Pose und Tiefe bei 53–68 % statt 50 %) und die Anfahrt
   vorsichtig ist.
4. **Warum gelernt den Detektor mehr kostet als die feste Marge.** Der Plan
   ist die Groesse, mit der der Look-ahead Arbeit neben dem Detektor
   zulaesst. `the_plan_size_trade_off` haelt ihn mit dem harten Boden
   kuenstlich ueber dem echten p99; doppelt so langsames Profil, 110 % Last,
   ein Seed:

   | Plan | Vetos | weitergereicht | Detektor | `high` | Fristverletzungen Detektor |
   |---|---:|---:|---:|---:|---:|
   | 2,0 × p99 (feste Marge) | 13 570 | 957 | 0 | 1000 | 0 |
   | 1,5 × p99 | 8 222 | 1 426 | 0 | 1000 | 0 |
   | 1,2 × p99 | 7 279 | 1 501 | 3 | 913 | 0 |
   | 1,1 × p99 | 6 846 | 1 538 | 39 | 825 | 0 |
   | 1,0 × p99 (gelernt) | 6 691 | 1 537 | 77 | 772 | 0 |
   | richtiges Profil, 110 % | 10 | 2 046 | 303 | 357 | 0 |

   Monoton: kleinerer Plan, weniger Vetos, mehr andere Arbeit, mehr Verlust
   beim Detektor. **Kein Fehler.** Der Look-ahead rechnet mit dem gelernten
   Quantil — Faktor 50 % ist genau das p99 der Karte —, nicht mit dem
   Median; der Median ist nur der Boden. **Und es ist nicht die Streuung,
   die den Detektor trifft: er verfehlt in keinem Lauf eine einzige Frist.**
   Er verliert Abdeckung, weil die Rampe `D = 1,5 T` und `A = 2 T`
   vereinbart. Nach ADR-0036 ist die laengste Luecke hoechstens
   `T + D + 4J − A`, hier rund `T/2`: wer jede Frist knapp haelt, laesst
   Luecken, und ein genauer Plan nutzt die Frist bis an ihren Rand. Wer den
   Detektor lueckenlos will, vereinbart `A ≥ T + D + 4J` — das ist Vertrag,
   nicht Marge.
5. **Konvergenz wie berechnet.** Doppelt so langsames Profil: Faktor 50 %,
   Geraetefaktor rund 61 %. 30 % zu schnelles Profil: 140–157 %.

**Wofuer taugt die Kalibrierung nach ADR-0036 also?** Sie macht das
Ergebnis unabhaengig vom Fehler des Profils. Ein zu pessimistisches Profil
war eine versteckte Prioritaet: der geschuetzte Strom bekam mehr Schutz, als
sein Vertrag verlangt, auf Kosten der anderen bis zum Aushungern; ein zu
optimistisches nahm ihm welchen. Die Kalibrierung gibt die Entscheidung dem
Vertrag zurueck. Sie schuetzt den Detektor nicht staerker als ein richtiges
Profil — wer mehr Schutz will, stellt ihn im Vertrag ein (`max_age`,
Frist, Klasse), nicht ueber ein falsches Profil. Und sie loest die Kante bei
100 % nicht.

**Die Frontier bei 90 %** (Messung vom 11.09., Nachlauf e: `gross` und
`auto` verfehlen 105–170 ‰, bei 100 % nur 1–2 ‰, `auto` bleibt ganz auf der
grossen Variante) erklaert die Kalibrierung ebenfalls nicht. Die Karte lief
dort am Leistungslimit, SM-Median rund 1770 MHz mit Einbruechen auf
1560 MHz, das Profil stammt aus einer Kalibrierung bei rund 1890 MHz. Die
grosse Variante braucht dann im Median etwa 13,6 ms, in den Einbruechen um
15 ms, bei 14 ms Periode. Gelernt stiege der Faktor ueber 100 % — der Plan
laege bei rund 16 ms und bliebe unter der Frist von 21 ms. Die Variantenwahl
prueft die Frist, nicht die Rate; eine Variante, die in die Frist passt und
nicht in die Periode, bleibt gewaehlt. Dafuer braucht die Wahl ein
Ratenkriterium (geplante Laufzeit gegen Periode). Erst dann zaehlt, dass der
Plan das echte p99 ist — und das liefert die Kalibrierung.

## Konsequenzen

- **Der Plan wird ein gemessenes p99 dieser Karte**, nicht mehr eines
  Profils von einer anderen. Das kann in beide Richtungen gehen; nach unten
  langsam, nach oben schnell.
- **Nach unten braucht er Hunderte Ausfuehrungen**, nach oben Dutzende. Bei
  einem Prozent Ziel wiegt eine Ueberziehung 99 eingehaltene Plaene auf; das
  ist die Definition, keine Traegheit.
- **Der Faktor springt nach jeder Ueberziehung um rund zehn Prozent** und
  sinkt dann langsam. Mit vielen Modellen auf einer Karte streut der
  Geraetefaktor mehr, weil jede Ueberziehung jedes Modells ihn anhebt.
- **Die Belegung bleibt Sache des Profils.** Ein Faktor je Modell mischt
  Belegungsgrade und Varianten; bei einem Slot ist das dasselbe, bei
  mehreren tragen die Profile je Belegungsgrad die Unterschiede (ADR-0006).
- **Beobachtbar:** `vig_margin_percent` je Modell zeigt den gelernten
  Faktor, neu `vig_learned_device_factor_percent` den Geraetefaktor. `vig
  doctor` nennt den Modus als Warnung, solange er nicht gemessen ist; `vig
  serve` meldet ihn beim Start.
- **Der gelernte Faktor ueberlebt keinen Neustart.** Ihn zu speichern ist
  der naechste Schritt; dann gehoert er an die Profilidentitaet (NV-03) und
  den Fingerabdruck der Karte, sonst erbte eine andere Karte ihn.

## Wie es gemessen wird

```bash
# Die Kante, mit und ohne, jeweils drei Wiederholungen je Punkt
VIG_RAMP_POINTS=90,95,100,105,110,125 target/release/load-ramp
VIG_RAMP_POINTS=90,95,100,105,110,125 VIG_RAMP_MARGIN_LEARNING=an target/release/load-ramp

# Absichtlich falsche Profile: die Konvergenz auf echter Hardware
VIG_RAMP_POINTS=90,100,110 VIG_RAMP_MARGIN_LEARNING=an VIG_RAMP_PROFILE_SCALE=2 target/release/load-ramp
VIG_RAMP_POINTS=90,100,110 VIG_RAMP_MARGIN_LEARNING=an VIG_RAMP_PROFILE_SCALE=0.7 target/release/load-ramp

# Der wahrscheinlichere Grund fuer die Kante: die Dispatchluecke
VIG_RAMP_POINTS=90,95,100,105,110 VIG_RAMP_PIPELINING=1 target/release/load-ramp
```

Abnahme der Kalibrierung: mit falschem Profil dieselbe Abdeckung wie mit
richtigem, sobald der Faktor angekommen ist. Dass der Detektor dabei mehr
verlieren kann als mit einem pessimistischen Profil und fester Marge, ist
nach Abschnitt 4 zu erwarten und kein Befund gegen sie. Dass sie die Kante bei 100 % loest,
ist nach der Simulation nicht zu erwarten; das soll die Rampe mit
Pipelining zeigen.

## Verworfen

**Das Online-p95 ersetzt das Offline-p99** („das Profil ist ein
Startwert"). Das war der erste Entwurf. Er hat einen festen Faktor
zwischen p95 und Plan und damit dasselbe Problem eine Ebene tiefer; und die
Basis wechselt mit dem Fuellstand der Zelle, sodass kein Regler auf ihr
konvergiert.

**Nur den Boden auf 100 % senken.** Hilft gegen die 10 % Marge, nicht gegen
ein pessimistisches Profil, und nicht auf anderer Hardware.

**`f64` und `exp()`.** Nicht bitgenau ueber Plattformen (Spec 18); der
Pade-Bruch ist exakt umkehrbar und reicht fuer Schritte von hoechstens fuenf
Prozent.

**Ein Faktor je Kamera.** Aus demselben Grund wie in ADR-0034: die Laufzeit
haengt am Modell, nicht am Sensor.
