# Quickstart

Fünf Minuten von null auf einen laufenden Governor.

## 1. GPU im Container prüfen

```bash
docker run --rm --device nvidia.com/gpu=all ubuntu:24.04 nvidia-smi -L
```

Schlägt das fehl, zuerst [`../triton/README.md`](../triton/README.md) — dort
stehen die drei Fallstricke, die keine Anleitung erwähnt.

## 2. Modellrepository bereitstellen

Ein gewöhnliches Triton-Modellrepository. Die einzige Vigilant-spezifische
Anforderung steht in jeder `config.pbtxt`:

```protobuf
# Kein dynamisches Batching. Die Warteschlange gehört vor den Governor,
# nicht dahinter (ADR-0002).
max_batch_size: 0
```

## 3. Konfiguration schreiben

Als Vorlage dient [`../../examples/gate_m3/vig.yaml`](../../examples/gate_m3/vig.yaml).
Die Laufzeitprofile darin gelten **nur für die dort vermerkte Umgebung**; für
die eigene Hardware neu messen:

```bash
vig profile -c vig.yaml
```

## 4. Starten

```bash
export VIG_MODELS=/pfad/zum/modellrepository
export VIG_CONFIG=$PWD/vig.yaml
docker compose up -d
```

## 5. Prüfen

```bash
vig doctor -c vig.yaml
```

`doctor` sagt vor dem ersten Request, was nicht funktionieren wird — und zwar
alles auf einmal. Es meldet unter anderem:

* eine geschützte Auslastung, die die Kapazität übersteigt,
* ein Modell ohne `max_age_ms`, dem damit das Frische-Verwerfen fehlt,
* ein Best-Effort-Modell, das länger dauert als die kürzeste geschützte
  Periode und deshalb unter Last nie starten wird (ADR-0012).

Vor dem ersten Request ausserdem einmal die Bereitschaft ansehen:

```bash
curl -fsS localhost:9090/readyz && echo bereit
curl -s localhost:9090/metrics | grep vig_reconcile_baseline_missing
```

`/readyz` sagt, ob der Governor etwas ausrichten kann — nicht, ob er lebt.
Dafuer gibt es `/healthz`. **Nie einen Restart an `/readyz` haengen:** ein
Neustart bringt ein verschwundenes Backend nicht zurueck und wirft den
Leasezustand weg, den die Erholung braucht.

`vig_reconcile_baseline_missing` muss `0` sein. Steht dort mehr, konnte der
Governor beim Start den Abschlusszaehler des Backends nicht lesen; ein
abgebrochener Aufruf haelt dann seinen Slotkredit fuer die Lebensdauer des
Prozesses. Abhilfe: Governor bei erreichbarem Backend neu starten. Warum das
so ist, steht im [Runbook](../../docs/runbook.md).

## 6. Umstellen

Im Client nur den Zielendpoint ändern:

```diff
- triton:8001
+ vig:9001
```

Modelle ohne Vigilant-Konfiguration werden unverändert durchgereicht. Die
QoS-Regeln lassen sich danach Modell für Modell ergänzen.

## Kennzahlen

```bash
curl localhost:9090/metrics
curl localhost:9090/healthz
```

Die aussagekräftigsten Zähler sind die, die zeigen, was das System **nicht**
getan hat: `superseded`, `stale`, `deferred_for_protected`,
`best_effort_starved`.
