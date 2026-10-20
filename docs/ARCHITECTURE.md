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

Measured at 5M entries (realistic tree): 78.4 B/entry in the published generation plus 27.4 B/entry
writer-private, and 107 B/entry RSS growth with one malloc arena (see BENCHMARKS.md).

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
- Case-sensitive mode is plain `memmem`.
- Whole word, prefix (`startwith:`), suffix (`endwith:`) and exact (`wfn:`) leaves find their
  needle in the raw arena the same way, then confirm the mode on each hit’s name.
- **Wildcards.** A pattern with `*` but no `?` is split into literal pieces. Each piece is placed at
  its leftmost occurrence after the previous one, which is exact for `*`-only patterns; with
  whole-name matching, the first piece must be a prefix and the last a suffix. Pieces are
  normalized like substrings, so wildcards honour case folding, the diacritics option and invalid
  bytes, and the longest piece prefilters the arena. Patterns containing `?` compile to a
  `regex::bytes` regex in which `*` spans any bytes (`?` is one valid character).
- **Regex.** When a pattern’s anchors are all plain `^`/`$` (no `\A`, `\z`, `(?m)` or `(?-m)`), it
  is compiled a second time with `^`/`$` bound to NUL. One search then covers a whole page arena,
  and each hit’s name is re-checked on its own. Patterns with other anchors are matched per name.
  The `regex` crate runs in linear time either way.
- **`ext:`** compares the last `max_len + 1` bytes of each ASCII name against the extensions,
  which are packed into sorted `u64`s. Non-ASCII names take the general per-entry check.

**Path matching** (`path:`, or any term containing `/`): for substring needles, each directory
memoizes a “path contains needle” flag, a “path ends with the needle up to its last `/`” flag, and
the last `len − 1` bytes of its path (from which its children’s flags are computed). A name holds
no `/`, so an entry matches iff its directory’s path contains the needle, or the needle has no `/`
and the name contains it (the arena scan above), or the directory’s second flag is set and the name
starts with the rest of the needle. No full path is built. Regex and wildcard path matching build
directory paths, cached per query.

**Sorted output.** Let H be the number of hits and N the index size. When H < N/32, the hit ids are
gathered, their order keys computed once (in parallel), and the `(key, id)` pairs sorted. Otherwise the requested presorted order is filtered against the
hit bitmap in parallel. Either way the result is a `Vec<u32>` in display order, and clients page
through it.

**Highlights** are computed only for rows a client fetches, by re-running the positive name or path
leaves on that row.

**Refinement.** ⏳ If the new query string extends the previous one (search-as-you-type) and the
predicate is monotone, only the previous result set is re-checked. Not implemented yet. Full
searches at 5M entries take 30–90 ms on the 4-core development VM (see BENCHMARKS.md), so
refinement is planned together with the daemon’s query sessions.

## 4. Daemon (`fossickd`, Phase 2)

The daemon is a library (`crates/daemon/src/lib.rs`) plus a thin binary. Threads and channels,
no async runtime:

| Thread | Job |
|---|---|
| writer | owns the `IndexWriter`: mount sync, scans, event application, publishing, snapshots |
| one reader per watched volume | blocks on fanotify or inotify, parses events, sends them to the writer |
| mounts | `POLLPRI` on `/proc/self/mountinfo` → “mounts changed” |
| accept + one reader and one executor per connection | the client protocol |
| live | re-runs live queries after a publish, at most 10 times a second |
| snapshot (short-lived) | writes a snapshot in the background |

### 4.1 Mounts and volumes ✅

