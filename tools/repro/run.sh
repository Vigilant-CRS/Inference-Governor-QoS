#!/usr/bin/env bash
# Das Reproduktionspaket: ein Befehl, drei Lastfaelle, oeffentliche Modelle.
#
#   tools/repro/run.sh [--with-llm] [arbeitsverzeichnis]
#
# `--with-llm` nimmt den dritten Lastfall dazu — Detektor neben einem echten
# Sprachmodell. Das kostet ein zweites Triton-Image (rund 35 GB) und 1,5 GB
# Gewichte; ohne den Schalter laufen die beiden Detektor-Lastfaelle
# vollstaendig.
#
# Es beantwortet die Frage, die dieses Projekt tragen muss, auf **deiner**
# Hardware: lohnt sich der Governor hier, und mit wie viel? Alle Modelle sind
# Apache-2.0 und frei ladbar (tools/repro/fetch-models.sh).
#
# Was es tut, in dieser Reihenfolge:
#   1. Voraussetzungen pruefen — und bei fehlenden **abbrechen**, nicht
#      irgendwie weitermachen.
#   2. Modelle holen und gegen feste SHA-256 pruefen.
#   3. Triton starten (dein laufender bleibt unberuehrt: eigene Ports).
#   4. `vig init` zeigen — wie eine erste Konfiguration entsteht.
#   5. **Die Profile auf dieser Maschine messen.** Die im Repository
#      hinterlegten Zahlen stammen von einer RTX 3070 Laptop und gelten dort
#      und nur dort (Spec 13.5). Gemessen wird mit `vig calibrate`.
#   6. Die drei Lastfaelle fahren und `vig-fit` ausgeben.
#
# Was es **nicht** tut: es misst Versorgung, nicht Erkennungsqualitaet. Und
# es ist kein Abnahmelauf — dafuer sind die Laufzeiten zu kurz.
set -uo pipefail

HIER=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd -- "$HIER/../.." && pwd)

# Der Schalter ist ein Argument und keine blosse Umgebungsvariable: einen
# Schalter, den man nicht sieht, benutzt niemand. VIG_REPRO_WITH_LLM bleibt
# als Alias, damit sich das Skript aus einem anderen heraus steuern laesst.
MIT_LLM=${VIG_REPRO_WITH_LLM:-0}
ARBEIT=""
for arg in "$@"; do
  case "$arg" in
    --with-llm) MIT_LLM=1 ;;
    -*) echo "Unbekannte Option: $arg" >&2; exit 2 ;;
    *) ARBEIT=$arg ;;
  esac
done
ARBEIT=${ARBEIT:-$PWD/repro-arbeit}
MODELLE=$ARBEIT/modelle
ERGEBNIS=$ARBEIT/ergebnis
CONTAINER=vig-repro-triton
HTTP_PORT=${VIG_REPRO_HTTP_PORT:-8100}
GRPC_PORT=${VIG_REPRO_GRPC_PORT:-8101}
METRICS_PORT=${VIG_REPRO_METRICS_PORT:-8102}
# Per Digest gepinnt: derselbe Tag kann ein anderes Image bedeuten.
IMAGE=${VIG_REPRO_IMAGE:-nvcr.io/nvidia/tritonserver:26.06-py3}

mkdir -p "$ERGEBNIS"

log() { echo "$(date '+%F %T') $*" | tee -a "$ERGEBNIS/ablauf.txt"; }
fehler() { echo "FEHLER: $*" >&2; exit 1; }

LLM_CONTAINER=vig-repro-vllm
aufraeumen() {
  [ -n "${SAUBER:-}" ] && return
  SAUBER=1
  # Beide, und auch bei Abbruch: ein liegengebliebenes vLLM haelt seinen
  # GPU-Speicher bis zum naechsten Neustart fest.
  log "raeumt auf: Container $CONTAINER, $LLM_CONTAINER"
  docker rm -f "$CONTAINER" "$LLM_CONTAINER" >/dev/null 2>&1
}
trap 'log "ABGEBROCHEN"; aufraeumen; exit 130' TERM INT
trap aufraeumen EXIT

