# ADR-0020: Vertragszusaetze sind additiv, versioniert und vom Betreiber

**Status:** Akzeptiert · 2026-09-09
**Betrifft:** `core/model`, `core/contract_ext`, `config/schema`; Paket NV-02
**Ausloeser:** Der Vertrag kennt Deadline und Hoechstalter, aber keinen
Verbrauchertakt, kein Missbudget und keine Freigabe

## Kontext

Der bestehende `ModelContract` beschreibt einen Strom ueber Periode, Deadline,
Hoechstalter, Kritikalitaet und Mindestqualitaet. Fuer die haeufige
Robotikanforderung reicht das nicht:

* **„Hoechstens zwei Ausfaelle in hundert Zyklen, nie zwei hintereinander"**
  laesst sich damit nicht ausdruecken. `Protected` ist eine Prioritaet, keine
  Weakly-hard-Bedingung.
* **„Nur die zertifizierte Variante darf laufen"** auch nicht. Die
  Mindestqualitaet ist eine Schwelle; eine Freigabe ist eine Liste. Eine
  Variante kann qualitativ ueber der Schwelle liegen und trotzdem nie
  zertifiziert worden sein.
* **„Der Verbraucher tastet alle 33 ms ab"** ist etwas anderes als „Requests
  kommen alle 33 ms". Der Unterschied entscheidet, ob eine Zusage ueberhaupt
  pruefbar ist.
* **„Wie stark ist die Zusage?"** — beobachtet, qualifiziert oder bewiesen —
  hatte gar kein Feld.

## Entscheidung

**Ein optionales, versioniertes Zusatzobjekt am bestehenden Vertrag.** Kein
zweites Auftragsmodell daneben. Die alten Felder bleiben die Ausgangswerte;
fehlt der Zusatz, verhaelt sich alles wie vorher, und alte YAML-Dateien laufen
unveraendert weiter.

**Eine unbekannte Zusatzversion wird abgelehnt, nicht ignoriert.** Ein Feld,
das der Governor nicht versteht, koennte genau die Einschraenkung enthalten,
auf die sich der Betreiber verlaesst. Dasselbe gilt fuer einen Tippfehler in
einem Feldnamen: `deny_unknown_fields`, damit eine stille Nichtbeachtung
unmoeglich ist.

**Der Vertragstakt kommt aus dem Vertrag, nicht aus den Ankuenften.** Ein
Missbudget ohne `consumer_period_ms` wird abgelehnt. Der Monitor rechnet
vergangene Zyklen aus dem Takt aus, nicht aus der Zahl der angenommenen
Requests — sonst koennte man jeden Vertrag dadurch einhalten, dass man alles
ablehnt.

**Jeder Zyklus wird an seinem eigenen Zeitpunkt bewertet.** Bei
`latest_state` versorgt ein Ergebnis von vor 33 ms auch den Zyklus, in dem
nichts Neues ankam, solange es unter dem Hoechstalter bleibt. Ein Monitor, der
nur den aktuellen Zeitpunkt kennt, wuerde ruhige Zyklen als Misses zaehlen und
damit die Groesse verderben, um die es geht.

**M, K und L werden auf Wohlgeformtheit geprueft.** `M >= K` erlaubt jeden
Zyklus als Miss und ist als Konfiguration fast immer ein Vertipper. `L > M`
beschreibt eine Regel, die nie greift. Beides wird abgelehnt. Und `M=2, K=100,
L=1` heisst „hoechstens zwei in hundert und nie zwei hintereinander" — nicht
`L=2`.

**Freigabe und Qualitaet sind zwei Fragen.** `approved_variants` ist eine
Maske ueber Variantenindizes, keine Schwelle. Der Scheduler fragt
`variant_usable`, das beide Bedingungen zusammen prueft: die Reihenfolge zweier
Bedingungen zu vergessen ist der billigste Weg zu einer unautorisierten
Lockerung. Ist keine Variante freigegeben, wird der Vertrag beim Start
abgelehnt statt im Feld zu scheitern.

**Geforderte Nachweisstufe und beobachtetes SLO sind getrennte Felder.**
`evidence_required` ist eine Anforderung des Betreibers; der Monitor liefert
eine Beobachtung. Es gibt keine Funktion, die aus der einen die andere macht.
Das Risiko, das Dokument 04 benennt — Anforderungen versehentlich aus
Laufzeitmessungen abzuleiten — ist damit nicht wegargumentiert, sondern
strukturell ausgeschlossen.

## Konsequenzen

**Zuerst Monitor, dann Policy.** Ein `MissWindow` ist ein Messgeraet. Eine
begrenzte Ringstruktur genuegt, um eine Weakly-hard-Bedingung zu beobachten;
sie durchzusetzen braucht kuenftige Kapazitaet und beherrschte Stoerungen. Der
Governor **meldet** deshalb Verletzungen (`vig_weakly_hard_violated`), er
verspricht nichts. Eine Zusage waere erst nach eigener Qualifikation
vertretbar.

Der Ring hat feste Groesse (1024 Zyklen, Spec L-003). Eine laengere Stille
wird gezaehlt, aber nicht mehr einzeln bewertet — bei einer Luecke, die laenger
ist als das Fenster, ist ohnehin jede Position ein Miss.

Der Zaehler traegt die Vertragsrevision, unter der er laeuft. Ein
Vertragswechsel legt einen neuen Monitor an, statt den alten weiterlaufen zu
lassen: Zahlen aus zwei Vertraegen in einem Fenster beschreiben keinen von
beiden.

## Alternativen

**Die Weakly-hard-Bedingung in die Kritikalitaetsklasse legen.** Haette keine
neuen Felder gebraucht und waere falsch: `Protected` ist eine Vorrangregel
zwischen Stroemen, eine Weakly-hard-Bedingung eine Aussage ueber einen
einzelnen Strom ueber Zeit. Sie zu vermischen haette beide unschaerfer
gemacht.

**Die Freigabe ueber die Mindestqualitaet abbilden.** Haette funktioniert,
solange die zertifizierten Varianten zufaellig die qualitativ besten sind. Beim
ersten Gegenbeispiel — eine zertifizierte kleine Variante neben einer
unzertifizierten grossen — waere die Konfiguration nicht mehr ausdrueckbar
gewesen.

**Ueber angenommene Requests zaehlen.** Waere einfacher gewesen und haette
einen Vertrag erzeugt, den man durch Nichtstun einhaelt.
