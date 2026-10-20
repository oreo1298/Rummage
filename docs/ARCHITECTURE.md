# Fossick architecture

Fossick is a Linux equivalent of voidtools Everything. It indexes every file and folder name on
every mounted filesystem, finds them as you type in milliseconds, and keeps the index current in
real time. This document describes the design and the phased plan. Where the original
specification met reality and lost, the result is recorded under **Deviations**.

Status markers: ✅ implemented and tested · 🚧 in progress · ⏳ planned.

## 1. Components

```
                       ┌──────────── /run/fossick/socket (AF_UNIX, postcard frames) ───────────┐
 fossick-qt ─┐         │                                                                        │
 fossick-gtk ┼─ fossick-client (Rust + C ABI) ─────────────────────────────────▶ fossickd (system, root-equivalent caps)
 fossick CLI ┘         │                                                    or fossickd --user (unprivileged, $HOME)
                       └────────────────────────────────────────────────────────────────────────┘
fossickd = mounts + volumes + scanner (fossick-scan) + watchers + writer thread + query workers
           + IPC server + permission filter, all on top of fossick-core
```

| Crate | Path | Role | Status |
|---|---|---|---|
| `fossick-core` | `crates/core` | index structures, query parser and evaluator, matcher, sorting, persistence. Pure logic, with the filesystem behind traits | 🚧 Phase 1 |
| `fossick-scan` | `crates/scan` | parallel `getdents64` + `statx` walker; mount and volume helpers | 🚧 Phase 1 (v0), Phase 2 (full) |
| `fossick` | `crates/cli` | CLI (the `es.exe` equivalent): in-process `--local` mode now, socket client in Phase 2 | 🚧 Phase 1 |
| `fossick-bench` | `crates/bench` | synthetic tree and index generator, criterion benchmarks, report tool | 🚧 Phase 1 |
| `fossick-proto` | `crates/proto` | IPC messages and framing | ⏳ Phase 2 |
| `fossickd` | `crates/daemon` | system service (also `--user` mode) | ⏳ Phase 2 |
| `fossick-client` | `crates/client` | Rust client and C ABI (`fossick.h` via cbindgen) | ⏳ Phase 2 |
| `fossick-qt` | `crates/gui-qt` | Qt 6 + QML + Kirigami frontend (cxx-qt) | ⏳ Phase 3 |
| `fossick-gtk` | `crates/gui-gtk` | GTK4 + libadwaita frontend (gtk4-rs) | ⏳ Phase 5 |

Frontends are thin. All searching happens in `fossickd`. The one exception is the CLI’s `--local`
mode, which runs the same core engine in-process for development and benchmarking.

## 2. Index data model (fossick-core)

### 2.1 Entries, pages and ids

- One global index holds every volume. An entry id is a `u32`: `page << 12 | slot`. A page holds
  4096 slots and belongs to exactly one volume, so the volume is stored per page, not per entry.
- Ids are stable for the entry’s lifetime. Freed slots go on per-volume free lists and are reused;
  nothing is ever renumbered. Ids never leave the daemon: clients address rows by position in a
  result set.
- Every column is a page table of `Arc<PageBuf<T>>` (copy-on-write per column page). Mutating one
  entry copies at most one 4096-element page of each column it touches (16 KB for `u32`). A
  `PageBuf` is either owned memory or a zero-copy view into a memory-mapped snapshot.

### 2.2 Per-entry columns

| Column | Type | Notes |
|---|---|---|
| name | page arena | Raw bytes, NUL-separated, slot order, plus `u32` start offsets. NUL can’t occur in a Linux file name, so a match can never cross from one name into the next. Rebuilt once per touched page per publish. |
| `parent` | u32 | `NONE` for volume roots |
| `size` | u32 | Files: size, or `ESCAPE` → side table (≥ 4 GiB). Directories: the directory-record id. |
| `mtime`, `ctime`, `btime` | u32 | Unix seconds; `UNKNOWN` and `ESCAPE` (pre-1970 or after 2106 → side table) |
| `atime` | u32 | Optional, off by default |
| `name_key` | u32 | Gapped order label of the name collation (see 2.5) |
| `mode` | u16 | `st_mode` (type + permission bits) |
| `owner` | u16 | Index into an interned `(uid, gid)` table; `ESCAPE` → side table |
| `inode` | u64 | |
| `flags` | u8 | `FREE`, `NON_ASCII`, `FOLD_SPECIAL` (contains U+212A or U+017F), `HIDDEN` (dot name), `VOLUME_ROOT`, `HAS_ACL` |
| `attrs` | u8 | statx attributes (compressed, immutable, append, nodump, encrypted, verity) + computed `SPARSE` |

