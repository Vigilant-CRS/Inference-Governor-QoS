# NV-06: scharf gegen Schatten

Datum: 11.09.2026. Maschine: RTX 3070 Laptop (8 GB), Treiber 580.178.04,
Triton 2.70.0, ONNX-Modelle, System Shared Memory. Last: die
Gate-M3-Konfiguration (`examples/gate_m3/vig.yaml`), einmal mit
`backend.prediction: shadow`, einmal mit `active`, abwechselnd je drei
Läufe. Rohdaten und Taktmitschnitt (alle 5 s):
`InferenceQoS-runtime/messungen/measure-2026-09-11/`.

## Die Frage

Die Abnahme von NV-06 verlangt, dass die zustandsabhängige Prognose den
bisherigen Weg „nicht nur durch mehr Ablehnung" schlägt. Seit `c760186` ist
sie ein Konfigurationsschritt. Was ändert dieser Schritt auf einer realen
Last?

## Teil 1: scharf ohne Marge — die Zusage bricht

Stand `97f13d7`. Abdeckung der Lieferfenster über Vigilant, dazu die
längste Versorgungslücke aus Verbrauchersicht und der Taktmedian im Lauf:

| Lauf | Detektor | Pose | Tiefe | längste Lücke D / P | VLM | verspätet | Takt |
|---|---:|---:|---:|---|---:|---:|---:|
| Schatten 1 | 99 % | 99 % | 98 % | 15 / 19 ms | 0 % | 0 | 1890 MHz |
| Schatten 2 | 99 % | 99 % | 99 % | 14 / 17 ms | 0 % | 0 | 1875 MHz |
| Schatten 3 | 99 % | 99 % | 99 % | 15 / 19 ms | 0 % | 0 | 1875 MHz |
| scharf 1 | 97 % | 96 % | 94 % | 34 / 67 ms | 21 % | 36 | 1710 MHz |
| scharf 2 | **90 %** | **90 %** | 100 % | **99 / 98 ms** | 65 % | 15 | 1680 MHz |
| scharf 3 | 99 % | 99 % | 100 % | 19 / 5 ms | 0 % | 0 | 1875 MHz |

Triton direkt lag in denselben Läufen bei 82–85 % (Detektor) und 90–92 %
(Pose).

**Zur Sauberkeit.** Um 11:17:54 startete auf derselben Maschine eine
Simulation, die einen Kern ohne Kernbindung belegte. Schatten 3 lief zur
Hälfte, scharf 3 ganz in dieser Zeit. Der Befund stützt sich auf die Läufe
1 und 2 beider Seiten, die vollständig davor lagen — und genau die beiden
scharfen Läufe dort zeigen den Bruch. Alle späteren Blöcke des ersten
Anlaufs wurden verworfen und neu gemessen.

**In zwei von drei Läufen startet der scharfe Modus das VLM, und die
geschützten Ströme bezahlen dafür.** Der Detektor fällt bis auf 90 % und ist
damit kaum noch besser als ein getunter Triton; die Pose liegt darunter. Die
längste Detektorlücke wächst von 15 auf 99 ms. Im dritten Lauf startet das
VLM nie, und alles bleibt wie im Schatten.

Ein Schalter, der je nach Lauf die zentrale Zusage hält oder bricht, ist
nicht einschaltbar. Die Voreinstellung `shadow` war richtig.

**Der Takt ist dabei kein Störfaktor, sondern Teil der Wirkung.** Die zwei
Läufe mit tieferem Taktmedian sind genau die, in denen das VLM lief. Jeder
Lauf hat rund sechs Proben unter 1700 MHz aus der Tritonhälfte, in der das
VLM immer rechnet; die beiden scharfen Läufe mit VLM haben doppelt so viele.
Mehr zugelassene Arbeit lässt das Leistungslimit (`SwPowerCap`) härter
greifen, der Takt sinkt, und die geschützten Laufzeiten steigen — genau in
dem Moment, in dem die Planung am wenigsten Reserve hat.

### Die Ursache

