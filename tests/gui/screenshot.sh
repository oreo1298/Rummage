#!/usr/bin/env bash
# Starts a user-mode daemon over a small fixture tree and saves screenshots of the Qt GUI
# (offscreen). Run inside an environment with Qt ≥ 6.8 and Kirigami (see CLAUDE.md):
#
#   tests/gui/screenshot.sh OUT_DIR [BIN_DIR]
set -euo pipefail
OUT=${1:?output directory}
BIN=${2:-target/debug}
mkdir -p "$OUT"
WORK=$(mktemp -d)
trap 'kill $(jobs -p) 2> /dev/null || true; rm -rf "$WORK"' EXIT
export HOME="$WORK/home" XDG_RUNTIME_DIR="$WORK/run" XDG_CONFIG_HOME="$WORK/config"
mkdir -p "$HOME/Documents/Reports" "$HOME/Music" "$HOME/Pictures" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
for i in $(seq 1 40); do echo "report $i" > "$HOME/Documents/Reports/report-$i.pdf"; done
printf 'x%.0s' $(seq 1 5000) > "$HOME/Documents/notes.txt"
touch "$HOME/Music/song.flac" "$HOME/Pictures/holiday.jpg" "$HOME/Pictures/café.png"
"$BIN/fossickd" --user > "$WORK/daemon.log" 2>&1 &
for _ in $(seq 1 100); do
  [ -S "$XDG_RUNTIME_DIR/fossick/socket" ] && break
  sleep 0.1
done
export FOSSICK_SOCKET="$XDG_RUNTIME_DIR/fossick/socket"
"$BIN/fossick-qt" --screenshot "$OUT/empty-query.png" 2500
echo "saved $OUT/empty-query.png"
"$BIN/fossick-qt" --search report --screenshot "$OUT/query.png" 2500
echo "saved $OUT/query.png"
for page in general search index shortcut; do
  "$BIN/fossick-qt" --settings "$page" --screenshot "$OUT/settings-$page.png" 2500
  echo "saved $OUT/settings-$page.png"
done

# Single instance: a second launch passes its search to the running window (new tab).
if command -v dbus-run-session > /dev/null; then
  dbus-run-session -- bash -c '
    FOSSICK_SINGLE_INSTANCE=1 "$0/fossick-qt" --screenshot "$1/second-instance.png" 4000 &
    first=$!
    sleep 1.5
    timeout 10 "$0/fossick" --gui holiday || { echo "FAIL: fossick --gui did not hand over"; exit 1; }
    wait $first
  ' "$BIN" "$OUT"
  echo "saved $OUT/second-instance.png"
fi

# Large result sets: the window has to go idle with a modest footprint (three launches). This
# is a smoke test: the table once built a row for every result (hundreds of MiB, a busy main
# thread) when results arrived before the first layout settled, a race seen in release builds
# that this debug-build check doesn’t reliably reproduce.
mkdir -p "$HOME/many"
(cd "$HOME/many" && seq 1 70000 | sed 's/^/f/' | xargs touch)
for _ in $(seq 1 100); do
  n=$("$BIN/fossick" --socket "$FOSSICK_SOCKET" --count '' 2> /dev/null || echo 0)
  [ "$n" -ge 70000 ] && break
  sleep 0.2
done
for i in 1 2 3; do
  DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent "$BIN/fossick-qt" --search "" > /dev/null 2>&1 &
  pid=$!
  sleep 3
  t0=$(awk '{print $14 + $15}' "/proc/$pid/stat")
  sleep 2
  t1=$(awk '{print $14 + $15}' "/proc/$pid/stat")
  rss=$(awk '/VmRSS/ {print int($2 / 1024)}' "/proc/$pid/status")
  kill "$pid"
  wait "$pid" 2> /dev/null || true
  echo "large result set, launch $i: $((t1 - t0)) CPU ticks in 2 s when idle, ${rss} MiB"
  if [ $((t1 - t0)) -gt 50 ] || [ "$rss" -gt 250 ]; then
    echo "FAIL: the window doesn't go idle with $n results"
    exit 1
  fi
done
