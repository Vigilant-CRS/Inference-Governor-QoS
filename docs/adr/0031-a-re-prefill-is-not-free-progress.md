# ADR-0031: Ein Re-Prefill ist kein kostenloser Fortschritt

**Status:** Akzeptiert · 2026-09-10
**Betrifft:** `core/generative`, `core/model`, `core/scheduler`,
`gateway/cooperative`, `gateway/actor`, `cli/doctor`, `cli/calibrate`; Paket
NV-16, ADR-0012, ADR-0014
**Ausloeser:** Der Doc-Kommentar an `Cooperative::base_cost` nannte die erneute
Prefill-Berechnung des gewachsenen Prompts als eine seiner Ursachen — und
behandelte sie als Konstante

## Kontext

ADR-0014 zerlegt einen generativen Auftrag in Quanten. Der Zustand reist im
Prompt: jedes Quantum bekommt den urspruenglichen Prompt plus alles bisher
Erzeugte. Das braucht keinen Eingriff in die KV-Cache-Verwaltung des Backends
und funktioniert mit jedem Server, der Textgenerierung anbietet.

Es hat einen Preis, und der stand bis hierher falsch im Modell. `base_cost`
war als **feste** Kosten je Quantum definiert, und ihr eigener Kommentar nannte
als Ursachen „Round-Trip, Scheduling im Backend und — ohne wirksames
Prefix-Caching — die erneute Prefill-Berechnung des gewachsenen Prompts".

Der gewachsene Prompt waechst. Eine Konstante kann ihn nicht beschreiben.

Die Folgen waren zwei, und sie zeigen in verschiedene Richtungen:

1. **Die Zuschneidung war zu optimistisch.** Jedes Quantum wurde gleich gross
   zugeschnitten, gleichgueltig wie weit der Auftrag schon war. Die spaeten
   zogen ueber ihre Luecke hinaus, und die geschuetzte Ankunft dahinter kam zu
   spaet — genau der Fehler, gegen den ADR-0015 die Quantenzuschneidung
   ueberhaupt eingefuehrt hat, nur eine Ebene tiefer.
