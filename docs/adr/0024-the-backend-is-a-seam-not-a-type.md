# ADR-0024: Das Backend ist eine Naht, kein Typ

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** `gateway/executor`, `gateway/testing`, `gateway/actor`; Paket NV-07
**Ausloeser:** Die Fehlerpfade, an denen sich der Slotkredit entscheidet, waren
ohne echte GPU nicht pruefbar

## Kontext

Der Actor kannte genau einen Backendtyp: `TritonClient`, an drei Aufrufstellen.
Das ist bequem, solange es nur Triton gibt, und kostet an einer Stelle richtig
Geld: **die interessanten Fehlerpfade sind nicht testbar.**

NV-00 hat den Unterschied eingefuehrt, der ueber den Slotkredit entscheidet —
„der Aufruf hat das Backend nie erreicht" gegen „der Aufruf brach unterwegs ab,
und die Recheneinheit rechnet vielleicht weiter". Beide Zustaende auf echter
Hardware herbeizufuehren ist muehsam, unzuverlaessig und im CI unmoeglich. Sie
blieben deshalb durch Einheitentests im Kern belegt und im Zusammenspiel
unbelegt.

## Entscheidung

**Ein `Executor`-Vertrag mit drei Methoden:** ausfuehren, Endnachweis holen,
Faehigkeiten nennen. Triton ist die erste Implementierung; alles
OIP-Spezifische bleibt dort.

**Der Executor besitzt nichts.** Er fuehrt aus und berichtet. Slots, Kredite,
Generationen und Deadlines bleiben vollstaendig beim Actor. Genau ein Besitzer
je Ressource — sonst gaebe es zwei Stellen, die einen Kredit freigeben koennen,
und die Frage „wer hat ihn zuletzt gehalten" waere nicht mehr beantwortbar.

**Die Nutzlast bleibt eine OIP-Nachricht.** Ein `Ticket` traegt Modellname,
Entkopplungsflag und den Request. Ihn hier schon in eine backendneutrale
Darstellung zu uebersetzen hiesse, ihn einmal mehr zu kopieren — bei 28 MB je
VLM-Anfrage ist das keine Abstraktion, sondern Bandbreite (ADR-0003). Die
Uebersetzung gehoert dorthin, wo ein zweiter Executor sie tatsaechlich
braucht, und nicht in eine Naht, die noch niemand von der zweiten Seite
betreten hat.

**Faehigkeiten nennen nur, was die Ablaufsteuerung anders macht.** Zwei
Flags: liefert das Backend einen Endnachweis, und hat es einen
Stream-Endpunkt. Eine Faehigkeitsliste, die niemand abfragt, ist
Dokumentation am falschen Ort.

**Ein Fake-Executor hinter einem Feature.** Er kennt kein Netz, keine Uhr und
keine GPU; er gibt zurueck, was ihm gesagt wurde, und schreibt mit, was von ihm
verlangt wurde. Hinter einem Feature und nicht als `pub` im Produktivpfad: ein
Backend, das Antworten erfindet, soll nicht versehentlich verlinkbar sein.

**Ohne Programmierung antwortet der Fake mit einem Fehler.** Ein Test, der eine
Antwort erwartet, ohne sie zu bestellen, soll auffallen und nicht durchgehen.

## Konsequenzen

Sechs Fehlerpfade sind jetzt im Zusammenspiel belegt, ohne GPU und ohne Netz:
ein Aufruf, der nie startete, gibt seinen Kredit zurueck; ein unterwegs
abgebrochener tut es nicht und loest den Abgleich aus; ein Timeout beantwortet
den Client nach der Frist und wartet trotzdem auf das Backend; nach zehn
Auftraegen bleibt kein Anspruch offen.

Die alte aeussere API bleibt: `actor::spawn` nimmt weiter einen
`TritonClient` und baut den Executor selbst. `spawn_with` ist die Naht fuer
Tests und fuer ein zweites Backend.

**Was dieses Paket nicht getan hat:** den Actor physisch in ein eigenes
`vig-runtime`-Crate zu verschieben, und die Nutzlast von OIP zu loesen. Beides
steht in NV-07 und ist bewusst aufgeschoben. Der Verzeichniswechsel aendert kein
Verhalten und traegt keine der Abnahmekriterien; die Nutzlast zu uebersetzen
kostet eine Kopie je Anfrage und ist ohne zweiten Executor nicht zu
rechtfertigen. Beides gehoert zu NV-08, wenn TensorRT tatsaechlich als zweiter
Executor dazukommt — dann gibt es eine zweite Seite, gegen die sich die
Abstraktion pruefen laesst.

## Alternativen

**Generisch ueber `E: Executor` statt `Arc<dyn Executor>`.** Haette einen
virtuellen Aufruf je Inferenz gespart — bei einer Inferenz von 4 ms ist das
nicht messbar — und den Actor-Typ durch das halbe Gateway gezogen.

**Den Fake als Mock-gRPC-Server.** Es gibt bereits einen (`tests/mock_backend`),
und er testet den Draht. Er kann aber nicht „brich unterwegs ab, und zwar
jetzt" — dafuer muesste er den Transport steuern, den er selbst benutzt.

**Auf die Naht verzichten und die Fehlerpfade weiter nur im Kern testen.**
Haette den Code kuerzer gelassen und die Frage offen, ob der Actor die
Kernentscheidung auch tatsaechlich umsetzt. Genau dort lagen die Fehler, die
NV-00 behoben hat.
