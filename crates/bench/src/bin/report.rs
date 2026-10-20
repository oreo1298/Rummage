//! `fossick-bench-report`: runs end-to-end benchmark scenarios and prints Markdown tables for
//! `docs/BENCHMARKS.md`.

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};
use fossick_bench::{Rng, Shape, TreeSpec, build_index, random_name, rss_bytes};
use fossick_core::buf::cow_bytes_copied;
use fossick_core::{EntryId, EntryMeta, FileKind};

#[derive(Parser)]
#[command(about = "Fossick benchmark scenarios")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build an index in memory and report time and memory per entry.
    Build {
        #[arg(long, default_value_t = 1_000_000)]
        entries: usize,
        #[arg(long, value_enum, default_value = "realistic")]
        shape: ShapeArg,
    },
    /// Build an index, then replay a checkout-like event storm with periodic publishes.
    Storm {
        #[arg(long, default_value_t = 5_000_000)]
        entries: usize,
        #[arg(long, default_value_t = 100_000)]
        events: usize,
        /// Events applied between publishes.
        #[arg(long, default_value_t = 5_000)]
        batch: usize,
        /// Spread events over random directories across the whole index (worst case) instead of
        /// one repository-like subtree.
        #[arg(long)]
        scattered: bool,
    },
    /// Query latency (p50/p99) for the representative query set and a typing simulation.
    Query {
        #[arg(long, default_value_t = 5_000_000)]
        entries: usize,
        #[arg(long, default_value_t = 30)]
        runs: usize,
    },
    /// Snapshot write and load times (warm and cold page cache) and time to first result.
    Load {
        #[arg(long, default_value_t = 5_000_000)]
        entries: usize,
        /// Where to write the snapshot.
        #[arg(long)]
        file: std::path::PathBuf,
    },
    /// Generate a tree on disk and time a full scan (cold and warm cache).
    Scan {
        #[arg(long, default_value_t = 1_000_000)]
        entries: usize,
        /// Directory to create the tree in (deleted afterwards).
        #[arg(long)]
        dir: std::path::PathBuf,
        /// Keep the generated tree.
        #[arg(long)]
        keep: bool,
    },
}

/// The representative query set (20 queries) used for latency numbers.
pub const QUERIES: [&str; 20] = [
    "a",
    "re",
    "rep",
    "report",
    "report 2023",
    "report|invoice",
    "*.pdf",
    "ext:jpg;png",
    "size:>100mb",
    "dm:thisyear",
    "folder:",
    "!a",
    "café",
    "nodiacritics:cafe",
    "regex:^data_[0-9]+",
    "path:music/",
    "src/main",
    "ww:test",
    "case:Report",
    "audio:",
];

#[derive(Clone, Copy, clap::ValueEnum)]
enum ShapeArg {
    Realistic,
    Deep,
    Wide,
}

fn trim() {
    // SAFETY: malloc_trim has no preconditions; it only returns free heap memory to the OS so
    // that RSS measurements reflect live data.
    #[allow(unsafe_code)]
    unsafe {
        libc::malloc_trim(0);
    }
}