# --- 1. Voraussetzungen ------------------------------------------------------
log "prueft Voraussetzungen"
for werkzeug in docker curl sha256sum; do
  command -v "$werkzeug" >/dev/null || fehler "$werkzeug fehlt"
done

# `--device nvidia.com/gpu=all` und nicht `--gpus all`: Docker ab Version 29
# loest `--gpus` ueber CDI auf und raet den Hersteller dabei falsch
# ("failed to discover GPU vendor from CDI"). Der explizite CDI-Geraetename
# funktioniert, sobald `nvidia-cdi-refresh` die Spezifikation erzeugt hat
# (deploy/triton/README.md). Wer eine aeltere Umgebung hat, setzt
# VIG_REPRO_GPU_FLAG="--gpus all".
GPU_FLAG=${VIG_REPRO_GPU_FLAG:---device nvidia.com/gpu=all}

# Eine GPU im Container ist die Voraussetzung, an der es am haeufigsten
# scheitert, und sie scheitert leise: Triton startet auch ohne und rechnet
# dann auf der CPU. Deshalb hier und nicht spaeter.
# shellcheck disable=SC2086
docker run --rm $GPU_FLAG "$IMAGE" nvidia-smi -L >"$ERGEBNIS/gpu.txt" 2>&1 \
  || fehler "keine GPU im Container. Siehe deploy/triton/README.md"
log "GPU im Container: $(head -1 "$ERGEBNIS/gpu.txt")"

# `CARGO_TARGET_DIR` hat Vorrang: wer sein Repository auf einer langsamen oder
# knappen Platte liegen hat, legt die Bauartefakte woanders ab, und dann steht
# dort kein `target/`.
BIN_DIR=${CARGO_TARGET_DIR:-$REPO/target}/release
VIG=${VIG_BIN:-$BIN_DIR/vig}
FIT=${VIG_FIT_BIN:-$BIN_DIR/vig-fit}
GATE=${VIG_GATE_BIN:-$BIN_DIR/gate-m3}
for binaer in "$VIG" "$FIT" "$GATE"; do
  [ -x "$binaer" ] || fehler "$binaer fehlt. Erst bauen: cargo build --release --workspace"
done

# --- 2. Modelle --------------------------------------------------------------
log "holt die Modelle nach $MODELLE"
HOLEN=("$HIER/fetch-models.sh")
[ "$MIT_LLM" = 1 ] && HOLEN+=(--with-llm)
HOLEN+=("$MODELLE")
"${HOLEN[@]}" >>"$ERGEBNIS/ablauf.txt" 2>&1 \
  || fehler "Modelle nicht beschaffbar (Digest? Netz?)"