Der bisherige Weg plant mit `max(offline_p99, online_p95) × Marge`, auf
dieser Konfiguration 110 %. Der scharfe Pfad in `variant.rs` nahm das **p95
der Zelle ohne Marge**. Wer mit einem p95 plant, überzieht per Definition bei
jedem zwanzigsten Lauf seinen Plan — und die Marge, die genau das abfangen
soll, fehlte ganz.

Der Schattenvergleich hatte es angekündigt, nur war es nicht zu lesen. In
allen sechs Läufen stand dort „vorsichtiger 0, mutiger ~3000": die Zelle
ohne Marge gegen das Profil mit Marge. Ein Teil dieser „Mutigkeit" war kein
Wissen über die Karte, sondern die fehlende Marge.

### Die Korrektur

`Prediction::with_margin`: die Zelle ersetzt das **Profil**, nicht die
Marge. Scharfer Pfad und Schattenvergleich rechnen seitdem mit derselben
Marge wie der bisherige Weg. Der Test
`an_active_predictor_keeps_the_operators_margin` fährt eine Zelle von 10 ms
gegen 200 % Marge und eine Deadline von 15 ms: mit Marge läuft die kleine
Variante, ohne liefe die große.

## Teil 2: scharf mit Marge — sicher, und auf dieser Last ohne Gewinn

Binaries vom Stand `68f8181` (Margenkorrektur `cebb582` und der Margenregler
mit Ziel aus ADR-0034), gemessen am 11.09. zwischen 13:19 und 13:31. Beim
Start liefen neben Triton nur Browser und Editoren; die Simulation einer
anderen Sitzung war beendet, deren spätere Builds begannen erst nach diesem
Block.

| Lauf | Detektor | Pose | Tiefe | längste Lücke D / P | VLM | verspätet |
|---|---:|---:|---:|---|---:|---:|
| Schatten 1 | 99 % | 99 % | 99 % | 15 / 18 ms | 0 % | 0 |
| Schatten 2 | 99 % | 99 % | 99 % | 21 / 25 ms | 0 % | 0 |
| Schatten 3 | 99 % | 99 % | 97 % | 24 / 8 ms | 3 % | 6 |
| scharf 1 | 99 % | 99 % | 99 % | 15 / 19 ms | 0 % | 0 |
| scharf 2 | 99 % | 99 % | 100 % | 22 / 25 ms | 1 % | 1 |
| scharf 3 | 99 % | 99 % | 100 % | 18 / 5 ms | 1 % | 1 |

**Die Korrektur wirkt.** Mit derselben Marge wie der bisherige Weg bricht der
scharfe Modus keine Zusage mehr: Detektor und Pose bei 99 % in allen sechs
Läufen, die längsten Lücken in derselben Spannweite wie im Schatten,
höchstens ein verspäteter Auftrag. Ohne Marge waren es am Vormittag 90 %
und 99 ms.

**Einen Gewinn bringt er auf dieser Last nicht.** Gate M3 hat je Modell nur
eine Variante; es gibt keine Qualität, die ein zu pessimistisches Profil
verschenken könnte, und das VLM bleibt bei einem 90-ms-Block unter Last
blockiert (ADR-0012). Der Schattenvergleich zeigt weiter fast nur
„mutiger" (rund 3000 gegen 0): die Karte läuft schneller, als das Profil
annimmt, und die Zelle weiß das — hier nützt es nur niemandem.

**Was daraus folgt.** Die Voreinstellung bleibt `shadow`. `active` ist seit
`cebb582` sicher einschaltbar; ob es nützt, muss eine Last mit Varianten
zeigen, auf der ein veraltetes Profil tatsächlich Qualität kostet — das ist
die Frontier-Messung (Spec 19.7), nicht Gate M3. Die Abnahmefrage von NV-06
— „schlägt Legacy nicht nur durch mehr Ablehnung" — ist damit auf dieser
Last so beantwortet: es schlägt Legacy nicht, und es lehnt auch nicht mehr
ab.
