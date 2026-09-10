# ADR-0029: Ein Hinweis darf verschaerfen, nie lockern

**Status:** Akzeptiert · 2026-09-10
**Betrifft:** `core/hints`, `core/scheduler`; Paket NV-18
**Ausloeser:** Die Anwendung weiss Dinge, die der Governor nicht wissen kann —
und darf sie deshalb noch lange nicht bestimmen

## Kontext

Ein Roboter, der geradeaus faehrt, braucht die Detektion dringender als einer,
der steht. Ein Greifvorgang hat einen Aktionshorizont: bis er abgeschlossen
ist, aendert eine neue Wahrnehmung nichts mehr an der Entscheidung.

Diese Kenntnis in die Planung zu lassen ist wertvoll und gefaehrlich. Der
gefaehrliche Teil ist nicht der Fehler in der Anwendung — den gibt es immer —
sondern die Moeglichkeit, dass ein Hinweis eine Zusage **lockert**, die der
Betreiber gegeben hat.

## Entscheidung

**Verschaerfen ist frei, lockern braucht eine Freigabe.** „Ich brauche es
dringender" wird angenommen: die Zusage des Betreibers wird dadurch nicht
schwaecher, nur teurer. „Ich brauche es weniger dringend" wird abgelehnt, es
sei denn, der Betreiber hat genau diesen Fall vorher freigegeben.

**Auch Verschaerfen hat eine Untergrenze.** Sicher ist es fuer die **Zusage**,
nicht fuer die **Nachbarn**: ohne Boden koennte eine Anwendung durch immer
schaerfere Forderungen die gesamte Kapazitaet auf sich ziehen.

**Jeder Hinweis traegt eine Frist.** Nach ihrem Ablauf gilt der Grundvertrag —
**nicht** der letzte bekannte Zustand. Ein Sensor, der ausfaellt, waehrend er
„alles ruhig" gemeldet hat, darf nicht dauerhaft Ruhe bedeuten.

**Ein Hinweis ohne gueltige Herkunft ist kein Hinweis.** Die Voreinstellung ist
geschlossen: ohne benannte Stelle wird niemand gehoert.

**Widerspruch heisst Grundvertrag.** Zwei gleich frische Hinweise mit
unvereinbarem Inhalt heben sich auf. Der Governor entscheidet nicht, welcher
recht hat — er hat dafuer keine Grundlage.

**Erst den Hinweis fuer sich pruefen, dann gegen die bestehenden.** Andersherum
bekaeme ein unfreigegebener Modus die Meldung „Widerspruch" statt seiner
eigenen, und der Betreiber suchte den Fehler an der falschen Stelle.

**Eine Lockerung hebt das Hoechstalter an, sie schaltet es nicht ab.** Das war
im ersten Entwurf anders und war falsch: „kein Hoechstalter" heisst, dass bis
zum Ende des Horizonts nichts mehr als veraltet verworfen wird — die GPU
rechnet dann ausgerechnet in dem Fenster am meisten Altlast, in dem niemand
hinsieht. Jetzt reicht das Hoechstalter bis zum Ende des Horizonts und schrumpft
mit ihm, faellt dabei aber nie unter den Vertrag: eine Lockerung, die
verschaerft, waere ein Widerspruch in sich.

**Ein Hinweis aendert keinen Vorrang.** Er ist eine Aussage ueber Frische. Wer
zwischen Stroemen vorgeht, entscheidet die Betreiberpolicy — dasselbe Argument
wie bei ADR-0027.

## Konsequenzen

Der Governor **interpretiert die Welt nicht**. Er liest keine Sensordaten,
leitet keinen Zustand ab und trifft keine Sicherheitsentscheidung. Eine
„Confidence" aus einem Modell ist hier kein Eingabewert; sie waere eine Zahl,
deren Zustandekommen der Governor nicht beurteilen kann.

Eine engere Freigabe wirkt sofort, auch auf bereits angenommene Hinweise. Der
Betreiber muss nichts zuruecknehmen; er aendert die Freigabe, und der naechste
Blick auf den Hinweis faellt anders aus.

Die Herkunft ist im Kern eine undurchsichtige Kennung. Wie sie belegt wird —
Token, mTLS, Unix-Peer — entscheidet die Schicht darueber. Der Kern prueft
Gleichheit gegen die Freigabe und sonst nichts.

## Alternativen

**Hinweise als Vorrang statt als Frische.** Waere maechtiger und haette die
Betreiberpolicy ausgehebelt: eine Anwendung koennte sich selbst nach vorne
bringen.

**Confidence-Werte annehmen und gewichten.** Klingt nach Feingefuehl und ist
eine Zahl, die der Governor nicht pruefen kann. Ein falsch kalibriertes Modell
haette damit direkten Zugriff auf die Zulassung.

**Den letzten Hinweis nach Fristablauf weitergelten lassen.** Bequem, und es
macht einen Sensorausfall zu einer Dauerentwarnung.