# --- 3. Triton ---------------------------------------------------------------
# Eigene Ports und eigener Name: ein bereits laufender Triton bleibt
# unberuehrt.
#
# Die Ports werden **nur auf dem Loopback des Hosts** veroeffentlicht und
# nicht ueber `--network host`: wer Triton direkt erreicht, umgeht Token,
# Frische und Budget des Governors (docs/security.md). Fuer einen Messaufbau
# gilt das genauso wie fuer ein Deployment.
log "startet Triton als $CONTAINER auf 127.0.0.1:$HTTP_PORT/$GRPC_PORT"
docker rm -f "$CONTAINER" >/dev/null 2>&1
# shellcheck disable=SC2086
# `--ipc=host` und `--allow-client-shm=true` gehoeren zusammen und sind nicht
# optional: der Lastfall (a) fuehrt seine Tensoren ueber System Shared Memory
# durch, und ohne beides bricht er mit
#   "Client shared memory is disabled. Start the server with
#    '--allow-client-shm=true' to enable."
# ab. Der Container hat sonst sein eigenes /dev/shm und findet die Region des
# Clients nicht (ADR-0003, deploy/triton/README.md).
#
# Das ist zugleich die groesste verbleibende Flaeche dieses Aufbaus: Triton
# sieht damit das /dev/shm des Hosts. Deshalb bleiben die Ports auf dem
# Loopback (docs/security.md).
docker run -d --name "$CONTAINER" $GPU_FLAG --ipc=host \
  -p "127.0.0.1:$HTTP_PORT:$HTTP_PORT" \
  -p "127.0.0.1:$GRPC_PORT:$GRPC_PORT" \
  -p "127.0.0.1:$METRICS_PORT:$METRICS_PORT" \
  -v "$MODELLE:/models:ro" "$IMAGE" \
  tritonserver --model-repository=/models --rate-limit=execution_count \
  --allow-client-shm=true \
  --http-port="$HTTP_PORT" --grpc-port="$GRPC_PORT" --metrics-port="$METRICS_PORT" \
  >/dev/null || fehler "Triton startet nicht"

for _ in $(seq 1 120); do
  curl -sf "http://127.0.0.1:$HTTP_PORT/v2/health/ready" >/dev/null 2>&1 && break
  sleep 1
done
curl -sf "http://127.0.0.1:$HTTP_PORT/v2/health/ready" >/dev/null 2>&1 || {
  docker logs "$CONTAINER" > "$ERGEBNIS/triton-log.txt" 2>&1
  fehler "Triton wurde nicht bereit. Log: $ERGEBNIS/triton-log.txt"
}
curl -s -X POST "http://127.0.0.1:$HTTP_PORT/v2/repository/index" | tee "$ERGEBNIS/modelle.json"
echo

# --- 4. vig init -------------------------------------------------------------
# Nicht, weil das Ergebnis hier gebraucht wird, sondern weil es der Weg ist,
# den ein Anwender mit seinen eigenen Modellen geht: die Maschine traegt ein,
# was sie weiss, und laesst offen, was nur der Betreiber weiss.
log "vig init gegen das laufende Backend"
"$VIG" init --endpoint "127.0.0.1:$GRPC_PORT" --out "$ERGEBNIS/init.yaml" --force \
  >>"$ERGEBNIS/ablauf.txt" 2>&1 || log "  vig init meldete einen Fehler (siehe ablauf.txt)"

