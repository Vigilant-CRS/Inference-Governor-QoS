# ADR-0037: Eine Domaene ist eine GPU mit genau einem Besitzer

**Status:** Akzeptiert · 2026-09-11
**Betrifft:** `config/schema`, `core/metrics`, `gateway/actor`,
`gateway/exporter`, `gateway/service`, `cli/doctor`, `cli/serve`; Paket
NV-22, ADR-0004, ADR-0020, ADR-0024, ADR-0028, ADR-0035
**Ausloeser:** Die Slotmenge modelliert eine Ausfuehrungseinheit. Wer zwei
GPUs hat, muss heute zwei Governor betreiben — oder eine davon verschweigen.

## Kontext

ADR-0004 sagt, was ein Slot ist: eine Recheneinheit, deren Kredit nur durch
Nachweis zurueckkommt. Die Slotmenge ist die Kapazitaet **einer** GPU. Zwei
Tritonserver auf derselben GPU sind deshalb kein zweiter Slot, und
`backend_endpoint` an einem Modell aendert nichts an der Kapazitaetsrechnung
(„die Slots modellieren die GPU, nicht den Prozess").

Eine Anlage mit zwei GPUs konnte der Governor bisher nur falsch beschreiben:
entweder als eine GPU mit zwei Slots — dann haelt er den Detektor auf GPU 0
und das VLM auf GPU 1 fuer austauschbar und plant die geschuetzte Arbeit mit
einer Kapazitaet, die fuer sie nicht existiert —, oder gar nicht.

Die Roadmap beschreibt die Domaene (Zielarchitektur, Abschnitt 3): der
Bereich gemeinsam verwalteter Ausfuehrungskapazitaet, zunaechst eine
physische GPU, mit **genau einem** logischen Kapazitaetsbesitzer. NV-22
verlangt Routing, unabhaengige Budgets und eine festgelegte Failover-Policy,
und als Abnahme: kein Doppelbesitz, Kopierzeit nicht unterschlagen, ein
Geraeteausfall veraendert nicht die Kredite anderer Domaenen, zustandsbehaftete
Auftraege wechseln nicht ohne gueltigen Zustandstransfer.

Diese Maschine hat eine GPU. Was hier entsteht, ist erreichbar und mit
Fake-Executoren geprueft, nicht qualifiziert.

## Entscheidung

1. **Eine Domaene ist eine physische GPU mit einem Kapazitaetsbesitzer.**
   Genau ein Scheduler-Actor je Domaene, mit eigener Slotmenge, eigenen
   Krediten und eigener Quarantaene. `backend.domains.<name>` nennt
   `gpu_index`, `grpc_endpoint`, `slots`, `pipelining_depth` und, wo
   gebraucht, `no_corun`, `preemptible_lanes` und `interference`. Der
   bestehende `backend`-Block ist die Domaene `default` auf GPU 0 und nimmt
   jedes Modell ohne `domain:`.
2. **Die Zuordnung ist fest und steht in der Konfiguration:**
   `models.<name>.domain`. Es gibt kein Verschieben eines Auftrags oder eines
   Stroms zwischen Domaenen zur Laufzeit — die feste Geraetezuordnung, die
   die Roadmap als Rueckfall nennt, ist hier die ganze Policy (siehe
   „Warum kein Failover").
3. **Kein Doppelbesitz, von der Konfiguration abgelehnt:** dieselbe GPU in
   zwei Domaenen; derselbe Endpunkt fuer Modelle zweier Domaenen; eine
   Domaene ohne Modell; ein Modell, das eine unbekannte Domaene nennt;
   `no_corun` oder eine Interferenzzeile zwischen Modellen verschiedener
   Domaenen (sie laufen ohnehin gleichzeitig, und eine gerichtete
   Interferenz ueber Geraetegrenzen hat niemand gemessen).
4. **Unabhaengig je Domaene:** Slots und Kredite, Quarantaene nach Timeout,
   Margenregler, Look-ahead (eine erwartete geschuetzte Ankunft auf GPU 0
   haelt keine Arbeit auf GPU 1 zurueck), Missbudget, Prognosezellen und
   beobachteter Hardwarezustand (je `gpu_index`), Interferenztabelle,
   Spuren, Erreichbarkeitsprobe, Abgleich und der Abhaengigkeitsgraph.
5. **Geteilt:** das Nutzlastbudget (`max_inflight_mib`) — es begrenzt den
   Hauptspeicher **eines** Governorprozesses, und der ist einer —,
   Zugangspruefung, Vertrauensmodus, Hinweispolicy, Prognosemodus und
   Timeouts. Die Hardwarebeobachtung bleibt ein einziger Waechter mit einer
   Abfrage je Takt; jede Domaene bekommt den Zustand ihrer GPU.
6. **Ein Abhaengigkeitsgraph je Domaene.** Eine Zusammenfuehrung muss in der
   Domaene ihrer Eltern laufen; `vig_depends_on` ueber Domaenengrenzen wird
   mit `unknown_parent` beantwortet. Das Ergebnis liegt auf einer anderen
   GPU, und eine Zusammenfuehrung darueber hat Kopierkosten, die der
   Governor nicht kennt — sie zu erlauben hiesse, sie zu unterschlagen.
7. **Kennzahlen:** die bisherigen Reihen bleiben unveraendert und
   domaenenuebergreifend (Summen; je Modell der Wert seiner Domaene). Dazu
   `vig_domain_*{domain="..."}` je Domaene. Bereit ist der Governor, wenn
   **jede** Domaene bereit ist; der Grund nennt die Domaene. Auftraege an
   gesunde Domaenen laufen unabhaengig davon weiter.

**Ohne `backend.domains` aendert sich nichts:** ein Actor, dieselbe
Aufloesung, dieselben Kennzahlen, bitgleiche Entscheidungen. Ein aelterer
Governor lehnt eine Konfiguration mit `domains:` ab (`deny_unknown_fields`) —
die sichere Richtung: eine Datei fuer zwei GPUs laeuft nicht still auf einer.
Die Schemaversion bleibt 1; die Ergaenzung ist additiv im Sinn von ADR-0020.

## Warum kein Failover

Faellt eine GPU aus, bekommt ihre Domaene Timeouts, dann Quarantaene, dann
sofortige Absagen — die bestehende Kette aus NV-00 und R11, je Domaene. Die
anderen Domaenen merken davon nichts. Ein automatisches Umlenken auf eine
andere GPU gibt es nicht:

- **Zustandsbehaftete Auftraege** (`stateful`, Sequenzen, zerlegte
  generative Auftraege mit Kontext) haben ihren Zustand im Backend der
  ausgefallenen GPU. Ohne Zustandstransfer waere ein Wechsel ein
  Neubeginn, der sich als Fortsetzung ausgibt.
- **Die Zielkapazitaet ist verplant.** Die geschuetzten Vertraege der
  anderen Domaene sind gegen deren Slots gerechnet. Ein umgelenkter Strom
  nimmt ihnen die Reserve — genau das, was „ein Ausfall veraendert nicht die
  Kredite anderer Domaenen" ausschliesst.
- **Die Kosten sind unbekannt.** Modell laden, Engine bauen, Tensoren an
  ein anderes Geraet kopieren: nichts davon ist gemessen. Eine
  Failover-Policy auf geschaetzten Zahlen ist das Risiko, das die Roadmap
  nennt — bessere Rechenzeit, schlechtere Gesamtlieferzeit.

Wer Redundanz braucht, konfiguriert sie ausdruecklich: dasselbe Modell unter
zwei logischen Namen in zwei Domaenen, und der Client waehlt.

## Was diese Entscheidung annimmt

**Dass zwei Domaenen einander nicht bremsen.** Das ist fuer die GPU selbst
richtig und fuer alles darum herum eine Annahme: PCIe-Bandbreite,
Hauptspeicherbandbreite, die CPU des Governors und der Clients, ein
gemeinsames Leistungs- und Waermebudget im selben Gehaeuse — auf einem
Laptop mit `SwPowerCap` ausdruecklich nicht. Die Interferenztabelle kennt
bewusst keine Zeilen ueber Domaenengrenzen; ob sie welche braucht, zeigt
erst eine Messung auf zwei GPUs.

## Konsequenzen

- Eine Anlage mit zwei GPUs ist mit **einem** Governor beschreibbar: ein
  Endpunkt fuer die Clients, eine Zugangspruefung, ein Nutzlastbudget, eine
  Metrikseite, ein `vig doctor`. Gegenueber zwei Governorprozessen ist das
  ein betrieblicher Gewinn und kein Planungsgewinn — jede Domaene plant
  genau wie ein eigener Governor.
- `vig doctor` prueft Auslastung, Spuren und Best-Effort-Machbarkeit je
  Domaene: eine geschuetzte Auslastung ist eine Eigenschaft einer GPU, nicht
  der Anlage.
- Metadaten-, Bereitschafts- und Konfigurationsabfragen eines Modells gehen
  an den Endpunkt dieses Modells. Vorher gingen sie immer an
  `backend.grpc_endpoint`, auch fuer ein Modell mit eigenem
  `backend_endpoint`; mit Domaenen faellt das erst richtig auf.
- **Nicht qualifiziert.** Belegt ist die Logik — Zuordnung, Routing,
  Unabhaengigkeit von Krediten, Quarantaene und Look-ahead, Kennzahlen —
  mit Fake-Executoren. Nicht belegt sind Laufzeiten, Interferenz ueber
  PCIe und Hauptspeicher und das Verhalten einer echten zweiten GPU.
- `vig calibrate` und `vig profile` kennen Domaenen noch nicht; sie messen
  gegen die Endpunkte, die in der Konfiguration stehen.
- **Shared-Memory-Registrierungen ueber den Governor gehen weiterhin nur an
  `backend.grpc_endpoint`.** Ein Modell einer anderen Domaene findet eine so
  registrierte Region nicht und braucht bis auf Weiteres den Kopierpfad —
  oder unter `trust: open` eine Registrierung direkt an seinem Server. Die
  Registrierung an alle Endpunkte zu verteilen ist ein eigener Schritt mit
  eigener Pruefung (Besitz, Abmeldung, Teilausfall) und nicht Teil dieser
  Entscheidung.

## Verworfen

**Eine Slotmenge ueber beide GPUs, mit Modellmasken je Slot.** Die Masken
gibt es seit ADR-0035. Aber Look-ahead, Margenregler und Hardwarezustand
waeren global: eine erwartete Detektorankunft auf GPU 0 hielte das VLM auf
GPU 1 zurueck, und die gedrosselte GPU 0 machte die Prognose fuer GPU 1
vorsichtiger. Den Scheduler dafuer umzubauen hiesse, eine zweite GPU in jede
Entscheidung zu tragen, die heute eine betrifft.

**Automatisches Failover.** Siehe oben.

**Zwei Governorprozesse, und nichts bauen.** Das geht heute schon und bleibt
erlaubt. Es kostet zwei Endpunkte, zwei Token-Konfigurationen, zwei
Nutzlastbudgets ueber denselben Hauptspeicher und zwei Metrikseiten — und
jeder Client muss wissen, welche GPU welches Modell rechnet.
