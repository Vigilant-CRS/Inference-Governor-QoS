#!/usr/bin/env bash
# Beobachtet einen langen Messlauf und gibt je Ereignis genau eine Zeile aus.
#
# Gedacht fuer `Monitor`: jede Zeile auf stdout wird eine Benachrichtigung.
#
# Verwendung:
#   tools/run-watch.sh <ausgabeverzeichnis> <pid> [gesamtfenster] [schrittweite] [intervall_s]
#
# ## Warum dieses Skript eine PID nimmt und kein Prozessmuster
#
# Der erste Versuch benutzte `pgrep -f "target/release/soak"` als
# Lebenspruefung. `pgrep -f` durchsucht die vollstaendige Kommandozeile — und
# das Muster stand in der Kommandozeile der Schleife selbst. Sie fand also
# sich selbst, die Abbruchbedingung wurde nie wahr, und der Abschluss wurde
# nie gemeldet: der Dauerlauf war um 01:52 fertig, der Watcher lief bis 05:48
# weiter. Eine Beobachtung, die still bleibt, sieht genauso aus wie ein Lauf,
# der noch laeuft — der teuerste Fehler, den ein Watcher machen kann.
#
# Der naheliegende Ausweg ist der Klammertrick: aus `soak` wird `[s]oak`. Er
# reicht nicht. Wird das Muster als **Argument** uebergeben, steht es wieder
# unverklammert in der eigenen Kommandozeile — und in der des `timeout`, das
# davorsteht. Ein Test mit einem kurzlebigen Prozess hat genau das gezeigt:
# der Watcher lief nach dessen Ende weiter.
#
# Eine PID kann sich nicht selbst treffen. Geprueft wird aber nicht mit
# `kill -0`, sondern ueber `/proc`: ein beendeter Prozess, den sein Elternteil
# noch nicht abgeholt hat, bleibt als Zombie in der Prozesstabelle stehen, und
# `kill -0` gelingt bei ihm weiterhin. Ein Zombie ist ein fertiger Lauf — auch
# das hat der Test gezeigt, und zwar erst im zweiten Durchgang.
#
# ## Schweigen ist kein Erfolg
#
# Gemeldet wird nicht nur der Abschluss, sondern auch Abbruch ohne
# Abschlussvermerk sowie Panics und Fehlerzeilen im Protokoll. Wenn der Lauf
# jetzt abstuerzte, kaeme eine Zeile.
set -u

OUT=${1:?Ausgabeverzeichnis fehlt}
PID=${2:?PID des Laufs fehlt}
TOTAL=${3:-480}
STEP=${4:-60}
# Abfrageintervall. Einstellbar, weil ein Test mit 30 Sekunden Wartezeit je
# Runde nicht praktikabel ist — und ein Watcher, der nur unter seinen eigenen
# Voreinstellungen laeuft, ist ungetestet.
INTERVAL=${5:-30}
CSV="$OUT/streams.csv"
LOG="$OUT/console.log"

windows() {
  [ -r "$CSV" ] || { echo 0; return; }
  local lines
  lines=$(wc -l < "$CSV")
  # Kopfzeile abziehen, drei Zeilen je Fenster (ein Strom je Zeile).
  echo $(( (lines - 1) / 3 ))
}

rss() {
  awk -F, 'END{print ($16 == "" ? "?" : $16)}' "$CSV" 2>/dev/null || echo "?"
}

# `grep -c` gibt bei null Treffern 0 aus **und** beendet mit 1. Ein
# `|| echo 0` dahinter haengt eine zweite Null an und macht die Zahl
# unbrauchbar — genau daran ist die erste Fassung dieses Skripts gescheitert.
count_errors() {
  [ -r "$LOG" ] || { echo 0; return; }
  local n
  n=$(grep -ciE "panicked|ERROR|FATAL" "$LOG" 2>/dev/null) || n=0
  echo "${n:-0}"
}

# Laeuft der Prozess noch? Ein Zombie zaehlt als beendet.
running() {
  # Erst die Lesbarkeit pruefen: sonst meldet die Shell den fehlgeschlagenen
  # Redirect auf stderr, und die Ausgabedatei fuellt sich mit Rauschen.
  [ -r "/proc/$PID/stat" ] || return 1
  local stat
  stat=$(tr '\0' ' ' < "/proc/$PID/stat" 2>/dev/null) || return 1
  # Feld 3 ist der Zustand; der Prozessname in Feld 2 kann Leerzeichen und
  # Klammern enthalten, deshalb wird hinter der letzten Klammer geschnitten.
  local state
  state=$(printf '%s' "${stat##*) }" | cut -d' ' -f1)
  [ "$state" != "Z" ]
}

last_reported=0
errors_reported=0

while true; do
  n=$(count_errors)
  if [ "$n" -gt "$errors_reported" ]; then
    echo "FEHLERZEILEN im Lauf: $n insgesamt — $LOG"
    errors_reported=$n
  fi

  if ! running; then
    w=$(windows)
    if grep -q "Fertig\." "$LOG" 2>/dev/null; then
      echo "LAUF FERTIG: $w von $TOTAL Fenstern. Auswertung: tools/soak-report.py $OUT"
    else
      echo "LAUF ABGEBROCHEN nach $w von $TOTAL Fenstern — Prozess weg, kein Abschlussvermerk. Ausgabe: $LOG"
    fi
    exit 0
  fi

  w=$(windows)
  if [ "$w" -ge $(( last_reported + STEP )) ]; then
    last_reported=$w
    pct=0
    [ "$TOTAL" -gt 0 ] && pct=$(( w * 100 / TOTAL ))
    echo "Lauf bei $w von $TOTAL Fenstern ($pct %), RSS $(rss) kB"
  fi
  sleep "$INTERVAL"
done
