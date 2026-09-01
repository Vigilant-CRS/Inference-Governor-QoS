# Was kann der Governor, das Clientcode nicht auch kann?

Stand: 2026-09-01 · RTX 3070, Triton 2.70.0, RF-DETR + Pose + Tiefe

## Der Einwand

*"Wozu ein Governor? Ich verwerfe veraltete Frames einfach im Client."*

Das ist der härteste Einwand gegen dieses Produkt, und er verdient eine
Messung statt eines Arguments. Der Eigenbau ist überschaubar: nur der neueste
Frame zählt, es ist immer nur einer unterwegs, trifft ein neuer ein, während
der alte noch läuft, wird der alte fallengelassen. Fünfzig Zeilen. Genau die
LATEST-Semantik aus Spec 9.3 — nur im Client statt im Governor.

Gemessen werden drei Arme gegen dieselbe GPU, dieselben Modelle, dasselbe
Shared Memory:

| Arm | Was er tut |
|---|---|
| **naiver Client** | schickt jeden Frame, Puffer 8 |
| **Eigenbau** | Supersession im Client, einer unterwegs |
| **OneTimer** | Governor davor, Client wie im naiven Fall |

## Ergebnis

Unabgedeckte Perioden des geschützten Stroms (RF-DETR), Median aus drei
Wiederholungen, Spannweite in Klammern:

| Last | naiver Client | Eigenbau | OneTimer |
|---:|---:|---:|---:|
| 100 % | 0 ‰ [0–83] | 0 ‰ [0–0] | 8 ‰ [8–8] |
| 125 % | 275 ‰ [253–328] | 350 ‰ [208–428] | **13 ‰** [10–17] |
| 150 % | 1000 ‰ [994–1000] | 375 ‰ [358–380] | **8 ‰** [8–65] |

Age of Information p95, schlechtester Strom:

| Last | naiver Client | Eigenbau | OneTimer |
|---:|---:|---:|---:|
| 100 % | 19 ms | 19 ms | 46 ms |
| 125 % | 22 ms | 22 ms | 72 ms |
| 150 % | 145 ms | 34 ms | 15 ms |

## Der Einwand ist zur Hälfte richtig

**Supersession im Client wirkt, und zwar stark.** Bei 150 % Angebotslast fällt
der naive Client vollständig aus — 1000 ‰, kein einziges Regelfenster mit
einem frischen Ergebnis, AoI 145 ms. Der Eigenbau kommt auf 375 ‰ und 34 ms.
Das ist keine Kleinigkeit, sondern der Unterschied zwischen unbrauchbar und
schlecht.

Wer **einen einzigen Strom** hat, sollte genau das bauen und nicht uns
einsetzen. Der Selbststau ist das Problem, das ein Client allein lösen kann,
und er löst es fast vollständig.

## Und zur anderen Hälfte falsch

Bei 375 ‰ ist Schluss, und zwar aus einem strukturellen Grund: **drei Pumpen
nebeneinander wissen nichts voneinander.** Jede hält ihren eigenen Strom
frisch, aber am Server entscheidet weiter die Ankunftsreihenfolge. Der
geschützte Detektor wartet hinter Pose- und Tiefenarbeit, die niemand dringend
braucht — und kein Clientcode kann das ändern, weil kein Client weiß, was die
anderen gerade angefordert haben.

Der Governor sitzt an der einen Stelle, an der diese Information zusammenläuft.
Deshalb 8 ‰ statt 375 ‰ — **Faktor 47 gegenüber dem Eigenbau**, nicht gegenüber
dem Strohmann.

## Der Preis steht daneben

| Last | Eigenbau, schlechtester Strom | OneTimer, schlechtester Strom |
|---:|---:|---:|
| 100 % | 0 ‰ | 120 ‰ |
| 125 % | 350 ‰ | 682 ‰ |
| 150 % | 375 ‰ | 1000 ‰ |

Der Eigenbau verteilt den Mangel gleichmäßig: bei 150 % verlieren alle drei
Ströme rund 37 % ihrer Fenster. OneTimer konzentriert ihn: der Detektor
verliert 0,8 %, Pose und Tiefe praktisch alles.

Das ist dieselbe Entscheidung wie in [`load-ramp.md`](load-ramp.md), hier nur
gegen einen ernstzunehmenden Gegner statt gegen den Standardfall. **Beides
sind gültige Antworten auf Überlast** — sie unterscheiden sich darin, ob das
System weiß, welcher Strom wichtiger ist. Wer das nicht sagen kann oder will,
braucht keinen Governor.

## Wann sich das lohnt — die ehrliche Fassung

| Situation | Empfehlung |
|---|---|
| unterhalb der Sättigung | kein Governor; er kostet 0,8 % der Zyklen |
| ein Strom, oberhalb der Sättigung | Supersession im Client, fünfzig Zeilen |
| mehrere Ströme unterschiedlicher Wichtigkeit, oberhalb der Sättigung | Governor, Faktor 47 |

## Grenzen

- **Drei Ströme, eine GPU, eine Maschine.** Mit zwei Ausführungseinheiten
  entspannt sich die Konkurrenz, und die Kante verschiebt sich.
- **Spannweite bei 125 %.** Der Eigenbau streut dort von 208 bis 428 ‰; drei
  Wiederholungen sind für diesen Punkt zu wenig, um ihn vom naiven Client
  sicher zu trennen. Die Aussage bei 150 % ist dagegen stabil.
- **Der Eigenbau ist wohlwollend modelliert.** Er nutzt dasselbe Shared Memory
  und dieselbe Verbindungskonfiguration wie der Governor. Ein realer
  Schnellschuss wäre eher schlechter.

## Reproduzieren

```bash
# Umgebung: ../triton/README.md
taskset -c 8-15 target/release/diy-baseline
```