# --- 5. Profile auf dieser Maschine messen ----------------------------------
# Die Zahlen in tools/repro/scenarios/ stammen von **einer** Maschine. Sie
# hier zu uebernehmen hiesse, mit fremden Laufzeiten zu planen; der Governor
# entscheidet auf Grundlage dieser Zahlen ueber Zulassung und Variantenwahl.
# Lastfall (a) steht bewusst **nicht** in dieser Schleife: ihn faehrt `wp26`,
# und das Werkzeug baut seine Konfiguration selbst aus Umgebungsvariablen. Es
# hier zu kalibrieren hiesse, gegen ein Modell zu messen, das ohne
# `--with-llm` gar nicht geladen ist.
for fall in b-vier-kameras c-zwei-groessen; do
  vorlage=$HIER/scenarios/$fall.yaml
  [ -f "$vorlage" ] || { log "  $fall: Vorlage fehlt, uebersprungen"; continue; }
  gemessen=$ERGEBNIS/$fall.gemessen.yaml
  log "misst die Profile fuer $fall"
  sed "s#__ENDPUNKT__#127.0.0.1:$GRPC_PORT#g" "$vorlage" > "$ERGEBNIS/$fall.vorlage.yaml"
  varianten=$(grep -c "backend_model:" "$ERGEBNIS/$fall.vorlage.yaml" 2>/dev/null) || varianten=0

  # Mehrere Anlaeufe, und zwar aus einem gemessenen Grund: `vig calibrate`
  # verwirft eine Messreihe, wenn die Karte waehrenddessen ihren Takt
  # aendert. Auf einem Laptop unter Leistungslimit ist das der Normalfall.
  # Je Variante eine eigene Reihe heisst: bei vier Kameraverträgen auf
  # demselben Modell multipliziert sich die Verwurfwahrscheinlichkeit.
  #
  # Zuerst auf dem Vertragsraster — das ist die Zahl, die in einen Vertrag
  # gehoert. Bleibt sie unerreichbar, dann Ruecken an Ruecken, **und das
  # steht dann auch so im Protokoll**: es ist eine Aussage ueber Kapazitaet
  # und nicht ueber das Verhalten unter einem Takt, also optimistisch.
  # Hintergrund: docs/benchmark/reproduce.md, Stolperstein 3.
  profile=0
  for versuch in raster-1 raster-2 raster-3 rueckenanruecken-1 rueckenanruecken-2; do
    case "$versuch" in
      raster-*) takt=(--periodic-us 33000) ;;
      *) takt=() ;;
    esac
    log "  $fall: calibrate ($versuch)"
    # `--model-repository` ist kein Beiwerk: ohne den Pfad bleibt der
    # Artefakt-Digest des Profils `unknown`, und eine unter derselben
    # Versionsnummer ausgetauschte Gewichtsdatei faellt nicht auf (ADR-0019).
    # `doctor` meldet sonst "Profilherkunft unvollstaendig belegt".
    "$VIG" calibrate -c "$ERGEBNIS/$fall.vorlage.yaml" -o "$gemessen" \
      --model-repository "$MODELLE" \
      --samples "${VIG_REPRO_SAMPLES:-100}" "${takt[@]}" \
      >>"$ERGEBNIS/ablauf.txt" 2>&1
    [ -f "$gemessen" ] || continue
    profile=$(grep -c "p50_us:" "$gemessen" 2>/dev/null) || profile=0
    log "    $profile von $varianten Profilen"
    if [ "$profile" -ge "$varianten" ] && [ "$varianten" -gt 0 ]; then
      case "$versuch" in
        rueckenanruecken-*)
          log "    ACHTUNG Ruecken an Ruecken gemessen, nicht auf dem Raster."
          log "            Die Profile sind damit eine Kapazitaetsaussage und"
          log "            optimistisch. Ursache und Abhilfe: reproduce.md."
          ;;
      esac
      break
    fi
  done
  [ -f "$gemessen" ] || { log "  $fall: calibrate hat nichts geschrieben"; continue; }

  # **Nachzaehlen, nicht dem Exitcode glauben.** `vig calibrate` endet auch
  # dann mit Erfolg, wenn es jede einzelne Messreihe verworfen hat, und
  # laesst das Profil der Vorlage unveraendert stehen (`apply()`
  # ueberspringt Varianten ohne qualifizierte Messung). Die Vorlagen dieses
  # Pakets tragen deshalb **keine** Profile: fehlt danach eines, wurde es
  # nicht gemessen, und das ist hier ein Abbruchgrund.
  #
  # Der haeufigste Grund auf einem Laptop ist ein wandernder Takt unter
  # Leistungslimit. Das ist kein Messfehler, sondern die Karte: eine Messung,
  # die ueber mehrere Betriebspunkte mittelt, beschreibt keinen davon.
  varianten=$(grep -c "backend_model:" "$ERGEBNIS/$fall.vorlage.yaml" 2>/dev/null) || varianten=0
  profile=$(grep -c "p50_us:" "$gemessen" 2>/dev/null) || profile=0
  if [ "$profile" -lt "$varianten" ]; then
    log "  $fall: NICHT GEMESSEN — $profile von $varianten Profilen."
    log "    Die Messreihen wurden verworfen (Details in ablauf.txt). Auf einer"
    log "    Karte unter Leistungslimit hilft ein festgehaltener Takt:"
    log "      sudo nvidia-smi -pm 1 && sudo nvidia-smi -lgc <mhz>"
    log "    Ohne gemessene Profile wird dieser Lastfall uebersprungen — mit"
    log "    fremden Laufzeiten zu planen waere schlimmer als gar keine Zahl."
    continue
  fi

  "$VIG" doctor -c "$gemessen" > "$ERGEBNIS/$fall.doctor.txt" 2>&1
  log "  doctor: $(tail -1 "$ERGEBNIS/$fall.doctor.txt")"
