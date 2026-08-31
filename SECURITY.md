# Security Policy

## Meldung von Schwachstellen

Sicherheitsrelevante Funde bitte **nicht** als oeffentliches Issue melden,
sondern an security@vigilant.example. Wir bestaetigen den Eingang innerhalb von
drei Werktagen.

## Bedrohungsmodell

Das Bedrohungsmodell des MVP ist in Spec Kapitel 22 beschrieben. Kurzfassung der
Angriffsflaechen:

- OIP-gRPC-Endpunkt (fremd kontrollierte Requestgroessen, Tensor-Metadaten,
  OneTimer-Parameter)
- Konfigurationsdateien
- Runtime- und Interferenzprofile
- Backend-Verbindung zu Triton

## Invarianten

Diese Regeln sind nicht verhandelbar und werden durch Lints, Tests und Fuzzing
abgesichert (Spec 8.3, 22.2, 26.4):

- Keine unbeschraenkte Allokation aus fremd kontrollierten Groessen.
- Alle Queues haben eine explizite Kapazitaet.
- Checked Integer- und Zeitarithmetik; kein Wraparound aus Clienteingaben.
- Keine Shell-Ausfuehrung aus Modellnamen oder Clientparametern.
- Keine Wall-Clock als Scheduling-Grundlage.
- `unsafe` ist im gesamten Workspace verboten (`unsafe_code = "forbid"`).
- Kein Logging vollstaendiger Tensor-Payloads.

## Umfang

OneTimer ist **kein** Safety-zertifiziertes System und erhebt keinen
Hard-Realtime-Anspruch (Spec 3.5). Die Kritikalitaetsklassen sind
produktinterne Prioritaeten, keine Sicherheitsintegritaetsstufen.
