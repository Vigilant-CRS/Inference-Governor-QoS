# ADR-0016: Ein unbestaetigtes Profil weitet die Marge, es verweigert nicht den Start

**Status:** Akzeptiert · 2026-09-01
**Betrifft:** Spec G-010 (Profile stale), L-014 (Umgebungsfingerprints)
**Ausloeser:** Abgleich der Spezifikation gegen den Code; G-010 war nicht umgesetzt

## Kontext

G-010 verlangt:

> Model hash/Backendversion geändert. Altes Profil darf nicht stillschweigend
> als exakt gültig gelten.

Bis hierher hatte `RuntimeProfile` nur p50/p95/p99 und die Anzahl der
Messungen. Wer eine Modelldatei austauschte oder Triton aktualisierte, plante
weiter mit den alten Zahlen — und nichts meldete sich. Da die gesamte
Admission Control auf dem Profil aufsetzt, ist das die unangenehmste Sorte
Fehler: das System gibt weiter Zusagen, nur beruhen sie auf einer Messung, die
nicht mehr zustaendig ist.

Zwei Fragen waren zu entscheiden: **woran** erkennt man die Aenderung, und
**was folgt** daraus.

## Entscheidung

**Erkannt wird an einem Fingerabdruck aus dem, was das Backend ueber sich
meldet:** Servername und -version, Modellname, Modellversionen, Plattform und
die Ein-/Ausgabesignatur. `onetimer profile` schreibt ihn in die
Konfiguration, `doctor` und `serve` vergleichen ihn beim Start.

Der Hash ist ein von Hand implementiertes FNV-1a und nicht `DefaultHasher` —
dessen Ergebnis darf sich laut eigener Dokumentation zwischen Rust-Versionen
aendern, und ein Fingerabdruck, der nach einem Toolchain-Update anders lautet,
wuerde bei jedem Upgrade falschen Alarm ausloesen. Eine Warnung, die zu oft
grundlos kommt, wird abgeschaltet und schuetzt dann gar nichts mehr.

**Bei Abweichung wird die Marge geweitet, nicht der Start verweigert.** Das
betroffene Modell startet mit einem Aufschlag von 40 Prozentpunkten auf die
konfigurierte Sicherheitsmarge; der Boden bleibt die konfigurierte Marge,
sodass der Online Estimator (WP11) bis dorthin zurueckregeln darf, sobald er
eigene Messungen hat.

## Warum nicht den Start verweigern

Der naheliegende Reflex waere, bei ungueltigem Profil abzubrechen. Das ist fuer
einen Governor die falsche Richtung. Er sitzt auf einem Roboter zwischen
Kamera und Steuerung; verweigert er den Dienst, faellt die Wahrnehmung
komplett aus. Ein zu vorsichtig geplantes Modell liefert weniger Durchsatz —
eine nicht gestartete Wahrnehmung liefert nichts.

Das ist dieselbe Richtung wie in [ADR-0010](0010-pessimistic-promises-optimistic-discards.md):
im Zweifel pessimistisch versprechen. Ein unbestaetigtes Profil ist Zweifel,
kein Beweis.

## Warum 40 Prozentpunkte

Der Wert muss nicht richtig sein, weil er nicht lange gilt: der Estimator
misst binnen Sekunden selbst und korrigiert. Er muss nur zwei Eigenschaften
haben — deutlich konservativ und begrenzt. Zu gross kostet Durchsatz, zu klein
waere genau das stillschweigend falsche Versprechen, das G-010 verbietet.

Der Rueckweg ist bewusst langsam: `RELAX_STEP` ist ein Prozentpunkt je
stabiler Phase, der Aufschlag also erst nach vierzig ruhigen Phasen abgebaut.
Das folgt [ADR-0013](0013-margin-corrects-forecasts-not-contracts.md) — nach
oben schnell, nach unten langsam.

## Konsequenzen

- Profile aus der Zeit vor dieser Entscheidung haben keinen Fingerabdruck.
  Sie werden **nicht** als unbestaetigt behandelt, sondern als nicht pruefbar:
  `doctor` warnt, `serve` warnt, die Marge bleibt unveraendert. Alles andere
  wuerde jede bestehende Konfiguration bei einem Update stillschweigend
  langsamer machen.
- Der Fingerabdruck sieht nur, was das Backend meldet. Wer die Gewichtsdatei
  unter derselben Versionsnummer und mit derselben Signatur austauscht, bleibt
  unentdeckt. Von aussen ist das nicht loesbar; dagegen hilft nur der
  Estimator, der die tatsaechlichen Laufzeiten misst.
- `actor::spawn` nimmt jetzt die Liste der unbestaetigten Modelle entgegen.
  Aufrufer, die keine Pruefung machen, muessen `&[]` uebergeben und sehen
  damit, dass es die Frage gibt.
