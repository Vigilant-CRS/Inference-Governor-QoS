# ADR-0032: Beendet, frisch, angenommen, verfuegbar — vier Zusagen, die zwischen den Komponenten zerfielen

**Status:** Akzeptiert · 2026-09-10
**Betrifft:** `gateway/actor`, `gateway/budget`, `core/scheduler`,
`core/contract_ext`, `sim/coverage`, `platform/actuation`; Review vom
10.09.2026 (R01–R07), ADR-0005, ADR-0012, ADR-0018
**Ausloeser:** Ein externes Codereview mit acht lauffaehigen Gegenproben. Alle
acht liefen auf dem damaligen Stand rot

## Kontext

Die Einzelteile dieses Governors sind getestet. Was das Review gefunden hat,
liegt nicht **in** ihnen, sondern **zwischen** ihnen: vier Begriffe, die an
jeder Komponentengrenze etwas anderes bedeuteten.

**Beendet.** Der Abschlussabgleich (ADR-0018) verglich Tritons aggregierten
Statistikzaehler mit der eigenen Auslieferungsnummer. Ein aggregierter Zaehler
sagt, wie viele Inferenzen ein Modell abgeschlossen hat — nicht **welche**.
Laeuft Auftrag A noch und wird B danach fertig, steigt der Zaehler auf das
Ziel von A, und A galt als beendet, waehrend seine Recheneinheit womoeglich
noch rechnete. Die Gegenrichtung war ebenso kaputt: ein Auftrag, der das
Backend nie erreicht hat, hob das Ziel trotzdem an, und ein spaeter
tatsaechlich abgeschlossener Auftrag konnte dauerhaft in Quarantaene bleiben.

**Frisch.** ADR-0005 sagt: das Alter eines Ergebnisses zaehlt ab der
**Aufnahme**. Der Weakly-hard-Monitor im Kern zaehlte ab der
**Fertigstellung**. Aufnahme bei 0 ms, Fertigstellung bei 50 ms, Abtastung bei
70 ms, Hoechstalter 66 ms: ein Miss — gerechnet wurden 20 ms, und das Bild galt
als frisch. Der Benchmarktracker im Simulator hatte denselben Begriff noch
einmal und noch einmal anders: eine Lieferung, die schon bei ihrer Ankunft zu
alt war, schob dort die Brauchbarkeitsgrenze nach vorn und verkuerzte die
gemeldete Versorgungsluecke. 150 ms ohne ein einziges brauchbares Ergebnis
erschienen als 90 ms.

Beides betrifft genau die Zahlen, mit denen dieses Projekt nach aussen
argumentiert.

**Angenommen.** `evidence_required: proven` liess sich konfigurieren. Der
Doc-Kommentar am Feld sagte seit jeher, es existiere, „damit ein Betreiber die
Forderung aufschreiben kann und eine Ablehnung bekommt statt eines
Achselzuckens" — nur gab es die Ablehnung nie. Daneben liessen sich `phase`,
`release_jitter_envelope`, `delivery_boundary` und
`minimum_background_progress_pct` speichern, und nichts wertete sie aus. Die
mitgelieferte Golden-Konfiguration dieses Repos setzt drei davon.

**Verfuegbar.** Das Nutzlastbudget lebte in `model_infer`. Nach einem
Client-Timeout endete es — waehrend das Backend weiterrechnete und die
Nutzlast weiter hielt. Zwei 16-Byte-Auftraege liefen so bei einem Budget von
16 Bytes. Und die Aktuation (ADR-0030) bestaetigte einen beobachteten Takt von
1470 MHz als Erfuellung eines zugesagten Bodens von 1500 MHz, weil die
Rundungstoleranz 50 MHz betrug.

## Entscheidung

**Ein aggregierter Zaehler belegt Ruhe, nicht einen einzelnen Auftrag.** Das
Ziel des Abgleichs ist „Basislinie plus **alle** Auslieferungen an dieses
Modell", gerechnet zum Zeitpunkt der Pruefung. Ist es erreicht, ist von der
Arbeit dieses Governors nichts mehr offen — und dann enden **alle** gehaltenen
Anspruechen dieses Modells gemeinsam. Das ist die schwaechere Aussage, und sie
ist die einzige, die stimmt. Der Abgleichs-Task urteilt nicht mehr; er meldet
den Zaehlerstand, und der Actor entscheidet, weil nur er die Auslieferungen
kennt.

Auslieferungen, die das Backend **nie erreicht** haben, werden aus der Summe
wieder herausgenommen. Sie erzeugen nie eine Fertigstellung; sie im Ziel zu
fuehren machte das Ziel unerreichbar.

Das Generation-Fencing entfaellt damit. Es schuetzte davor, dass eine
verspaetete Meldung einen abgeloesten Kredit freigibt. Der Nachweis wird jetzt
gegen den Live-Zustand gefuehrt, und ein alter Zaehlerstand kann nur zu wenig
sein, nie zu viel.

**Ein Begriff von Frische, eine Rechnung.** Der Kern speichert die
**Aufnahmezeit** des zuletzt gelieferten gueltigen Ergebnisses und daneben,
bis wann es traegt: `Aufnahme + max_age`. Die Versorgungsluecke laeuft ab
diesem Ablauf. Der Benchmarktracker rechnet dieselbe Regel: ein Ergebnis
versorgt von seiner Auslieferung bis zum Ablauf seines Hoechstalters — und
wenn der Ablauf vor der Auslieferung liegt, war es nie brauchbar und schliesst
keine Luecke.