- `/proc/self/mountinfo` is parsed at startup (octal escapes decoded) and watched with `POLLPRI`.
- Mounts are classified:
  - **excluded:** pseudo filesystems (proc, sysfs, devtmpfs, cgroup, …, gvfs/portal FUSE), and
    anything mounted under `/proc`, `/sys`, `/dev`, `/run` (except `/run/media`), `/snap`,
    `/var/lib/docker`, `/var/lib/containers`, `/var/snap`;
  - **local** (watched live): ext2/3/4, xfs, btrfs, f2fs, vfat, exfat, ntfs3, fuseblk, jfs,
    reiserfs, nilfs2, bcachefs, zfs, hfs(plus), apfs, ufs, an overlay mounted at `/`, and unknown
    types backed by a block device;
  - **static** (indexed once): iso9660, udf, squashfs, erofs, cramfs and read-only mounts;
  - **memory** (tmpfs, ramfs): opt-in with `index_tmpfs`;
  - **network** (nfs, cifs, smb3, 9p, ceph, sshfs, rclone, davfs, …): opt-in per mount point,
    polled.
- `volumes = [...]` limits indexing to listed mount points; `exclude = [...]` drops paths.
- **Deduplication:** the same tree mounted twice (same device and mountinfo root) is indexed once,
  at its shortest mount point. When one chosen mount’s root lies inside another’s on the same
  device (a bind mount of a subdirectory, or a btrfs subvolume reachable from the top-level
  mount), the deeper mount is the primary place for that subtree and is excluded from the other
  mount’s scan. Snapshot directories are excluded unless `include_snapshots`.
- **Identity** (matching a snapshot’s volumes to mounts): the filesystem UUID from
  `/dev/disk/by-uuid` plus the btrfs subvolume id (or the bind root); for network filesystems,
  type + source + root; otherwise type + mount point, marked volatile. A volume found at a new
  mount point is moved (`IndexWriter::set_volume_mount`) instead of rescanned. libblkid probing is
  not implemented (see TODO).
- Unmounted volumes stay in the index, flagged offline (or are emptied with `keep_offline =
  false`). Mount points of other indexed volumes are left out of their parent volume.

### 4.2 Scanner ✅

- Work-stealing walk with raw `getdents64` + `statx` (`AT_SYMLINK_NOFOLLOW`, `AT_STATX_DONT_SYNC`,
  `AT_NO_AUTOMOUNT`). Symlinks are entries, never followed. Mount points are recognised by
  `stx_mnt_id` (so nested btrfs subvolumes are followed).
- Per directory it can also read the POSIX access ACL (`fgetxattr` on the open directory),
  record the file handle (`name_to_handle_at`, for fanotify), and call a hook before reading
  (inotify adds its watch there).
- Listings go to the writer parents-first through a bounded channel. Scanner threads run at idle
  I/O priority and nice 19.
- `scan_inline` runs the same per-directory code sequentially on the caller’s thread, for the
  small directories that appear while watching.
- Per-device thread counts from `queue/rotational` are not implemented; `scan_threads` sets the
  count (default: all CPUs).

### 4.3 Real-time updates ✅

Each volume gets the best method that works, detected at runtime and shown by
`fossick --status`:

1. **fanotify**: one group per volume, `FAN_MARK_FILESYSTEM` with `FAN_REPORT_DFID_NAME`, plus
   `FAN_RENAME` when the kernel accepts it (≥ 5.17). Needs `CAP_SYS_ADMIN`.
2. **inotify**: one watch per directory, added *before* the directory is read (scanner hook), so
   nothing created during a scan is missed. Watches are keyed by the directory’s (device, inode),
   not by index entry, because the entry doesn’t exist yet when the watch is added. When
   `max_user_watches` runs out, the volume switches to polling and the status says why.
3. **polling**: pruned rescans every `poll_interval_secs`; every `poll_full_every`-th poll reads
   everything, because changed file sizes and dates don’t touch directory timestamps.
4. **static**: read-only media.

Events say *where* something changed; the writer re-reads the filesystem to learn *what*:

- `Child(dir, name)`: stat the entry and insert, update or remove it. A new directory is scanned
  inline right away (it may already have contents).
- `Itself(dir)` (fanotify reports changes to a directory itself with the name `.`): re-read its
  mode, owner, times and ACL.