Directories also have a record in dense directory pages, indexed by `DirId`: back-pointer to the
entry, folder size (u64), ns-precision mtime and ctime (reconciliation needs them), `dir_key`
(u32), child file and folder counts, and `restricted` (the number of directories in the subtree,
including itself, that are not listable by “others”; used by the permission layer to avoid leaking
folder sizes).

A path is rebuilt from the parent chain plus the volume’s mount path. The chain depth is capped, so
a corrupt snapshot can’t loop forever.

### 2.3 Writer-private structures

The writer thread owns structures that searches never need. They are rebuilt in the background
after a snapshot load:

- **Child lists** per directory: `(name hash, id)` pairs kept sorted, with small pending-add and
  pending-remove buffers that are compacted lazily (threshold √n), so a storm of 100k creates in
  one directory isn’t quadratic. The stored hash never changes while a pair is in the list, so lazy
  removal stays correct across renames. Used for name lookup on events, enumeration, subtree
  removal and rescan diffs. ✅
- **Directory handle map** (Phase 2): raw `fsid + file handle` bytes → `DirId`, captured with
  `name_to_handle_at` during scans, so a fanotify event resolves with one hash probe and no
  `open_by_handle_at` (which would return ESTALE once the directory is gone).
- **Inode → ids** for files with nlink > 1 (Phase 2), free lists, the directory-order map
  (`dir_key` → `DirId`), and the POSIX-ACL side table.

### 2.4 Generations (RCU)

The writer applies a batch of mutations to its private copy, then **publishes** an immutable
`Arc<Generation>` through `ArcSwap`. Publishing:

1. removes dirty ids from every order (using the keys captured before they changed),
2. assigns name and dir labels for new and renamed entries,
3. rebuilds the name arenas of touched pages,
4. inserts the dirty live ids back into the orders,
5. records the change set (inserted / removed / modified ids) for live result updates.

Readers grab the current generation lock-free and keep it for the duration of a query. Old
generations disappear when their last reader drops them. Publishing is cheap: it clones page
tables (`Vec<Arc<…>>`) and copies only pages touched since the last publish.

### 2.5 Presorted orders (Everything’s “fast sort”)

- `name_key`: every distinct name (collation = simple-folded bytes, ties on raw bytes) gets a
  `u32` label, spread evenly at bulk build. Inserts take the midpoint of their gap; when a gap is
  exhausted, a window is relabelled (monotonic, so no order changes position).
- `dir_key`: every directory gets a label in full-path collation order (depth-first, siblings by
  name, components compared one at a time). This makes every directory’s children, and its whole
  subtree, contiguous in path order.
- Order keys (`u64` primary, `u64` secondary, id as final tie-break):

  | Order | Primary | Secondary |
  |---|---|---|
  | name | `name_key` | `dir_key[parent]` |
  | path | `dir_key[parent]` | `name_key` |
  | size | size (folder size for directories) | `name_key` |
  | date modified | mtime | `name_key` |

- An order is a persistent sequence of `Arc<[u32]>` blocks (≤ 512 ids) with cached fence keys
  (the key of each block’s first id), so locating a block needs no key computation. Removals find
  ids by scanning one block (no key computation at all), and inserts binary-search inside one
  block. Only touched blocks are copied; the four orders are updated in parallel. If more than 1/8
  of an order is dirty, it is rebuilt with a parallel sort instead. Fences are refreshed after
  monotone relabelling. ✅
- Extension, type, date created, date accessed and run-count sorts are computed when requested (a
  parallel sort of the result ids), not maintained. ⏳ Maintaining them after first use is a
  Phase 6 optimisation.

### 2.6 Persistence

`FSIDX` v1 snapshot: header (magic, format version, little-endian marker, creation time,
generation, volume table), section table (kind, page, offset, length, xxh3-64), then raw column,
arena and order sections aligned to 64 bytes. It is:

- written to a temp file in the same directory, `fsync`ed, renamed over the old file, then the
  directory is `fsync`ed. The file is never modified in place.
- loaded with `mmap(MAP_PRIVATE, PROT_READ)` + `MADV_WILLNEED`. The header, section table, names,
  parents and flags are verified before serving; other sections are verified in the background, and
  a mismatch drops the snapshot and rescans. Every accessor is bounds-checked, so corrupt data
  yields wrong-but-safe results, never UB or a panic.
- re-mapped after each write so the replaced file’s blocks are freed.
- mode 0600 in `/var/lib/fossick` (system) or `~/.cache/fossick` (user mode).

