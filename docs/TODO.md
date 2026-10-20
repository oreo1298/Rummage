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

## Client features from the spec not started yet

- [ ] Bookmarks, saved filters and search history: client side, with the GUI (Phase 3); CLI access
  later.
- [ ] Global hotkey, `fossick --toggle` (Phase 3).

## Left over from Phase 2 (daemon)

- [ ] **C ABI for `fossick-client`** (cbindgen header + C smoke test). Deferred to Phase 3, where
  the Qt frontend decides between cxx-qt and the C ABI.
- [ ] **Incremental live results:** live queries are re-run in full after a publish (≤ 10 Hz)
  instead of splicing the change set into the sorted result.
- [ ] **Incremental visibility maps:** a non-root user’s visibility is recomputed for every new
  generation they query (≈ 10–40 ms at 5M entries on the VM). File-only changes could patch it.
- [ ] **libblkid fallback** for volume UUIDs where `/dev/disk/by-uuid` doesn’t exist.
- [ ] **Scanner threads per device type** (`queue/rotational`).
- [ ] **`rc:` / recent changes**: needs a change log in the daemon.
- [ ] **Polkit** for admin requests (rescan) from non-root users; today only root may rescan.
- [ ] **Filesystem matrix beyond ext4/xfs:** btrfs, f2fs, vfat, exfat and ntfs3 run in the CI job;
  results go into `docs/FILESYSTEMS.md`.
- [ ] **Network filesystems** (polling) have no automated test yet.
- [ ] **OpenRC and runit services** (Phase 4, with packaging).
- [ ] **Event-storm benchmarks with the daemon** (`fossick-bench-report storm` measures the core
  writer only): end-to-end latency from a filesystem change to a visible result.

## Phases 3–6

Not started. See `docs/ARCHITECTURE.md` §7: GUIs, packaging and the Phase 6 upgrades.
