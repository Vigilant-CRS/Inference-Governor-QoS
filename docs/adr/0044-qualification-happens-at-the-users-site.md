# ADR-0044: Die Qualifikation findet beim Anwender statt

**Status:** Akzeptiert · 2026-09-14
**Betrifft:** `cli/autotune` (neu), `cli/init`, `cli/calibrate`, `cli/doctor`,
`vig-bench/vig-fit`; ADR-0004 (Slots), ADR-0019 (Profilmanifest), ADR-0026
(gerichtete Interferenz), ADR-0035 (`source: declared`), ADR-0039 (zweites
Backend)
**Ausloeser:** Jede veroeffentlichte Zahl dieses Projekts stammt von einer
einzigen Maschine ([support-matrix](../support-matrix.md)). Auf der
Telefon-GPU kehrt sich das Vorzeichen um
([android-gpu.md](../benchmark/android-gpu.md)).

## Kontext

Ein Interessent stellt genau eine Frage: **Traegt meine Hardware meine Last,
und bringt der Governor mir dabei etwas?** Wir koennen sie nicht beantworten.
Unsere Zahlen kommen von einem RTX-3070-Laptop mit Triton 2.70 und einem
Modellsatz, den wir ausgesucht haben. Auf einem Pixel 2 vor einem
TFLite-Backend ist der Befund ein anderer — dort ueberlappt das Backend
Transport- und CPU-Anteile, die wir serialisieren, und ein Slot beschreibt es
falsch.

Die ehrliche Antwort auf eine Frage, die wir nicht beantworten koennen, ist
nicht eine vorsichtigere Zahl. Sie ist ein Werkzeug, das die Frage **dort**
beantwortet, wo sie gestellt wird. Die Bausteine dafuer gab es einzeln:
`vig init`, `vig calibrate`, `vig-fit`, `vig doctor`. Was fehlte, war die
Verbindung — und die Regeln, nach der sie ein Ergebnis zusammenfasst.

Denn genau daran entscheidet sich alles: Ein Werkzeug, das eine Qualifikation
*erteilen* kann, wird benutzt, um sie zu erteilen. Aus einer Messung unter
Fremdlast wird dann ein Datenblatt, und aus einer verworfenen Reihe eine
geschaetzte Zahl, die in der Konfigurationsdatei aussieht wie eine gemessene.

## Entscheidung

**`vig autotune` misst beim Anwender und erteilt nie eine Qualifikation.**
Es kann sie nur **verweigern** (etwas hielt nicht) oder **offenlassen**
(nichts sprach dagegen). Ein positives Ergebnis heisst „nichts sprach
dagegen", nicht „freigegeben", und der Bericht sagt diesen Unterschied.

Daraus folgen vier Regeln, die der Code durchhaelt:

1. **Eine verworfene Messreihe bleibt verworfen.** Die Groesse, die sie
   ergeben haette, wird **nicht gesetzt**. Der Bericht nennt sie, den Grund
   (meist ein wandernder Takt) und die Abhilfe. Die Schwelle wird nicht
   gelockert.
2. **Kein geratener Wert sieht aus wie ein gemessener.** Was nicht messbar
   war, bleibt leer oder traegt sichtbar `source: declared` (ADR-0035).
   Slots kommen aus der *gemessenen* Nebenlaeufigkeit des Backends, nicht aus
   einer Annahme (ADR-0004).
3. **Fremdlast entwertet die Zelle.** Lief waehrend eines Messschritts
   anderes auf der Maschine, wird er als `contaminated` gefuehrt, und ein
   Bericht aus verschmutzten Zellen ist keine Qualifikation.
4. **Ein Ergebnis gegen uns ist ein normales Ergebnis.** Sagt `vig-fit`, dass
   der Governor auf dieser Last nichts bringt, ist genau das die Schlagzeile
   des Berichts — als Feststellung, nicht als Fussnote.

