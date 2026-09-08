# ADR-0014: Das Quantum ist so gross, wie der Slack es zulaesst

**Status:** Akzeptiert · 2026-09-01
**Betrifft:** Spec 15.3 (kooperative Quanten), WP26; loest ADR-0012
**Grundlage:** Gate-M3-Messung vom 2026-09-01

## Kontext

Gate M3 hat auf echter Hardware bestaetigt, was ADR-0012 aus dem Simulator
vorhergesagt hatte: ein nicht unterbrechbarer 95-ms-Block laeuft neben einer
33-ms-Periode **nie**. 78 Best-Effort-Requests erreichten einen terminalen
Zustand, ohne je ausgefuehrt worden zu sein.

Damit funktioniert genau das Bild nicht, mit dem Spec 1.3 das Produkt
begruendet: Detektor und VLM auf einer GPU.

Spec 10.10 nennt den Grund und Spec 15.3 den Ausweg: generative Modelle haben
natuerliche Unterbrechungspunkte — Token-Grenzen, chunked prefill, iterative
Reasoning-Schritte. Ein 95-ms-Block ist keine physikalische Notwendigkeit,
sondern eine Folge davon, dass er als **ein** Auftrag gestellt wurde.

## Entscheidung

**1. Ein Quantum ist ein eigener Backendauftrag mit begrenzter Tokenzahl.**

OneTimer zerlegt einen generativen Auftrag in eine Folge kuerzerer Auftraege
und laesst jeden einzeln durch die Zulassung laufen. Zwischen zwei Quanten ist
der Slot frei, und geschuetzte Arbeit kommt vorbei.

Der Zustand wandert dabei im Prompt: jedes Quantum bekommt den urspruenglichen
Prompt plus das bisher Erzeugte. Das ist zustandslos und braucht keinen
Eingriff in die KV-Cache-Verwaltung des Backends. Die Kosten der wiederholten
Prefill-Berechnung traegt das Backend ueber Prefix-Caching; ohne dieses
Caching ist das Verfahren nicht wirtschaftlich, und das gehoert in die
`doctor`-Pruefung.

**2. Die Quantengroesse ist nicht konfiguriert, sondern abgeleitet.**

Eine feste Chunkgroesse waere in beide Richtungen falsch: zu klein kostet sie
dauernd Durchsatz, zu gross rettet sie die geschuetzte Arbeit nicht. Der
Scheduler kennt aber bereits die Antwort — der Look-ahead sagt ihm, wie lange
es bis zur naechsten erwarteten geschuetzten Ankunft dauert.

```text
quantum_tokens = verbleibender_slack * tokens_pro_ms
                 begrenzt auf [min_tokens, verbleibende_tokens]
```

Bei viel Reserve entsteht **ein** grosses Quantum und damit kaum Zusatzaufwand.
Wird es eng, werden die Quanten klein. Die Zerlegung passt sich der Lage an,
statt sie zu erraten.

**3. Der Client sieht davon nichts.**

Er stellt einen Auftrag und bekommt eine Antwort. Die Zerlegung ist eine
Eigenschaft der Ausfuehrung, keine der Schnittstelle. Wuerde OneTimer Quanten
nach aussen sichtbar machen, waere es kein Drop-in-Governor mehr, und Spec
L-002 waere verletzt.

**4. Ein abgebrochener Auftrag liefert kein halbes Ergebnis.**

Bricht die Kette zwischen zwei Quanten ab — Backendfehler, Ueberalterung,
Ueberlastzustand `PROTECTED_ONLY` —, endet der Auftrag mit einem expliziten
Fehler. Ein halb erzeugter Text, der wie ein vollstaendiger aussieht, waere
schlimmer als kein Ergebnis; die Anwendung kann den Unterschied sonst nicht
erkennen.

## Nachtrag 2026-09-08: die Quantenkosten sind affin, nicht proportional

Die urspruengliche Umsetzung leitete die Dauer eines Quantums allein aus seiner
Tokenzahl ab. Auf der Messmaschine kostet aber **jeder Auftrag einen festen
Sockel von 14–18 ms** — Round-Trip, Backend-Scheduling und, trotz aktivem
Prefix-Caching, die erneute Prefill-Berechnung des gewachsenen Prompts. Bei
einer 33-ms-Periode und rund 15 ms Detektorlaufzeit bleiben etwa 18 ms Slack:
der Sockel ist damit so gross wie die Luecke, in die das Quantum passen soll.

Ein rein proportionales Modell ist dort nicht ungenau, sondern strukturell
falsch. Es kann nicht ausdruecken, dass **gar kein** Quantum passt, weil es die
Kosten mit der Tokenzahl gegen null gehen laesst.

`Cooperative` traegt deshalb jetzt ein Pflichtfeld `base_cost_us`, und
`size_quantum` rechnet `Sockel + Token/Rate`. Der Wert wird gemessen, nicht
geraten — wie `tokens_per_second`. Offen bleibt, ihn in `onetimer calibrate`
zu erheben, statt ihn von Hand einzutragen.

Erst mit diesem Modell zeigt die Messung, was diese Entscheidung versprochen
hat: 40 statt 1 Generierung fuer 7 Punkte Detektor-Abdeckung
(`docs/reviews/2026-09-07/gpu/ERGEBNIS.md`). Der vorherige Messstand, der
ADR-0014 als wirkungslos auswies, beruhte auf diesem Modellfehler und darauf,
dass die Zerlegung im Actor gar nicht angeschlossen war.

## Was das nicht ist

**Keine GPU-Praeemption.** Ein laufendes Quantum wird nicht unterbrochen. Die
Blockadedauer sinkt von der Dauer des gesamten Auftrags auf die eines Quantums
— sie wird nicht null. Spec 3.5 verbietet ausdruecklich, hier mehr zu
behaupten.

**Nicht fuer beliebige Modelle.** Ein Detektor hat keine Token-Grenze; sein
Vorwaertslauf ist unteilbar. Kooperative Quanten gelten nur fuer Modelle, deren
Arbeit fachlich zerlegbar ist, und das muss in der Konfiguration stehen statt
geraten zu werden.

## Konsequenzen

- Ein Modell wird in der Konfiguration als `cooperative` markiert und nennt
  seine Erzeugungsrate. Ohne diese Angabe kann die Quantengroesse nicht
  abgeleitet werden — und ein geratener Wert waere hier besonders teuer.
- Der Durchsatz des generativen Modells sinkt gegenueber dem ungeteilten Lauf.
  Das ist der Preis dafuer, dass es ueberhaupt laeuft: heute ist er null.
- Die Aussage aus Spec 3.4 („VLM neben Detektor") wird damit auf einem Slot
  einloesbar. Bis zur Messung bleibt sie eine Konstruktion, keine Zusage.
