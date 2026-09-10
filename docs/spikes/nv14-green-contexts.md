# NV-14: Green Contexts partitionieren SMs — und nur SMs

Datum: 10.09.2026. Maschine: RTX 3070 Laptop (Ampere, sm86, 40 SMs), Treiber
580.173.02, CUDA 12.4. Probe: [1](nv14-green-contexts-probe1.cu),
[2](nv14-green-contexts-probe2.cu).

Eine Qualifikation, kein Ausbau. Was hier steht, ist gemessen.

## Die API ist da

`cuGreenCtxCreate`, `cuDevSmResourceSplitByCount`, `cuDeviceGetDevResource`,
`cuGreenCtxGetDevResource` und `cuCtxFromGreenCtx` stehen in
`/usr/include/cuda.h` (CUDA 12.4). Was **nicht** da ist:
`cuGreenCtxStreamCreate` — die kam später. Ein Stream in einer Partition
entsteht hier über `cuCtxFromGreenCtx` plus `cuCtxPushCurrent`.

## Die Aufteilung

```
Geraet: NVIDIA GeForce RTX 3070 Laptop GPU, sm86, 40 SMs
Ganzer SM-Satz: 40 SMs
  minCount  2: 10 Partitionen, je 4 SMs (Rest 0)
  minCount  4: 10 Partitionen, je 4 SMs (Rest 0)
  minCount  8:  5 Partitionen, je 8 SMs (Rest 0)
  minCount 16:  2 Partitionen, je 16 SMs (Rest 8)
```

Die Granularität ist **4 SMs**, wie der Header für Compute 8.x ankündigt
(„minimum count is 4 SMs and must be a multiple of 2"). Eine Anforderung von
2 wird auf 4 aufgerundet; eine von 16 lässt 8 SMs als Rest liegen, weil
40 kein Vielfaches von 16 ist. Zehn Prozent der Karte, die niemand bekommt —
das sind die **Reservekosten** einer ungünstig gewählten Aufteilung.

## Die Partition begrenzt wirklich

Ein rechenlastiger Kernel mit 160 Blöcken:

| | Dauer | Faktor |
|---|---:|---:|
| ganze Karte, 40 SMs | 22,2 ms | 1,00x |
| eine Partition, 10 SMs | 93,1 ms | **4,20x** |
| erwartet bei echter Begrenzung | | 4,00x |

Das ist keine Empfehlung, sondern eine Schranke. Der Kernel bekommt seine
zehn SMs und nicht mehr — auch wenn der Rest der Karte leer steht.

Zwei disjunkte Partitionen laufen nebeneinander: 37,9 ms für beide gegen
38,4 ms für einen allein, also 0,99x. Echte räumliche Nebenläufigkeit.

## Und sie schützt nur gegen SM-Konkurrenz

Derselbe Aufbau, zwei Partitionen zu je 10 SMs, in der Nachbarpartition ein
Gegner, der Speicher liest und schreibt:

| Opfer | allein | neben dem Gegner | Faktor |
|---|---:|---:|---:|
| rechenlastig (`__sinf`-Schleife) | 79,1 ms | 72,8 ms | 0,92x |
| bandbreitenlastig (256 MB kopieren) | 1,9 ms | 3,2 ms | **1,62x** |

Das rechenlastige Opfer merkt nichts — es braucht keine Bandbreite. Das
bandbreitenlastige wird um **62 %** langsamer, obwohl die SM-Partitionen
disjunkt sind.

Der Header sagt es selbst, in anderen Worten: „Even if the green contexts have
disjoint SM partitions, it is not guaranteed that the kernels launched in them
will run concurrently or have forward progress guarantees. This is due to other
resources (like HW connections)."

## Was das für Vigilant heißt

**Green Contexts sind eine echte räumliche Partitionierung und keine
Prioritätsempfehlung.** Das ist mehr, als Tritons Rate Limiter kann, und es
ist auf dieser Karte verfügbar.

**Sie lösen den Engpass dieses Projekts aber nicht.** Der Engpass in den
Gate-M3-Messungen ist ein 90-ms-Block, der die Karte belegt — ein
Zeitproblem. Eine SM-Partition macht daraus zwei kleinere Karten, auf denen
beide Ströme langsamer laufen: der Detektor bekäme statt 13 ms rund 52 ms,
wenn man ihm ein Viertel gibt. Für eine 33-ms-Periode ist das keine Lösung,
sondern das Ende.

Wo sie helfen würden: bei einem Hintergrundstrom, der **dauerhaft** ein
kleines Stück Karte bekommen soll, ohne dem Vordergrund dazwischenzukommen.
Das ist die Zusage `minimum_background_progress_pct` aus NV-02 — und die wird
heute über Zeitscheiben eingehalten, nicht über Raum.

**Gegen einen Bandbreitengegner schützen sie nicht.** Wer eine
Ressourcendomäne als „isoliert" verkauft, verkauft für speicherlastige
Modelle eine Zusage, die 62 % daneben liegt. Das ist genau die
„Produktklasse deterministisch", die die Abnahme von NV-14 ausschließt.

## Urteil

Qualifiziert, mit klarem Ergebnis: die Partitionierung funktioniert, sie
begrenzt nachweisbar, und sie ist gegen den Interferenzpfad blind, der auf
dieser Karte der teuerste ist. Für einen Ausbau spricht ein benannter
Anwendungsfall mit dauerhaftem Hintergrundstrom — nicht der Engpass, den die
bisherigen Messungen zeigen.

Ein Ausbau bräuchte dieselbe `unsafe`-Entscheidung wie NV-09 und NV-15, plus
eigene Profile je Partitionsgröße: ein Profil, das auf 40 SMs gemessen wurde,
gilt auf 10 nicht.