**Was `autotune` nicht misst, sind Vertraege.** Periode, Frist und
Hoechstalter sind Zusagen des Betreibers an seine Anwendung. Kein Messlauf
kann sie herausfinden. Stehen sie nicht da, bricht `autotune` nach Schritt 1
ab und sagt, welche Felder fehlen — statt plausible einzusetzen.

**`vig-cli` haengt nicht von `vig-bench` ab.** `vig-fit` gehoert zum
Messkasten, nicht zum Produkt. `autotune` sucht es neben dem eigenen Binary
oder unter `VIG_FIT_BIN`; fehlt es, bleibt die Frage „lohnt es sich?"
**offen** und der Bericht nennt den Befehl. Eine Abhaengigkeit vom Messkasten
waere der bequemere Weg und wuerde die Grenze aufloesen, die ADR-0033 zieht.

**Keine Plattformannahme.** Derselbe `vig` laeuft statisch auf `aarch64` vor
einem TFLite-Backend. Dort gibt es kein `nvidia-smi` und deshalb keinen
Hardwarezustand. `autotune` scheitert daran nicht und misst auch nicht still
etwas anderes: Der Bericht fuehrt „Takt nicht beobachtbar" als eigenen
Abschnitt und sagt, dass die Reihen nicht gegen einen wandernden Takt
abgesichert werden konnten.

## Konsequenzen

- **Die Qualifikation verschiebt sich zum Anwender**, und damit auch die
  Beweislast. Wir behaupten nichts ueber fremde Hardware; wir liefern das
  Werkzeug, das dort misst, und ein eingefrorenes Ergebnis mit Manifest
  (ADR-0019): Hardware, Treiber, Digests, Zeitpunkt, Identitaet des
  Governors.
- **Der Ausgabewert trennt Fehler von Ergebnis.** „Der Governor bringt hier
  nichts" ist Exitcode 0. Nur ein Schritt, der nicht durchlief, und eine
  Messung, die keine einzige Reihe behalten hat, sind ein Fehlschlag. Ein
  Urteil gegen uns darf keine CI rot faerben.
- **Der Lauf ist wiederaufnehmbar**, und jeder Schritt schreibt sofort auf
  die Platte. Eine halbe Stunde Messung, die ein Abbruch vollstaendig
  vernichtet, wird kein zweites Mal gestartet.
- **Die Dauer ist eine Zusage.** Der Befehl schaetzt vorab und nennt, wenn
  der volle Umfang laenger dauert als die zugesagte halbe Stunde, den
  kuerzeren Umfang (`--quick`, `--only`).

  Die erste Fassung dieser Zusage pruefte sich selbst nicht: Die Schaetzung
  bestand aus vier festen Konstanten, deren Summe (1065 s) die Schwelle
  (1800 s) nie erreichen konnte — der Hinweis auf `--quick` war toter Code,
  und der Unittest verglich eine Konstantensumme mit einer Konstanten, konnte
  also auch dann nicht fehlschlagen, wenn ein echter Lauf Stunden gebraucht
  haette. Seitdem haengt die Schaetzung an der tatsaechlichen Matrix
  (Modelle, Slots, Proben; die Paarmessung waechst quadratisch), und ein Test
  belegt, dass die Warnung bei grosser Matrix **ausloest**.

  **Was die Schaetzung nicht kann:** die Laufzeit der Modelle vorhersagen,
  die sie noch nicht gemessen hat. Der Faktor stammt von der
  Referenzmaschine; auf dem Pixel 2 dauert ein Detektoraufruf zehnmal so
  lange. Die Ansage sagt das jetzt dazu, statt eine Genauigkeit zu
  behaupten, die vor der ersten Messung niemand haben kann.
