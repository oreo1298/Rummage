# Filesystem support matrix

This table is filled **only from actual test results**: loopback images in the development VM or
the privileged CI job, or runs on real hardware. A filesystem that hasn’t been tested says
“untested”; nothing here is assumed.

**What the tests check** (`tests/integration/`):

- `storm.sh`: thousands of random creates, writes, renames, moves (files and whole directories),
  deletes, chmods, symlinks and hard links, with names that aren’t valid UTF-8, while the daemon
  watches. Then the index must equal a fresh `find` walk (paths, kinds, file sizes). Then changes
  are made while the daemon is stopped, and after restart (pruned rescan + metadata refresh) the
  index must match again. Then a second live storm on the reloaded index.
- `multiuser.sh`: three users and a group, mode bits, group membership, POSIX ACLs (named user,
  named group, mask), a traverse-only directory with a listable child. Each user’s results must
  equal what `find` run as that user lists, before and after live permission changes;
  `content:` must only read files the user can read; folder sizes and child counts of subtrees
  the user can’t list stay hidden.

Numbers are “storm took X s; index caught up Y s later” for the first live storm, then the
entries compared after the restart and after the second storm. CI runs on GitHub’s
`ubuntu-latest` runners (kernels 6.17 and 7.0, Azure builds), run 18 (commit `4269d58`).

| Filesystem | Scan | Real-time method (default) | Fallback tested | Tested where | Result |
|---|---|---|---|---|---|
| ext4 | getdents64 + statx | fanotify (FID, `FAN_RENAME`) | inotify, polling | dev VM (6.18), CI (6.17) | storm ✅ fanotify (8k ops: 8.4 s, caught up 0.81 s; 33,028 / 33,200 entries), inotify ✅ (5k: 4.9 s, 0.84 s), polling ✅ (dev VM); multi-user ✅ |
| xfs | getdents64 + statx | fanotify (FID, `FAN_RENAME`) | inotify | dev VM (6.18), CI (7.0) | storm ✅ fanotify (11.6 s, 1.53 s; 84,468 / 135,545), inotify ✅ (6.0 s, 1.55 s); multi-user ✅ |
| btrfs | getdents64 + statx (nested subvolumes followed) | fanotify (FID, `FAN_RENAME`) | inotify | CI (6.17) | storm ✅ fanotify (13.9 s, 1.50 s; 88,184 / 136,207); inotify ✅ since e280ddb (run 31: 6.7 s, 1.50 s; 62,613 / 80,510 / 110,141), green in runs 30 and 31; runs 17, 18 and 26–29 each lost the first file created in a new directory after the directory was opened for listing (btrfs lists only the entries that existed at open; the scanner now reads through a second open made after the watch, see `docs/TODO.md`); multi-user ✅ (runs 15, 31) |
| f2fs | getdents64 + statx | fanotify (FID, `FAN_RENAME`) | inotify | CI (6.17) | storm ✅ fanotify (8.7 s, 1.98 s; 84,803 / 112,769), inotify ✅ (3.8 s, 1.09 s); multi-user ✅ |
| vfat | getdents64 + statx (always full rescans) | fanotify (FID) | inotify | CI (7.0) | storm ✅ fanotify (59 s of writing, caught up 2.5 s; 118,107 / 164,600), inotify ✅ (16.7 s, 1.48 s); multi-user n/a (no Unix permissions) |
| exfat | getdents64 + statx (always full rescans) | **inotify** (no file handles, so no fanotify) | — | CI (7.0) | storm ✅ inotify (107 s of writing, caught up 4.0 s; 113,909 / 155,827); multi-user n/a |
| ntfs3 | getdents64 + statx | fanotify (FID) | inotify | CI (7.0) | storm ✅ fanotify (16.0 s, 2.25 s; 115,333 / 157,903), inotify ✅ (7.3 s, 1.40 s); multi-user n/a (mounted without permission mapping) |
| tmpfs | getdents64 + statx | fanotify (opt-in: `index_tmpfs`) | | CI systemd test (hardened service) | indexed and watched ✅, per-user filtering ✅ |
| ext4 hot-plugged after the service started | getdents64 + statx | fanotify | | CI systemd test | picked up and watched inside the service’s mount namespace ✅ |
| nfs, cifs, sshfs | | polling (opt-in) | | | untested |
| iso9660, udf, squashfs, erofs | getdents64 + statx | static | | dev VM (squashfs mounts of the sandbox) | indexed once ✅ |

The slow vfat/exfat storms are the writing itself (FAT directories are linear lists); the
daemon’s full rescans of those volumes after a restart took 1.95 s and 3.05 s for ~115k entries.

CI rows come from the GitHub Actions `integration` (loop-device storms) and `systemd` jobs.

## Behaviour worth knowing

- **Pure appends don’t change a directory.** A pruned rescan (after a restart, after a queue
  overflow, or a regular poll) only re-reads directories whose mtime/ctime changed, so a file whose
  size changed while nothing watched it is caught by the next *full* pass: the metadata refresh
  after startup (`refresh_metadata`, on by default) or every `poll_full_every`-th poll. With
  fanotify or inotify running, appends are seen live.
- **Hard links.** Events name one path; the daemon copies the new size and dates to every indexed
  name of the same inode.
- **Renames during bursts of events** are resolved through fanotify’s directory handles, or, for
  inotify, by checking that a directory is still where the index thinks before believing an entry
  is gone (see ARCHITECTURE.md §4.3).

## Known platform limitations

- **System daemon and `/tmp`, `/var/tmp`.** `PrivateTmp=yes` gives the daemon its own temporary
  directories, so the host’s `/tmp` and `/var/tmp` are not indexed in system mode. User mode has no
  such restriction.
- **fanotify needs `CAP_SYS_ADMIN`**, and a filesystem that can encode file handles. Without them
  (user mode, some FUSE filesystems) the daemon uses inotify, limited by
  `fs.inotify.max_user_watches`; when that runs out the volume is polled and `fossick --status`
  says so. Raise the limit with `sysctl fs.inotify.max_user_watches=1048576`.
- **btrfs.** Nested subvolumes report their own `st_dev`. The scanner follows them inside a mount
  (it compares mount ids) and leaves out subvolumes that are mounted separately. A directory read
  lists only the entries that existed when the directory was opened, so with inotify the scanner
  adds the watch first and then reads through a fresh open; otherwise an entry created in between
  would be missed.
- **FUSE filesystems mounted by another user without `allow_other`** are unreadable even to root,
  so the system daemon can’t index them. User mode can.
- **Access times** are not tracked live (that would need an event for every read); they are
  refreshed by rescans when `track_atime` is on.
