# ADR-0026: Interferenz ist gerichtet und nicht additiv

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** `core/interference`, `cli/calibrate`; Paket NV-11, ADR-0006
**Ausloeser:** `no_corun` ist symmetrisch, binaer und aus einer Heuristik
gewonnen, die als allgemeine Durchsatzaussage formuliert war

## Kontext

ADR-0006 hat die Interferenzmatrix bewusst verschoben und durch zwei
Naeherungen ersetzt: den Slot-Belegungsgrad als Ersatz fuer „wer laeuft
gerade daneben", und `no_corun` als binaere Verbotsliste. Das war die richtige
Reihenfolge — messen, was billig zu messen ist, und den Rest ehrlich als
Naeherung ausweisen.

Drei Dinge daran sind zu grob geworden:

1. **Interferenz ist nicht symmetrisch.** Ein 95-ms-VLM verlaengert einen
   5-ms-Detektor um ein Vielfaches seiner eigenen Laufzeit; der Detektor
   verlaengert den VLM um wenige Prozent. Der Kalibrator hat bis hierher nur
   **eine** Richtung gemessen (`skip(i+1)`) und das Ergebnis symmetrisch
   angewandt.
2. **Ein Verhaeltnis ist keine Kosten.** „Faktor 2" heisst bei 5 ms etwas
   anderes als bei 95 ms. Geplant wird mit absoluten Zeiten.
3. **Die 2x-Regel war als Wahrheit formuliert.** Der eigene Doc-Kommentar
   sagte, ab dem Doppelten bringe Nebenlaeufigkeit „keinen Durchsatz mehr" —
   das gilt fuer zwei Auftraege aehnlicher Laenge und nicht allgemein.

## Entscheidung

**Eine gerichtete Tabelle mit absoluten Zusatzkosten.**
`record_pair(victim, co_tenant, added)` sagt nichts ueber die Umkehrung; beide
Richtungen sind eigene Messungen, und die Tabelle zaehlt sie getrennt.

**Nichts wird hochgerechnet.** Fuer einen Nachbarn gilt der Paarwert. Fuer
zwei oder mehr gilt nur eine **ausdruecklich gemessene** Kombination; gibt es
keine, ist die Antwort `NotExtrapolated` und nicht eine Summe. Zwei Nachbarn
koennen weniger kosten als die Summe — wenn sie sich gegenseitig verdraengen —
und mehr, wenn sie zusammen eine Grenze reissen, die keiner allein erreicht.
Ein addierter Laufzeitbound sieht aus wie eine Zahl und ist eine Erfindung.

**Nicht gemessen ist nicht kostenlos.** `added()` gibt `None` und nicht null.
Wer daraus null macht, plant eine Messung ein, die es nicht gibt. Der Aufrufer
entscheidet konservativ — im Zweifel serialisieren, wie bisher.

**Ein Modell stoert sich nicht selbst.** Zwei Instanzen desselben Modells sind
eine Aussage ueber den Belegungsgrad und nicht ueber Interferenz.

**Die Konfliktart wird mitgefuehrt.** Compute, Speicherbandbreite,
Speicherkapazitaet, Phase. Fuer die Planung heute eine Zahl; fuer den
Betreiber der Unterschied zwischen „mehr Slots helfen" und „mehr Slots helfen
nicht".

**Die 2x-Regel ist eine Heuristik mit einer benannten Annahme.** Sie stimmt
fuer zwei Auftraege aehnlicher Laenge. Bei sehr unterschiedlichen Laufzeiten
kann Nebenlaeufigkeit den Durchsatz erhoehen, obwohl der kurze Auftrag stark
leidet — ob das gut ist, entscheidet der Vertrag und nicht diese Zahl. Der
Kalibrator **schlaegt** deshalb `no_corun` vor, nennt beide Richtungen samt
absoluter Zusatzzeit, und weist ausdruecklich auf Paare hin, deren Richtungen
um mehr als Faktor zwei auseinanderliegen: dort legt eine symmetrische Regel
auf eine unsymmetrische Wirklichkeit, und das ist eine Entscheidung, keine
Ableitung.

## Konsequenzen

Der Kalibrator misst jetzt doppelt so viele Paarungen. Bei vier Modellen sind
das zwoelf statt sechs Messreihen — Zeit, die sich lohnt, weil die Haelfte
davon vorher geraten war.

`no_corun` bleibt symmetrisch: der Slot verbietet das gleichzeitige Laufen,
nicht eine Richtung. Ein Paar wird vorgeschlagen, wenn **eine** Richtung die
Heuristik reisst; die Ausgabe nennt, welche.

Die Kombinationsliste ist auf 64 Eintraege begrenzt. Vollstaendige Abdeckung
ist bei acht Modellen jenseits jedes Messbudgets; wer mehr wissen will, misst
gezielt (Spec L-003).

Die Tabelle ist noch **nicht** an die Zulassung angeschlossen. Sie zu fuellen
braucht eine Messkampagne, und die braucht die GPU. Solange bleibt der
Belegungsgrad die Naeherung, die er laut ADR-0006 immer war — nur ist jetzt
die Struktur da, in die die Messung hineingeht, und sie kann nicht mehr
stillschweigend addieren.

## Alternativen

**Die symmetrische Liste behalten und nur besser messen.** Haette die
Asymmetrie weiter unter einem Mittelwert versteckt.

**Paardaten additiv fortschreiben.** Haette immer eine Zahl geliefert und in
genau den Faellen falsche, in denen es darauf ankommt — bei drei
gleichzeitigen Modellen unter Last.

**Die 2x-Regel abschaffen.** Sie ist als Vorschlag brauchbar; falsch war nur,
sie als allgemeine Wahrheit aufzuschreiben.

## Nachtrag, 10.09.2026: angeschlossen

Bis hierher war dieses ADR eine Beschreibung ohne Wirkung. `vig calibrate`
mass beide Richtungen, berichtete sie — und warf sie weg. In der
Konfiguration landete nur `no_corun`, und das sagt „gar nicht zusammen", nie
„so viel kostet es".

Die Tabelle steht jetzt als `backend.interference` in der Konfiguration und
wird beim Start in den Scheduler gegeben. Bei jeder Planung wird nachgesehen,
welche Modelle gerade laufen; ist die Paarung gemessen, kommt ihr Aufschlag
auf die Prognose.

**Nur wo gemessen.** Eine unbekannte Paarung bekommt keinen erfundenen
Aufschlag. Der Slot-Belegungsgrad bleibt die Naeherung, die er laut ADR-0006
immer war, und eine zweite unbelegte Zahl daneben waere schlimmer als keine.

**Nur wo es etwas aendert.** Ein Paar, das ohnehin ueber `no_corun`
serialisiert wird, bekommt keinen Eintrag: die beiden laufen nie gleichzeitig.

**Die Ursache bleibt offen.** `ConflictKind` wird als `Unspecified`
geschrieben. Die Messung sagt, **wie viel** dazukommt, nicht **warum** —
Rechenwerke, Bandbreite, Kapazitaet oder ueberlappende Phasen zu unterscheiden
braucht mehr als eine Laufzeitdifferenz. Fuer die Planung ist es
Dokumentation, fuer den Betreiber der Unterschied zwischen „mehr Slots helfen"
und „mehr Slots helfen nicht".
