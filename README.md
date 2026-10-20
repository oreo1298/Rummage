# Fossick

Instant, Everything-style file-name search for Linux. Fossick indexes every file and folder name
on every mounted filesystem, finds them as you type, and stays current in real time.

> **Status: Phase 1 of 6 complete. Nothing is packaged yet.**
>
> What works today:
>
> - the search engine (index, Everything-compatible query language, sorting, snapshots);
> - a command-line tool that indexes a directory in-process and searches it.
>
> The system daemon, real-time updates, GUIs and packages are planned. See
> [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design and plan, and
> [`docs/TODO.md`](docs/TODO.md) for what is missing.

Fossick is an independent project, not affiliated with voidtools. It reimplements the behaviour
and search syntax of voidtools Everything for Linux.

## Try it (from source)

You need Rust 1.88 or newer (`rustup` or your distribution’s `rust` / `cargo` package).

```sh
git clone https://github.com/oreo1298/Rummage.git fossick && cd fossick
cargo build --release
./target/release/fossick --local ~ 'ext:pdf dm:thisyear'
./target/release/fossick --local / --stats --count ''           # index the whole root filesystem
./target/release/fossick --local /usr -n 10 --sort size --desc 'size:>100mb'
./target/release/fossick --local ~ -0 '*.rs' | xargs -0 wc -l
```

`--local DIR` scans `DIR` in-process (it never crosses into other mounts) and searches it. To reuse
an index, save it with `--save FILE` and search it later with `--load FILE`. Searching without
`--local` or `--load` needs the daemon (Phase 2); until then the CLI exits with status 3.

### Command-line options

| Option | Meaning |
|---|---|
| `-r`, `--regex` | the query is a regular expression |
| `--case` | match case |
| `-w`, `--whole-word` | whole words only |
| `-p`, `--path` | match the full path instead of the name |
| `--ignore-diacritics` | `cafe` matches `café` |
| `-n`, `--limit N`; `--offset N` | limit and skip results |
| `-s`, `--sort FIELD` [`--desc`] | sort by `name`, `path`, `size`, `ext`, `type`, `dm`, `dc`, `da`, `dch`, `runcount` or `dr` |
| `-0`, `--null` | NUL-separated paths, for `xargs -0` |
| `--json` | JSON array |
| `--count` | number of results only |
| `-l`, `--long` | size, date modified and path |
| `--stats` | timings on stderr |
| `--macro 'name=query'` | define a macro (repeatable) |
| `--atime` | also index access times |

Exit status:

| Code | Meaning |
|---|---|
| 0 | results found |
| 1 | no results |
| 2 | usage error |
| 3 | daemon unavailable |
| 4 | invalid query |
| 5 | I/O error |

Paths are printed as the exact bytes on disk. JSON output adds `path_hex` for names that aren’t
valid UTF-8.

## Search syntax

Everything 1.4 syntax plus the useful 1.5 additions and Linux-specific functions. The full reference
is in [`docs/SYNTAX.md`](docs/SYNTAX.md). A few examples:

| Query | Finds |
|---|---|
| `report pdf` | names containing both words |
| <code>song mp3&#124;flac</code> | `song` and (`mp3` or `flac`) |
| `*.iso` | names ending in `.iso` |
| `size:>1gb` | larger than 1 GiB (folders use folder size) |
| `dm:lastweek ext:jpg;png` | images modified last calendar week |
| `parent:/etc` | entries directly inside `/etc` |
| `dupe: pic:` | pictures whose name occurs more than once |
| `content:TODO ext:rs` | Rust files containing `TODO` (read on demand) |
| `perm:+x file:` | executable files |
| `owner:alice !hidden:` | alice’s non-dot files |

## Development

The commands, conventions and settled decisions are in [`CLAUDE.md`](CLAUDE.md). The short
version:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --release -p fossick-bench --bin fossick-bench-report -- query --entries 1000000
```

Benchmark results are in [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md). Fuzzing instructions are in
[`fuzz/README.md`](fuzz/README.md).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
