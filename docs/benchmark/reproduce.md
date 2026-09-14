# Auf der eigenen Maschine nachfahren

Stand: 2026-09-14 · RTX 3070 Laptop (8 GB), Treiber 580.178.04, Triton 2.70.0
(26.06-py3), ONNX-Runtime-Backend

Jede andere Zahl in diesem Verzeichnis stammt von Modellen, die niemand
ausserhalb hat: einem internen Detektionsmodell und ResNet-Stellvertretern mit
passenden Formen und Laufzeiten. Fuer die Scheduling-Aussage ist das
zulaessig — der Scheduler sieht Laufzeiten, keine Gewichte. Zum **Nachfahren**
taugt es nicht.

Dieses Dokument beschreibt den Aufbau, mit dem sich die Kernaussage mit
ausschliesslich frei lizenzierten Modellen wiederholen laesst, und nennt die
Stolpersteine, die dabei Zeit kosten.

```bash
cargo build --release --workspace
tools/repro/run.sh
```

Alles Weitere macht das Skript: Voraussetzungen pruefen, Modelle holen und
gegen feste SHA-256 pruefen, Triton auf eigenen Ports starten, **die Profile
auf dieser Maschine messen** und die Lastfaelle fahren.

**Wie lange das dauert.** Gemessen am 14.09.2026 auf der oben genannten
Maschine: die Detektor-Lastfaelle rund **15 Minuten**, davon allein acht auf
Lastfall (b), der sich auf dieser Karte nicht kalibrieren liess und acht
vergebliche Anlaeufe brauchte; mit `--with-llm` kommen gut **sieben Minuten**
dazu. Auf einer Karte, auf der (b) durchlaeuft, verschiebt sich das.

**Was beim ersten Mal dazukommt — und es dominiert.** Die Modelle sind mit
258 MB harmlos (mit Sprachmodell 1,5 GB mehr). Die Container-Images sind es
nicht: `tritonserver:26.06-py3` bringt **22,7 GB** mit, das vLLM-Image fuer
`--with-llm` weitere **35,3 GB**. Bei uns lagen beide laengst lokal, und
deshalb stand hier zuerst eine Viertelstunde. Wer frisch anfaengt, wartet auf
den Download um Groessenordnungen laenger als auf die Messung. Eine
Dauerangabe, die das verschweigt, ist von der eigenen, laengst eingerichteten
Maschine abgelesen — und faellt genau dem auf, der uns nachrechnen wollte.

## Die Modelle

Alle Apache-2.0. Die Lizenz stammt jeweils vom Basismodell: die
ONNX-Konvertierungen auf HuggingFace tragen kein eigenes Lizenzfeld, und was
dort kein Feld hat, ist nicht lizenzfrei, sondern ungeklaert.

