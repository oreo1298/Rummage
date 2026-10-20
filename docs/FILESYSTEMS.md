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

| Filesystem | Scan | Real-time method (default) | Fallback tested | Tested where | Result |
|---|---|---|---|---|---|
| ext4 | getdents64 + statx | fanotify (FID, `FAN_RENAME`) | inotify, polling | dev VM, kernel 6.18 | storm ✅ (fanotify 10k ops, inotify 8k, polling 3k), multi-user ✅ (fanotify, inotify) |
| xfs | getdents64 + statx | fanotify (FID, `FAN_RENAME`) | inotify | dev VM, kernel 6.18 | storm ✅ (fanotify 10k, inotify 8k), multi-user ✅ |
| btrfs | | | | CI job `integration` | untested yet |
| f2fs | | | | CI job `integration` | untested yet |
| vfat | | | | CI job `integration` | untested yet |
| exfat | | | | CI job `integration` | untested yet |
| ntfs3 | | | | CI job `integration` | untested yet |
| tmpfs | getdents64 + statx | (opt-in) | | systemd CI test | untested yet |
| nfs, cifs, sshfs | | polling (opt-in) | | | untested |
| iso9660, udf, squashfs, erofs | getdents64 + statx | static | | dev VM (squashfs mounts of the sandbox) | indexed once ✅ |

Rows marked “CI job” are filled from the GitHub Actions results of the `integration` job.

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
  (it compares mount ids) and leaves out subvolumes that are mounted separately.
- **FUSE filesystems mounted by another user without `allow_other`** are unreadable even to root,
  so the system daemon can’t index them. User mode can.
- **Access times** are not tracked live (that would need an event for every read); they are
  refreshed by rescans when `track_atime` is on.
