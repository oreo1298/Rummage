//! Wall-clock benchmarks (criterion). Reported, not gated: shared CI runners vary too much.
//! Index-scale numbers (1M/5M entries, p50/p99, memory, load) come from `fossick-bench-report`.

use std::hint::black_box;
use std::sync::atomic::AtomicBool;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use fossick_bench::{TreeSpec, build_index};
use fossick_core::persist::{self, Verify};
use fossick_core::query::{Clock, CompileContext, Macros, NumericIds, SearchOptions, compile_str};
use fossick_core::search::{SearchContext, Sort, search};

const QUERIES: [&str; 8] = [
    "re",
    "report",
    "report 2023|invoice",
    "*.pdf",
    "regex:^data_[0-9]+",
    "café",
    "path:music/",
    "size:>100mb",
];

fn benches(c: &mut Criterion) {
    let (w, _) = build_index(&TreeSpec::new(1_000_000));
    let g = w.handle().load();
    let macros = Macros::default();
    let ctx = CompileContext {
        options: SearchOptions::default(),
        clock: Clock::system(),
        macros: &macros,
        users: &NumericIds,
    };
    let mut group = c.benchmark_group("query_1m");
    group.sample_size(20);
    for q in QUERIES {
        let compiled = compile_str(q, &ctx).expect("valid query");
        group.bench_with_input(BenchmarkId::from_parameter(q), &compiled, |b, compiled| {
            let cancel = AtomicBool::new(false);
            let sctx = SearchContext::basic(&cancel);
            b.iter(|| black_box(search(&g, compiled, Sort::default(), &sctx).map(|r| r.ids.len())));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("snapshot_1m");
    group.sample_size(10);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bench.fsidx");
    group.bench_function("write", |b| {
        b.iter(|| persist::write(&g, &path).expect("write"))
    });
    group.bench_function("load_warm_essential", |b| {
        b.iter(|| {
            black_box(
                persist::load(&path, Verify::Essential)
                    .expect("load")
                    .generation
                    .len(),
            )
        });
    });
    group.finish();

    let mut group = c.benchmark_group("build");
    group.sample_size(10);
    group.bench_function("index_100k", |b| {
        b.iter(|| black_box(build_index(&TreeSpec::new(100_000)).0.len()))
    });
    group.finish();
}

criterion_group!(core, benches);
criterion_main!(core);