| Modell | Basismodell | Lizenz | Groesse | Rolle |
|---|---|---|---:|---|
| `rtdetr_r18` | [PekingU/rtdetr_r18vd](https://huggingface.co/PekingU/rtdetr_r18vd) | Apache-2.0 | 83 MB | die kleine Variante |
| `rtdetr_r50` | [PekingU/rtdetr_r50vd](https://huggingface.co/PekingU/rtdetr_r50vd) | Apache-2.0 | 175 MB | die grosse Variante |
| `qwen` | [Qwen/Qwen3-0.6B](https://huggingface.co/Qwen/Qwen3-0.6B) | Apache-2.0 | 1,5 GB | das Sprachmodell, nur `--with-llm` |

Die Digests stehen in [`tools/repro/fetch-models.sh`](../../tools/repro/fetch-models.sh)
und werden bei jedem Lauf geprueft. Weicht einer ab, bricht das Skript ab,
statt weiterzumessen: eine Messung gegen unbekannte Gewichte ist keine
Messung.

**Nicht dabei, und warum.** YOLOv8 ist AGPL, und seine ONNX-Fassungen
antworten ohne Anmeldung mit HTTP 401. YOLOX liegt hinter einer
Weiterleitung, die sich schlecht gegen einen Digest pruefen laesst. Beides
waere bequemer gewesen und beides haette das Paket wertlos gemacht.

### Warum ausgerechnet zwei RT-DETR-Groessen

Die Variantenwahl setzt voraus, dass zwei Varianten **dasselbe bedeuten** —
sonst waehlt der Governor dem Client unbemerkt etwas anderes aus, als er
bestellt hat. Fuer dieses Paar ist das geprueft und nicht angenommen:

* **Gleiche I/O-Signatur** (`pixel_values` → `logits`, `pred_boxes`), gelesen
  mit [`tools/onnx-signature.py`](../../tools/onnx-signature.py).
* **Gleiche Vorverarbeitung.** Beide `preprocessor_config.json` stimmen in den
  wirksamen Feldern ueberein: `do_normalize: false`, Skalierung 1/255,
  bilinearer Resize auf 640x640, kein Padding. Bemerkenswert daran ist, dass
  RT-DETR **keine** ImageNet-Normierung verwendet, obwohl `image_mean` und
  `image_std` in der Datei stehen — sie sind dort wirkungslos. Wer sie
  anwendet, bekommt systematisch schlechtere Detektionen und sieht es an
  keiner Tensorform.
* **Gleiche Klassen in gleicher Reihenfolge**: beide `config.json` tragen
  dieselben 80 COCO-Klassen, feldweise verglichen. Die Reihenfolge *ist* die
  Bedeutung.

Genau diese Pruefung fehlt den vier RF-DETR-Varianten aus
[`rfdetr-variants.md`](rfdetr-variants.md), und dort ist das Ergebnis: sie
sind **nicht** austauschbar. Hier sind sie es.

## Drei Stolpersteine, die Zeit kosten

Beide sind beim Aufbau dieses Pakets tatsaechlich aufgetreten. Sie stehen
hier, weil ein Reproduktionspaket genau dafuer da ist.

### 1. Triton laedt gar nichts, wenn die Ausgabedimensionen fest sind

Die Messwerkzeuge lesen die Eingabeform aus den Modellmetadaten und ersetzen
darin nur die Batchdimension (`crates/vig-bench/src/bin/gate-m3.rs`). Ein
Modell mit vier dynamischen Achsen ergibt so keine Tensorgroesse. Die
naheliegende Abhilfe — in der `config.pbtxt` alle Formen festnageln — fuehrt
dazu, dass der Server **gar nicht erst startet**:

```
ERROR: Failed to create instance: model 'rtdetr_r50', tensor 'logits':
the model expects 3 dimensions (shape [-1,300,80]) but the model
configuration specifies 3 dimensions (shape [1,300,80])
...
error: creating server: Internal - failed to load all models
```

Richtig ist die Mischung: die **Eingabe** fest, damit die Werkzeuge eine
Groesse bilden koennen, die **Ausgaben** mit ihrer dynamischen Batchachse.

```protobuf
max_batch_size: 0
input  [ { name: "pixel_values" data_type: TYPE_FP32 dims: [  1, 3, 640, 640 ] } ]
output [ { name: "logits"       data_type: TYPE_FP32 dims: [ -1, 300,  80    ] },
         { name: "pred_boxes"   data_type: TYPE_FP32 dims: [ -1, 300,   4    ] } ]
```

Der Fehler ist unangenehm, weil er nicht das einzelne Modell ueberspringt,
sondern den ganzen Serverstart abbricht.

### 2. Die Klassenausgabe traegt Logits, keine Wahrscheinlichkeiten

Das steht an keiner Tensorform und faellt im Betrieb nicht auf — ein Client,
der die Werte fuer Wahrscheinlichkeiten haelt, schwellt einfach gegen die
falsche Skala und findet zu wenig. Nachgemessen am laufenden Modell, mit
einem gleichmaessig grauen Bild:

| Modell | `logits` min | max | `pred_boxes` min | max |
|---|---:|---:|---:|---:|
| `rtdetr_r18` | −9,77 | −3,60 | 0,0036 | 1,0000 |
| `rtdetr_r50` | −8,89 | −3,76 | 0,0001 | 1,0000 |

Ausserhalb von [0, 1] — also Logits vor der Sigmoid-Funktion. In der
Konfiguration steht deshalb `unit: logits`, und das ist gemessen und nicht
abgeschrieben. `pred_boxes` liegt in [0, 1], also `coordinates: normalized`.

Das **Boxlayout** (`cxcywh` gegen `xyxy`) laesst sich aus einem Wertebereich
*nicht* ableiten, und es steht in den Konfigurationen dieses Pakets deshalb
auch nicht. Nicht beschrieben ist eine ehrliche Aussage; eine geratene waere
keine.

### 3. Auf einem Laptop unter Leistungslimit qualifiziert sich kein getaktetes Profil

Der Stolperstein, der am meisten Zeit gekostet hat — und er trifft jeden, der
dieses Paket auf einem Laptop nachfaehrt.

`vig profile` und `vig calibrate` **verwerfen** eine Messreihe, wenn sich der
Hardwarezustand waehrend der Reihe aendert:

```
FEHLER rtdetr_r18: Messung verworfen: Hardwarezustand geaendert:
       GPU 0 clock_sm_mhz: ~1900 -> ~1600
WARNUNG Die Karte ist waehrend der Messung gedrosselt: [SwPowerCap].
```

Auf der Messmaschine ist das der Normalfall und nicht die Ausnahme: die Karte
regelt dauerhaft am Software-Leistungslimit und pendelt zwischen 1650 und
1920 MHz — in einem Lauf lag `SwPowerCap` in 30 von 70 Stichproben an. Die
Meldeschwelle betraegt 100 MHz.

**Kuerzere Serien helfen nicht.** Mit `--samples 100` — dem Minimum, unter dem
ein p99 kein Quantil mehr waere, sondern das Maximum — qualifizierte sich in
acht Versuchen **kein einziger** Lauf mit beiden Modellen.

**Es liegt an der Luecke zwischen den Freigaben, nicht am Modell.** Dieselben
zwei Modelle, je drei Versuche:

| Betriebsart | `rtdetr_r18` | `rtdetr_r50` |
|---|---:|---:|
| getaktet (33-ms-Raster) | 0 von 3 | 1 von 3 |
| Ruecken an Ruecken | 2 von 3 | 3 von 3 |

Auf dem Raster faellt die Karte zwischen zwei Freigaben im Takt zurueck. Das
schnellere Modell trifft es haerter, weil seine Luecke groesser ist: `rtdetr_r50`
fuellt die 33-ms-Periode fast aus und haelt die Karte damit auf Takt,
`rtdetr_r18` braucht etwa die Haelfte.

Was daraus folgt:

* **Die Schwelle wird nicht gelockert**, und das ist richtig — eine Messung,
  die ueber mehrere Betriebspunkte mittelt, beschreibt keinen davon. Das
  Werkzeug sagt das von sich aus.
* **`--no-hardware-probe` ist keine Loesung.** Es unterdrueckt das Auslesen der
  Geraeteidentitaet; man bekaeme damit keine besseren Zahlen, sondern dieselben
  ohne den Hinweis, dass die Karte geregelt hat.
* **Die Abhilfe ist ein festgehaltener Takt** — `sudo nvidia-smi -pm 1` und
  `sudo nvidia-smi -lgc <mhz>`. Das braucht Rechte und aendert den Zustand der
  Maschine; `tools/repro/run.sh` tut es deshalb **nicht** von sich aus,
  sondern sagt es dir.
* **Ruecken an Ruecken ist ein Ausweg mit Preis.** Es haelt die Karte
  beschaeftigt und qualifiziert sich meistens, ist aber eine Aussage ueber
  **Kapazitaet** und nicht ueber das Verhalten unter einem Takt. Ein so
  gemessenes Profil ist optimistisch; wer damit plant, sollte die
  Sicherheitsmarge nicht zusaetzlich senken.

### Warum die Vorlagen keine Zahlen enthalten

`vig calibrate` laesst das Profil einer Variante, deren Messreihe es verworfen
hat, **unveraendert stehen** und endet trotzdem mit Exitcode 0. Stuenden in den
Vorlagen unter `tools/repro/scenarios/` Laufzeiten, bekaeme man im Fehlerfall
eine Konfiguration, die gemessen aussieht und mit den Laufzeiten einer fremden
Maschine plant — ohne jedes Anzeichen.

Die Vorlagen tragen deshalb **kein** `profile:`. Fehlt es hinterher weiterhin,
scheitert der Start hoerbar:

```
FAIL models.detektor.variants[0].profile: kein Laufzeitprofil;
     `vig profile` ausfuehren oder profile: {p50_us, p95_us, p99_us, samples} angeben
RESULT NOT_READY
```

`run.sh` zaehlt nach der Kalibrierung nach, statt dem Exitcode zu glauben, und
bricht den Lastfall ab, wenn Profile fehlen. Ein Unittest haelt fest, dass die
Vorlagen zahlenfrei bleiben
([`repro_scenarios.rs`](../../crates/vig-config/tests/repro_scenarios.rs)).

## Die drei Lastfaelle

| # | Lastfall | Was er prueft |
|---|---|---|
| a | Ein Detektor (`protected`, 30 Hz) neben dem Sprachmodell (`best_effort`) | der Fall „humanoider Roboter": laeuft der Bericht ueberhaupt, ohne dass die Wahrnehmung dafuer zahlt ([wp26](wp26.md), ADR-0012/0014) |
| b | Vier Kameras auf einen Detektor, mit unterschiedlicher Wichtigkeit | ob die Summe der Reserven aufgeht, wenn mehrere Stroeme denselben Slot teilen (S8 aus [scenarios.md](scenarios.md)) |
| c | Zwei Groessen desselben Detektors | die Variantenwahl mit **echten** Modellen statt Laufzeit-Stellvertretern |

Lastfall (c) ist der, wegen dem dieses Paket ueberhaupt entstanden ist: die
bisherige Variantenmessung ([frontier](../../crates/vig-bench/src/bin/frontier.rs))
benutzt ResNet-50 gegen ResNet-18 als **Laufzeitpaar** — sie erzeugen eine
reproduzierbare Spreizung, sind aber auf Protokollebene nicht austauschbar
und tragen eine erklaerte, nicht gemessene Qualitaet. `rtdetr_r50` gegen
`rtdetr_r18` ist ein echtes Variantenpaar.

Alle vier Kameras in Lastfall (b) fragen **dasselbe** Backendmodell. Eine
Kamera ist kein eigenes Modell, sondern ein eigener Vertrag; der Unterschied
zwischen ihnen ist Wichtigkeit und Takt, nicht Rechenlast. Das hat eine
praktische Folge fuer die Kalibrierung, siehe Stolperstein 3.

## Ergebnisse

Gemessen am 14.09.2026 auf der oben genannten Maschine. Jede Zelle traegt
ihren Status; verschmutzte Zellen stehen mit dabei, statt still zu fehlen.

### Die gemessenen Profile

Ruecken an Ruecken, 100 Messwerte je Reihe (auf dem Vertragsraster
qualifizierte sich keine — Stolperstein 3):

| Modell | p50 | p95 | p99 |
|---|---:|---:|---:|
| `rtdetr_r50` | 26 570 us | 27 367 us | 27 577 us |
| `rtdetr_r18` | 14 194 us | 14 925 us | 15 661 us |

Die grosse Variante braucht also rund das **1,9-fache** der kleinen. Damit
liegt die geschuetzte Auslastung bei 33 ms Periode und 110 % Marge bei 91 %,
und `vig doctor` meldet `READY_WITH_WARNINGS` samt
`OK detektor: Varianten fachlich austauschbar`.

### Lastfall (c): zwei Groessen — der Governor verliert hier

**Versorgungsvergleich** (`vig-fit`, Verbrauchersicht, unabgedeckte
Abtastungen je Promille, drei Laeufe):

| Last | direkt | Governor | Status |
|---:|---:|---:|---|
| 90 % | 0 ‰ | 0 ‰ | gueltig (Lauf 1, 3) |
| 100 % | 0 ‰ | 0–3 ‰ | gueltig |
| 110 % | 0 ‰ | 3 ‰ | gueltig |
| 125 % | 2 ‰ | 2 ‰ | gueltig |

Das Urteil des Werkzeugs lautet in zwei von drei Laeufen woertlich: *„Bis
125 % Angebotslast verliert auch der direkte Weg nichts. Auf dieser Maschine,
mit diesen Modellen und Vertraegen lohnt sich der Governor nicht."*

**Lauf 2 zeigte bei 90 % Last 158 ‰ direkt gegen 0 ‰ mit Governor und eine
Luecke von 928 ms — und geht trotzdem nicht in die Aussage ein.** Die
Systemlast lag waehrenddessen bei 1,7–1,8; eine 928-ms-Luecke auf dem
direkten Arm bei *unterlasteter* GPU ist der Fingerabdruck einer fremden
Stoerung, nicht eines Schedulingerfolgs. Ihn als Sieg zu berichten waere
Auswaehlen und kein Messen.

**Direktvergleich** (`gate-m3`, 30 s je Puffertiefe, Kopierpfad):

| Lauf | Abdeckung Triton | Vigilant | Faktor | Status |
|---|---:|---:|---:|---|
| 1 | 99 % | 93 % | −21,0x | **verschmutzt** (`security-…` 64 %, `nv23_bounded-…` 100 % nebenher) |
| 2 | 100 % | 94 % | −60,0x | gueltig |
| 3 | 99 % | 89 % | −52,5x | gueltig |

Verbrauchersicht: Abdeckung 100 % gegen 99–100 %, laengste Luecke 30 ms gegen
31–37 ms, mittleres Antwortalter 42–44 ms gegen 46–47 ms.

**Der Governor ist hier also schlechter, und seine eigene Zaehlerzeile sagt
warum:**

```
angenommen 1780  weitergereicht 1780  supersediert 0  stale 0
unmachbar 0  verspaetet 0  zurueckgestellt 0  best-effort ausgehungert 0
```

Er hat nichts verworfen, nichts zurueckgehalten, nichts ersetzt — es gab
nichts zu entscheiden. Ein einzelner Strom bei 91 % Auslastung hat keine
Konkurrenz, gegen die man ihn schuetzen koennte; es bleibt sein eigener
Aufwand von rund 5 Prozentpunkten Abdeckung. Das ist kein Widerspruch zu den
Gate-M3-Zahlen dieses Repositorys, sondern dieselbe Aussage von der anderen
Seite: [`load-ramp.md`](load-ramp.md) verortet den Knick zwischen 100 und
110 %, und die Startseite empfiehlt fuer einen einzelnen Strom ausdruecklich
**keinen** Governor.

Was dieser Lastfall damit **nicht** belegt: dass die Variantenwahl etwas
bringt. Sie ist nachweislich aktiv (`doctor`, und der Funktionstest
`repro_scenario_mock.rs` nagelt sie fest), aber ohne Konkurrenz um den Slot
gibt es keinen Grund zu degradieren. Fuer eine Aussage ueber ihren Nutzen
braeuchte es den Aufbau aus [`frontier`](../../crates/vig-bench/src/bin/frontier.rs):
feste grosse gegen feste kleine gegen automatische Wahl.

### Lastfall (b): auf dieser Karte nicht messbar

Vier Kameras auf einen Detektor — **kein Ergebnis**, und zwar aus dem Grund
aus Stolperstein 3, nur verschaerft: vier Vertraege auf demselben Modell
heissen vier unabhaengige Messreihen, von denen jede einzeln verworfen werden
kann.

Acht Anlaeufe, mit 200 und mit 100 Messwerten, bestes Ergebnis **2 von 4**
Profilen. Insgesamt 29 verworfene Reihen; die Qualifikationszeilen lauten
0 von 4, 2 von 4, 1 von 4. Die Gruende verteilen sich auf

* 15x wechselnde Drosselgruende (`[SwPowerCap]` ↔ `[]`),
* 11x eine Taktstufe (`~1800` ↔ `~1900`),
* 3x beides zusammen mit `HwThermalSlowdown`.

Auf einer Karte mit festgehaltenem Takt ist dieser Lastfall messbar; hier
nicht. Die Konfiguration bleibt im Paket, weil an ihr nichts falsch ist —
es fehlt die Maschine, die ihren Takt haelt.

### Lastfall (a): Detektor neben dem Sprachmodell — der direkte Weg gewinnt doppelt

Detektor `rtdetr_r18` bei 33 ms, Sprachmodell Qwen3-0.6B ueber das
vLLM-Backend in einem eigenen Prozess, ein Slot, 30 s je Betriebsart, drei
Laeufe. Datenpfad: System Shared Memory.

Median der drei Laeufe (Einzelwerte in Klammern, wo sie streuen):

| Betriebsart | Detektor-Abdeckung | Antwortalter p95 | Generierungen | Zeichen |
|---|---:|---:|---:|---:|
| direkt zu Triton | **100 %** (97/100/100) | 17 ms | **26** | **7 878** |
| Governor ohne Zerlegung | 96 % | 14 ms | 2 | 606 |
| Governor mit Zerlegung | **33 %** (33/33/32) | 43 ms | 20 | 5 175 |

**Der direkte Weg ist in beiden Dimensionen besser.** Er haelt den Detektor
bei 100 % *und* bringt 26 Berichte durch. Der Governor ohne Zerlegung schuetzt
den Detektor praktisch gleich gut (96 %) und erdrosselt den Bericht auf ein
Zwoelftel — das ist die Aushungerung aus ADR-0012, wie erwartet. Die
Zerlegung loest sie (20 Generierungen), kostet aber zwei Drittel der
Detektorabdeckung.

Bemerkenswert dabei: **`Protected-Deadline-Misses 0` in allen drei Laeufen.**
Der Governor verletzt keine Frist — er liefert schlicht seltener ein frisches
Ergebnis. Von 1 215 angenommenen Anfragen reichte er 829–846 weiter und
stellte 6–38 zurueck. Abdeckung und Deadline-Treue sind eben zwei
verschiedene Groessen (ADR-0005).

**Warum das so ausgeht, steht in den eigenen Zahlen:** der Detektor braucht
p99 15,7 ms bei 33 ms Periode, also rund 47 % eines Slots. Das ist **keine
Ueberlast**. Triton faehrt beide Stroeme ueberlappend, und die Karte traegt
das. Der Governor tritt hier gegen einen Gegner an, der gar nicht in Not ist —
dieselbe Ursache wie in Lastfall (c), nur mit einem echten Sprachmodell
daneben.

Ein Nebenbefund, der fuer die Zerlegung wichtig ist: der **feste Sockel je
Generierungsauftrag** ist auf dieser Maschine mit diesem Modell gemessen
**20–45 ms** (Median rund 21 ms, aus der Kostenprobe am Anfang jedes Laufs).
`wp26` traegt als Vorgabe 18 000 us — fast das Tausendfache, gemessen an einem
anderen Modell auf einer anderen Belegung. Wer die Zerlegung mit einer
fremden Sockelzahl plant, schneidet die Quanten grob falsch zu.

### Was das fuer die Kernaussage heisst

**Zwei von drei Lastfaellen zeigen nicht, was man sich erhofft — und der
dritte war nicht messbar.** Ehrlich zusammengefasst:

| Lastfall | Ergebnis |
|---|---|
| (a) Detektor + Sprachmodell | direkt gewinnt doppelt: 100 % gegen 33 % Abdeckung **und** 26 gegen 20 Berichte |
| (b) vier Kameras | auf dieser Karte **nicht messbar** (29 verworfene Messreihen) |
| (c) zwei Groessen | Governor verliert: 89–94 % gegen 99–100 %, Faktor −52x bis −60x |

Das widerspricht den Gate-M3-Zahlen dieses Repositorys **nicht**, und es ist
wichtig zu verstehen warum. Gate M3 misst bei **103 %** geschuetzter
Auslastung — im Ueberlastbereich, fuer den der Governor gebaut ist. Die drei
Lastfaelle hier landen bei 47 % (a) und 91 % (c), also darunter. Und
[`load-ramp.md`](load-ramp.md) verortet den Knick zwischen 100 und 110 %:
unterhalb davon kostet der Governor nur seinen eigenen Aufwand. Die
Startseite sagt dasselbe unter „When not to use it", und `vig-fit` sagt es in
zwei von drei Laeufen woertlich: *„lohnt sich der Governor nicht"*.

**Was diese Messung also belegt:** dass die Werkzeuge mit oeffentlichen
Modellen laufen, dass sie ihre eigene Grenze zuverlaessig anzeigen, und dass
die Empfehlung „unterhalb der Saettigung keinen Governor" auch mit echten
fremden Modellen gilt. Das ist eine Bestaetigung der Ehrlichkeit des
Produkts, nicht seiner Leistung.

**Was sie nicht belegt:** die Kernaussage im Ueberlastbereich. Dafuer fehlt
ein oeffentliches Modellpaar, dessen serialisierte Auslastung auf einer
8-GB-Laptopkarte ueber 100 % kommt, ohne dass die Profilmessung am
wandernden Takt scheitert (Stolperstein 3). Mit `rtdetr_r50` als geschuetztem
Strom bei 33 ms waere man dort — genau diese Konfiguration laesst sich hier
aber nicht kalibrieren. **Das ist die offene Luecke dieses Pakets**, und sie
gehoert benannt statt umgangen.

## Was dieser Aufbau nicht beantwortet

* **Nicht, ob die Erkennung gut genug ist.** Gemessen wird die Versorgung —
  der Anteil der Perioden, in denen ein Ergebnis unter `max_age` vorlag —,
  nicht die Genauigkeit. Die deklarierten Qualitaetswerte der beiden
  Varianten sind `user_declared` und stammen aus keiner Messung auf einem
  Datensatz.
* **Nicht, was auf anderer Hardware passiert.** Eine Messung gilt fuer die
  Maschine, auf der sie lief (Spec 13.5). Genau deshalb misst
  `tools/repro/run.sh` die Profile auf **deiner** Maschine neu, statt die
  hier abgedruckten zu uebernehmen.
* **Nicht, was ueber Stunden passiert.** Dafuer gibt es den
  [Dauerlauf](soak.md).
