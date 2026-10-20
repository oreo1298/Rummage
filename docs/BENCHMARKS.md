# Benchmarks

All numbers are real measurements. Each table says which machine produced it.

**Development VM** (every table below): 4-core Intel Xeon @ 2.10 GHz (no SMT), 15 GB RAM, virtio
disk with ext4, Linux 6.18, rustc 1.99.1, release profile (thin LTO). It is **not** the target
machine (Ryzen 9 9950X3D, 16 cores / 32 threads, 96 GB). Searches use every core, so expect the
target machine to be several times faster. Run the same commands there for real numbers; nothing
here was measured on it.

The synthetic trees come from `fossick-bench` (`crates/bench`): realistic name vocabularies with
10% non-ASCII names (in both NFC and NFD), a share of names that aren’t valid UTF-8, extensions
drawn from a realistic mix, and sizes/dates spread over 20 years. Shapes: *realistic* (mixed
depth, ~12% folders), *deep* (long chains) and *wide* (huge directories).

## Reproduce

```sh
B="cargo run --release -p fossick-bench --bin fossick-bench-report --"
$B build --entries 5000000 [--shape deep|wide]     # build time and memory per entry
$B query --entries 5000000 --runs 20               # 20 queries + typing simulation
$B load --file /tmp/idx.fsx --entries 5000000      # snapshot write, cold/warm load
$B storm --entries 5000000 [--scattered]           # 100k-event storm, publish every 5000
$B scan --dir /tmp/tree --entries 1000000          # on-disk tree: cold and warm scan (root)
MALLOC_ARENA_MAX=1 $B build --entries 5000000      # RSS without glibc per-thread arenas
cargo bench -p fossick-bench --bench core          # criterion wall-clock benches
cargo bench -p fossick-bench --bench instructions  # instruction counts (needs valgrind)
```

## Targets from the spec

The spec’s targets are for the 9950X3D with a warm cache. This table compares them with the VM;
it doesn’t claim anything about the target machine.

| Target | Development VM | Status on the VM |
|---|---|---|
| full scan of 1M files < 5 s | 2.02 s scan, 2.64 s scan + index build (warm) | met |
| snapshot load to searchable < 1 s | 5M entries: 146 ms warm, 291 ms cold | met |
| query p99 < 50 ms at 5M entries | 10 of 20 queries; worst p99 92 ms (`a`, 2.2M results) | **not met** on 4 cores |
| < ~120 B RAM per entry | 106 B logical + writer-private; 107 B RSS growth at 5M | met |
| near-zero idle CPU | daemon not built yet (Phase 2) | not measured |

## Index build and memory

`build`: generate the tree in memory, insert every entry through `IndexWriter`, publish once.
“Logical” counts the bytes held by the published generation (columns, name arenas, directory
records, the four presorted orders, side tables). “Writer-private” is the writer’s child lists,
directory sequence and free lists. RSS growth is measured after `malloc_trim`.

| Shape | Entries | Build + first publish | Logical | Writer-private | RSS growth | Peak RSS |
|---|---:|---:|---:|---:|---:|---:|
| realistic | 1M | 2.77 s | 78.6 B/entry | 21.4 B/entry | 166.6 B/entry | 421 MiB |
| deep | 1M | 3.06 s | 94.1 B/entry | 51.3 B/entry | 241.9 B/entry | 476 MiB |
| wide | 1M | 2.48 s | 74.1 B/entry | 8.2 B/entry | 176.8 B/entry | 408 MiB |
| realistic | 5M | 20.02 s | 78.4 B/entry | 27.4 B/entry | 116.9 B/entry | 1867 MiB |
| realistic, `MALLOC_ARENA_MAX=1` | 1M | 3.02 s | 78.6 B/entry | 21.4 B/entry | 109.1 B/entry | 403 MiB |
| realistic, `MALLOC_ARENA_MAX=1` | 5M | 19.48 s | 78.4 B/entry | 27.4 B/entry | 107.3 B/entry | 1779 MiB |

Logical breakdown at 5M (realistic): names 19.1, columns 38.0, directory records 5.3, orders
16.0, side tables 0.0 B/entry.

The gap between logical + private memory and RSS growth with default settings is glibc
fragmentation. The parallel build allocates transient buffers in per-thread malloc arenas, and
`malloc_trim` can’t return all of it. With one arena the gap shrinks to ~9 B/entry. The daemon
(Phase 2) will cap malloc arenas for that reason. The deep shape costs more because it has more
directories (directory records and child lists) and longer paths in the path order.

## Query latency

`query`: 20 representative queries, each run 30 times (1M) or 20 times (5M) on a fresh search,
sorted by name, including fetching the first 50 rows (path building). Times are wall-clock for
`search()` plus the first window. The typing simulation runs each prefix of `report` as a new
query (no refinement yet).

