# ADR-0035: Praemption ist eine gemessene Eigenschaft des Backends

**Status:** Akzeptiert · 2026-09-11
**Betrifft:** `core/slots`, `core/feasibility`, `core/scheduler`,
`core/variant`, `config/schema`, `gateway/actor`, `gateway/exporter`,
`cli/calibrate`, `cli/doctor`; Paket NV-15, ADR-0012, ADR-0014, ADR-0033,
ADR-0034
**Ausloeser:** XSched laeuft seit dem 11.09. unter dem qualifizierten Triton
(Spike NV-15, Nachtrag 2) — und der Governor haelt das VLM trotzdem bei 0 %

## Kontext

ADR-0012 sagt, warum ein langer Hintergrundauftrag unter Last nie startet:
er ist ein **unteilbarer Block** auf dem Slot, den die geschuetzte Arbeit
braucht. Ein 90-ms-VLM neben einem 33-ms-Detektor gefaehrdet immer die
naechste Detektorankunft, und der Look-ahead haelt ihn deshalb zurueck. In
Gate M3 steht das VLM bei 0 %. ADR-0014 antwortet mit Zerlegung und ADR-0031
beziffert deren Preis.

XSched macht den Block teilbar. Ein Tritonprozess niedriger Prioritaet wird
auf Level 2 unterbrochen, sobald der Prozess hoher Prioritaet Arbeit hat; was
von der Unterbrechung uebrig bleibt, ist eine **Restblockierung** — im Spike
rund 14 ms statt 50 ms, in der Funktionsprobe 98–110 ms statt 182–205 ms fuer
eine 96-ms-Aufgabe.

Der Governor weiss davon nichts. Er plant das VLM weiter auf dem geschuetzten
Slot, mit `no_corun [detector, vlm]`, und der Look-ahead rechnet mit den
vollen 90 ms. Das Ergebnis bleibt 0 %, obwohl das Backend es besser koennte.

ADR-0033 hat den Weg vorgegeben: Praemption gehoert in den Backendprozess,
und der Governor plant mit ihr als **gemessener Eigenschaft des Backends** —
er ruft XSched nie auf.

## Entscheidung

1. **Eine Spur statt eines Slots.** `backend.preemptible_lanes: N` legt N
   zusaetzliche Slots an, die nur Modelle mit `preemptible:` ausfuehren; die
   regulaeren Slots fuehren diese Modelle dafuer nicht mehr aus. Eine Spur
   ist kein zusaetzlicher Rechenkern, sondern der Prozess niedriger
   Prioritaet. Sie belegt keinen geschuetzten Slot — deshalb verspaetet ein
   laufender Hintergrundauftrag keinen geschuetzten Start.
2. **Die Restblockierung R ist eine Messung.**
   `preemptible: { residual_blocking_us: R, source: measured }` am Modell.
   `vig calibrate` misst R als p99 der geschuetzten Laufzeit mit laufender
   Hintergrundarbeit minus p99 allein, im Maximum ueber die geschuetzten
   Modelle. Von Hand eingetragen heisst `source: declared`, und `vig doctor`
   warnt.
3. **Geschuetzte Arbeit plant mit R, solange die Spur belegt ist** — als
   Untergrenze, nicht als Zuschlag: `max(konservativ, allein_konservativ + R)`.
   Der Online-Schaetzer fuehrt ueberlappte Laeufe unter dem Belegungsgrad mit
   belegter Spur, getrennt vom Alleinlauf. Hat er die Ueberlappung dort schon
   beobachtet, gilt seine Zahl; eine Summe zahlte R zweimal. Die Marge
   (ADR-0034) regelt beides wie bisher.
4. **Der Look-ahead fragt fuer einen praemptierbaren Kandidaten, ob R in den
   Slack passt**, nicht ob seine Laufzeit in die Luecke passt. Er rechnet R
   fuer **jede** erwartete geschuetzte Ankunft im Horizont, nicht nur fuer
   die vor dem nominellen Ende: ein unterbrochener Auftrag endet spaeter, als
   seine Laufzeit sagt.
5. **Ein praemptierbarer Auftrag wird nicht in Quanten zugeschnitten**
   (ADR-0014): er muss in keine Luecke passen.