done

# --- 6. Die Lastfaelle -------------------------------------------------------
# `vig-fit` beantwortet die Frage des Interessenten in einem Satz, ueber
# mehrere Lastpunkte. „Brauchst du nicht" ist ein normales Ergebnis.
for fall in b-vier-kameras c-zwei-groessen; do
  gemessen=$ERGEBNIS/$fall.gemessen.yaml
  [ -f "$gemessen" ] || continue
  log "vig-fit fuer $fall"
  VIG_FIT_JSON="$ERGEBNIS/$fall.fit.json" "$FIT" "$gemessen" \
    | tee "$ERGEBNIS/$fall.fit.txt"
done

# Der Gate-M3-artige Vergleich: derselbe Workload einmal direkt gegen Triton
# und einmal ueber den Governor, mit Fenster- **und** Verbrauchersicht.
gemessen=$ERGEBNIS/c-zwei-groessen.gemessen.yaml
if [ -f "$gemessen" ]; then
  log "Gate-M3-artiger Vergleich (Kopierpfad, damit es ohne /dev/shm laeuft)"
  VIG_GATE_COPY=1 VIG_GATE_SECONDS=${VIG_REPRO_GATE_SECONDS:-30} \
    "$GATE" "$gemessen" | tee "$ERGEBNIS/gate.txt"
fi

