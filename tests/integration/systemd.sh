#!/usr/bin/env bash
# The packaged systemd units: the hardened service starts, runs as the fossick user with only
# CAP_SYS_ADMIN and CAP_DAC_READ_SEARCH, filters results for other users, and indexes a
# filesystem mounted after it started (mount propagation into its namespace). Needs root and
# systemd as PID 1 (the CI runner).
set -euo pipefail
cd "$(dirname "$0")/../.."
. tests/integration/common.sh
need_root
[ "$(cat /proc/1/comm)" = systemd ] || { echo "SKIP: systemd is not PID 1"; exit 77; }
BIN_SRC=${BIN_SRC:-target/release}

cleanup() {
  systemctl stop fossickd.service fossickd.socket 2> /dev/null || true
  umount /mnt/fossick-ci-hot 2> /dev/null || true
  [ -n "${LOOP:-}" ] && losetup -d "$LOOP" 2> /dev/null || true
  rm -f /etc/systemd/system/fossickd.service /etc/systemd/system/fossickd.socket
  systemctl daemon-reload
  userdel fsk_ci 2> /dev/null || true
}
trap cleanup EXIT

install -m 755 "$BIN_SRC/fossickd" "$BIN_SRC/fossick" /usr/local/bin/
install -m 644 packaging/systemd/fossick.sysusers /usr/lib/sysusers.d/fossick.conf
systemd-sysusers
sed 's|/usr/bin/fossickd|/usr/local/bin/fossickd|' packaging/systemd/fossickd.service \
  > /etc/systemd/system/fossickd.service
cp packaging/systemd/fossickd.socket /etc/systemd/system/
mkdir -p /etc/fossick /srv/fossick-ci/data /mnt/fossick-ci-hot
cat > /etc/fossick/fossickd.toml <<CFG
[index]
volumes = ["/srv/fossick-ci", "/mnt/fossick-ci-hot"]
CFG
# /srv/fossick-ci as its own (tmpfs) mount keeps the test fast; tmpfs must be enabled for it.
mountpoint -q /srv/fossick-ci || mount -t tmpfs -o mode=755 tmpfs /srv/fossick-ci
echo 'index_tmpfs = true' >> /etc/fossick/fossickd.toml
mkdir -p /srv/fossick-ci/public /srv/fossick-ci/private
touch /srv/fossick-ci/public/visible.txt /srv/fossick-ci/private/secret.txt
chmod 700 /srv/fossick-ci/private
systemctl daemon-reload
systemctl start fossickd.socket fossickd.service
FOSSICK=/usr/local/bin/fossick
for _ in $(seq 1 100); do
  "$FOSSICK" --status 2> /dev/null | grep -q "/srv/fossick-ci .* ready" && break
  sleep 0.1
done
"$FOSSICK" --status
journalctl -u fossickd --no-pager | tail -20

note "privileges"
pid=$(systemctl show -p MainPID --value fossickd)
[ "$pid" != 0 ] || die "fossickd is not running"
user=$(ps -o user= -p "$pid")
[ "$user" = fossick ] || die "runs as $user, not fossick"
caps=$(grep CapEff /proc/"$pid"/status | awk '{print $2}')
# CAP_DAC_READ_SEARCH = bit 2, CAP_SYS_ADMIN = bit 21.
[ "$caps" = 0000000000200004 ] || die "effective capabilities are $caps"
grep -q "NoNewPrivs:.*1" /proc/"$pid"/status || die "NoNewPrivs not set"
echo "ok: runs as fossick with CapEff=$caps and no_new_privs"

note "filtering for an unprivileged user"
id fsk_ci > /dev/null 2>&1 || useradd --no-create-home fsk_ci
got=$(runuser -u fsk_ci -- "$FOSSICK" 'path:/srv/fossick-ci/' | sort)
echo "$got" | grep -q "visible.txt" || die "fsk_ci can't see public files: $got"
echo "$got" | grep -q "secret.txt" && die "fsk_ci sees private files: $got"
echo "ok: fsk_ci sees public files only"

note "a filesystem mounted after the service started"
truncate -s 512M /var/tmp/fossick-ci-hot.img
mkfs.ext4 -q -F /var/tmp/fossick-ci-hot.img
LOOP=$(losetup --find --show /var/tmp/fossick-ci-hot.img)
mount "$LOOP" /mnt/fossick-ci-hot
touch /mnt/fossick-ci-hot/hotplugged-file
for _ in $(seq 1 100); do
  "$FOSSICK" 'hotplugged-file' 2> /dev/null | grep -q /mnt/fossick-ci-hot && break
  sleep 0.1
done
"$FOSSICK" 'hotplugged-file' | grep -q /mnt/fossick-ci-hot/hotplugged-file \
  || { "$FOSSICK" --status; die "the hotplugged filesystem was not indexed"; }
"$FOSSICK" --status | grep "/mnt/fossick-ci-hot"
echo "ok: hotplugged mount indexed and watched"
touch /mnt/fossick-ci-hot/live-after-mount
sleep 1
"$FOSSICK" 'live-after-mount' | grep -q live-after-mount || die "no live updates on the hot mount"
echo "ok: live updates on the hotplugged mount"

note "configuration reload through systemctl and through pkexec-style --install-config"
mkdir -p /srv/fossick-ci/public/node_modules
touch /srv/fossick-ci/public/node_modules/dep.js
for _ in $(seq 1 50); do "$FOSSICK" dep.js | grep -q dep.js && break; sleep 0.1; done
cp /etc/fossick/fossickd.toml /var/tmp/fossick-ci.toml
sed -i '/^\[index\]/a exclude_names = ["node_modules"]' /var/tmp/fossick-ci.toml
"$FOSSICK" --install-config /var/tmp/fossick-ci.toml
for _ in $(seq 1 50); do "$FOSSICK" dep.js | grep -q dep.js || break; sleep 0.1; done
"$FOSSICK" dep.js | grep -q dep.js && die "exclude_names did not apply after --install-config"
sed -i '/^exclude_names/d' /etc/fossick/fossickd.toml
systemctl reload fossickd
for _ in $(seq 1 50); do "$FOSSICK" dep.js | grep -q dep.js && break; sleep 0.1; done
"$FOSSICK" dep.js | grep -q dep.js || die "systemctl reload did not apply"
echo "ok: --install-config and systemctl reload apply the configuration"
echo "PASS systemd"