On a version mismatch, the loader runs a migration if one exists. Otherwise it reports
`Incompatible`, and the daemon rebuilds the index in the background. Big-endian hosts always
rebuild.

### 2.7 Memory budget

Estimated at 5 M entries with ~20-byte names. The measured figure is in `docs/BENCHMARKS.md`.

| Item | B/entry |
|---|---|
| name bytes + NUL + offset | 25 |
| parent, size, 3 timestamps, name_key | 24 |
| mode, owner, flags, attrs | 6 |
| inode | 8 |
| directory records (~10% × 44 B) | 4.4 |
| child lists (writer) | ~5 |
| 4 presorted orders | 16 |
| side tables, page slack, allocator | ~5 |
| **total** | **≈ 93** (target < 120) |

## 3. Query engine

### 3.1 Syntax

Everything 1.4 syntax plus the 1.5 extras that make sense on Linux. The full reference with
examples is in [`docs/SYNTAX.md`](SYNTAX.md); the compatibility corpus is `tests/corpus/`.

- Operators: space = AND, `|` = OR (binds tighter than AND: `a b|c` = `a AND (b OR c)`), `!` = NOT,
  `< >` grouping, `"quoted phrases"`.
- Wildcards `*` and `?`. A term containing a wildcard matches the whole name.
- Modifiers prefix a term or group: `case: nocase: ww: wholeword: noww: path: nopath: file: folder:
  regex: noregex: diacritics: nodiacritics: wfn: wholefilename: nowfn: wildcards: nowildcards:
  ascii: utf8: noascii:`.
- Functions: `size: dm: datemodified: dc: datecreated: da: dateaccessed: dch: datechanged: dr:
  daterun: rc: ext: type: parent: infolder: nosubfolders: depth: parents: len: dupe: sizedupe:
  namepartdupe: dmdupe: dcdupe: empty: child: childcount: childfilecount: childfoldercount:
  childfile: childfolder: startwith: endwith: root: content: attrib: attributes: runcount: frn:
  fsi:` and the Linux additions `perm: owner: group: uid: gid: symlink: hidden: inode: fstype:
  volume: tag:`.
- Comparisons `: = < <= > >= a..b`, size units and constants, date forms and constants, and
  macros (`audio: video: pic: doc: exe: zip:` plus user macros).

Functions the parser accepts but can’t evaluate yet (media properties such as `width:` and
`artist:`, `tag:`, `filelist:`, …) return `QueryError::NotSupported` and are listed in
`docs/TODO.md`.

### 3.2 Evaluation

The query is parsed into an AST, macros are expanded, and the AST is compiled into a predicate tree
whose leaves are sorted by estimated cost. Evaluation is **page-vectorised**: each 4096-entry page
yields a 64-word bitmap. AND / OR / NOT combine bitmaps, and an AND passes its running candidate
mask down so expensive leaves skip pages with no candidates. Pages are processed in parallel with
rayon; each writes its own bitmap words, so no atomics are needed. A cancellation flag is checked
per page.

**Name matching.** The default is case-insensitive substring under simple case folding.

- When the folded needle is ASCII, one ASCII case-insensitive scan over the page arena is exact for
  every name, because multi-byte UTF-8 never contains ASCII bytes. The two exceptions, U+212A and
  U+017F, are flagged per entry and re-checked. Hits map to slots through the offset table.
- Non-ASCII needles skip pages with no `NON_ASCII` entries and match against a lazily built,
  per-page side arena of folded NFC names (only for the non-ASCII entries).
- Diacritics-insensitive mode uses a second side arena (NFD, marks stripped, folded).
- Bytes that aren’t valid UTF-8 match only themselves.
- Case-sensitive mode is plain `memmem`. Whole-word, wildcard and regex leaves use `regex::bytes`,
  which runs in linear time.

**Path matching** (`path:`, or any term containing `/`): for substring needles, each directory
memoizes a “path contains needle” flag and the last `len − 1` bytes of its path. That covers
matches spanning the `/` boundary without building full paths. Regex and wildcard path matching
build directory paths, cached per query.

**Sorted output.** Let H be the number of hits and N the index size. When H < N/32, the hit ids are
gathered and sorted by order key. Otherwise the requested presorted order is filtered against the
hit bitmap in parallel. Either way the result is a `Vec<u32>` in display order, and clients page
through it.

**Highlights** are computed only for rows a client fetches, by re-running the positive name or path
leaves on that row.

**Refinement.** ⏳ If the new query string extends the previous one (search-as-you-type) and the
predicate is monotone, only the previous result set is re-checked. Not implemented yet: full
searches already meet the latency target on the development VM (see BENCHMARKS.md).