5a. **Ein zerlegbarer Auftrag blockiert nur fuer ein Quantum** — Nachtrag vom
   16.09.2026. Die Zulassungspruefung rechnet fuer ein Modell mit
   `cooperative:` nicht mit der Laufzeit des ganzen Auftrags, sondern mit der
   des **spaetesten** Quantums: `min_tokens` bei vollem Kontext, also der
   unguenstigste Fall (NV-16). Liegt die unter der engsten Zusage, ist das
   Modell kein Blocker. Liegt sie darueber, bleibt der Befund — nennt aber
   einen anderen Ausweg (kleineres `min_tokens`, kleineres
   `max_total_tokens`, ein wirksamer Prefix-Cache), denn zum Zerlegen zu
   raten waere hier sinnlos.

   *Anlass:* die Regel fragte urspruenglich nicht nach `cooperative:`. Ihr
   Befundtext empfahl die Zerlegung als wirksamsten Ausweg — wer ihm folgte,
   bekam denselben Befund erneut, und der Governor verweigerte den Start.
   Damit war ADR-0014 in genau dem Fall gesperrt, fuer den es geschrieben
   wurde: auf **einer** Ausfuehrungseinheit. Auf zwei Slots faellt es nicht
   auf, weil die Regel dort ohnehin schweigt — und das erklaert, warum kein
   ausgeliefertes Beispiel `cooperative:` trug. Gefunden beim Versuch, das
   erste solche Beispiel zu messen (`examples/cooperative_llm/vig.yaml`);
   Tests in `crates/vig-config/tests/blocking_work.rs`.
6. **Seine eigene Fertigstellung wird gelernt, nicht angenommen.** Die
   Wandzeit eines unterbrochenen Auftrags ist laenger als sein Profil; der
   Schaetzer lernt sie in der Zelle seines Belegungsgrads. Die
   Verwerfensregel nach Hoechstalter (ADR-0010) bleibt dieselbe und liest
   diese Beobachtung mit.
7. **Die Konfiguration lehnt ab, was sich widerspricht:** eine geschuetzte
   Klasse als praemptierbar; R = 0; ein praemptierbares Modell im selben
   Prozess wie geschuetzte (die Prioritaet gilt je Prozess); `no_corun`
   zwischen einem praemptierbaren und einem geschuetzten Modell;
   praemptierbare Modelle ohne Spur, eine Spur ohne Modell.

Ohne `preemptible:` und `preemptible_lanes` gibt es keine Spur und kein R;
jede Entscheidung bleibt bitgleich.

## Warum nicht die Auslastung

Die naheliegende Schaetzung fuer die Wandzeit eines unterbrochenen Auftrags
ist `Laufzeit / (1 − U_geschuetzt)`. Auf der Gate-M3-Last ist U laut Vertrag
103 % — die Formel sagte „nie fertig", und die Verwerfensregel wuerde jeden
Hintergrundauftrag vor dem Start verwerfen: genau die Aushungerung, um die
es geht. Tatsaechlich liegt die geschuetzte Auslastung darunter, weil
Supersession Frames verwirft. Die Vertragsauslastung ist fuer diese Frage
die falsche Groesse; die gelernte Wandzeit ist die richtige.

## Konsequenzen

- **Der VLM-Strom kann unter Last laufen**, ohne Zerlegung, wenn R in den
  Slack der geschuetzten Arbeit passt. Passt es nicht, haelt der Look-ahead
  ihn weiter zurueck — eine Spur ist kein Freibrief, die Zahl entscheidet.
- **Die geschuetzte Planung wird waehrend der Ueberlappung vorsichtiger**
  (um R). Das ist der ehrliche Preis der Praemption und steht als
  `vig_protected_overlap_extra_us_total` im Export.
- **Die Aussage haengt am Backendaufbau.** Zwei Tritonprozesse unter XSched
  sind eine Betriebsentscheidung mit eigener Qualifikation (ADR-0033); ohne
  diesen Aufbau ist `preemptible:` eine falsche Behauptung, und genau
  deshalb ist R eine Messung des Aufbaus.
- **Mehrere Spuren sind erlaubt**, R gilt dann im Maximum ueber die
  laufenden praemptierbaren Auftraege. Gemessen ist nur eine.

## Verworfen

**Das VLM auf einem zweiten regulaeren Slot (`slots: 2`).** Der Governor
haelte es dann fuer eine unabhaengige Recheneinheit und plante die
geschuetzte Arbeit ohne jede Verzoegerung — genau die erfundene Kapazitaet,
die ADR-0004 verbietet.

**R als Zuschlag auf jede geschuetzte Prognose.** Sobald der Schaetzer die
Ueberlappung beobachtet hat, enthielte seine Zahl R schon; eine Summe
zahlte es zweimal.

**Den Governor die Praemption ausloesen lassen.** XSched steuert Queues im
Prozess, der sie angelegt hat; der Governor saesse im falschen Prozess, mit
einer FFI, die ADR-0033 ausschliesst.
