# Messszenarien: was wir prüfen, wogegen, und woran es scheitern darf

Stand: 2026-09-11. Dieses Dokument legt fest, **welche Lastfälle** die
Aussagen dieses Projekts tragen, bevor gemessen wird. Es entstand aus zwei
Befunden desselben Tages: Die Messkette vom 11.09. zeigte, dass der Vorsprung
des Governors stark vom Lastfall abhängt (20–125x bei stationärer Überlast,
kein Gewinn oder ein Verlust bei Lastspitzen, ein Verlust von 16,5 % eines
nachrangigen Stroms genau bei 100 %). Und der externe Review
([2026-09-11-runtime](../reviews/2026-09-11-runtime/REVIEW.md)) stellte fest,
dass mehrere Gewinne mit Hintergrundarbeit bezahlt werden, die in den
Tabellen nicht neben dem Gewinn stand.

## Regeln für jede Messzelle

1. **Nutzen und Preis in derselben Tabelle.** Neben der Abdeckung der
   geschützten Ströme stehen immer der Verlust der übrigen Ströme und der
   Fortschritt der Hintergrundarbeit: fertige Aufträge je Minute bzw.
   Token je Sekunde. Ein Gewinn ohne seinen Preis wird nicht berichtet.
2. **Jede Zelle hat einen Status:** `gültig`, `verschmutzt` (Fremdlast laut
   Wächter), `kaputt` (Aufbau gescheitert) oder `unvollständig`. Nur gültige
   Zellen gehen in eine Aussage ein.
3. **Mindestens drei Wiederholungen,** Median und Spannweite. Ein Maximum aus
   einem Lauf ist keine Aussage ([gate-m3-r03](gate-m3-r03.md)).
4. **Ein Manifest je Lauf:** Commit, Hash des eingefrorenen Binaries,
   Konfiguration, Modell- und Container-Digests, Treiber, aktive
   XSched-Parameter.
5. **Die stärkste faire Baseline.** Triton mit Prioritäten und Rate Limiter,
   Triton mit XSched, ein selbstgebauter Scheduler (immer das neueste Bild,
   Frist zuerst; [diy-baseline](diy-baseline.md)), und für Sprachmodelle vLLM
   mit Prioritätsplanung.

## Die Szenarien

| # | Lastfall | Was er prüft | Baseline | Plattform |
|---|---|---|---|---|
| S1 | **Gate M3:** Detektor, Pose, Tiefe (geschützt) + VLM (Hintergrund), 103 % | den Kernfall: geschützte Ströme unter Überlast | Triton getunt | Laptop |
| S2 | **Rampe 50–150 %**, mit 95 und 105 % | wo sich der Governor lohnt und wo er zu vorsichtig ist | Triton getunt | Laptop |
| S3 | **Detektor + Sprachmodell:** Alarmpfad geschützt, Lagebericht im Hintergrund (Vig-Edge-Pilot) | Aufgabenmetriken statt Lieferfenster: Alarmzeit, Trefferquote, Fehlalarme, Berichte je Minute | Triton + vLLM direkt | Laptop, Pixel 2 (LLM auf CPU) |
| S4a | **Ein Sprachmodell, zwei Klassen:** interaktiver Assistent (Time-to-first-Token ≤ 500 ms, Tokenabstand ≤ 100 ms) + Zusammenfasser mit langen Prompts | kooperative Zerlegung (ADR-0014) und Prefill-Kosten (NV-16) gegen die eingebaute Prioritätsplanung von vLLM | vLLM `--scheduling-policy priority` | Laptop |
| S4b | **Zwei Sprachmodelle nebeneinander**, zwei vLLM-Prozesse auf einer GPU | ob der Governor zwei generative Backends fair teilt, wenn keines vom anderen weiß | beide direkt | Laptop |
| S5 | **Drei Klassen:** Detektor (geschützt), VLM auf Alarm (erhöht), LLM-Bericht (Hintergrund) | gemischte Wichtigkeit, Anwendungshinweise (ADR-0029) | Triton + vLLM mit Prioritäten | Laptop |
| S6 | **Langer Job unter Sättigung:** 70–85 % Last, ein nicht unterbrechbarer Block von 90 ms | der Fall, in dem der Governor auch ohne Überlast helfen muss ([tensorrt](tensorrt.md): 76 %) | Triton getunt | Laptop |
| S7 | **Lastspitze als zusätzliche Kamera** bzw. als Häufung von VLM-Anfragen nach einem Alarm | realistische Spitzen; die bisherige Spitze (dieselbe Kamera liefert schneller) prüft einen Fall, den kein Vertrag beschreibt | Triton getunt | Laptop |
| S8 | **Mehrere gleichrangige geschützte Ströme** (2–4 Kameras) | die Summe der Reserven; die Schranke aus NV-23 gilt nur für einen Strom | Triton getunt | Laptop |
| S9 | **Präemption:** Triton + XSched gegen Vigilant + Lane mit gemessenem R | ob Planung mit Präemption mehr bringt als Präemption allein | Triton + XSched (LV2, TSG auf Level 3) | Laptop |
| S10 | **Selbstkalibrierung:** S2 mit absichtlich falschem Profil (×2, ×0,7) und mit dem Profil einer anderen Maschine | ob die gelernte Marge das Profil korrigiert (ADR-0038) | dieselbe Konfiguration ohne Lernen | Laptop, Pixel 2 |
| S11 | **Zweite GPU:** S1 im Kleinen (Detektor, Pose, Tiefe als TFLite) auf der Adreno 540 | dieselbe Logik auf anderer GPU und anderem Backend (ADR-0039) | Backend direkt | Pixel 2 |
| S12 | **Dauerlauf 8 h:** Grundlast 90 % mit Spitzen 150 % | Speicher, Fehler, Margendrift ([soak](soak.md)) | — | Laptop |

## Was zuerst kommt

Die Reihenfolge folgt dem, was eine Aussage am meisten trägt, nicht dem, was
am leichtesten zu messen ist:

1. **S3, der Pilot**, sobald die Pufferfehler aus dem Review behoben sind
   (R03, R07). Er misst, was ein Anwender merkt, und hat seine Kriterien K1–K8
   vor der Messung festgelegt ([edge-pilot](../pilot/edge-pilot.md)).
2. **S9 und S10**, weil sie die zwei Schwächen vom 11.09. adressieren: die
   Kante bei 100 % und das VLM, das ohne Präemption nie läuft.
3. **S4a**, weil ein Assistent neben einem Zusammenfasser der häufigste
   Einsatz eines kleinen Sprachmodells auf einem Edge-Gerät ist und vLLM
   dafür eine eingebaute Antwort hat, gegen die wir bestehen müssen.
4. **S6, S7, S8**, weil sie die Grenzen der bisherigen Aussage prüfen.
5. **S11** auf dem Telefon, parallel, weil es die Laptop-GPU nicht braucht.

## Was ausdrücklich nicht geprüft wird

- **Durchsatz-Jobs**, bei denen jedes Bild zählt: Dafür ist der Governor das
  falsche Werkzeug; er verwirft absichtlich.
- **Mehr als eine GPU:** NV-22 ist gebaut und mit Fake-Backends getestet
  ([ADR-0037](../adr/0037-a-domain-is-a-gpu-with-one-owner.md)); ohne zweite
  GPU gibt es keine Messung.
- **Erkennungsqualität echter Varianten:** Die Frontier-Messung prüft den
  Mechanismus der Variantenwahl mit angegebener Qualität. Eine gemessene
  Genauigkeit echter Modellvarianten fehlt.
