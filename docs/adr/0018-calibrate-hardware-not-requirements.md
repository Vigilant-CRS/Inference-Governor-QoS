# ADR-0018: Der Kalibrator misst Hardware, keine Anforderungen

**Status:** Akzeptiert · 2026-09-02
**Betrifft:** WP12 (Interferenzprofiler), ADR-0004, ADR-0006
**Auslöser:** Die Frage, ob sich das System selbst einrichten kann

## Kontext

Eine Konfiguration von OneTimer enthält zwei grundverschiedene Sorten Zahlen,
die bisher nebeneinander standen, ohne dass der Unterschied benannt war.

**Hardwaretatsachen:** wie lange eine Inferenz dauert, wie stark zwei Modelle
einander bremsen, ab wann Nebenläufigkeit nichts mehr bringt. Diese Zahlen
gelten für ein bestimmtes Gerät und sind nachmessbar.

**Anforderungen:** wie frisch ein Ergebnis sein muss, welcher Strom wichtiger
ist, welche Deadline gilt. Diese Zahlen sagen, was der Roboter braucht, und
stehen in keinem Messgerät.

Beide von Hand zu pflegen ist der Zustand vor dieser Entscheidung. Für die
erste Sorte ist das unnötige Arbeit und obendrein fehleranfällig: ADR-0006 hat
`no_corun: [detector, vlm]` als *typischen Fall* in die Spezifikation
geschrieben — eine plausible Vermutung, die niemand nachgemessen hatte.

## Entscheidung

**`onetimer calibrate` misst die erste Sorte und rührt die zweite nicht an.**

Gemessen wird je Variante die Laufzeit allein und unter Nebenlast, für jeden
Belegungsgrad bis zur konfigurierten Slotzahl. Dazu paarweise, wie stark ein
Modell ein anderes bremst. Das Ergebnis geht als vollständige Konfiguration in
eine **neue** Datei; Verträge, Klassen und Qualitäten werden unverändert
übernommen.

Damit ist WP12 in der Form eingelöst, die ADR-0006 offengelassen hatte — nicht
als vollständige Verlangsamungsmatrix, sondern als gemessene
Belegungsgradprofile plus gemessene `no_corun`-Paare.

## Warum die Verträge unangetastet bleiben

Ein System, das sich seine eigenen Deadlines ausdenkt, kann an ihnen nicht mehr
gemessen werden. Die Konfiguration wäre dann keine Zusage mehr, sondern eine
Momentaufnahme dessen, was die Hardware gerade schafft — und genau die Zusage
ist das Produkt.

Es ist dieselbe Grenze wie in [ADR-0017](0017-load-that-breaks-the-contract-is-a-finding.md):
dort meldet der Governor, dass die Last nicht zum Vertrag passt, und ändert
nichts. Hier misst der Kalibrator, was die Hardware kann, und ändert ebenfalls
nichts an dem, was sie können *soll*.

## Die Schwelle für `no_corun` ist keine Geschmacksfrage

Ein Paar wird ab einer Verlangsamung von **2,0x** eingetragen. Der Wert ist der
Punkt, an dem sich das Vorzeichen des Nutzens umdreht: bremst ein Modell ein
anderes auf die doppelte Laufzeit, brauchen zwei Aufträge nebeneinander genauso
lange wie nacheinander. Nebenläufigkeit bringt dann keinen Durchsatz mehr und
kostet nur noch Latenz.

Messung auf einer RTX 3070 mit den Modellen aus `examples/gate_m3`, zwei
Slots. Drei Läufe, damit die Streuung sichtbar bleibt statt hinter einer
einzelnen Momentaufnahme zu verschwinden:

| Paar | 30 Messungen | 120 Messungen | 120 Messungen | Folge |
|---|---:|---:|---:|---|
| Detektor neben VLM | 4,97x | 4,89x | **5,35x** | `no_corun` |
| Pose neben VLM | 3,64x | 7,22x | **5,32x** | `no_corun` |
| Tiefe neben VLM | 1,73x | 2,36x | **2,30x** | `no_corun` |
| Tiefe neben Detektor | 1,34x | 1,39x | — | erlaubt |
| Detektor neben Pose | 1,21x | 1,19x | — | erlaubt |
| Tiefe neben Pose | 1,10x | 1,20x | — | erlaubt |

Die Vermutung aus ADR-0006 bestätigt sich in jedem Lauf — und der Kalibrator
findet zwei weitere Paare, die dort niemand aufgeschrieben hatte.

### Die Messung streut, und ein Grenzfall kann kippen

Die Spalten unterscheiden sich erheblich. *Pose neben VLM* schwankt zwischen
3,64x und 7,22x; *Tiefe neben VLM* liegt einmal bei 1,73x und zweimal knapp
über 2,3x — dieses Paar **wechselt die Seite der Schwelle**.

Daraus folgen zwei Dinge, die zur Entscheidung gehören:

1. Die Mindestmessanzahl von 100 aus `RuntimeProfile` gilt hier genauso. Der
   30er-Lauf wurde von `doctor` folgerichtig als ungültig abgelehnt — die
   bestehende Schranke fängt den Fall bereits ab, ohne dass der Kalibrator eine
   eigene brauchte.
2. Ein Wert dicht an der Schwelle ist eine Aussage über die Messung, nicht über
   die Hardware. Wer eine Verlangsamung nahe 2,0x sieht, sollte den Eintrag als
   Vorschlag lesen und nicht als Befund. Weit darüber liegende Paare — hier die
   beiden mit über 5x — sind dagegen in jedem Lauf stabil.

Der Kalibrator mittelt bewusst **nicht** über Wiederholungen. Ein einzelner
Lauf mit ausgewiesenen Zahlen ist ehrlicher als ein geglätteter Wert, dem man
die Streuung nicht mehr ansieht.

## Konsequenzen

- Das Konfigurationsschema kennt jetzt `under_load`: Profile je Belegungsgrad,
  aufsteigend. Ohne die Liste bleibt es beim Alleinprofil, also beim bisherigen
  Verhalten. `VariantProfile::from_levels` existierte im Kern seit ADR-0004 und
  war bis hierher **unbenutzt** — die Konfiguration konnte nur Alleinbetrieb
  ausdrücken.
- Die Konfiguration ist jetzt auch serialisierbar. Dabei gehen Kommentare
  verloren, weil YAML über `serde` geschrieben und nicht als Text bearbeitet
  wird. Deshalb schreibt `calibrate` nie in die Vorlage, sondern immer in eine
  neue Datei: eine von Hand gepflegte Konfiguration enthält Begründungen, und
  die sind mehr wert als die Bequemlichkeit einer Ersetzung an Ort und Stelle.
- Die Messung dauert. Bei voller Messanzahl und einem langsamen Modell im Satz
  sind es Minuten — sie gehört vor die Inbetriebnahme, nicht in den Startpfad.
- Der Kalibrator misst, was er vorfindet. Läuft anderes auf der GPU, misst er
  das mit. Das ist keine Schwäche des Werkzeugs, sondern der Grund, warum die
  Messung auf einem ruhigen Gerät stattfinden muss.
