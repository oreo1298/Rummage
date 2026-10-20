# CLAUDE.md — working notes for Fossick

Fossick is an open-source, Everything-style instant file-name search for Linux (daemon + CLI +
Qt/GTK frontends). Repo: `github.com/oreo1298/Rummage` (the repo keeps its old name; the product is
**Fossick** everywhere: binaries, packages, paths and docs). License: GPL-3.0-or-later.

## Commands

```sh
cargo fmt --all --check                                   # formatting
cargo clippy --workspace --exclude fossick-qt --all-targets -- -D warnings  # lint (clean)
cargo test --workspace --exclude fossick-qt               # all tests but the Qt GUI
cargo test -p fossick-core                                # core only
cargo run -p fossick -- --local /usr 'ext:so size:>1mb'   # in-process CLI (no daemon)
cargo bench -p fossick-bench                              # criterion benches (wall clock)
cargo run -p fossick-bench --release --bin fossick-bench-report -- --entries 1000000
cargo +nightly fuzz run query_parser                      # fuzzing (needs cargo-fuzz)
# Daemon integration tests (root; loop filesystems, real users; see tests/integration/):
cargo build --release -p fossickd -p fossick
sudo tests/integration/storm.sh                           # FSTYPE=xfs WATCH=inotify|poll OPS=…
sudo tests/integration/multiuser.sh                       # FSTYPE=…, WATCH=…
sudo tests/integration/reload.sh                          # config reload, exclusions
sudo tests/integration/systemd.sh                         # needs systemd as PID 1 (CI)
# Qt GUI (needs Qt ≥ 6.8 + KF6 Kirigami, e.g. Ubuntu 26.04 / Debian 13 / Arch; CI job gui-qt):
cargo clippy -p fossick-qt --all-targets -- -D warnings
cargo build -p fossick-qt -p fossickd -p fossick
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software XDG_CURRENT_DESKTOP=KDE \
  tests/gui/screenshot.sh shots target/debug              # screenshots against a user daemon
```

In this sandbox (Ubuntu 24.04, Qt 6.4) the Qt GUI is built in an Ubuntu 26.04 Docker image
(`mirror.gcr.io/library/ubuntu:26.04` + the packages listed in the `gui-qt` CI job), with
`~/.cargo` and `~/.rustup` mounted and `CARGO_TARGET_DIR=target/qt`.

Crates: `core` (index, query, search, permissions, snapshots), `proto` (IPC messages), `scan`
(walker, file handles, pruned rescans), `daemon` (`fossickd`), `client` (connect/autospawn),
`cli` (`fossick`), `ui` (toolkit-free frontend logic: daemon session, row cache, selection,
columns, settings, desktop actions), `gui-qt` (`fossick-qt`, cxx-qt + QML/Kirigami), `bench`.

Rust: edition 2024, **MSRV 1.88** (raised from the planned 1.85 because ICU4X 2.x, used for
Unicode case folding and normalization, needs 1.88). CI checks MSRV.

## Workflow rules

- Phases come from `docs/ARCHITECTURE.md` (“Phases”). Never start a phase with failing tests.
- Every commit uses conventional-commit style (`feat(core): …`, `fix(scan): …`, `docs: …`) and
  is made only on a green tree (fmt, clippy `-D warnings`, tests). Push to `origin main`: the
  container is ephemeral.
- Never fake progress. Unfinished or stubbed work goes in `docs/TODO.md`. Functions the parser
  accepts but can’t evaluate yet return `QueryError::NotSupported`, never a silent match-all.
- When reality contradicts the spec (kernel limits, filesystem quirks), pick the most honest
  fallback, document it in `docs/FILESYSTEMS.md` or `docs/ARCHITECTURE.md` (“Deviations”), and tell
  the user.
- Don’t copy code from Everything (closed source). FSearch, plocate and mlocate are prior art
  (GPL); reuse code only with attribution and compatible terms.
- No invented URLs presented as live (apt repo, AUR, COPR, OBS stay marked “not live yet” until
  the user sets them up).

## Conventions

- `unsafe` is denied workspace-wide. Each exception needs `#[allow(unsafe_code)]` plus a `SAFETY:`
  comment. In `fossick-core` the only exception is the memory mapping of snapshot files.
- File names are raw bytes (`&[u8]`) end to end. Display uses lossy escaping
  (`fossick_core::name::display_name`); copying out always uses the original bytes.
- Entry ids (`EntryId`, u32 = page << 12 | slot) are internal and never leave the daemon.
- Columns are copy-on-write per 4 Ki-entry page (`Arc` per column page). Readers only see published
  `Generation`s (ArcSwap), so writers never block readers.
- Case-insensitive matching uses Unicode **simple** case folding (ICU4X `simple_fold`), so
  `ß` ≠ `ss`. The diacritics-insensitive mode is NFD + strip `Mn` marks.
- Errors: `thiserror` in libraries, `anyhow` only in binaries.
- Tests live next to the code (`#[cfg(test)]`). Cross-crate and corpus tests go in `tests/`. The
  syntax corpus is `tests/corpus/*.toml`.

## Settled decisions (see docs/ARCHITECTURE.md for the reasoning)

| Topic | Decision |
|---|---|
| Name / ids | Fossick; `fossick`, `fossickd`, `fossick-qt`, `fossick-gtk`; app id `io.github.oreo1298.Fossick`; system user `fossick` |
| Paths | `/run/fossick/socket`, `/var/lib/fossick` (0700, files 0600), `/etc/fossick/fossickd.toml`, `~/.config/fossick`, `~/.cache/fossick`, `$XDG_RUNTIME_DIR/fossick/socket` (user mode) |
| User mode | same daemon binary run unprivileged (`fossickd --user`), autospawned by the client |
| Permission rule | an entry is visible iff every ancestor directory is listable (r+x) by the peer; stricter than the spec’s wording, which leaked ancestor names |
| Mount crossing | never cross mount points (statx mnt_id); follow nested btrfs subvolumes |
| PrivateTmp | system daemon does not index the host’s /tmp and /var/tmp |
| atime | optional column, off by default; `da:` approximate |
| Timestamps | file times stored as u32 seconds (escape table for out-of-range); directories keep ns for reconciliation |
| Bench gate | CI gates on instruction counts (valgrind), not wall clock |
| Qt GUI | needs Qt ≥ 6.6 + KF6 (no Ubuntu 24.04-based builds) |