| Query | Results (1M) | p50 / p99 (ms), 1M | Results (5M) | p50 / p99 (ms), 5M |
|---|---:|---:|---:|---:|
| `a` | 443,753 | 12.2 / 21.3 | 2,220,397 | 61.0 / 91.9 |
| `re` | 80,828 | 6.5 / 8.0 | 404,539 | 31.3 / 37.2 |
| `rep` | 20,639 | 6.1 / 10.9 | 102,397 | 32.2 / 42.5 |
| `report` | 20,639 | 6.3 / 10.7 | 102,397 | 31.6 / 36.1 |
| `report 2023` | 1 | 5.5 / 7.6 | 3 | 28.1 / 34.9 |
| <code>report&#124;invoice</code> | 40,918 | 11.0 / 12.6 | 204,066 | 49.5 / 65.3 |
| `*.pdf` | 26,551 | 9.2 / 10.8 | 132,145 | 45.5 / 63.8 |
| `ext:jpg;png` | 52,713 | 10.9 / 12.2 | 263,298 | 54.6 / 65.1 |
| `size:>100mb` | 152,979 | 6.0 / 7.8 | 765,967 | 29.3 / 36.5 |
| `dm:thisyear` | 0 | 4.2 / 4.8 | 0 | 19.1 / 27.3 |
| `folder:` | 119,870 | 4.7 / 6.0 | 600,868 | 22.3 / 26.5 |
| `!a` | 556,248 | 11.9 / 17.2 | 2,779,604 | 60.3 / 62.8 |
| `café` | 5,472 | 1.9 / 2.3 | 27,774 | 8.9 / 12.1 |
| `nodiacritics:cafe` | 5,472 | 4.3 / 14.9 | 27,774 | 23.1 / 76.7 |
| `regex:^data_[0-9]+` | 0 | 6.0 / 10.2 | 0 | 28.7 / 51.4 |
| `path:music/` | 75,588 | 10.2 / 14.4 | 443,887 | 53.5 / 66.5 |
| `src/main` | 139 | 9.0 / 12.4 | 2,751 | 52.9 / 73.1 |
| `ww:test` | 9,756 | 6.8 / 7.6 | 48,564 | 37.5 / 42.4 |
| `case:Report` | 4,039 | 2.7 / 5.9 | 19,792 | 13.7 / 18.4 |
| `audio:` | 53,050 | 12.9 / 16.3 | 264,113 | 62.2 / 71.4 |

Worst p99: 21.3 ms at 1M, 91.9 ms at 5M. The `nodiacritics:` p99 includes the one-time build of
the diacritics-stripped side arenas on its first run. `dm:thisyear` matches nothing because the
generator’s dates end before the benchmark’s clock year.

Typing simulation (each keystroke a fresh search):

| Keystrokes | p50 / p99 (ms), 1M | p50 / p99 (ms), 5M |
|---|---:|---:|
| `r` | 12.2 / 17.6 | 61.2 / 70.2 |
| `re` | 6.3 / 8.0 | 33.7 / 41.9 |
| `rep` | 6.2 / 7.7 | 33.1 / 40.2 |
| `repo` | 6.1 / 7.8 | 34.2 / 55.3 |
| `repor` | 5.8 / 7.4 | 34.9 / 39.9 |
| `report` | 6.1 / 8.9 | 32.1 / 34.8 |

### What changed after the first run

The first full run on this VM (same commands, before the query fixes) had a worst p99 of 546 ms at
5M. The fixes, each covered by a brute-force equivalence test (`search::tests::
fast_paths_match_per_entry_reference`):

| Query (5M) | p50 before → after (ms) | Fix |
|---|---:|---|
| `*.pdf` | 478 → 45 | `*`-only wildcards matched as literal pieces with an arena prefilter, not a regex |
| `rep` | 135 → 32 | small result sets sort precomputed `(key, id)` pairs instead of looking keys up per comparison |
| `regex:^data_[0-9]+` | 279 → 29 | one regex search per page arena with `^`/`$` bound to NUL |
| `src/main` | 233 → 53 | path substring via per-directory “ends with the needle’s head” flags, no path building |
| `café` | 102 → 9 | non-ASCII needles skip ASCII names entirely |
| `ext:jpg;png` | 80 → 55 | extensions compared as packed `u64`s against each name’s tail |

The wildcard change also fixed a bug: the old regex translation’s `.*` didn’t match bytes that
aren’t valid UTF-8, so `*.pdf` missed such names (26,508 vs 26,551 results at 1M).

## Snapshot load

`load`: build, write the snapshot (tmp + fsync + rename + dir fsync), then load it with a cold
page cache (`posix_fadvise(DONTNEED)`) and a warm one, and run a first query.

| Entries | Snapshot | Write | Cold load → searchable | Warm load → searchable | Warm, verify all sections |
|---:|---:|---:|---:|---:|---:|
| 1M | 75 MiB (79 B/entry) | 0.11 s | 39 ms → 55 ms | 13 ms → 27 ms | 28 ms → 43 ms |
| 5M | 376 MiB (79 B/entry) | 0.50 s | 203 ms → 291 ms | 76 ms → 146 ms | 141 ms → 223 ms |

