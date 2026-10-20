#!/usr/bin/env bash
# Configuration reloads: exclusions by name and by path take effect without a restart (with
# --reload and SIGHUP), and an invalid file is rejected while the old configuration stays.
# Needs root.
set -euo pipefail
cd "$(dirname "$0")/../.."
. tests/integration/common.sh
need_root
trap cleanup_common EXIT
setup_work
make_fs "${FSTYPE:-ext4}"
mkdir -p "$MNT/app/node_modules/pkg" "$MNT/skip/deep" "$MNT/keep"
touch "$MNT/app/node_modules/pkg/index.js" "$MNT/skip/deep/file" "$MNT/keep/file"
start_daemon
count() { "$FOSSICK" --socket "$SOCK" --count "path:\"$MNT/\" $1"; }
# wait_count QUERY N: until the query has N results.
wait_count() {
  for _ in $(seq 1 100); do
    [ "$(count "$1")" = "$2" ] && return 0
    sleep 0.1
  done
  die "expected $2 results for $1, got $(count "$1")"
}
wait_count "index.js" 1
wait_count "deep" 1

note "exclude node_modules (by name) and $MNT/skip (by path), then --reload"
sed -i "/^\[index\]/a exclude_names = [\"node_modules\", \"*.tmp\"]\nexclude = [\"$MNT/skip\"]" "$WORK/fossickd.toml"
"$FOSSICK" --socket "$SOCK" --reload
wait_count "index.js" 0
wait_count "node_modules" 0
wait_count "deep" 0
wait_count "keep" 1
echo "ok: excluded entries removed"
mkdir -p "$MNT/other/node_modules"
touch "$MNT/other/node_modules/new.js" "$MNT/keep/x.tmp" "$MNT/keep/live.txt"
wait_count "live.txt" 1
settle
wait_count "new.js" 0
wait_count "x.tmp" 0
echo "ok: new entries matching the exclusions are not indexed"

note "an invalid configuration is rejected and the old one stays"
cp "$WORK/fossickd.toml" "$WORK/good.toml"
echo "bogus_key = 1" >> "$WORK/fossickd.toml"
code=0
"$FOSSICK" --socket "$SOCK" --reload 2> "$WORK/reload.err" || code=$?
[ "$code" = 7 ] || die "expected exit code 7 for an invalid configuration, got $code"
touch "$MNT/keep/still-live.txt"
wait_count "still-live.txt" 1
echo "ok: rejected ($(head -c 120 "$WORK/reload.err"))"

note "SIGHUP reloads too: drop the exclusions again"
grep -v "^exclude" "$WORK/good.toml" > "$WORK/fossickd.toml"
kill -HUP "$DAEMON_PID"
wait_count "index.js" 1
wait_count "new.js" 1
wait_count "deep" 1
echo "ok: entries are back after SIGHUP"
echo "PASS reload"
