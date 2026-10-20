#!/usr/bin/env bash
# Permission filtering with several real users: each user must see exactly the entries `find`
# can list for them (mode bits, groups, POSIX ACLs, unlistable ancestors), including after
# live changes. Needs root.
#
#   sudo BIN_SRC=target/release tests/integration/multiuser.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
. tests/integration/common.sh
need_root
command -v setfacl > /dev/null || { echo "SKIP: setfacl (package acl) missing"; exit 77; }

USERS=(fsk_alice fsk_bob fsk_carol)
cleanup() {
  cleanup_common
  for u in "${USERS[@]}"; do userdel "$u" 2> /dev/null || true; done
  groupdel fsk_team 2> /dev/null || true
}
trap cleanup EXIT

setup_work
groupadd -f fsk_team
for u in "${USERS[@]}"; do
  id "$u" > /dev/null 2>&1 || useradd --no-create-home --shell /usr/sbin/nologin "$u"
done
usermod -a -G fsk_team fsk_bob

make_fs "${FSTYPE:-ext4}"
M=$MNT
mkdir -p "$M/pub/sub" && touch "$M/pub/a.txt" "$M/pub/sub/b.txt"
mkdir "$M/alice" && touch "$M/alice/secret.txt" && chown -R fsk_alice "$M/alice" && chmod 700 "$M/alice"
mkdir "$M/team" && touch "$M/team/plan.txt" && chgrp fsk_team "$M/team" && chmod 750 "$M/team"
mkdir -p "$M/mid/inner" && touch "$M/mid/inner/x.txt" && chmod 711 "$M/mid"
mkdir "$M/acl" && touch "$M/acl/y.txt" && setfacl -m u:fsk_bob:--- "$M/acl"
mkdir "$M/grpacl" && touch "$M/grpacl/z.txt" && chmod 700 "$M/grpacl" && setfacl -m g:fsk_team:rx "$M/grpacl"
mkdir -p "$M/deep/a/b/c" && touch "$M/deep/a/b/c/d.txt" && chmod 750 "$M/deep/a"
mkdir "$M/x-only" && chmod 701 "$M/x-only" && mkdir "$M/x-only/open" && touch "$M/x-only/open/f"
start_daemon

check_all() {
  local label=$1
  for u in root "${USERS[@]}"; do
    expected=$(runuser -u "$u" -- find "$M" -mindepth 1 2> /dev/null | sort || true)
    got=$(runuser -u "$u" -- "$FOSSICK" --socket "$SOCK" "path:\"$M/\"" | sort || true)
    if [ "$expected" != "$got" ]; then
      diff <(echo "$expected") <(echo "$got") >&2 || true
      die "$label: $u sees a different set than find (< find, > fossick)"
    fi
    echo "ok: $label: $u sees $(echo -n "$expected" | grep -c '^' || true) entries, same as find"
  done
}

check_all "initial scan"

note "live changes"
touch "$M/alice/new-secret.txt" "$M/pub/new-public.txt"
mkdir "$M/team/sub" && touch "$M/team/sub/t.txt"
setfacl -m u:fsk_carol:rx "$M/alice"     # carol may now list alice/
chmod 755 "$M/mid"                       # mid becomes listable for everyone
setfacl -m u:fsk_bob:rx "$M/acl"         # bob regains acl/
settle
check_all "after live changes"

note "content: respects file permissions"
echo "needle-7f3a" > "$M/pub/readable.txt"
echo "needle-7f3a" > "$M/pub/unreadable.txt"
chmod 600 "$M/pub/unreadable.txt"
settle
got=$(runuser -u fsk_carol -- "$FOSSICK" --socket "$SOCK" "path:\"$M/\" content:needle-7f3a" || true)
[ "$got" = "$M/pub/readable.txt" ] || die "content: for carol returned: $got"
got=$(runuser -u root -- "$FOSSICK" --socket "$SOCK" "path:\"$M/\" content:needle-7f3a" | wc -l)
[ "$got" = 2 ] || die "content: for root found $got files"
echo "ok: content: only reads files the user can read"

note "folder sizes and child counts"
# carol can’t list team/: its child count is unknown to her, and the volume root’s size is hidden
# (it contains restricted folders).
got=$(runuser -u fsk_carol -- "$FOSSICK" --socket "$SOCK" "path:\"$M/\" childcount:2 folder:" | sort)
echo "$got" | grep -q "^$M/team\$" && die "carol learned team/’s child count"
got=$(runuser -u fsk_carol -- "$FOSSICK" --socket "$SOCK" -l "folder: wfn:pub path:\"$M/\"")
echo "$got" | grep -q "^ *[0-9]" || die "pub/ should show a size: $got"
got=$(runuser -u fsk_carol -- "$FOSSICK" --socket "$SOCK" -l "folder: wfn:team path:\"$M/\"")
echo "$got" | grep -q "^ *-" || die "team/ size should be hidden: $got"
echo "ok: counts and sizes of unlistable subtrees stay hidden"

note "rescan is root-only"
if runuser -u fsk_alice -- "$FOSSICK" --socket "$SOCK" --rescan "$M" 2> /dev/null; then
  die "alice could start a rescan"
fi
"$FOSSICK" --socket "$SOCK" --rescan "$M"
echo "ok: rescan denied to users, allowed to root"
echo "PASS multiuser (${FSTYPE:-ext4})"
