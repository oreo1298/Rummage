#!/usr/bin/env bash
# Event storms: after random creates, renames, moves and deletes, and after changes made while
# the daemon was stopped, the index must equal a fresh walk of the filesystem (paths, kinds and
# file sizes). Needs root.
#
#   sudo FSTYPE=xfs OPS=20000 BIN_SRC=target/release tests/integration/storm.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
. tests/integration/common.sh
need_root
trap cleanup_common EXIT
FSTYPE=${FSTYPE:-ext4}
OPS=${OPS:-10000}
# Polling sees appends (which don't touch the directory) only in full polls: make every poll
# full, so sizes can be compared within the wait.
if [ "${WATCH:-}" = poll ]; then export POLL_FULL_EVERY=${POLL_FULL_EVERY:-1}; fi
setup_work
make_fs "$FSTYPE"
M=$MNT
start_daemon
method=$("$FOSSICK" --socket "$SOCK" --status | awk -v m="$M" '$1 == m {print $4}')
echo "$FSTYPE is watched by: $method"

# truth / indexed: "kind size path" lines (size only for regular files).
truth() {
  find "$M" -mindepth 1 -printf '%y %s %p\n' | awk '{ if ($1 != "f") $2 = "-"; print }' | sort
}
indexed_long() {
  "$FOSSICK" --socket "$SOCK" --json "path:\"$M/\"" | python3 -c '
import json, sys
kinds = {"file": "f", "folder": "d", "symlink": "l", "other": "?"}
for r in json.load(sys.stdin):
    p = bytes.fromhex(r["path_hex"]).decode("utf-8", "surrogateescape") if "path_hex" in r else r["path"]
    k = kinds[r["type"]]
    s = str(r["size"]) if k == "f" else "-"
    sys.stdout.buffer.write(("%s %s " % (k, s)).encode() + p.encode("utf-8", "surrogateescape") + b"\n")
' | sort
}

compare() {
  local label=$1 tries=${2:-${TRIES:-40}}
  local expected got
  expected=$(truth)
  for _ in $(seq 1 "$tries"); do
    got=$(indexed_long)
    [ "$expected" = "$got" ] && break
    sleep 0.25
    expected=$(truth)
  done
  if [ "$expected" != "$got" ]; then
    diff <(echo "$expected") <(echo "$got") | head -30 >&2 || true
    echo "--- daemon log (scans, warnings, errors):" >&2
    grep -av 'fossickd: debug:' "$WORK/daemon.log" | tail -40 >&2 || true
    grep -a -m3 -B20 'invariant broken' "$WORK/daemon.log" >&2 || true
    # With DEBUG=1: what the daemon did about the first mismatched name.
    local first
    first=$(diff <(echo "$expected") <(echo "$got") | grep -a -m1 '^[<>]' | cut -d' ' -f4- || true)
    if [ -n "$first" ] && grep -q 'fossickd: debug:' "$WORK/daemon.log"; then
      # Its last two components: specific enough, and still valid if an ancestor moved.
      local tail2
      tail2="$(basename "$(dirname "$first")")/$(basename "$first")"
      echo "--- debug lines naming $tail2 (and its directory):" >&2
      grep -aF -e "$tail2" -e "/$(basename "$(dirname "$first")") " "$WORK/daemon.log" \
        | tail -40 >&2 || true
      grep -a 'unindexed directory\|not indexed yet\|not applying\|ambiguous' "$WORK/daemon.log" \
        | tail -10 >&2 || true
    fi
    "$FOSSICK" --socket "$SOCK" --status >&2 || true
    die "$label: index differs from the filesystem (< disk, > index)"
  fi
  echo "ok: $label: $(echo "$expected" | wc -l) entries match"
}

note "storm of $OPS operations while watching"
t0=$(date +%s.%N)
python3 tests/integration/storm.py "$M" "$OPS" 1
t1=$(date +%s.%N)
compare "live storm"
t2=$(date +%s.%N)
echo "storm took $(echo "$t1 - $t0" | bc) s; index caught up $(echo "$t2 - $t1" | bc) s later"

note "changes while the daemon is stopped (pruned rescan on restart)"
stop_daemon
python3 tests/integration/storm.py "$M" $((OPS / 4)) 2
start_daemon
compare "after restart"
grep -E "scanned .*unchanged" "$WORK/daemon.log" | tail -1

note "a second live storm on the reloaded index"
python3 tests/integration/storm.py "$M" $((OPS / 2)) 3
compare "second storm"
echo "PASS storm ($FSTYPE, $method)"
