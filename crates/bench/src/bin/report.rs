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
}

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
    }
    Ok(())
}