**Eine falsche Zusage wird abgelehnt, eine unerfuellte gemeldet.** Das sind
zwei verschiedene Faelle, und sie bekommen zwei verschiedene Antworten:

* `evidence_required: proven` verspricht analytische Absicherung. Nichts in
  diesem Projekt liefert sie. Die Konfiguration wird **abgelehnt**.
* `release_jitter_envelope` und `delivery_boundary: consumer` beschreiben
  Verfeinerungen, die dieses Produkt nicht misst. Sie machen die Datei nicht
  ungueltig — sonst liesse sich keine bestehende mehr lesen —, aber der Dienst
  nennt sie beim Start, und `vig doctor` warnt davor.
* `phase` und `minimum_background_progress_pct` werden **umgesetzt** statt
  abgelehnt. Ein Feld, das eine gemessene Zahl veraendert, gehoert nicht auf
  eine Ausnahmeliste: `phase` verschiebt das Abtastraster auf
  `phase + k * period`, und ein Mindestfortschritt ist ein
  Weakly-hard-Kriterium — „mindestens 20 % versorgt" heisst „hoechstens 80 %
  im Fenster verfehlt".

**Das Nutzlastbudget endet mit der Ausfuehrung.** Die Reservierung reist mit
dem Request in den Actor und stirbt dort, wenn der Slotkredit endet — dieselbe
Lebensdauer, dieselbe Begruendung. Sie ist jetzt ein eigener, oeffentlicher
Baustein (`gateway::budget`), weil sie zwei Schichten weit reist.

**Die Toleranz federt Rundung ab, keine Zusage.** Der **beobachtete** Takt
muss selbst innerhalb aller Grenzen liegen, nicht nur nahe am angeforderten.

**Die Tokenobergrenze wird garantiert, nicht geschaetzt.** `Bytes / 4` ist
eine Schaetzung: vier Ein-Byte-Token zaehlten als eines. Verbraucht wird jetzt
das Kleinere aus zwei **Obergrenzen** — was beim Backend bestellt war, denn
`max_tokens` setzt es in echten Token durch, und wie viele Token in diesen
Bytes ueberhaupt Platz haben, denn jedes belegt mindestens eines. Das Minimum
zweier Obergrenzen ist wieder eine.

## Konsequenzen

**Gemessene Zahlen aendern sich, und zwar nach unten.** Die laengste
Versorgungsluecke faellt jetzt anders aus: im Regressionstest 180 ms statt
185 ms, weil sie ab dem Ablauf des letzten brauchbaren Ergebnisses laeuft und
nicht ab dessen Fertigstellung. Im Benchmarktracker sind es 150 ms statt 90 ms.
Aeltere Messberichte dieses Projekts sind damit **nicht** mit neuen
vergleichbar, wo veraltete Lieferungen vorkamen. Das ist der Preis dafuer,
dass die Zahl jetzt bedeutet, was sie sagt.

**Ein zerlegter generativer Auftrag verbraucht sein Tokenbudget schneller.**
Die garantierte Obergrenze zaehlt bis zu viermal so viel wie die alte
Schaetzung. Wer dieselbe Ausgabelaenge will, muss `max_total_tokens` anheben —
und weiss dann, was er zulaesst. Vorher wusste er es nicht.

**Der Abgleich braucht Ruhe.** Ein Slotkredit in Quarantaene endet erst, wenn
alle Auslieferungen an dieses Modell abgeschlossen sind. Unter Dauerlast kann
das dauern. Das ist keine Verschlechterung, sondern die ehrliche Fassung
dessen, was vorher zu frueh behauptet wurde — und die Alternative waere ein
Nachweis je Auftrag, den Tritons Statistik nicht hergibt.

**Was der Golden-Vertrag dieses Repos fordert, bekommt er teilweise nicht.**
`release_jitter_ms` und `delivery_boundary: consumer` stehen dort, und beide
werden nicht durchgesetzt. Der `doctor` sagt das jetzt. Ein Beispiel, das mehr
verspricht als das Produkt haelt, ist selbst ein Befund.

## Alternativen

**Einen Nachweis je Auftrag vom Backend verlangen.** Triton bietet ihn nicht;
seine Statistik ist aggregiert. Ein Request-Identifikator im Abschlussereignis
waere die saubere Loesung und braucht eine Backenderweiterung. Bis dahin ist
die Ruhe-Aussage die staerkste, die zu haben ist.

**Alle unerfuellten Vertragsfelder ablehnen.** Waere die konsequenteste
Antwort und macht jede bestehende Konfiguration ungueltig, einschliesslich der
eigenen Golden-Datei. Die Zusage „eine neue Binary liest jede aeltere
Konfiguration" ist selbst eine Zusage, und sie hier zu brechen loeste ein
Problem durch ein groesseres.

**Das Nutzlastbudget beim Timeout freigeben und auf einen Speicherwaechter
setzen.** Verschiebt die Frage von „wie viel darf gleichzeitig da sein" zu
„wann greifen wir ein, wenn es zu viel ist". Ein Budget, das nicht sagt, was
es begrenzt, ist keines.

**Einen echten Tokenizer je Backend mitfuehren.** Waere die genaue Antwort auf
die Tokenfrage und bindet dieses Produkt an die Tokenisierung jedes Modells,
das ein Kunde einsetzt. Zwei Obergrenzen, die beide ohne Tokenizer auskommen,
tragen die Zusage auch.
