# GPU-Messung nach den Reparaturen

Datum: 2026-09-08 · RTX 3070 Laptop (8 GB), Treiber 580.173.02,
Triton 2.70.0 (`26.06-py3`) und vLLM (`26.06-vllm-python-py3`), Ubuntu 26.04.
Reservierte Kerne (`taskset -c 8-15`), Messung jeweils bei Load < 2 gestartet.

Zweck: prüfen, ob die Reparaturen aus dem Review vom 07.09. die dokumentierten
Messergebnisse halten — und ob die Befunde, die nur am Code nachgewiesen waren,
sich auf echter Hardware bestätigen.

## Gate M3 — hält

Getunte Triton-Baseline (`--rate-limit=execution_count`, Prioritäten je
Instance-Group), zwei Läufe.

| Strom | Triton dokumentiert | Triton jetzt | OneTimer dokumentiert | OneTimer jetzt |
|---|---:|---:|---:|---:|
| detector | 84 % | 84 % | 99 % | 99 % |
| pose | 91 % | 91 % | 99 % | 99 % |
| depth | 97 % | 97 % | 99 % | 99 % / 100 % |
| vlm | 100 % | 100 % | 0 % | 0 % / 1 % |

Faktoren unabgedeckter Perioden: detector 22,4x → **22,6–22,9x**,
pose 10,6x → **11,9–12,0x**, depth 2,6x → **5,4x bis „besser"**.

Die Kernaussage von Gate M3 überlebt die Reparaturen unverändert und wird in
zwei Punkten etwas besser. Governor-Zähler nahezu identisch zur Dokumentation
(3718/3721 angenommen, 78–80 stale, 0 Protected-Deadline-Misses).

Rohdaten: `gate-m3-nach-reparatur.txt`, `gate-m3-lauf2.txt`.

## WP26 — die dokumentierte Aussage kippt

**Dokumentiert (2026-09-01):** „Lösen kooperative Quanten die Aushungerung?
**nein**, gemessen und begründet." Mit und ohne Zerlegung identisch: 2
Generierungen, 606 Zeichen.

Das war kein Messergebnis, sondern ein Fehlerbild. Zwei Ursachen:

1. **F03** — der Auftragszustand wurde erst in der *Fortsetzung* angelegt, also
   nie. Es wurde überhaupt nichts zerlegt. Dass beide Betriebsarten identische
   Zahlen lieferten, war der Fingerabdruck des Fehlers.
2. **Das Kostenmodell war strukturell falsch.** `size_quantum` rechnete die
   Dauer eines Quantums rein proportional zur Tokenzahl. Gemessen kostet jeder
   Auftrag einen festen Sockel von 14–18 ms — bei rund 4 ms je Token und etwa
   18 ms Slack ist das der ganze Unterschied. Zusätzlich stand im Benchmark
   `tokens_per_second: 55`; heute misst derselbe Benchmark rund 242.

Nach Reparatur beider Punkte, zwei Läufe:

| Betriebsart | Detektor-Abdeckung | Antwortalter p95 | Generierungen | Zeichen |
|---|---:|---:|---:|---:|
| direkt zu Triton | 58 % / 37 % | 138 / 188 ms | 67 | 20301 |
| Governor ohne Zerlegung | 99 % / 98 % | 33 ms | 1 / 2 | 303 / 606 |
| Governor **mit** Zerlegung | 92 % / 91 % | 65 ms | **40 / 41** | **9300 / 9532** |

**Die Zerlegung funktioniert.** Sie kauft rund **20x mehr Fortschritt für die
Best-Effort-Last** (2 → 40 Generierungen) für etwa **7 Punkte
Detektor-Abdeckung** (98 % → 91 %) und ein verdoppeltes Antwortalter
(33 → 65 ms). Null Protected-Deadline-Misses in beiden Betriebsarten.

Das ist keine Aushungerung mehr, sondern ein sichtbarer und einstellbarer
Kompromiss — genau das, was ADR-0014 versprochen hat. `docs/benchmark/wp26.md`
und die Zeile in `docs/benchmark/README.md` sind damit überholt.

Rohdaten: `wp26-nach-reparatur.txt` (nur F03 repariert, Kostenmodell noch
falsch: 1 Generierung), `wp26-mit-sockelkosten.txt`, `wp26-lauf2.txt`.

## F13 — Triton mit gemeinsamer begrenzter Ressource

Der letzte offene Einwand gegen den Wettbewerbsvergleich: die mitgelieferte
Baseline setzte `rate_limiter { priority }`, aber keine
`rate_limiter { resources }`. Priorität ordnet nur Wartende; eine gemeinsam
begrenzte Ressource erzwingt dagegen echten wechselseitigen Ausschluss über
Modellgrenzen hinweg. Das ist der stärkste Aufbau, den Triton für dieses
Problem anbietet.

Nachgeholt: alle fünf Instance-Groups zusätzlich mit
`resources [ { name: "gpu" count: 1 global: true } ]`, zwei Läufe.

| Strom | Triton nur Priorität | Triton + Ressource | Vigilant |
|---|---:|---:|---:|
| detector | 84 % | **89–91 %** | 99 % |
| pose | 91 % | **78–79 %** | 99 % |
| depth | 97 % | **91–95 %** | 99–100 % |

**Die Ressource hilft — und schiebt das Problem nur weiter.** Der Detektor
gewinnt 5 bis 7 Punkte, die Pose verliert 12. Das ist folgerichtig: der
wechselseitige Ausschluss wirkt, aber der Rate Limiter kennt weder Frische noch
Deadlines. Er kann deshalb nur umverteilen, wer wartet — nicht entscheiden, ob
sich das Warten überhaupt noch lohnt.

Keine der drei geprüften Triton-Konfigurationen bringt alle drei geschützten
Ströme gleichzeitig in die Nähe von 99 %. Der Einwand aus F13 ist damit
beantwortet: der Vorsprung entsteht nicht daran, dass die Baseline schlecht
konfiguriert war.

Rohdaten: `gate-m3-triton-resources.txt`, `gate-m3-triton-resources-lauf2.txt`.
Die Modellkonfigurationen im Runtime-Ordner sind anschließend wieder auf ihren
Ausgangsstand zurückgesetzt.

## Was diese Messung nicht zeigt

- **Die Baseline „direkt zu Triton" schwankt stark** (77 % dokumentiert,
  58 % und 37 % gemessen). Beide Triton-Server teilen sich eine 8-GB-Karte,
  die bei dieser Messung zu 7,5 GB belegt war. Absolute Vergleiche gegen den
  dokumentierten Stand sind dadurch nicht belastbar; der Vergleich *innerhalb*
  eines Laufs (ohne gegen mit Zerlegung) ist es.
- **Der Sockel ist nicht kalibriert, sondern von Hand eingetragen.** Der
  Benchmark misst ihn (14–18 ms je Lauf), verwendet aber den konfigurierten
  Wert von 18.000 µs. `onetimer calibrate` sollte ihn messen, wie es für
  `tokens_per_second` bereits gilt.
- **Kein neuer Dauerlauf**, keine Lastrampe, kein Jetson, keine Modellgüte.
