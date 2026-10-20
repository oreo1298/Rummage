# TODO: known gaps

This lists everything that is planned but not implemented, or only partly implemented. If a
feature is not listed here and not marked ✅ in `docs/ARCHITECTURE.md`, treat it as not done.

## Left over from Phase 1 (core, CLI, benchmarks)

- [ ] **Search refinement:** when a keystroke extends the previous query, only the previous hits
  should be re-checked. Today every keystroke is a full search.
- [ ] **Maintained orders for extension, type, created, accessed and changed.** These are sorted
  on request (a parallel sort of the hits) instead of being kept presorted after first use.
- [ ] **“Folders first” sort option.**
- [ ] **Locale-aware collation and natural number order.** Names sort by Unicode code point after
  simple case folding, so `file10` sorts before `file2`.
- [ ] **Highlighting** is best effort for names whose NFC form has a different length (decomposed
  accents): no highlight instead of a wrong one.
- [ ] **Memory:** writer-private child lists cost ~8 B/entry plus 72 B per directory. That could be
  ~5 B/entry with a single vector and boxed buffers.
- [ ] **`search::matches_one`** rebuilds global predicates (`child:`, `dupe:`) on every call.
- [ ] **`?` wildcards** still compile to a regex, where `?` is one valid UTF-8 character, so it
  never matches a byte that isn’t valid UTF-8. `*`-only patterns don’t have this limit.
- [ ] **Huge result sets:** queries with millions of hits (`a` at 5M: 61 ms p50 on the VM) spend
  most of their time filtering the presorted order. The first visible window could be produced
  before the full list.
- [ ] **Scattered storms:** publishing 5,000 events spread over a 5M index takes ~210 ms on the VM
  (96 MiB copied). Subtree-shaped storms take 85 ms. Reduce per-publish copying (name page
  rebuilds, order blocks).
- [ ] **Run history** in CLI local mode is empty (the daemon keeps it per user).

## Query functions accepted but not evaluated yet (return `NotSupported`)

- [ ] Image properties: `width:`, `height:`, `dimensions:`, `orientation:`, `bitdepth:` (Phase 6,
  read on demand).
- [ ] Audio tags: `album:`, `artist:`, `genre:`, `title:`, `track:`, `comment:`, `year:`,
  `length:` (Phase 6).
- [ ] `tag:` (Phase 6, `user.xdg.tags` xattr).
- [ ] `rc:` / recent changes (needs change tracking in the daemon, Phase 2).
- [ ] `filelist:`, `filelistfilename:` (EFU support, Phase 6).
- [ ] `shell:` (map to XDG user directories).

## Client features

- [x] Bookmarks, custom filters and search history in the GUI (Phase 3).
- [ ] Bookmarks and history from the CLI.
- [x] Global shortcut (GlobalShortcuts portal) and `fossick --toggle` (Phase 3).

## Left over from Phase 2 (daemon)

- [ ] **C ABI for `fossick-client`** (cbindgen header + C smoke test). The Qt frontend uses
  cxx-qt and didn’t need it; it remains useful for third-party clients (Phase 6).
- [ ] **Incremental live results:** live queries are re-run in full after a publish (≤ 10 Hz)
  instead of splicing the change set into the sorted result.
- [ ] **Incremental visibility maps:** a non-root user’s visibility is recomputed for every new
  generation they query (≈ 10–40 ms at 5M entries on the VM). File-only changes could patch it.
- [ ] **libblkid fallback** for volume UUIDs where `/dev/disk/by-uuid` doesn’t exist.
- [ ] **Scanner threads per device type** (`queue/rotational`).
- [ ] **`rc:` / recent changes**: needs a change log in the daemon.
- [x] **Polkit** for admin requests: the GUI runs `fossick --install-config` / `--rescan` through
  `pkexec` (action `io.github.oreo1298.Fossick.admin`). The daemon still accepts them from root
  only.
- [ ] **btrfs + inotify:** CI runs 17, 18 and 26–28 each lost one file created in a brand-new
  directory. Two earlier causes were fixed (a race between adding a watch and recording it, and
  events of a stale watch being dropped). The remaining one: btrfs on recent kernels lists only
  the entries that existed when a directory was *opened*, and the scanner opened the directory
  before the inotify watch was added, so a file created in between was neither listed nor
  reported. The scanner now reads through a second open made after the watch exists. Confirm over
  several CI runs before ticking this.
- [ ] **UUIDs without udev:** `FS_IOC_GETFSUUID` (Linux 6.5+) could identify volumes before udev
  publishes `/dev/disk/by-uuid`; until then the fallback identity is matched by mount point.
- [ ] **Network filesystems** (polling) have no automated test yet.
- [ ] **OpenRC and runit services** (Phase 4, with packaging).
- [ ] **Event-storm benchmarks with the daemon** (`fossick-bench-report storm` measures the core
  writer only): end-to-end latency from a filesystem change to a visible result.

## Left over from Phase 3 (Qt GUI)

- [ ] **Real-desktop checks:** the GUI is tested offscreen only (screenshots in CI). Still to
  try on Plasma and GNOME sessions: Orca, the GlobalShortcuts portal’s dialog, the tray
  (StatusNotifierItem), drag and drop into file managers, Wayland activation with `--toggle`, and
  fractional scaling.
- [ ] **Translations:** only the English plural forms exist; messages from Rust code (daemon
  errors, action failures) aren’t translatable yet.
- [ ] **Natural sort** in the GUI follows the daemon (see “Locale-aware collation” above).
- [ ] **Preview pane**, **fuzzy matching** and **frecency** (Phase 6; no settings for them yet).
- [ ] **Index settings** rewrite `fossickd.toml` without its comments (TOML round trip). A
  comment-preserving editor (`toml_edit`) would keep hand-written notes.
- [ ] **Packaging** must install the desktop file, icon and polkit policy, and depend on the
  Breeze icons (Phase 4). The polkit action names `/usr/bin/fossick`, so other prefixes need
  the path rewritten.

## Phases 4–6

Not started. See `docs/ARCHITECTURE.md` §7: packaging, the GTK GUI and the Phase 6 upgrades.
