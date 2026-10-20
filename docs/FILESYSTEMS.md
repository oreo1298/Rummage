# Filesystem support matrix

This table is filled **only from actual test results** (loopback images in the privileged CI job,
or runs on real hardware). A filesystem that hasn’t been tested says “untested”; nothing here is
assumed.

| Filesystem | Scan method | Real-time method | Tested where | Notes |
|---|---|---|---|---|
| (none yet) | | | | Phase 2 adds the daemon, watchers and the loopback test matrix. |

## Known platform limitations (by design, not yet measured)

- **System daemon and `/tmp`, `/var/tmp`.** `PrivateTmp=yes` gives the daemon its own temporary
  directories, so the host’s `/tmp` and `/var/tmp` are not indexed in system mode. User mode has no
  such restriction.
- **btrfs.** Nested subvolumes report their own `st_dev`. The scanner follows them inside a mount
  and deduplicates subvolumes that are mounted separately.
- **FUSE filesystems mounted by another user without `allow_other`** are unreadable even to root,
  so the system daemon can’t index them. User mode can.