- **Eine Fortsetzung vervollstaendigt den Bericht, statt ihn zu kuerzen.**
  Der Zustand traegt den ganzen bisherigen Stand — Schritte, verworfene
  Reihen, Urteile, den Pfad der eingefrorenen Konfiguration — und nicht nur
  die Namen der erledigten Schritte. Vorher begann ein fortsetzender Lauf mit
  einer leeren Qualifikation und ueberschrieb die Berichte damit: Nach einem
  Abbruch hinter `measure` nannte der fertige Bericht weder die verworfenen
  Reihen noch die entstandene Konfiguration, obwohl beides auf der Platte lag.
  Ein Test faehrt diesen Fall ueber zwei Aufrufe.

  Dazu traegt der Zustand einen Fingerabdruck aus Endpunkt und
  Konfigurations-Hash. Wechselt einer von beiden zwischen zwei Aufrufen, gilt
  der alte Stand nicht mehr — sonst liefen `fit` und `check` gegen eine
  Messung von einer anderen Maschine, ohne ein Wort darueber.
- **Die Validierung gegen die eigenen Aufbauten hat drei Luecken gezeigt**
  ([validierung-autotune.md](../benchmark/validierung-autotune.md)), die
  offen bleiben:
  - **Die Fremdlasterkennung ist zu grob.** Sie vergleicht `/proc/loadavg`
    gegen eine feste Schwelle. Auf dem Telefon laufen Backend und Messklient
    auf demselben Geraet, die gemeldete Last ist die Arbeit der Messung
    selbst — der Lauf galt als verschmutzt, obwohl nichts Fremdes lief. Auf
    einem Laptop unter der ueblichen Messdisziplin (gebundene Kerne, ruhige
    Maschine) trifft sie richtig. Eine belastbare Erkennung muesste die
    gebundenen Kerne betrachten statt der Gesamtlast, oder die Beobachtung
    als Angabe fuehren, ohne daraus ein Urteil abzuleiten.
  - **Die Belegungsstufe misst auf Backends mit einem Thread je Modell die
    falsche Groesse.** Sie belegt den zweiten Slot mit demselben Modell; dort
    wartet der Auftrag im Modellthread, statt nebenher zu laufen. Das ist
    eine Warteschlange, keine Nebenlaeufigkeit. `autotune` uebernimmt die
    Zahl kommentarlos.
  - **Die eingefrorene Konfiguration unterscheidet nicht zwischen gemessen
    und uebernommen.** Wird eine Reihe verworfen, bleibt der vorhandene Wert
    stehen — richtig so, denn geschaetzt wird nichts. Aber er steht danach
    mit demselben `samples:` und derselben `source:` da wie ein frisch
    gemessener. Der Bericht sagt, wie viele Reihen verwertbar waren; die
    Datei, die in Betrieb geht, sagt es nicht.

    **Das gilt weiterhin fuer die Soloprofile.** Fuer die
    **Interferenztabelle** war es schlimmer und ist behoben: Dort loeschte
    `apply()` die Tabelle bedingungslos und schrieb nur die Paare zurueck,
    die dieser Lauf messen konnte. Ein frueher gemessener Aufschlag
    verschwand damit ersatzlos, und ein verworfenes Paar war in der Datei
    nicht mehr von einem Paar zu unterscheiden, das gemessen und fuer
    unkritisch befunden wurde. Entfernt wird jetzt nur, was dieser Lauf neu
    setzt, was er serialisiert und was er messen wollte und verwerfen musste;
    die verworfenen Paare nennt der Lauf ausdruecklich. Nebenbei fiel dabei
    auf, dass ein Lauf mit `slots: 1` — der gar keine Paare misst — die
    Tabelle trotzdem leerte.
- **Was offen bleibt:** `autotune` misst Versorgung, nicht Erkennungsguete;
  es sagt nichts ueber andere Hardware als die, auf der es lief, und nichts
  ueber Stunden — dafuer gibt es den Dauerlauf. Ob die Belegungsstufe des
  Kalibrators fuer Backends mit einem Thread je Modell die richtige Groesse
  misst, ist ein offener Befund aus
  [android-gpu.md](../benchmark/android-gpu.md) und bleibt es.
