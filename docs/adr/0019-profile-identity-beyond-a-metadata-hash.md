# ADR-0019: Profilidentitaet ist mehr als ein Metadaten-Hash

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** ADR-0016 (unbestaetigte Profile), Spec G-010, L-014; Paket NV-03
**Ausloeser:** Der Fingerabdruck aus ADR-0016 kann den haeufigsten stillen
Fehlerfall nicht sehen — ausgetauschte Gewichte unter gleicher Versionsnummer

## Kontext

ADR-0016 hat die richtige Frage gestellt: Gilt dieses Profil hier noch? Die
Antwort war ein FNV-1a-Hash ueber das, was das Backend ueber sich meldet —
Servername, Serverversion, Modellname, Modellversionen, Plattform, I/O-Signatur.
Das faengt Serverwechsel, Backendwechsel und geaenderte Tensorformen, und es
faengt sie billig.

Vier Faelle faengt es nicht, und alle vier sind im Feld ueblich:

1. **Getauschte Gewichte.** Jemand legt eine neu trainierte Datei unter
   dieselbe Versionsnummer. Name, Form, Datentyp, Plattform: unveraendert.
   Der Hash schweigt, die Laufzeit ist eine andere.
2. **Andere Runtime unter gleicher Metadatenlage.** Ein TensorRT-Plan, neu
   gebaut mit einer anderen TensorRT-Version, meldet dieselbe Plattform.
3. **Andere Aufteilung derselben Karte.** Zwei Instanzen statt einer, MPS
   statt exklusiv, ein aktivierter Rate Limiter. Nichts davon steht in den
   Modellmetadaten.
4. **Anderes Geraet.** Der Inferenzserver kennt seine GPU nicht und meldet sie
   nicht.

In allen vier Faellen plant der Governor weiter mit Zahlen, die fuer eine
andere Umgebung gemessen wurden — und meldet dabei "Profil passt".

## Entscheidung

**Das Profil bekommt neben dem Hash ein Manifest** (`ProfileManifest`,
Revision 2) mit fuenf Bloecken: Artefakt, Runtime, Geraet, Ressourcenaufteilung,
Messbedingungen — dazu die beanspruchte Gueltigkeitsdomaene.

**Der Artefakt-Digest liest das Dateisystem.** SHA-256 ueber die Dateien in
den Versionsverzeichnissen des Modells (`<modell>/<version>/…`).

> **Korrektur vom 10.09.2026.** Zuerst hiess die Regel „alle Dateien des
> Modellverzeichnisses ausser `config.pbtxt`". Sie hielt genau so lange, bis im
> Modellverzeichnis eine `PROVENANCE.txt` lag: der Digest aenderte sich, weil
> jemand eine Notiz bearbeitet hatte. Ein Artefaktdigest, den ein Kommentar
> verschiebt, ist keiner. Digestiert wird deshalb nur, was unter den rein
> numerischen Unterverzeichnissen liegt — das ist Tritons eigene Aussage
> darueber, wo das Artefakt endet und die Verwaltung anfaengt. Gibt es keine
> Versionsverzeichnisse, gilt die alte Regel weiter. Das ist die einzige Stelle,
an der Fall 1 ueberhaupt sichtbar ist; ueber das Inferenzprotokoll ist er es
nicht. Der Pfad steht als `backend.model_repository` in der Konfiguration und
ist optional: wo der Governor das Repository nicht sieht — entfernter Server,
Container ohne gemeinsames Volume — bleibt das Feld leer.

**`config.pbtxt` gehoert nicht zum Artefakt.** Darin stehen Instanzanzahl,
Batchgrenzen und Rate-Limiter-Ressourcen. Das ist die Aufteilung des Geraets,
und die hat einen eigenen Block. Waere sie im Artefakt-Digest, waere jede
Umkonfiguration ein "anderes Modell", und die Unterscheidung, um die es hier
geht, waere wieder verloren. Geaenderte Tensorformen faengt weiterhin die
I/O-Signatur im Hash aus ADR-0016 — beide Mechanismen ergaenzen sich.

**Ein fehlendes Feld ist `unknown`, nie `verified`.** Der Gesamtbefund ist so
gut wie das schwaechste identitaetstragende Feld: ein Widerspruch macht das
Profil ungueltig, eine Luecke macht es unbelegt. Zwei Mal Schweigen ergibt
ausdruecklich **keine** Uebereinstimmung.

**Identitaetstragend sind Artefakt, Runtime, Geraet und Aufteilung — nicht die
Messbedingungen und nicht die Gueltigkeitsdomaene.** Ein mit anderer
Batchgroesse gemessener Wert ist keine Aussage ueber ein anderes Modell,
sondern ueber einen anderen Betriebspunkt. Abweichungen dort werden gemeldet,
aber sie invalidieren nichts; was daraus folgt, entscheidet die Planung
(NV-06), nicht die Identitaetspruefung.

**Was der Betreiber weiss, sagt der Betreiber.** Geraetename, Treiber,
Bibliotheksversion und Aufteilung sind ueber das Protokoll nicht erreichbar.
`vig profile` und `vig calibrate` nehmen sie als Flags entgegen und schreiben
sie mit. Nicht angegeben heisst nicht angegeben — geraten wird nichts. Die
automatische Erfassung (NV-04) wird diese Flags vorbelegen, nicht ersetzen.

## Konsequenzen

Die Folge einer erkannten Abweichung bleibt die aus ADR-0016: **weitere Marge,
kein verweigerter Start.** Ein Governor auf einem Roboter, der den Dienst
verweigert, laesst die Wahrnehmung komplett ausfallen; ein zu vorsichtig
geplantes Modell liefert nur weniger. Neu ist allein, dass mehr Abweichungen
ueberhaupt erkannt werden — und dass `doctor` das abweichende **Feld** nennt
statt nur zweier Hashes.

Solange NV-04 fehlt, sind Geraet, Treiber und Aufteilung zur Laufzeit nicht
beobachtbar. Ein vollstaendig ausgefuelltes Manifest ist dann `Unverified` und
nicht `Verified`. Das ist gewollt: es beschreibt den Wissensstand korrekt. Es
loest deshalb auch keine Margenerhoehung aus — die bleibt Widerspruechen
vorbehalten.

Alte Konfigurationen laufen unveraendert weiter. Ein Profil ohne Manifestblock
wird als Legacy-Manifest der Revision 1 gelesen, in dem jedes Feld `unknown`
ist. Ein Hash laesst sich nicht in eine Herkunftsangabe zurueckrechnen, und so
zu tun, als koenne er das, waere genau der Fehler, den dieses ADR abstellt.

## Alternativen

**Nur den Hash um den Artefakt-Digest erweitern.** Billiger, aber der Befund
bliebe "irgendetwas ist anders". Ein Betreiber, der auf einem Roboter im Feld
steht, braucht "der Treiber ist ein anderer", nicht "der Hash ist ein anderer".

**Den Digest verpflichtend machen.** Haette den haeufigen Fall des entfernten
Servers unbedienbar gemacht und Betreiber dazu gebracht, die Pruefung ganz
abzuschalten. Eine Warnung, die zu oft grundlos kommt, schuetzt am Ende nichts —
dasselbe Argument wie in ADR-0016.

**Fehlende Felder als "passt" werten.** Waere bequem und ist der Fehler, um den
es geht. Ein leeres Feld auf beiden Seiten sagt nichts ueber die Wirklichkeit,
nur etwas ueber die Sorgfalt beim Messen.