# --- Lastfall (a): Detektor neben einem echten Sprachmodell -----------------
# Nur mit `--with-llm`, und das aus einem sachlichen Grund: das vLLM-Backend
# steckt in einem **anderen** Triton-Image (rund 35 GB) und laesst sich nicht
# mit dem onnxruntime-Backend in einen Prozess legen. Wer das nicht holen
# will, bekommt die Lastfaelle (b) und (c) trotzdem vollstaendig.
if [ "$MIT_LLM" = 1 ]; then
  WP26=${VIG_WP26_BIN:-$BIN_DIR/wp26}
  LLM_IMAGE=${VIG_REPRO_LLM_IMAGE:-nvcr.io/nvidia/tritonserver:26.06-vllm-python-py3}
  LLM_HTTP=${VIG_REPRO_LLM_HTTP_PORT:-8110}
  LLM_GRPC=${VIG_REPRO_LLM_GRPC_PORT:-8111}

  # Das Detektorprofil fuer `wp26` stammt aus der Kalibrierung dieser
  # Maschine. Ohne das plante der Governor in Lastfall (a) mit den
  # Vorgabewerten eines ganz anderen Modells — und eine Messung, deren Plan
  # auf fremden Laufzeiten beruht, sagt nichts ueber diese hier. Laesst sich
  # das Profil nicht lesen, wird der Lastfall **uebersprungen** und nicht mit
  # geratenen Zahlen gefahren.
  kalibriert=$ERGEBNIS/c-zwei-groessen.gemessen.yaml
  P50=""; P95=""; P99=""
  if [ -f "$kalibriert" ]; then
    # Die Zahl steht **hinter** dem Schluessel. Sie aus dem Treffer zu
    # schneiden statt ihn an Nichtziffern zu zerlegen, ist keine Stilfrage:
    # `p50_us` enthaelt selbst die Ziffern „50", und die zerlegende Fassung
    # lieferte still 50/95/99 statt der Messwerte.
    # Deckt Block- und Inline-Schreibweise ab.
    werte=$(awk '
      /backend_model:[[:space:]]*rtdetr_r18/ { gefunden = 1 }
      gefunden && match($0, /p50_us:[[:space:]]*[0-9]+/) { s = substr($0, RSTART, RLENGTH); sub(/^p50_us:[[:space:]]*/, "", s); p50 = s }
      gefunden && match($0, /p95_us:[[:space:]]*[0-9]+/) { s = substr($0, RSTART, RLENGTH); sub(/^p95_us:[[:space:]]*/, "", s); p95 = s }
      gefunden && match($0, /p99_us:[[:space:]]*[0-9]+/) { s = substr($0, RSTART, RLENGTH); sub(/^p99_us:[[:space:]]*/, "", s); p99 = s }
      p50 != "" && p95 != "" && p99 != "" { print p50, p95, p99; exit }
    ' "$kalibriert")
    read -r P50 P95 P99 <<<"$werte"
  fi

  if [ ! -x "$WP26" ]; then
    log "Lastfall (a) uebersprungen: $WP26 fehlt"
  elif [ ! -d "$MODELLE-llm" ]; then
    log "Lastfall (a) uebersprungen: Sprachmodell fehlt (fetch-models.sh --with-llm)"
  elif [ -z "$P50" ] || [ -z "$P95" ] || [ -z "$P99" ]; then
    log "Lastfall (a) uebersprungen: kein gemessenes Detektorprofil in $kalibriert"
  else
    log "startet das vLLM-Backend als $LLM_CONTAINER auf 127.0.0.1:$LLM_GRPC"
    docker rm -f "$LLM_CONTAINER" >/dev/null 2>&1
    # shellcheck disable=SC2086
    docker run -d --name "$LLM_CONTAINER" $GPU_FLAG \
      -p "127.0.0.1:$LLM_HTTP:$LLM_HTTP" -p "127.0.0.1:$LLM_GRPC:$LLM_GRPC" \
      -v "$MODELLE-llm:/models:ro" "$LLM_IMAGE" \
      tritonserver --model-repository=/models \
      --http-port="$LLM_HTTP" --grpc-port="$LLM_GRPC" --allow-metrics=false \
      >/dev/null || log "vLLM-Backend startet nicht"

    # Grosszuegig: das Modell wird beim ersten Start geladen und kompiliert.
    for _ in $(seq 1 300); do
      curl -sf "http://127.0.0.1:$LLM_HTTP/v2/health/ready" >/dev/null 2>&1 && break
      sleep 2
    done

    if curl -sf "http://127.0.0.1:$LLM_HTTP/v2/health/ready" >/dev/null 2>&1; then
      log "Lastfall (a): Detektor neben dem Sprachmodell (wp26)"
      VIG_WP26_DETECTOR_ENDPOINT="127.0.0.1:$GRPC_PORT" \
      VIG_WP26_LLM_ENDPOINT="127.0.0.1:$LLM_GRPC" \
      VIG_WP26_DETECTOR_MODEL=rtdetr_r18 \
      VIG_WP26_LLM_MODEL=qwen \
      VIG_WP26_DETECTOR_P50_US="$P50" \
      VIG_WP26_DETECTOR_P95_US="$P95" \
      VIG_WP26_DETECTOR_P99_US="$P99" \
        "$WP26" | tee "$ERGEBNIS/a-detektor-und-sprachmodell.txt"
    else
      docker logs "$LLM_CONTAINER" > "$ERGEBNIS/vllm-log.txt" 2>&1
      log "Lastfall (a): vLLM wurde nicht bereit, Log in vllm-log.txt"
    fi
    docker rm -f "$LLM_CONTAINER" >/dev/null 2>&1
  fi
fi

log "fertig. Alles unter $ERGEBNIS"
echo
echo "Die Zahlen gelten fuer diese Maschine und diese Vertraege. Gemessen"
echo "wurde die Versorgung, nicht die Erkennungsqualitaet."
