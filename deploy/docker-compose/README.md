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

Im Compose-Netz heisst das Backend `triton`, nicht `127.0.0.1`:

```yaml
backend:
  grpc_endpoint: "triton:8001"
```

## 4. Starten

```bash
export VIG_MODELS=/pfad/zum/modellrepository
export VIG_CONFIG=$PWD/vig.yaml
docker compose up -d
```

## 5. Prüfen

```bash
docker compose run --rm vig doctor -c /etc/vig/vig.yaml
```

`doctor` laeuft im selben Netz wie der Governor und erreicht Triton deshalb
unter demselben Namen. `doctor` sagt vor dem ersten Request, was nicht funktionieren wird — und zwar
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

## Was diese Datei absichert — und was nicht

Die Voreinstellung ist **ein Gerät, ein Betreiber**:

* Beide Ports (`9001` gRPC, `9090` Metriken) sind nur auf `127.0.0.1` des
  Hosts veröffentlicht. Triton hat gar keinen veröffentlichten Port; es ist
  nur im Compose-Netz erreichbar, also nie am Governor vorbei.
* `vig` startet mit `--insecure-open`, weil es im Container auf `0.0.0.0`
  lauschen muss. Das ist nur richtig, solange die Ports auf Loopback des
  Hosts bleiben. Ohne das Flag verweigert `vig serve` jeden Start ausserhalb
  von Loopback ohne mTLS oder Token.
* `vig` läuft mit schreibgeschütztem Dateisystem, ohne Capabilities und mit
  `no-new-privileges`.
* Triton braucht `ipc: host` für Shared Memory mit den Clients auf dem Host.
  Das teilt den IPC-Namensraum des Hosts; jeder Prozess dort, der auf
  `/dev/shm` schreiben darf, kann Regionen anlegen, die Triton liest. Das ist
  das dokumentierte Restrisiko dieser Datei
  ([`docs/security.md`](../../docs/security.md)).
* Die Administrationsendpunkte (Modelle laden/entladen, Tracing, Loglevel,
  CUDA-Speicher) sind gesperrt, bis `backend.security.admin_token_file`
  gesetzt ist.

### Zugriff von einem anderen Rechner

Nicht die Portbindung auf `0.0.0.0` ändern und `--insecure-open` stehen
lassen. Stattdessen:

1. In `vig.yaml` eine Identitätsprüfung einrichten und den strikten Modus
   einschalten:

   ```yaml
   backend:
     trust: strict
     security:
       tls_cert: /etc/vig/tls/server.pem
       tls_key: /etc/vig/tls/server.key
       client_ca: /etc/vig/tls/clients.pem     # mTLS; oder:
       token_file: /etc/vig/tokens              # "name:token" je Zeile, >= 16 Zeichen
       admin_token_file: /etc/vig/admin.tokens  # nur, wenn Administration gebraucht wird
   ```

2. Die Dateien schreibgeschützt einhängen (`volumes:` von `vig`).
3. In `docker-compose.yml` `--insecure-open` aus `command:` entfernen und die
   Portbindung für `9001` auf die gewünschte Adresse ändern. `9090` (Metriken,
   ohne Authentifizierung) bleibt auf Loopback; ein Prometheus auf einem
   anderen Rechner liest über einen Tunnel oder eine authentifizierende
   Instanz davor.
4. `docker compose run --rm vig doctor -c /etc/vig/vig.yaml` — `doctor` meldet
   eine Konfiguration, die `serve` verweigern würde.

Die ganze Liste steht in [`docs/security.md`](../../docs/security.md).

## Kennzahlen

```bash
curl localhost:9090/metrics
curl localhost:9090/healthz
```

Die aussagekräftigsten Zähler sind die, die zeigen, was das System **nicht**
getan hat: `superseded`, `stale`, `deferred_for_protected`,
`best_effort_starved`.
