# Fuzzing

Requires nightly Rust and `cargo install cargo-fuzz`. Run from the repository root:

```sh
mkdir -p fuzz/corpus/query_parser fuzz/corpus/snapshot_loader
cargo +nightly fuzz run query_parser fuzz/corpus/query_parser fuzz/seeds/query_parser -- -max_total_time=600
cargo +nightly fuzz run snapshot_loader fuzz/corpus/snapshot_loader fuzz/seeds/snapshot_loader -- -max_len=400000 -max_total_time=600
```

- `query_parser`: parse → print → parse must round-trip; parsing and compiling must never panic.
- `snapshot_loader`: loading arbitrary bytes with checksums disabled must fail cleanly or yield
  a generation whose accessors never panic (every read is bounds-checked).

The IPC decoder target is added with the daemon (Phase 2).