## 4. Daemon (Phase 2)

### 4.1 Mounts and volumes

- `/proc/self/mountinfo` is parsed at startup and watched with `POLLPRI` for mounts, unmounts and
  hotplug.
- Filesystem types fall into four classes:
  - **always excluded:** proc, sysfs, devtmpfs, devpts, cgroup, cgroup2, debugfs, tracefs,
    securityfs, pstore, bpf, configfs, fusectl, mqueue, hugetlbfs, autofs, binfmt_misc, efivarfs,
    nsfs;
  - **local, indexed by default:** ext2/3/4, btrfs, xfs, f2fs, bcachefs, zfs, ntfs3, fuseblk/ntfs-3g,
    exfat, vfat, hfsplus, iso9660, udf, jfs, overlayfs;
  - **tmpfs:** opt-in;
  - **network** (nfs, cifs/smb3, sshfs, 9p, ceph, davfs): opt-in per mount, kept current by polling.
- Volume identity:
  1. `/dev/disk/by-uuid`;
  2. libblkid probe;
  3. btrfs fsid + subvolume id;
  4. for network filesystems: source + fstype;
  5. otherwise mount point + fstype (volatile).
- Bind mounts and subvolumes mounted more than once are deduplicated by (device, mountinfo root,
  root inode). Snapshot directories (`.snapshots`, timeshift) are excluded unless enabled.
- Offline volumes keep their index (configurable) and are shown greyed out.

### 4.2 Scanner

- A work-stealing walk using raw `getdents64` and `statx` (`AT_SYMLINK_NOFOLLOW`,
  `AT_STATX_DONT_SYNC`). Symlinks are indexed as entries, never followed.
- It never crosses mount points (statx `STATX_MNT_ID`), but does follow nested btrfs subvolumes.
- Threads per device come from `/sys/block/*/queue/rotational`: NVMe ≈ cores, HDD 1–2.
- Scanner threads run at ioprio IDLE and nice 19, set per thread, so queries keep normal priority.

### 4.3 Real-time updates

Each volume gets the best method that works on it, detected at runtime:

1. **fanotify** (`FAN_MARK_FILESYSTEM` + `FAN_REPORT_DFID_NAME`, plus `FAN_REPORT_TARGET_FID` and
   `FAN_RENAME` where the kernel has them);
2. **inotify** (manages the watch budget, warns with instructions when `max_user_watches` is too
   low, and covers only subtrees if the budget can’t cover the whole volume);
3. **polling** (periodic pruned rescans);
4. **static** (read-only media).

The per-volume method is reported over IPC and shown in the UI.

- A dedicated reader thread drains events into the writer’s queue. Events caused by fossickd itself
  are dropped by pid.
- Events are coalesced into a dirty-directory set. Simple events apply directly; ambiguous ones
  re-list the directory once per batch. Without `FAN_RENAME`, moved directories are held as
  detached subtrees for a short window so a matching MOVED_TO can re-attach them.
- `FAN_Q_OVERFLOW` or an unknown handle marks the volume dirty and triggers a pruned rescan.

### 4.4 Startup reconciliation

1. Load the snapshot and serve searches immediately.
2. Rescan in the background. A directory whose mtime and ctime are unchanged, and older than the
   filesystem’s timestamp granularity relative to the snapshot time, skips `getdents` but is still
   descended.
3. A separate low-priority pass refreshes file metadata.

### 4.5 IPC

- Socket: `/run/fossick/socket`, mode 0666. Authorization comes from `SO_PEERCRED` +
  `SO_PEERGROUPS`.
- Framing: little-endian `u32` length, then a postcard payload; frames are capped at 16 MiB.
- Hello/HelloAck handshake with protocol version negotiation. New enum variants are only ever
  appended.
- Queries are cancellable and replace the connection’s previous query. Results are paged
  (`GetRows{offset, count}`). The daemon pushes `ResultsChanged`, coalesced to ≤ 10 Hz.
- Socket activation is used when `LISTEN_FDS` is set; otherwise the daemon binds the socket itself.
  systemd is not required.

### 4.6 Permission filtering

- An entry is visible to a peer iff **every ancestor directory is listable (r+x)** by that peer,
  judged from the stored mode, uid and gid plus the POSIX ACLs of directories. The chain crosses
  mount points. Uncertain means deny. Root sees everything.
- The filter applies to rows, counts, pushes, `dupe:`, `childcount:`, `parent:` and `content:`.
  `content:` also needs read permission on the file itself.
