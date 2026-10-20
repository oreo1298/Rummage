# Shared helpers for the integration tests (sourced). They need root: they create users, mount
# loop filesystems and run fossickd.

die() { echo "FAIL: $*" >&2; exit 1; }
note() { echo "--- $*"; }

need_root() {
  [ "$(id -u)" = 0 ] || { echo "SKIP: needs root"; exit 77; }
}

# setup_work: a world-traversable work directory with copies of the binaries.
setup_work() {
  BIN_SRC=${BIN_SRC:-target/release}
  WORK=$(mktemp -d /var/tmp/fossick-it.XXXXXX)
  chmod 755 "$WORK"
  mkdir -p "$WORK/bin"
  cp "$BIN_SRC/fossickd" "$BIN_SRC/fossick" "$WORK/bin/"
  chmod 755 "$WORK/bin" "$WORK/bin/"*
  FOSSICKD="$WORK/bin/fossickd"
  FOSSICK="$WORK/bin/fossick"
  SOCKDIR="/run/fossick-it-$$"
  SOCK="$SOCKDIR/socket"
  DAEMON_PID=
  LOOP=
}

# make_fs TYPE: a 512 MiB (sparse) loop filesystem mounted at $WORK/mnt.
make_fs() {
  local type=$1
  truncate -s 512M "$WORK/img"
  case $type in
    xfs) mkfs.xfs -q -f "$WORK/img" ;;
    ext4) mkfs.ext4 -q -F "$WORK/img" ;;
    btrfs) mkfs.btrfs -q -f "$WORK/img" ;;
    vfat) mkfs.vfat "$WORK/img" > /dev/null ;;
    exfat) mkfs.exfat "$WORK/img" > /dev/null ;;
    f2fs) mkfs.f2fs -q -f "$WORK/img" ;;
    ntfs3) mkfs.ntfs -Q -F "$WORK/img" > /dev/null ;;
    *) die "unknown filesystem $type" ;;
  esac
  mkdir -p "$WORK/mnt"
  LOOP=$(losetup --find --show "$WORK/img")
  mount -t "$type" "$LOOP" "$WORK/mnt"
  chmod 755 "$WORK/mnt" 2> /dev/null || true
  MNT="$WORK/mnt"
}

start_daemon() {
  mkdir -p "$SOCKDIR"
  cat > "$WORK/fossickd.toml" <<CFG
[index]
volumes = ["$MNT"]
snapshot_interval_secs = 3600
watch_method = "${WATCH:-auto}"
poll_interval_secs = 10
poll_full_every = ${POLL_FULL_EVERY:-6}
[server]
socket = "$SOCK"
CFG
  "$FOSSICKD" --config "$WORK/fossickd.toml" --state-dir "$WORK/state" ${DEBUG:+--debug} \
    >> "$WORK/daemon.log" 2>&1 &
  DAEMON_PID=$!
  wait_ready
}

wait_ready() {
  for _ in $(seq 1 200); do
    if "$FOSSICK" --socket "$SOCK" --status 2> /dev/null | grep -q " ready "; then
      return 0
    fi
    sleep 0.05
  done
  cat "$WORK/daemon.log" >&2
  die "daemon did not become ready"
}

stop_daemon() {
  if [ -n "$DAEMON_PID" ]; then
    kill "$DAEMON_PID" 2> /dev/null || true
    wait "$DAEMON_PID" 2> /dev/null || true
    DAEMON_PID=
  fi
}

cleanup_common() {
  stop_daemon
  if [ -n "${MNT:-}" ]; then umount "$MNT" 2> /dev/null || umount -l "$MNT" 2> /dev/null || true; fi
  if [ -n "$LOOP" ]; then losetup -d "$LOOP" 2> /dev/null || true; fi
  rm -rf "$SOCKDIR"
  [ -n "${KEEP_WORK:-}" ] || rm -rf "$WORK"
}

# indexed [ARGS...]: every indexed path below $MNT, sorted (as the calling user).
indexed() {
  "$FOSSICK" --socket "$SOCK" "$@" "path:\"$MNT/\"" | sort
}

# Waits until the daemon has caught up with recent changes.
settle() { sleep "${SETTLE:-0.6}"; }
