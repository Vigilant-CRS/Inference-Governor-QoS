# ADR-0017: Eine Last, die den Vertrag sprengt, ist ein Befund

**Status:** Akzeptiert · 2026-09-02
**Betrifft:** Spec 10.8 (erwartete Ankünfte), L-017 (Metriken)
**Auslöser:** Befund aus dem Dauerlauf (`docs/benchmark/soak.md`)

## Kontext

Der Dauerlauf hat eine Lücke sichtbar gemacht, die keine der kurzen Messungen
zeigen konnte: liefert eine Quelle dauerhaft schneller, als ihr Vertrag sagt,
degradiert der Governor **still**. Er verwirft mehr Frames, die Abdeckung des
geschützten Stroms fällt von 97,6 % auf 67 % — und nichts im System sagt,
warum.

Der Betreiber sieht schlechtere Zahlen und hat keinen Hinweis darauf, dass
seine Konfiguration nicht mehr zur Wirklichkeit passt. Er wird den Fehler
zuerst beim Governor suchen, denn dort sind die Zahlen schlechter geworden.

Das ist genau die Sorte stiller Fehlfunktion, gegen die dieses Projekt sonst
konsequent vorgeht: Aushungerung ist ein Befund ([ADR-0012](0012-starvation-is-a-finding.md)),
ein Profil aus fremder Umgebung ist ein Befund ([ADR-0016](0016-unverified-profiles-widen-the-margin.md)).

## Entscheidung

Der Scheduler führt je Modell den **gleitenden Mittelwert des
Ankunftsabstands** mit und vergleicht ihn mit der vertraglichen Periode.
Beobachteter und vereinbarter Wert stehen als Gauges in `/metrics`; liegt die
Rate dauerhaft mehr als 20 % über dem Vertrag, warnt der Governor im Protokoll
und nennt beide Zahlen.

**Er ändert nichts.** Weder passt er die Periode an, noch drosselt er die
Quelle, noch weitet er eine Marge.

## Warum nur melden

Der Governor kann den Vertrag einhalten **oder** die Last bedienen, nicht
beides. Welches von beidem richtig ist, weiß nur der Betreiber:

- Eine Kamera, die statt 30 mit 45 Bildern liefert, kann ein Defekt sein — dann
  ist Verwerfen genau richtig und eine automatische Anpassung würde den Fehler
  kaschieren.
- Sie kann auch eine bewusste Änderung sein, die noch nicht in der
  Konfiguration steht — dann gehört die Periode angepasst, und zwar dort, wo
  sie überprüfbar steht, nicht durch stille Selbstanpassung zur Laufzeit.

Eine Automatik müsste zwischen beidem raten. Ein Governor, der seine eigenen
Verträge umschreibt, ist außerdem in keiner Abnahme mehr nachvollziehbar: die
Konfiguration wäre dann keine Zusage mehr, sondern eine Momentaufnahme.

## Die Trägheit ist der eigentliche Entwurf

Zwei Zahlen entscheiden, ob die Meldung nützt oder nervt.

**Das Gewicht des Mittelwerts** liegt bei 1/32. Die erste Fassung nahm 1/8 —
damit schlägt der Wert schon nach fünf dichten Ankünften um, und ein
gewöhnlicher Burst wäre als Konfigurationsfehler gemeldet worden. Mit 1/32
liegt die Zeitkonstante bei rund 32 Abständen, also etwa einer Sekunde bei
30 ms Periode: träge genug für Bursts, schnell genug für eine echte
Ratenänderung. Ein Test hält fest, dass eine dauerhafte Verdopplung binnen
100 Ankünften erkannt wird — sonst wäre die Trägheit nur Blindheit.

**Die Toleranz** liegt bei 20 %. Fünf Prozent Abweichung sind Taktungenauigkeit
und Jitter; zwanzig Prozent dauerhaft sind es nicht.

Dazu eine Sperrfrist von einer Minute je Modell. Eine Warnung, die bei jedem
Tick im Protokoll steht, wird nach einer Minute weggefiltert und schützt dann
nichts mehr.

## Konsequenzen

- Zwei neue Gauges: `onetimer_arrival_period_us` und
  `onetimer_contract_period_us`. Sie stehen bewusst beide da, damit sich das
  Verhältnis in Prometheus ohne Kenntnis der Konfigurationsdatei bilden lässt.
- Vor 64 gemessenen Abständen wird nichts behauptet. Der Anlauf eines Stroms
  ist keine Aussage über seinen Dauerbetrieb.
- Ein Strom, der **langsamer** liefert als vereinbart, löst nichts aus. Er
  belastet niemanden; die ungenutzte Kapazität ist die Sache des Betreibers.
- Der heiße Pfad wird um einen Vergleich, eine Schiebeoperation und zwei
  Speicherzugriffe je Ankunft teurer.
