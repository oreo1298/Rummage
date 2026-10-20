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
- [ ] **`search::matches_one`** rebuilds global predicates (`child:`, `dupe:`) on every call. Live
  result updates in Phase 2 need an incremental path.
- [ ] **`?` wildcards** still compile to a regex, where `?` is one valid UTF-8 character, so it
  never matches a byte that isn’t valid UTF-8. `*`-only patterns don’t have this limit.
- [ ] **Huge result sets:** queries with millions of hits (`a` at 5M: 61 ms p50 on the VM) spend
  most of their time filtering the presorted order. The first visible window could be produced
  before the full list.
- [ ] **Scattered storms:** publishing 5,000 events spread over a 5M index takes ~210 ms on the VM
  (96 MiB copied). Subtree-shaped storms take 85 ms. Reduce per-publish copying (name page
  rebuilds, order blocks).
- [ ] **Run history:** `runcount:`, `dr:` and the run-count sort are implemented. Without a run
  history (CLI local mode) counts are 0. Per-user history is stored by the daemon in Phase 2.

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

## Phases 2–6

Not started. See `docs/ARCHITECTURE.md` §7. That covers the daemon, watchers, IPC, permission
filtering, systemd units, GUIs, packaging and the Phase 6 upgrades.
