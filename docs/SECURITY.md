# Security model

The system daemon runs with `CAP_DAC_READ_SEARCH`, so it can read every directory and therefore
knows every file name on the machine. Its job is to answer each client with **only what that
client could have listed itself**. This document says how, and where the limits are.

## Who is asking

The socket (`/run/fossick/socket`) is world-connectable. Authorization comes from the kernel, not
from anything the client sends:

- `SO_PEERCRED` gives the peer’s uid and primary gid;
- `SO_PEERGROUPS` gives its supplementary groups. Without it (very old kernels) the peer is
  treated as having no supplementary groups, so group-based access is denied rather than
  guessed.

uid 0 sees everything. A user daemon (`fossickd --user`) listens in the user’s
`$XDG_RUNTIME_DIR` (mode 0700, socket 0600) and refuses any other uid.

## What a user may see

An entry is visible iff **every directory from its volume root down to its parent is listable
(read and search permission)** by the user, and every directory above the volume’s mount point is
too. Listability uses the stored mode, owner, group and POSIX access ACL of each directory, with
the kernel’s algorithm (owner bits for the owner; named-user entries and the group class limited
by the mask; “other” otherwise). A directory whose ACL can’t be parsed is listable by nobody but
root.

This is stricter than “execute on the ancestors, read and execute on the parent”: under that rule
a user who may only traverse `/home/alice` (mode 0711) would see `/home/alice/public/…` and so
learn the name `public`, which `ls /home/alice` would have refused. With Fossick’s rule the user
sees exactly what `find` run as that user prints, which is what the integration test checks
(`tests/integration/multiuser.sh`).

## Indirect leaks that are closed

| Channel | Treatment |
|---|---|
| result rows, totals | filtered by the visibility map |
| `Status` entry counts | per-user visible counts |
| `dupe:`, `sizedupe:`, `child:` | computed over visible entries only |
| `childcount:`, `childfilecount:`, `empty:` | match only directories the user can list |
| folder size column, `size:` on folders, sorting by size | hidden (unknown, sorted last) when the folder contains a directory that “others” can’t list or that has an ACL |
| `content:` | the file is opened without following symlinks and without blocking (FIFOs), then the user’s read permission (mode + ACL) is checked on the opened file, so it can’t be swapped in between |
| `parent:`, `infolder:` | results are filtered; children of unlistable directories are invisible anyway |
| run history | stored per uid; a user only ever sees their own |
| `Rescan` | root only (polkit for others is planned) |

## Known limits

- **Staleness.** Permissions come from the index, which follows the filesystem within
  milliseconds (fanotify) to seconds (inotify batches) or a poll interval. A directory that was
  just made private may remain visible to others for that long.
- **Mandatory access control** (SELinux, AppArmor, Landlock) and **mount namespaces of other
  processes** are not modelled: the daemon judges by DAC and ACLs as seen in its own namespace.
- **Folder sizes** are hidden more often than strictly necessary (see the table above).
- **Timing.** Query times depend on the whole index, not only on what the user may see.

## Hardening of the daemon process

See `packaging/systemd/fossickd.service` and ARCHITECTURE.md §4.7. The daemon runs as the
`fossick` user with only `CAP_SYS_ADMIN` (fanotify filesystem marks) and `CAP_DAC_READ_SEARCH`,
`NoNewPrivileges`, a read-only view of the system (`ProtectSystem=strict`,
`ProtectHome=read-only`), a private `/tmp`, `AF_UNIX` only, and a system-call filter. The CI job
`systemd` verifies the effective capabilities and that results are filtered.

Untrusted input is fuzzed: the IPC decoder, the query parser and the snapshot loader
(`fuzz/`). The `unsafe` code is confined to `crates/scan/src/handle.rs` (file handles) and
`crates/daemon/src/sys.rs` (fanotify, `SO_PEERGROUPS`, socket activation, malloc tuning), each
block with a safety comment; `fossick-core` uses `unsafe` only for the snapshot `mmap`.

## Reporting a vulnerability

Please open a private security advisory on the GitHub repository rather than a public issue.
