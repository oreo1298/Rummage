# Fossick

Instant, Everything-style file-name search for Linux. Fossick indexes every file and folder name
on every mounted filesystem, finds them as you type, and stays current in real time.

> **Status: Phases 1–2 of 6 complete. Nothing is packaged yet.**
>
> What works today:
>
> - the search engine (index, Everything-compatible query language, sorting, snapshots);
> - the daemon `fossickd`: indexes every mounted filesystem, keeps the index current in real
>   time (fanotify, inotify or polling), and filters every result by the asking user’s
>   permissions;
> - the `fossick` command-line client (through the daemon, or in-process with `--local`).
>
> The GUIs and packages are planned. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the
> design and plan, [`docs/TODO.md`](docs/TODO.md) for what is missing, and
> [`docs/SECURITY.md`](docs/SECURITY.md) for the permission model.

Fossick is an independent project, not affiliated with voidtools. It reimplements the behaviour
and search syntax of voidtools Everything for Linux.

## Try it (from source)

You need Rust 1.88 or newer (`rustup` or your distribution’s `rust` / `cargo` package).

```sh
git clone https://github.com/oreo1298/Rummage.git fossick && cd fossick
cargo build --release
```

### With the system daemon (indexes everything, filters per user)

```sh
sudo install -m 755 target/release/fossickd target/release/fossick /usr/local/bin/
sudo install -m 644 packaging/systemd/fossick.sysusers /usr/lib/sysusers.d/fossick.conf
sudo systemd-sysusers
sed 's|/usr/bin/fossickd|/usr/local/bin/fossickd|' packaging/systemd/fossickd.service \
  | sudo tee /etc/systemd/system/fossickd.service > /dev/null
sudo cp packaging/systemd/fossickd.socket /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now fossickd.socket fossickd.service

fossick --status                       # volumes and how each is kept current
fossick 'ext:pdf dm:thisyear'
fossick -n 10 --sort size --desc 'size:>100mb'
fossick -0 '*.rs' | xargs -0 wc -l
```

The configuration file is `/etc/fossick/fossickd.toml` (all keys are optional; the documented
defaults are in [`packaging/fossickd.toml`](packaging/fossickd.toml)). Without systemd, run
`sudo fossickd` (it binds `/run/fossick/socket` itself).

### Without root (user daemon)

If no system daemon is running, `fossick` starts `fossickd --user`, which indexes your home
directory only (with inotify) and keeps its index in `~/.cache/fossick`:

```sh
target/release/fossick 'report pdf'     # starts the user daemon on first use
```

### Without any daemon

`--local DIR` scans `DIR` in-process (it never crosses into other mounts) and searches it.
`--save FILE` keeps that index, and `--load FILE` searches it later:

```sh
target/release/fossick --local ~ 'ext:pdf dm:thisyear'
target/release/fossick --local / --stats --count ''
```

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
| `--atime` | also index access times (`--local`) |
| `--status` | show the daemon’s volumes, watch methods and entry counts |
| `--rescan [MOUNT]` | ask the daemon to rescan a volume, or every volume without MOUNT (root for the system daemon) |
| `--socket PATH`, `--user-daemon` | choose the daemon |

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