2. **Der Preis der Zerlegung war unsichtbar.** `vig doctor` meldete ein
   zerlegbares Modell als geloest („wird aber in Quanten zerlegt") und nannte
   nicht, was die Loesung kostet.

## Entscheidung

**Die Kosten eines Quantums sind kontextabhaengig.** `Cooperative` bekommt
`prefill_per_token`, und die Zuschneidung rechnet mit `Kontext = Prompt + alles
bisher Erzeugte`. Ein spaetes Quantum faellt damit kleiner aus als ein
frueheres. Gemessen im Testlauf: 400, 300, 200, 100 Token bei gleichem Budget.

**Null heisst gemessen, nicht angenommen.** `prefill_per_token = 0` bedeutet
„das Backend faehrt einen wirksamen Prefix-Cache" oder „nicht gemessen" — und
der Unterschied gehoert ins Profilmanifest, nicht in eine Vermutung im Code.
Bei null verhaelt sich die Zuschneidung exakt wie vor NV-16; jede bestehende
Konfiguration bleibt unveraendert gueltig.

**Der Prompt ist Kontext, schon beim ersten Quantum.** Er ist in absoluten
Zahlen der groesste einzelne Prefill des ganzen Auftrags. Ihn erst ab der
ersten Fortsetzung zu zaehlen hiesse, ausgerechnet den teuersten Schritt gratis
zu planen.

**Der Kern muss wissen, ob ein Auftrag wirklich zerlegt wird.** Ein Vertrag mit
`cooperative` sagt, dass das **Modell** zerlegbar ist. Ob ein einzelner Auftrag
es wird, entscheidet die Ausfuehrung: ein Request ohne Texteingang wird es nie.
Deshalb traegt der Deskriptor jetzt `decomposable`. Ohne dieses Feld schnitte
der Kern ein Quantum zu und meldete dessen Dauer an Look-ahead und
Slotbelegung, waehrend das Backend den ganzen Auftrag rechnet — beide plaenen
dann mit einer Zahl, die um Groessenordnungen zu klein ist.

**Der Rueckfall auf den ungeteilten Lauf wird gerechnet, nicht geraten.**
`max_overhead_permille` im Vertrag setzt eine Grenze; ueberschreitet der
vorausgerechnete Aufschlag sie, laeuft der Auftrag am Stueck. Ohne gesetzte
Grenze bleibt es beim bisherigen Verhalten — eine Grenze, die niemand gesetzt
hat, darf keine bestehende Konfiguration stillschweigend umstellen.

**Ungeteilt heisst nicht unbegrenzt.** Der `GenerativeJob` ist die einzige
Stelle im Baum, die `max_total_tokens` durchsetzt (Spec 8.3: keine
unbeschraenkte Arbeit aus fremd kontrollierter Eingabe). Faellt er beim
Rueckfall weg, faellt die Grenze mit — ein Client ohne eigenes `max_tokens`
bekaeme freie Fahrt, waehrend der Kern die Profillaufzeit der Variante
eingeplant hat. Der Rueckfall baut deshalb **ein** Quantum ueber das volle
zulaessige Budget und reicht genau das weiter. Das zweite Codereview dieses
Pakets hat den Fehler gefunden; die erste Fassung des Tests dazu akzeptierte
den unbegrenzten Aufruf noch als richtiges Ergebnis.

**Prefill, Dekodierung und Sockel werden getrennt gebucht.** Ein Re-Prefill
erzeugt kein einziges Token. Ihn als Fortschritt zu buchen hiesse, dieselbe
Arbeit zweimal zu verkaufen. `vig_generative_prefill_us_total`,
`vig_generative_decode_us_total` und `vig_generative_fixed_us_total` stehen
deshalb nebeneinander in Prometheus; erst ihr Verhaeltnis sagt, ob die
Zerlegung noch traegt. Der Sockel gehoert dazu, weil er auf der Messmaschine
der **groesste** Einzelterm ist — ohne ihn liesse das Verhaeltnis ausgerechnet
den dominierenden Kostenanteil aus.

Gebucht wird nach jedem abgeschlossenen Quantum, nicht bei der Fortsetzung:
sonst fiele das **letzte** heraus, und bei n Quanten waeren n-1 gezaehlt. Der
Prefill zaehlt erst ab dem zweiten — der erste faellt auch beim ungeteilten
Lauf an und ist kein Preis der Zerlegung. Gescheiterte oder veraltete Quanten
haben gerechnet, aber wie lange, ist nicht bekannt; dafuer einen Modellwert zu
buchen hiesse, Arbeit zu erfinden.

**Gemessen wird mit Median und Warmlauf.** Der Mittelwert ueber fuenf Runden
traegt jeden Ausreisser mit einem Fuenftel weiter, und ein einziger Stall im
Sekundenbereich reichte, um den errechneten Kontextterm den ganzen Sockel
auffressen zu lassen — ein Sockel von null ist genau der Zustand, gegen den
ADR-0015 geschrieben wurde. Der Median ist dagegen unempfindlich. Der
Warmlauf faellt weg, weil die allererste Anfrage an ein Modell dessen
Initialisierung traegt: faellt sie in die kurze Messreihe, meldet die CLI
„Kontext kostenlos", wo sie einen Warmlauf gemessen hat. Uebersteigt der
errechnete Prefill trotzdem den Sockel, ist die Messung widerspruechlich —
dann gilt der unbereinigte Sockel, und der Kontextterm wird verworfen statt
beide gemeinsam unbrauchbar zu machen.

**Gemessen wird in zwei Geraden, nicht in einer.** `vig calibrate` variiert die
Zahl erzeugter Token bei festem Prompt — daraus kommt die Erzeugungsrate — und
zusaetzlich die **Promptlaenge** bei fester Tokenzahl. Ohne die zweite Messung
kuerzt sich der Prefill-Anteil aus der Differenz definitionsgemaess heraus, und
`prefill_per_token_us` bliebe in jeder erzeugten Konfiguration auf null. Der
Sockel wird anschliessend um den Prefill des Kalibrierprompts bereinigt, sonst
stuende dieser Anteil zweimal im Modell.

## Konsequenzen

**Ein neuer Verhungerungspfad, und er ist benannt.** Die Kosten des
kleinstmoeglichen Quantums wachsen mit dem Fortschritt des Auftrags. Ab dem
Punkt, an dem sie die Luecke zur naechsten geschuetzten Ankunft uebersteigen,
vetoiert der Look-ahead jede weitere Fortsetzung — dauerhaft. Der Auftrag
zaehlt dann in `deferred_for_protected` und endet ueber `max_age` oder seine
Deadline, nicht ueber ein Ergebnis.

Das ist kein Fehler, sondern die ehrliche Antwort: ohne wirksames
Prefix-Caching gibt es fuer diesen Auftrag ab dieser Kontextlaenge keine
Luecke mehr, in die er passt. Vor NV-16 fiel es nicht auf, weil der Governor
Quanten startete, die ihre Luecke ueberzogen — und die geschuetzte Ankunft
dahinter kam zu spaet. Wer den Fall vermeiden will, setzt
`max_overhead_permille` oder sorgt fuer einen Cache. Ein Auftrag ohne `max_age`
bleibt sonst stehen.

**Die Zerlegung ist teurer, als dieses Projekt bisher gesagt hat.** Mit den
Zahlen aus WP26 (18 ms Sockel, 242 Token/s, Quanten zu 4 Token, 64 Token
Gesamtbudget) kosten allein die Round-Trips 95 % mehr Arbeit als der
ungeteilte Lauf — ohne jeden Prefill-Anteil. `vig doctor` sagt das jetzt, und
es ist eine Untergrenze: gerechnet ohne Prompt.

**Wo „quadratisch" gilt und wo nicht.** Bei fester Auftragsgroesse feiner zu
zerlegen kostet **linear** mehr: die Prefill-Summe `q * n * (n-1) / 2` wird zu
`total * (n-1) / 2`, weil das Quantum schrumpft, waehrend die Zahl der Quanten
waechst. Was der Prefill-Term aendert, ist nicht die Form dieser Geraden,
sondern ihre Steigung. Quadratisch wird es bei **fester Quantengroesse und
laengerem Auftrag**: doppelt so viele Quanten, und jedes traegt einen laengeren
Kontext. Das erste Codereview dieses Pakets hat einen Test gefunden, der
„superlinear" behauptete und auch ohne den Prefill-Term gruen war; beim
Nachrechnen stellte sich heraus, dass auch die Behauptung nicht stimmte. Beides
steht jetzt richtig im Test.

**Was auf dieser Maschine nicht gemessen ist.** Der `vlm`-Strom in den
Benchmarks ist ein ResNet-50 mit Batch 48 — ein Platzhalter fuer einen langen,
nicht unterbrechbaren Block, und er hat keinen Texteingang.
`vig calibrate` meldet fuer ihn korrekt „Kostenmodell nicht messbar" und laesst
die vorhandenen Werte stehen. `prefill_per_token_us` steht deshalb in jeder
Beispielkonfiguration dieses Repos auf null, und das heisst hier ausdruecklich
**nicht gemessen** und nicht „kostenlos".

Damit ist die Zuschneidung gegen einen echten Sprachmodell-Backend belegt durch
Tests und Rechnung, nicht durch eine Messung. Die erste Installation mit einem
echten generativen Backend muss `vig calibrate` laufen lassen und die Zahl
gegen das Kostenmodell pruefen — das ist die Abnahme, die hier offen bleibt.

**TTFT und TBT laufen gegenlaeufig.** Kleine Quanten verkuerzen die Wartezeit
auf das erste Token und verlaengern den Abstand zwischen den spaeteren.
`vig doctor` nennt beide Zahlen, damit die Wahl eine Wahl ist — und sagt dazu,
dass beide ohne Prompt gerechnet sind. Die Wartezeit zwischen zwei Quanten ist
die **Ausfuehrungszeit** der laengsten geschuetzten Arbeit, nicht deren
Periode: die Periode sagt, wie oft sie kommt, nicht wie lange sie dauert.

**Der genannte Aufschlag ist eine Untergrenze.** `doctor` rechnet ohne Prompt,
weil er zur Konfigurationszeit keinen kennt; das Gateway rechnet zur Laufzeit
mit dem echten. Ein Vertrag, den das Werkzeug gruen meldet, kann dort trotzdem
als zu teuer abgelehnt werden. Die Schwelle ist in beiden dieselbe: frueher
schwieg `doctor` bei `prefill_per_token_us == 0`, weil die Null zweideutig ist
— waehrend das Gateway denselben Vertrag ablehnte. `base_cost_us` ist ein
Pflichtfeld und gemessen; der Aufschlag aus Round-Trips allein ist eine
belastbare Zahl, und die Zweideutigkeit gehoert in den Text, nicht in die
Bewertung.

## Alternativen

**Den Sockel einfach hoeher ansetzen.** Waere die billigste Antwort und
verschiebt den Fehler nur: ein zu hoher Sockel macht die fruehen Quanten
unnoetig klein und die spaeten immer noch zu gross. Eine Konstante kann eine
wachsende Groesse nicht beschreiben, gleichgueltig welchen Wert man ihr gibt.

**Prefix-Caching voraussetzen.** Das Modulkommentar von `gateway/cooperative`
tat das bis hierher („ohne dieses Caching ist das Verfahren nicht
wirtschaftlich"). Es ist eine Annahme ueber ein fremdes Backend, und sie steht
in einem Modul, das ausdruecklich mit jedem Server funktionieren soll. Jetzt
wird sie gemessen.

**Die Zerlegung bei zu hohem Aufschlag automatisch abschalten.** Genau das tut
`max_overhead_permille` — aber nur, wenn der Betreiber eine Grenze nennt. Sie
mit einer Voreinstellung zu versehen haette bestehende Installationen
umgestellt, ohne dass jemand danach gefragt hat.

**Einen echten Tokenizer je Backend mitfuehren.** Die Schaetzung „rund vier
Zeichen je Token" ist grob. Sie muss die Zuschneidung nur in die richtige
Richtung bewegen, und ein Tokenizer je Backend waere ein Preis, den diese
Genauigkeit nicht wert ist.
