# ADR-0025: Gleiche Form ist nicht gleiche Bedeutung

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** `core/semantics`, `core/model`, `config/schema`, `cli/doctor`;
Paket NV-10
**Ausloeser:** Die Signaturpruefung faengt den Fall, in dem ein
Variantenwechsel den Client garantiert bricht — und genau nicht den
schlimmeren

## Kontext

Der Governor waehlt die Variante je Request und sagt es dem Client nicht.
Diese Freiheit setzt voraus, dass alle Varianten dasselbe **bedeuten**. Bisher
wurde das an der I/O-Signatur geprueft: Namen, Datentypen, Formen. Der eigene
Doc-Kommentar sagte dazu schon, dass gleiche Signatur „die notwendige, nicht
die hinreichende Bedingung" sei.

Der ungefaehrliche Fall ist der, den die Signatur faengt: verschiedene Form,
der Client bricht sofort, jemand sieht es. Der gefaehrliche sieht so aus:

Zwei Detektoren, beide `[1, 300, 6]` in `FP32`, beide mit Ausgabe `boxes`. Der
eine liefert `xyxy` in Pixeln und COCO-Labels in Standardreihenfolge, der
andere `cxcywh` normiert und dieselben Labels in anderer Reihenfolge. Die
Signatur ist identisch. Der Client bekommt Zahlen, die aussehen wie erwartet
und etwas anderes heissen — und das faellt erst auf, wenn ein Roboter danach
greift.

Der Messaufbau dieses Projekts hat den Fall selbst: die RF-DETR-Varianten
unterscheiden sich in Eingabeaufloesung und Klassenzahl,
`detector_large`/`detector_small` im Ausgabenamen. Deshalb konnte hier nie
Variantenwahl gezeigt werden.

## Entscheidung

**Eine fachliche Beschreibung je Variante:** Ausgabeart, Labelmenge **samt
Reihenfolge**, Koordinatenkonvention, Einheit, Layout — dazu ein
Eingabevertrag mit Layout, Farbraum, Normierung und Aufloesung.

**Die Labelreihenfolge geht in den Fingerabdruck ein.** Dieselben Klassen
anders sortiert sind dieselben Zahlen mit anderem Inhalt. FNV-1a mit
Feldtrennern, von Hand: damit das Ergebnis nicht von der Rust-Version abhaengt
und `["ab","c"]` nicht wie `["a","bc"]` hasht.

**Der Eingabevertrag gehoert dazu.** Eine Variante, die 640 statt 512 Pixel
braucht, ist kein Ersatz, sondern eine andere Vorverarbeitung. Und sie kostet
Zeit: `preprocess_us` geht in die Planung ein, weil Resize und Boxdekodierung
**ausserhalb** des Backends entstehen und aus jedem Backendprofil
herausfallen. Zwei Varianten mit verschiedener Aufloesung unterscheiden sich
hier oft mehr als in der Inferenz selbst.

**Schweigen ist kein Beleg fuer Gleichheit.** Zwei Varianten ohne
Semantikangabe sind nicht deshalb austauschbar, weil beide schweigen. Ist gar
keine beschrieben, gilt der Zustand vor NV-10 — die Signatur entscheidet, und
`doctor` sagt, dass sie allein entscheidet. Ist **eine** beschrieben, muessen
es alle sein: eine halb beschriebene Variantenreihe ist gefaehrlicher als eine
gar nicht beschriebene, weil sie nach Sorgfalt aussieht.

**Ein Tippfehler wird abgelehnt, nicht als „nicht angegeben" gelesen.** Nicht
angegeben schaltet die automatische Wahl ab; ein Tippfehler wuerde sie
stillschweigend auf eine falsche Bedeutung stellen.

**Ausdrueckliche Undurchsichtigkeit ist erlaubt.** `kind: opaque` heisst:
hier hat jemand hingesehen und entschieden, dass die Bedeutung ausserhalb des
Governors liegt. Zwei solche Ausgaben gelten als gleich, wenn ihr Layout
uebereinstimmt. Das ist etwas anderes als „nicht beschrieben", und die
Unterscheidung ist der Punkt.

**Geprueft werden nur freigegebene Varianten.** Eine gesperrte Variante mit
abweichender Bedeutung ist kein Grund, die automatische Wahl abzuschalten —
sie laeuft ohnehin nie (NV-02).

**Aus einem Score folgt keine Freigabe.** Dieses Modul vergleicht Bedeutungen.
Ob eine Variante fachlich zugelassen ist, sagt die Freigabeliste, nicht ihre
Genauigkeit auf einem Datensatz, den jemand einmal gemessen hat.

## Konsequenzen

`doctor` nennt jetzt das widersprechende **Feld** und die beiden Varianten:
„Ausgabe 0: verschiedene Labels oder verschiedene Labelreihenfolge — gleiche
Form, andere Bedeutung". Und wo keine Semantik hinterlegt ist, sagt er, dass
Austauschbarkeit allein an der Signatur haengt.

Der Rueckfall ist die freigegebene feste Variante — nicht die naechstbeste und
nicht die schnellste. Ein Governor, der bei unklarer Bedeutung „irgendeine"
nimmt, hat das Problem nicht geloest, sondern verschoben.

Der Kern haelt keine Zeichenketten: Layout, Farbraum und Normierung werden zu
64-Bit-Kennzahlen. `xyxy` und `cxcywh` bleiben unterscheidbar, ohne dass der
Scheduler Text vergleicht.

## Alternativen

**Die Semantik aus den Backendmetadaten ableiten.** Triton meldet Namen,
Datentyp und Form. Labelreihenfolge und Koordinatenkonvention meldet es nicht,
und es kann sie nicht melden — sie stehen im Modell, nicht in seiner
Signatur.

**Nur die Labelanzahl vergleichen.** Haette den Aufloesungsfall gefangen und
den Reihenfolgefall genau nicht — also den, um den es geht.

**Fehlende Beschreibung als „austauschbar" lesen.** Haette die Umstellung
reibungslos gemacht und den bestehenden Zustand als geprueft ausgegeben.