fn mib(b: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let v = b as f64 / (1024.0 * 1024.0);
    v
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

fn build(entries: usize, shape: Shape) {
    trim();
    let (rss0, _) = rss_bytes();
    let mut spec = TreeSpec::new(entries);
    spec.shape = shape;
    let t = Instant::now();
    let (w, _) = build_index(&spec);
    let elapsed = t.elapsed();
    trim();
    let (rss1, peak) = rss_bytes();
    let g = w.handle().load();
    let stats = g.memory_stats();
    let n = g.len() as u64;
    let per = |b: usize| {
        #[allow(clippy::cast_precision_loss)]
        let v = b as f64 / n as f64;
        v
    };
    println!("| metric | value |");
    println!("|---|---|");
    println!("| entries | {n} |");
    println!("| build + first publish | {:.2} s |", elapsed.as_secs_f64());
    println!(
        "| published generation (logical) | {:.1} B/entry |",
        per(stats.total())
    );
    println!(
        "| … names / columns / dirs / orders / side | {:.1} / {:.1} / {:.1} / {:.1} / {:.1} |",
        per(stats.names),
        per(stats.columns),
        per(stats.dirs),
        per(stats.orders),
        per(stats.side_tables)
    );
    println!(
        "| writer-private structures | {:.1} B/entry |",
        per(w.private_heap_bytes())
    );
    #[allow(clippy::cast_precision_loss)]
    let rss_per = (rss1.saturating_sub(rss0)) as f64 / n as f64;
    println!("| RSS growth (after malloc_trim) | {rss_per:.1} B/entry |");
    println!("| peak RSS during build | {:.0} MiB |", mib(peak));
}

fn storm(entries: usize, events: usize, batch: usize, scattered: bool) {
    let t = Instant::now();
    let (mut w, _) = build_index(&TreeSpec::new(entries));
    println!(
        "built {} entries in {:.1} s",
        w.len(),
        t.elapsed().as_secs_f64()
    );
    trim();
    let (rss_before, _) = rss_bytes();
    let g = w.handle().load();
    let mut files: Vec<EntryId> = Vec::new();
    let mut dirs: Vec<EntryId> = Vec::new();
    for id in g.iter() {
        if g.kind(id) == FileKind::Dir {
            dirs.push(id);
        } else {
            files.push(id);
        }
    }
    drop(g);
    // Scattered: 2% of all directories, anywhere in the index (worst case for copy-on-write).
    // Subtree: the directories of one subtree holding ~2% of the index, like a git checkout.
    let mut rng = Rng::new(7);
    let repo_dirs: Vec<EntryId> = if scattered {
        dirs.iter().copied().filter(|_| rng.chance(0.02)).collect()
    } else {
        let target = files.len() / 50;
        let mut best = dirs[0];
        let mut best_diff = usize::MAX;
        let g = w.handle().load();
        let sizes = subtree_sizes(&g);
        for &d in &dirs {
            let n = sizes.get(&d).copied().unwrap_or(0);
            let diff = n.abs_diff(target);
            if diff < best_diff {
                best = d;
                best_diff = diff;
            }
        }
        dirs.iter()
            .copied()
            .filter(|&d| {
                let mut cur = Some(d);
                while let Some(c) = cur {
                    if c == best {
                        return true;
                    }
                    cur = g.parent(c);
                }
                false
            })
            .collect()
    };
    let repo: std::collections::HashSet<EntryId> = repo_dirs.iter().copied().collect();
    let mut repo_files: Vec<EntryId> = files
        .iter()
        .copied()
        .filter(|&f| w.parent(f).is_some_and(|p| repo.contains(&p)))
        .collect();
    println!(
        "storm over {} directories / {} files; {} events, publish every {}",
        repo_dirs.len(),
        repo_files.len(),
        events,
        batch
    );
    let spec = TreeSpec::new(0);
    let mut publish_times = Vec::new();
    let mut cow_per_publish = Vec::new();
    let mut apply_time = Duration::ZERO;
    let mut done = 0;
    let mut peak_rss = 0;
    while done < events {
        let t = Instant::now();
        for _ in 0..batch.min(events - done) {
            let roll = rng.below(100);
            if roll < 50 && !repo_files.is_empty() {
                #[allow(clippy::cast_possible_truncation)]
                let f = repo_files[rng.below(repo_files.len() as u64) as usize];
                if w.is_live(f) {
                    #[allow(clippy::cast_possible_wrap)]
                    let m =
                        EntryMeta::file(rng.below(1 << 20), 1_700_000_000 + rng.below(1000) as i64);
                    let _ = w.update(f, &m);
                }
            } else if roll < 70 {
                let d = *rng.pick(&repo_dirs);
                let name = random_name(&mut rng, &spec, false);
                if let Ok(id) = w.insert(
                    d,
                    &name,
                    &EntryMeta::file(rng.below(1 << 16), 1_700_000_000),
                ) {
                    repo_files.push(id);
                }
            } else if roll < 90 && !repo_files.is_empty() {
                #[allow(clippy::cast_possible_truncation)]
                let i = rng.below(repo_files.len() as u64) as usize;
                let f = repo_files.swap_remove(i);
                if w.is_live(f) {
                    let _ = w.remove(f);
                }
            } else if !repo_files.is_empty() {
                #[allow(clippy::cast_possible_truncation)]
                let f = repo_files[rng.below(repo_files.len() as u64) as usize];
                if let (true, Some(p)) = (w.is_live(f), w.parent(f)) {
                    let name = random_name(&mut rng, &spec, false);
                    let _ = w.rename(f, p, &name);
                }
            }
            done += 1;
        }
        apply_time += t.elapsed();
        let cow0 = cow_bytes_copied();
        let t = Instant::now();
        w.publish();
        publish_times.push(t.elapsed());
        if std::env::var_os("FOSSICK_TRACE_PUBLISH").is_some() {
            eprintln!("{:?}", w.last_publish_stats());
        }
        cow_per_publish.push(cow_bytes_copied() - cow0);
        peak_rss = peak_rss.max(rss_bytes().0);
    }
    publish_times.sort_unstable();
    cow_per_publish.sort_unstable();
    trim();
    let (rss_after, _) = rss_bytes();
    #[allow(clippy::cast_precision_loss)]
    let ev_rate = events as f64 / apply_time.as_secs_f64();
    println!("| metric | value |");
    println!("|---|---|");
    println!("| index size | {} entries |", w.len());
    println!("| events / publish | {batch} |");
    println!("| mutation throughput (writer, excl. publish) | {ev_rate:.0} events/s |");
    println!(
        "| publish latency p50 / p99 / max | {:.1} / {:.1} / {:.1} ms |",
        percentile(&publish_times, 0.5).as_secs_f64() * 1e3,
        percentile(&publish_times, 0.99).as_secs_f64() * 1e3,
        publish_times.last().map_or(0.0, |d| d.as_secs_f64() * 1e3)
    );
    let median_cow = cow_per_publish[cow_per_publish.len() / 2];
    println!(
        "| bytes copied per publish (median / max) | {:.1} / {:.1} MiB |",
        mib(median_cow),
        mib(*cow_per_publish.last().unwrap_or(&0))
    );
    println!(
        "| RSS before / peak during / after storm | {:.0} / {:.0} / {:.0} MiB |",
        mib(rss_before),
        mib(peak_rss),
        mib(rss_after)
    );
    match w.check_invariants() {
        Ok(()) => println!("| invariants after storm | ok |"),
        Err(e) => println!("| invariants after storm | **FAILED: {e}** |"),
    }
}

fn compile(q: &str) -> fossick_core::query::Compiled {
    use fossick_core::query::{
        Clock, CompileContext, Macros, NumericIds, SearchOptions, compile_str,
    };
    let macros = Macros::default();
    let ctx = CompileContext {
        options: SearchOptions::default(),
        clock: Clock::system(),
        macros: &macros,
        users: &NumericIds,
    };
    compile_str(q, &ctx).expect("benchmark queries compile")
}

/// Runs one query (sorted by name) and returns (elapsed, result count).
fn timed_search(
    g: &std::sync::Arc<fossick_core::Generation>,
    c: &fossick_core::query::Compiled,
) -> (Duration, usize) {
    use fossick_core::search::{SearchContext, Sort, search};
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let ctx = SearchContext::basic(&cancel);
    let t = Instant::now();
    let r = search(g, c, Sort::default(), &ctx).expect("not cancelled");
    // Include fetching the first visible window (path building for 50 rows).
    for &id in r.ids.iter().take(50) {
        std::hint::black_box(g.path(id));
    }
    (t.elapsed(), r.ids.len())
}

fn query(entries: usize, runs: usize) {
    let t = Instant::now();
    let (w, _) = build_index(&TreeSpec::new(entries));
    let g = w.handle().load();
    println!(
        "built {} entries in {:.1} s; {} runs per query, sorted by name, first 50 rows fetched",
        g.len(),
        t.elapsed().as_secs_f64(),
        runs
    );
    println!();
    println!("| query | results | p50 (ms) | p99 (ms) |");
    println!("|---|---:|---:|---:|");
    let mut all_p99 = Vec::new();
    for q in QUERIES {
        let c = compile(q);
        let mut times = Vec::with_capacity(runs);
        let mut n = 0;
        for _ in 0..runs {
            let (d, count) = timed_search(&g, &c);
            times.push(d);
            n = count;
        }
        times.sort_unstable();
        let p50 = percentile(&times, 0.5).as_secs_f64() * 1e3;
        let p99 = percentile(&times, 0.99).as_secs_f64() * 1e3;
        all_p99.push(p99);
        println!("| `{q}` | {n} | {p50:.2} | {p99:.2} |");
    }
    all_p99.sort_by(f64::total_cmp);
    println!();
    println!(
        "worst p99 over the set: {:.2} ms",
        all_p99.last().copied().unwrap_or(0.0)
    );
    println!();
    println!("Typing simulation (each keystroke is a new query, fresh search):");
    println!();
    println!("| keystrokes | results | p50 (ms) | p99 (ms) |");
    println!("|---|---:|---:|---:|");
    for k in 1..="report".len() {
        let q = &"report"[..k];
        let c = compile(q);
        let mut times = Vec::new();
        let mut n = 0;
        for _ in 0..runs {
            let (d, count) = timed_search(&g, &c);
            times.push(d);
            n = count;
        }
        times.sort_unstable();
        println!(
            "| `{q}` | {n} | {:.2} | {:.2} |",
            percentile(&times, 0.5).as_secs_f64() * 1e3,
            percentile(&times, 0.99).as_secs_f64() * 1e3
        );
    }
}

/// Drops `path` from the page cache (best effort) so the next read is cold.
fn evict(path: &std::path::Path) {
    use std::os::fd::AsRawFd;
    if let Ok(f) = std::fs::File::open(path) {
        // SAFETY: posix_fadvise only takes an fd and integer arguments; the fd is valid for the
        // duration of the call.
        #[allow(unsafe_code)]
        unsafe {
            libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
    }
}

fn load(entries: usize, file: &std::path::Path) -> Result<()> {
    use fossick_core::persist::{self, Verify};
    let (w, _) = build_index(&TreeSpec::new(entries));
    let g = w.handle().load();
    let t = Instant::now();
    persist::write(&g, file)?;
    let write_time = t.elapsed();
    let size = std::fs::metadata(file)?.len();
    drop(g);
    drop(w);
    trim();
    let c = compile("report");
    println!("| metric | value |");
    println!("|---|---|");
    println!("| entries | {entries} |");
    #[allow(clippy::cast_precision_loss)]
    let per = size as f64 / entries as f64;
    println!(
        "| snapshot size | {:.0} MiB ({per:.0} B/entry) |",
        mib(size)
    );
    println!(
        "| snapshot write (tmp + fsync + rename) | {:.2} s |",
        write_time.as_secs_f64()
    );
    for (label, cold, verify) in [
        (
            "cold cache, verify essential sections",
            true,
            Verify::Essential,
        ),
        (
            "warm cache, verify essential sections",
            false,
            Verify::Essential,
        ),
        ("warm cache, verify all sections", false, Verify::All),
    ] {
        if cold {
            evict(file);
        }
        let t = Instant::now();
        let snap = persist::load(file, verify)?;
        let loaded = t.elapsed();
        let g = std::sync::Arc::new(snap.generation);
        let (q, n) = timed_search(&g, &c);
        println!(
            "| load ({label}) | {:.0} ms; first query `report` {:.1} ms ({n} results); searchable after {:.0} ms |",
            loaded.as_secs_f64() * 1e3,
            q.as_secs_f64() * 1e3,
            (loaded + q).as_secs_f64() * 1e3
        );
    }
    std::fs::remove_file(file)?;
    Ok(())
}

fn scan(entries: usize, dir: &std::path::Path, keep: bool) -> Result<()> {
    use fossick_core::{IndexConfig, IndexWriter};
    use fossick_scan::{ScanOptions, scan_into, stat_path};
    let root = dir.join("tree");
    if !root.exists() {
        let t = Instant::now();
        fossick_bench::generate_on_disk(&TreeSpec::new(entries), &root)?;
        println!(
            "generated {entries} entries on disk in {:.1} s",
            t.elapsed().as_secs_f64()
        );
    }
    println!("| run | entries | dirs | scan | scan + index build |");
    println!("|---|---:|---:|---:|---:|");
    for run in [
        "cold cache (drop_caches)",
        "warm cache",
        "warm cache, idle priority",
    ] {
        if run.starts_with("cold") {
            let _ = std::process::Command::new("sync").status();
            let dropped = std::fs::write("/proc/sys/vm/drop_caches", "3").is_ok();
            if !dropped {
                println!("| (could not drop caches; cold run is not cold) | | | | |");
            }
        }
        let (meta, _, _) = stat_path(&root)?;
        let mut w = IndexWriter::new(IndexConfig::default());
        let (_, r) = w.add_volume(
            "bench",
            root.as_os_str().as_encoded_bytes(),
            "disk",
            None,
            &meta,
        )?;
        let opts = ScanOptions {
            low_priority: run.ends_with("idle priority"),
            ..ScanOptions::default()
        };
        let t = Instant::now();
        let stats = scan_into(&root, &mut w, r, &opts)?;
        let scanned = t.elapsed();
        w.publish();
        println!(
            "| {run} | {} | {} | {:.2} s | {:.2} s |",
            stats.entries,
            stats.dirs,
            scanned.as_secs_f64(),
            t.elapsed().as_secs_f64()
        );
    }
    if !keep {
        std::fs::remove_dir_all(dir.join("tree"))?;
    }
    Ok(())
}

/// Number of entries in each directory's subtree.
fn subtree_sizes(g: &fossick_core::Generation) -> std::collections::HashMap<EntryId, usize> {
    let mut sizes = std::collections::HashMap::new();
    for id in g.iter() {
        let mut cur = g.parent(id);
        while let Some(p) = cur {
            *sizes.entry(p).or_insert(0) += 1;
            cur = g.parent(p);
        }
    }
    sizes
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Build { entries, shape } => build(
            entries,
            match shape {
                ShapeArg::Realistic => Shape::Realistic,
                ShapeArg::Deep => Shape::Deep,
                ShapeArg::Wide => Shape::Wide,
            },
        ),
        Cmd::Storm {
            entries,
            events,
            batch,
            scattered,
        } => storm(entries, events, batch, scattered),
        Cmd::Query { entries, runs } => query(entries, runs),
        Cmd::Load { entries, file } => load(entries, &file)?,
        Cmd::Scan { entries, dir, keep } => scan(entries, &dir, keep)?,
    }
    Ok(())
}
