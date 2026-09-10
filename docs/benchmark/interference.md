# NV-11: die Interferenztabelle ist angeschlossen und auf dieser Maschine nicht messbar

Datum: 10.09.2026. Maschine: RTX 3070 Laptop (8 GB), Treiber 580.173.02,
Triton 2.70.0 (26.06-py3), vier ONNX-Modelle, zwei Slots.

## Was jetzt gilt

Die gerichtete Interferenztabelle steht als `backend.interference` in der
Konfiguration und wird beim Start in den Scheduler gegeben. Bei jeder Planung
sieht er nach, welche Modelle gerade laufen; ist die Paarung gemessen, kommt
ihr Aufschlag auf die Prognose. Eine ungemessene Paarung bekommt nichts —
der Slot-Belegungsgrad bleibt die Naeherung, die er laut ADR-0006 immer war.

Zwei Tests belegen beides: dass ein gemessener Aufschlag in der geplanten
Laufzeit ankommt, und dass eine ungemessene Gegenrichtung keinen erfundenen
bekommt.

## Was die Messung ergeben hat

Nichts, und das ist das Ergebnis.

```
Qualifikation: 0 von 4 Messreihen verwertbar.
  4 verworfen.
```

Jede Reihe wurde verworfen, und zwar aus demselben Grund:

```
Messreihe verworfen: Hardwarezustand geaendert:
  GPU 0 clock_sm_mhz: ~1500 -> ~1800
  GPU 0 clock_sm_mhz: ~1800 -> ~1500
  GPU 0 clock_sm_mhz: ~1500 -> ~1900
  GPU 0 clock_sm_mhz: ~1700 -> ~1600
```

Der SM-Takt dieser Karte wandert waehrend jeder Messreihe zwischen rund 1500
und 1900 MHz. Ein Wechsel um mehr als 100 MHz verwirft die Reihe: sie gilt
dann fuer keinen Betriebspunkt, und ein Mittelwert ueber mehrere Betriebspunkte
beschreibt keinen davon.

Eine einzige Paarmessung kam durch — `vlm` neben `depth`, Faktor 2,25,
+113 625 us — und die landet nicht in der Tabelle, sondern in `no_corun`: das
Paar wird ohnehin serialisiert und laeuft nie gleichzeitig.

## Warum die Schwelle nicht gelockert wird

Weil das die Messung ergaebe, die das Codereview vom 10.09. als Fehler
benannt hat: „ein anderes und systematisch guenstigeres Messbild". Vor
diesem Stand haette `vig calibrate` die Zahlen geschrieben, ohne den
Taktwechsel zu erwaehnen — Ruecken an Ruecken gemessen, Fehlschlaege
uebersprungen, Mittelwert ueber alles.

Die 100-MHz-Schwelle ist dieselbe, die `vig profile` seit NV-05 anwendet, und
sie liegt unter fuenf Prozent des Maximaltakts dieser Karte. Sie zu lockern,
damit ein Lauf durchgeht, hiesse die Zahl zu retten und die Aussage zu
verlieren.

## Was es bräuchte

**Einen festgehaltenen Takt.** `nvidia-smi -lgc 1800,1800` meldet auf dieser
Maschine:

```
The current user does not have permission to change clocks for GPU 00000000:01:00.0.
```

Genau dafuer gibt es NV-13 (`backend.actuation`, ADR-0030) — und genau
deshalb steht dort, dass der Regler „opt-in, beobachtet, mit einem Boden"
gebaut ist und seine **Politik** offen bleibt. Ohne die Rechte, den Takt zu
stellen, ist auf dieser Maschine keine qualifizierte Interferenzmessung zu
haben.

**Oder eine Maschine, die ihren Takt haelt.** Eine Desktop- oder
Datacenter-Karte ohne Leistungslimit haelt ihren Takt ueber eine Messreihe.
Diese hier tut es nicht, und `vig doctor` sagt das seit ADR-0021 vor jeder
Messung.

## Was das fuer die veroeffentlichten Zahlen heisst

Die Gate-M3-Vergleiche in diesem Repo sind unter demselben wandernden Takt
entstanden. Sie sind trotzdem gueltig, weil **beide** Vergleichsseiten
darunter liefen — das steht seit dem ersten Bericht so da. Was sie nicht
sind: absolute Zahlen fuer diese Karte.

Die Interferenztabelle ist etwas anderes. Sie ist keine Vergleichszahl,
sondern ein **absoluter** Aufschlag in Mikrosekunden, mit dem geplant wird.
Ein Wert, der ueber vier Betriebspunkte gemittelt ist, waere fuer die Planung
schlechter als gar keiner — deshalb bleibt die Tabelle leer.