- `Rename(from, to)`: if the entry at the new place has the indexed inode, rename it in the
  index (keeping its subtree); otherwise refresh both ends.

**Renames racing with events.** An event can be read after its directory has been renamed on disk
but before the rename event is applied, so the index’s path for the directory is stale. With
fanotify the writer opens the event’s directory by its handle (`open_by_handle_at`, needs
`CAP_DAC_READ_SEARCH`) and stats the entry relative to the real directory. Without a handle,
a missing entry only counts as deleted if its directory still exists at the indexed path with the
indexed inode; otherwise the event is retried after the batch’s renames, up to three times, then a
pruned rescan settles it.

**Hard links.** An event names one path, but every name of a file shares its size and dates.
Files seen with more than one link are tracked per (volume, inode), and a change to one name is
copied to the others (the first time, the volume’s inode column is searched for them).

Events are applied in batches: the writer keeps draining its queue for up to 50 ms, then publishes.
Events caused by the daemon itself (snapshot writes) are dropped by pid. `FAN_Q_OVERFLOW` or
`IN_Q_OVERFLOW` triggers a pruned rescan. During a scan, events wait in the queue and are applied
after it, re-reading the filesystem, so the result is current either way.

### 4.4 Startup reconciliation ✅

1. The snapshot is loaded and served right away (`Verify::All`: 141 ms for 5M entries warm).
2. Each volume gets a **pruned rescan**: a directory whose current mtime and ctime equal the
   recorded ones (and are older than the snapshot minus 2 s, for coarse timestamps) is not read
   again, but its known subdirectories are still visited.
3. Then one full low-priority rescan per volume refreshes file sizes and dates
   (`refresh_metadata`, on by default).

A directory’s recorded times always describe *its own last complete listing*: they are taken
just before the directory is read, and are not touched when its metadata changes through an event
or through its parent’s listing. Otherwise a directory whose listing failed, or whose contents
changed while events were being applied, could look current forever. A directory that disappears
during a scan (renamed, say) yields an incomplete listing, never an empty one, so its indexed
contents aren’t thrown away.

### 4.5 IPC ✅

- `fossick-proto`: postcard messages in frames of a little-endian `u32` length plus payload, 16 MiB
  cap. `Hello` → `Welcome` handshake with the protocol version. Enum variants are only appended.
  The decoder is fuzzed (`fuzz/fuzz_targets/ipc_decoder.rs`).
- Requests: `Query` (replaces and cancels the connection’s previous query), `Cancel`,
  `Rows{offset, count}` (≤ 10,000), `Sort`, `Status`, `ReportRun`, `Rescan` (root only), `Goodbye`.
  Responses: `Result{total, generation, elapsed}`, `Rows`, `Changed` (live push), `Status`,
  `Error`, `Ok`.
- Each connection has a reader thread (which can cancel the running query) and an executor that
  owns the result list. Live queries are re-run after publishes, at most 10 times a second, and
  `Changed` is pushed only if the result changed. Incremental splicing of live results is not
  implemented (see TODO).
- Socket: `/run/fossick/socket`, mode 0666 (system) or `$XDG_RUNTIME_DIR/fossick/socket`, 0600
  (user mode). systemd socket activation (`LISTEN_FDS`) is supported but not required.
- `fossick-client` connects to the system daemon, else the user daemon, starting
  `fossickd --user` in its own process group when needed. The CLI uses it whenever `--local` and
  `--load` aren’t given.

### 4.6 Permission filtering ✅

- Credentials: `SO_PEERCRED` for uid and primary gid, `SO_PEERGROUPS` for supplementary groups.
  Without `SO_PEERGROUPS` the peer gets no supplementary groups (fail closed).
- An entry is visible iff **every directory from its volume root down to its parent is listable
  (read and search)** by the peer, and every directory above the mount point is too (checked
  live). Listability follows the kernel’s POSIX ACL algorithm (owner, named users, group class
  with mask, other) using the stored mode, owner and directory ACLs. A directory whose ACL can’t
  be parsed is listable by nobody but root. Root sees everything. In user mode only the daemon’s
  own user may connect.
