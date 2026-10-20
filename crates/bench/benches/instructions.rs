//! Instruction-count benchmarks (Callgrind via gungraun). Deterministic, so CI can fail on a
//! regression of more than 10% (`--callgrind-limits='ir=10%'`).
//!
//! Callgrind only counts the benchmarked function’s own thread, so the global rayon pool is
//! built with a single worker that *is* the current thread (each benchmark runs in its own
//! process): everything executes, and is counted, on the benchmark thread, with no scheduling
//! noise.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use fossick_bench::{Rng, TreeSpec, build_index, random_name};
use fossick_core::query::{
    Clock, CompileContext, Compiled, Macros, NumericIds, SearchOptions, compile_str,
};
use fossick_core::search::{SearchContext, Sort, search};
use fossick_core::{EntryId, EntryMeta, Generation, IndexWriter};
use gungraun::{LibraryBenchmarkConfig, library_benchmark, library_benchmark_group, main};

fn compile(q: &str) -> Compiled {
    let macros = Macros::default();
    let ctx = CompileContext {
        options: SearchOptions::default(),
        clock: Clock::at(1_718_206_200, jiff::tz::TimeZone::UTC),
        macros: &macros,
        users: &NumericIds,
    };
    compile_str(q, &ctx).expect("valid query")
}

fn index_50k() -> Arc<Generation> {
    build_index(&TreeSpec::new(50_000)).0.handle().load()
}

/// Makes the global rayon pool run on the calling thread. Must run before any rayon use.
fn serial_rayon() {
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .use_current_thread()
        .build_global();
}

fn setup_query(q: &str) -> (Arc<Generation>, Compiled) {
    serial_rayon();
    (index_50k(), compile(q))
}

#[library_benchmark]
#[bench::substring(setup_query("report"))]
#[bench::short(setup_query("re"))]
#[bench::and_or(setup_query("report 2023|invoice"))]
#[bench::wildcard(setup_query("*.pdf"))]
#[bench::regex(setup_query("regex:^data_[0-9]+"))]
#[bench::non_ascii(setup_query("café"))]
#[bench::path(setup_query("path:music/"))]
#[bench::size(setup_query("size:>1mb file:"))]
fn query_50k(input: (Arc<Generation>, Compiled)) -> usize {
    let (g, c) = input;
    let cancel = AtomicBool::new(false);
    let r = search(&g, &c, Sort::default(), &SearchContext::basic(&cancel)).expect("search");
    black_box(r.ids.len())
}

#[library_benchmark]
#[bench::mixed(setup_compile(
    "report 2023|invoice !old size:>1mb dm:lastweek ext:pdf;doc regex:\"^a(b|c)$\""
))]
fn parse_and_compile(q: &str) -> Compiled {
    black_box(compile(black_box(q)))
}

fn setup_compile(q: &'static str) -> &'static str {
    serial_rayon();
    q
}

fn setup_storm() -> (IndexWriter, Vec<EntryId>) {
    serial_rayon();
    let (w, _) = build_index(&TreeSpec::new(50_000));
    let g = w.handle().load();
    let files: Vec<EntryId> = g
        .iter()
        .filter(|&id| !g.kind(id).is_dir())
        .take(2_000)
        .collect();
    (w, files)
}

#[library_benchmark]
#[bench::mutations_2000(setup_storm())]
fn publish_2000_mutations(input: (IndexWriter, Vec<EntryId>)) -> usize {
    let (mut w, files) = input;
    let mut rng = Rng::new(1);
    let spec = TreeSpec::new(0);
    for (i, &f) in files.iter().enumerate() {
        match i % 4 {
            0 => {
                let _ = w.update(f, &EntryMeta::file(rng.below(1 << 20), 1_700_000_000));
            }
            1 => {
                let _ = w.remove(f);
            }
            2 => {
                if let Some(p) = w.parent(f) {
                    let _ = w.rename(f, p, &random_name(&mut rng, &spec, false));
                }
            }
            _ => {
                if let Some(p) = w.parent(f) {
                    let _ = w.insert(
                        p,
                        &random_name(&mut rng, &spec, false),
                        &EntryMeta::file(1, 1),
                    );
                }
            }
        }
    }
    let g = w.publish();
    black_box(g.len())
}

library_benchmark_group!(name = engine; benchmarks = query_50k, parse_and_compile, publish_2000_mutations);

main!(
    config = LibraryBenchmarkConfig::default();
    library_benchmark_groups = engine
);