- Folder sizes are hidden from a non-root peer unless that peer can list the whole subtree.
- A directory-visibility bitmap is cached per (credentials, generation).

### 4.7 systemd hardening

| Setting | Value |
|---|---|
| `User` | `fossick` |
| `AmbientCapabilities`, `CapabilityBoundingSet` | `CAP_SYS_ADMIN CAP_DAC_READ_SEARCH` |
| `ProtectSystem` | `strict` |
| `ProtectHome` | `read-only` |
| `PrivateTmp` | `yes` |
| `NoNewPrivileges` | `yes` |
| `RestrictAddressFamilies` | `AF_UNIX` (+ `AF_INET`/`AF_INET6` through a drop-in only when the HTTP server is enabled) |
| `SystemCallFilter` | `@system-service` + fanotify and handle syscalls |
| `StateDirectory`, `RuntimeDirectory` | set |
| `PrivateDevices` | not used: libblkid needs `/dev` |

CI verifies that a loop device mounted after the service has started is indexed, i.e. mount
propagation into the service’s namespace works.

## 5. Frontends (Phases 3 and 5)

- The shared design lives in `docs/UI.md` (⏳).
- Both GUIs are thin. `fossick-client` handles connect and autospawn, paging, push notifications,
  run reporting, the freedesktop trash, and the portal and D-Bus helpers: OpenURI,
  `FileManager1.ShowItems`, GlobalShortcuts, and `org.freedesktop.Application` activation
  (forwarding `XDG_ACTIVATION_TOKEN` for `fossick --toggle` on Wayland).
- Qt: cxx-qt + QML + Kirigami.
- GTK: gtk4-rs + libadwaita, with `GtkColumnView` over a lazy `GListModel`.

## 6. Deviations from the specification

1. **Test environment.** Development runs in a 4-core Xeon VM (Ubuntu 24.04 userland, kernel 6.18,
   no loadable modules, so only ext4, xfs, fuse and tmpfs are available; no systemd as PID 1; no
   Qt ≥ 6.6). Filesystem matrix tests, systemd hardening checks and Qt builds therefore run in
   GitHub Actions. Benchmark numbers recorded here are from that VM unless stated otherwise.
2. **PrivateTmp** hides the host’s `/tmp` and `/var/tmp` from the system daemon, so they aren’t
   indexed.
3. **“Never cross devices”** became “never cross mount points” (btrfs subvolumes and overlayfs
   report other `st_dev` values inside one mount).
4. **Supplementary groups** come from `SO_PEERGROUPS`; `SO_PEERCRED` alone lacks them. Without
   `SO_PEERGROUPS`, group-based access is denied.
5. **Permission rule tightened:** all ancestors must be listable, not just traversable (the
   original rule leaked ancestor names).
6. **`content:`** is checked against the DAC/ACL model. SELinux/AppArmor restrictions aren’t
   modelled.
7. **atime** is optional and not tracked live.
8. **Benchmark CI gate** uses instruction counts; wall-clock numbers are reported, not gated.
9. **Qt/Kirigami** needs Qt ≥ 6.6 + KF6, which rules out Ubuntu 24.04-based distros (they get the GTK
   GUI).
10. **User mode** is the same daemon run unprivileged, not an indexer embedded in each GUI or CLI
    process.
11. **Volumes without a UUID** get fallback identities.
12. **Precision:** file timestamps are stored to the second. Case folding is Unicode simple
    folding, so `ß` ≠ `ss`.
13. **MSRV** is 1.88, not the planned 1.85, because ICU4X 2.x requires it.
14. **Syntax reference:** voidtools.com was unreachable from the development sandbox. The syntax
    corpus was assembled from search-engine copies of the documentation plus known behaviour, and
    each case notes its source.

## 7. Phases

| Phase | Scope | Status |
|---|---|---|
| 1 | Core (index, persistence, full query parser, matcher, sorting), in-process CLI, benchmark harness | ✅ |
| 2 | Daemon: mounts, universal scanner, fanotify/inotify/polling, reconciliation, IPC, permission filtering, systemd unit, CLI over the socket, user mode | ⏳ |
| 3 | Qt/Kirigami GUI | ⏳ |
| 4 | Packaging (Arch first, then deb, rpm, tarball), README, release CI | ⏳ |
| 5 | GTK4/libadwaita GUI | ⏳ |
| 6 | Upgrades: preview, KRunner, GNOME provider, fuzzy/frecency, duplicates, tags, EFU, content index, HTTP, more distros | ⏳ |

Every phase ends with passing tests, a clean `cargo clippy -- -D warnings`, updated docs and a
conventional commit.