- Visibility is computed per (credentials, generation) and cached (`core::perm::visibility`:
  directories top-down, then entries in parallel). It is passed to the search as an entry mask
  plus a “listable directories” mask.
- What the filter covers: rows, totals, `Status` counts (per-user visible counts), `dupe:` and
  `child:` (computed over visible entries only), `childcount:`/`empty:` (only for directories the
  peer can list), folder sizes and `size:` on folders (hidden, and sorted last, when the folder
  contains a restricted directory: one “others” can’t list, or one with an ACL), and `content:`,
  which opens the file without following symlinks or blocking, then checks the peer’s read
  permission (mode and ACL) on the opened file.
- Tested with real users (`tests/integration/multiuser.sh`): each user’s results must equal what
  `find` run as that user lists, before and after live permission changes.

### 4.7 systemd hardening ✅ (verified in CI by `tests/integration/systemd.sh`)

| Setting | Value |
|---|---|
| `User` | `fossick` (sysusers.d) |
| `AmbientCapabilities`, `CapabilityBoundingSet` | `CAP_SYS_ADMIN CAP_DAC_READ_SEARCH` |
| `ProtectSystem` / `ProtectHome` / `PrivateTmp` | `strict` / `read-only` / `yes` |
| `NoNewPrivileges` | `yes` |
| `RestrictAddressFamilies` | `AF_UNIX` (the HTTP server of Phase 6 adds INET in a drop-in) |
| `SystemCallFilter` | `@system-service` + fanotify and file-handle syscalls |
| `StateDirectory`, `RuntimeDirectory` | `fossick` (0700, 0755) |
| also | `ProtectKernel*`, `ProtectControlGroups`, `ProtectClock`, `ProtectProc=invisible`, `RestrictNamespaces`, `MemoryDenyWriteExecute`, `LockPersonality`, … |
| `PrivateDevices` | not used: `/dev/disk/by-uuid` is read for volume identity |

The CI test checks the running daemon’s user, effective capabilities and `NoNewPrivs`, that an
unprivileged user’s results are filtered, and that a loop filesystem mounted after the service
started is indexed and watched (mount propagation into the service’s namespace).

### 4.8 Memory

`fossickd` caps glibc malloc arenas at 2 (`mallopt(M_ARENA_MAX)`) and calls `malloc_trim` after
startup scans; see BENCHMARKS.md for why.

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
15. **Folder sizes for unprivileged users** are hidden when the folder contains any directory
    that “others” can’t list or that carries an ACL, even if this particular user could list it.
    That is stricter than necessary, but it needs no per-user subtree computation and never leaks.
16. **Volume identity without libblkid:** `/dev/disk/by-uuid` covers block devices with udev; the
    libblkid fallback (containers without udev) isn’t implemented.
17. **Scanner threads** are not chosen per device type yet (`queue/rotational`); one setting
    applies to all volumes.

## 7. Phases

| Phase | Scope | Status |
|---|---|---|
| 1 | Core (index, persistence, full query parser, matcher, sorting), in-process CLI, benchmark harness | ✅ |
| 2 | Daemon: mounts, universal scanner, fanotify/inotify/polling, reconciliation, IPC, permission filtering, systemd unit, CLI over the socket, user mode | ✅ (C ABI deferred, see TODO) |
| 3 | Qt/Kirigami GUI | ⏳ |
| 4 | Packaging (Arch first, then deb, rpm, tarball), README, release CI | ⏳ |
| 5 | GTK4/libadwaita GUI | ⏳ |
| 6 | Upgrades: preview, KRunner, GNOME provider, fuzzy/frecency, duplicates, tags, EFU, content index, HTTP, more distros | ⏳ |

Every phase ends with passing tests, a clean `cargo clippy -- -D warnings`, updated docs and a
conventional commit.