“Searchable after” is load time plus the first `report` query (88 ms cold and 71 ms warm at 5M).
The essential sections (header, names, parents, flags) are verified before serving; the rest can
be verified in the background.

## Event storms (writer and publish)

`storm`: on a 5M index, replay 100,000 mixed events (create, delete, rename, move, metadata
change), publishing every 5,000 events. *subtree* concentrates them in one subtree, like a
`git checkout`; *scattered* spreads them over the whole index, the worst case for copy-on-write.

| Metric | subtree | scattered |
|---|---:|---:|
| directories / files touched | 10,434 / 76,709 | 11,951 / 87,198 |
| writer mutation throughput (excluding publish) | 310,332 events/s | 59,723 events/s |
| publish latency p50 / p99 / max | 85.0 / 98.3 / 98.3 ms | 212.3 / 237.7 / 237.7 ms |
| bytes copied per publish (median / max) | 19.5 / 20.6 MiB | 95.6 / 97.2 MiB |
| RSS before / peak / after | 559 / 652 / 603 MiB | 579 / 838 / 627 MiB |
| index invariants after the storm | ok | ok |

A 5,000-event publish doesn’t meet the plan’s 50 ms publish cadence on this VM. Real storms are
usually subtree-shaped, and the daemon publishes smaller batches more often when idle, but a
scattered storm of this size takes ~4.5 s of writer time per 100k events here.

## Scan

`scan`: a generated tree of 1,000,000 entries (119,870 directories) on the VM’s ext4 disk.
`drop_caches` before the cold run.

| Run | Scan | Scan + index build |
|---|---:|---:|
| cold cache | 5.99 s | 6.75 s |
| warm cache | 2.02 s | 2.64 s |
| warm cache, idle I/O priority + nice 19 | 2.12 s | 2.83 s |

Real filesystems scanned with `fossick --local`: `/usr` (93K entries) in 0.19 s warm; the root
filesystem (234K entries) in 0.58 s warm and 3.64 s cold.

## Daemon (`fossickd`)

Development VM, the daemon indexing the VM’s own root filesystem (ext4, 274K entries) with
fanotify, queried with the `fossick` CLI over the socket.

| Metric | Value |
|---|---|
| initial full scan of `/` (270K entries, 35K directories) | 1.69 s |
| new file → visible in search (5 samples, includes starting the CLI each time) | 17, 26, 27, 49, 83 ms |
| CPU while idle | 0.01 s over 20 s |
| resident memory | 80 MiB (fixed overhead dominates at this size; per-entry cost is in the build tables above) |
| threads while idle | 10 |
| user daemon: first query including starting `fossickd --user` (small home) | 71 ms |

Event storms on loop filesystems (`tests/integration/storm.sh`, 512 MiB images): time from the
end of a storm until the index equals a fresh walk.

| Filesystem, watcher | Operations | Entries afterwards | Caught up after |
|---|---:|---:|---:|
| ext4, fanotify | 10,000 | 33,070 | 0.8–1.2 s |
| ext4, inotify | 8,000 | 33,017 | 1.0–1.2 s |
| xfs, fanotify | 10,000 | 84,570 | 2.1–2.5 s |
| xfs, inotify | 8,000 | 62,438 | 1.5–2.0 s |
| ext4, polling (10 s interval) | 3,000 | 32,899 | 9.0 s |

The storms are generated by a Python script that runs as fast as it can (create, write, rename,
move, delete, chmod, symlink, hard link, bursts of hundreds of files); the daemon applies events
in batches and publishes at most every 50 ms.

## Instruction counts (CI regression gate)

`cargo bench -p fossick-bench --bench instructions` counts instructions with Callgrind
(gungraun) on a 50K-entry index with a single-threaded rayon pool, so the numbers are
deterministic. CI fails when a benchmark regresses by more than 10% against the base commit.
Wall-clock numbers vary too much on shared runners to gate on.

Counts after the Phase 1 query fixes and the Phase 2 follow-up (`cargo bench -p fossick-bench
--bench instructions`, development VM):

| Benchmark | Instructions |
|---|---:|
| `report` | 15.6M |
| `re` | 16.6M |
| `report 2023\|invoice` | 16.1M |
| `*.pdf` | 17.1M |
| `regex:^data_[0-9]+` | 12.8M |
| `café` | 11.5M |
| `path:music/` | 13.2M |
| `size:>1mb file:` | 8.7M |
| parse + compile (mixed query) | 0.60M |
| 2,000 mutations + publish | 52.9M |

The gate caught two regressions from the first round of fast paths (CI run #8): building the
arena regex eagerly (+85% compile) and a slower per-entry leaf dispatch (+22% for `size:`). Both
were fixed in the next commit.
