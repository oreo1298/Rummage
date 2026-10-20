//! `fossick`: command-line search (the Everything `es.exe` equivalent).
//!
//! Phase 1 supports in-process searches (`--local DIR`, `--load SNAPSHOT`); searching through
//! the system daemon arrives with the daemon.

mod output;
mod users;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use clap::Parser;
use fossick_core::persist::{self, Verify};
use fossick_core::query::{Clock, CompileContext, Macros, SearchOptions, compile_str};
use fossick_core::search::{SearchContext, Sort, SortField, count, search};
use fossick_core::{Generation, IndexConfig, IndexWriter};
use fossick_scan::content::FsContent;
use fossick_scan::mime::MimeGlobs;
use fossick_scan::{ScanOptions, scan_into, stat_path};

/// Exit codes (documented in `fossick --help`).
mod exit {
    pub const FOUND: u8 = 0;
    pub const NOT_FOUND: u8 = 1;
    // 2 = usage error (clap)
    pub const NO_DAEMON: u8 = 3;
    pub const QUERY_ERROR: u8 = 4;
    pub const IO_ERROR: u8 = 5;
}

#[derive(Parser, Debug)]
#[command(
    name = "fossick",
    version,
    about = "Instant file name search (Everything syntax)",
    after_help = "EXIT STATUS: 0 = results found, 1 = no results, 2 = usage error, \
                  3 = daemon unavailable, 4 = invalid query, 5 = I/O error.\n\n\
                  Examples:\n  fossick --local ~ 'ext:pdf dm:thisyear'\n  \
                  fossick --local /usr -n 20 --sort size --desc 'size:>100mb'\n  \
                  fossick --local . -0 '*.rs' | xargs -0 wc -l"
)]
struct Cli {
    /// Search query (Everything syntax). Multiple arguments are joined with spaces.
    query: Vec<String>,

    /// Index DIR in-process now and search it (no daemon needed).
    #[arg(long, value_name = "DIR", conflicts_with = "load")]
    local: Option<PathBuf>,
    /// Search a saved index snapshot instead of scanning.
    #[arg(long, value_name = "FILE")]
    load: Option<PathBuf>,
    /// Save the index built with --local to FILE.
    #[arg(long, value_name = "FILE", requires = "local")]
    save: Option<PathBuf>,

    /// Treat the query as a regular expression.
    #[arg(short = 'r', long)]
    regex: bool,
    /// Match case.
    #[arg(long = "case")]
    case: bool,
    /// Match whole words.
    #[arg(short = 'w', long)]
    whole_word: bool,
    /// Match against the full path instead of the name.
    #[arg(short = 'p', long = "path")]
    match_path: bool,
    /// Ignore diacritics (cafe matches café).
    #[arg(long)]
    ignore_diacritics: bool,

    /// Maximum number of results to print.
    #[arg(short = 'n', long, value_name = "N")]
    limit: Option<usize>,
    /// Skip the first N results.
    #[arg(long, value_name = "N", default_value_t = 0)]
    offset: usize,
    /// Sort by: name, path, size, ext, type, dm, dc, da, dch, runcount, dr.
    #[arg(short = 's', long, value_name = "FIELD", default_value = "name")]
    sort: String,
    /// Sort descending.
    #[arg(long, conflicts_with = "asc")]
    desc: bool,
    /// Sort ascending (default).
    #[arg(long)]
    asc: bool,

    /// Separate results with NUL bytes (for xargs -0).
    #[arg(short = '0', long = "null")]
    null: bool,
    /// Print results as a JSON array.
    #[arg(long, conflicts_with_all = ["null", "count"])]
    json: bool,
    /// Print only the number of results.
    #[arg(long)]
    count: bool,
    /// Print size, date modified and path.
    #[arg(short = 'l', long)]
    long: bool,
    /// Print timing statistics to stderr.
    #[arg(long)]
    stats: bool,
    /// Scanner threads for --local (default: all CPUs).
    #[arg(long, value_name = "N", default_value_t = 0)]
    threads: usize,
    /// Index access times too (enables da: and the date-accessed sort for --local).
    #[arg(long)]
    atime: bool,
    /// Define a macro, e.g. --macro 'media=audio:|video:' (repeatable).
    #[arg(long = "macro", value_name = "NAME=QUERY")]
    macros: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err((code, msg)) => {
            let _ = writeln!(io::stderr(), "fossick: {msg}");
            ExitCode::from(code)
        }
    }
}

type Failure = (u8, String);

fn load_generation(cli: &Cli) -> Result<Arc<Generation>, Failure> {
    if let Some(snapshot) = &cli.load {
        let t = Instant::now();
        let snap = persist::load(snapshot, Verify::All)
            .map_err(|e| (exit::IO_ERROR, format!("{}: {e}", snapshot.display())))?;
        if cli.stats {
            eprintln!(
                "loaded {} entries ({} MiB) in {:.1} ms",
                snap.generation.len(),
                snap.file_len() >> 20,
                t.elapsed().as_secs_f64() * 1e3
            );
        }
        return Ok(Arc::new(snap.generation));
    }
    let Some(dir) = &cli.local else {
        return Err((
            exit::NO_DAEMON,
            "the Fossick daemon is not available yet (it arrives in Phase 2); \
             use --local DIR to search a directory in-process"
                .into(),
        ));
    };
    let dir = std::fs::canonicalize(dir)
        .map_err(|e| (exit::IO_ERROR, format!("{}: {e}", dir.display())))?;
    let (meta, _, _) =
        stat_path(&dir).map_err(|e| (exit::IO_ERROR, format!("{}: {e}", dir.display())))?;
    let mut w = IndexWriter::new(IndexConfig {
        track_atime: cli.atime,
    });
    let (_, root) = w
        .add_volume(
            "local",
            dir.as_os_str().as_encoded_bytes(),
            "local",
            None,
            &meta,
        )
        .map_err(|e| (exit::IO_ERROR, e.to_string()))?;
    let opts = ScanOptions {
        threads: cli.threads,
        ..ScanOptions::default()
    };
    let t = Instant::now();
    let stats =
        scan_into(&dir, &mut w, root, &opts).map_err(|e| (exit::IO_ERROR, e.to_string()))?;
    let scanned = t.elapsed();
    let g = w.publish();
    if cli.stats {
        eprintln!(
            "scanned {} entries in {} directories in {:.2} s (+{:.2} s index build), {} errors",
            stats.entries,
            stats.dirs,
            scanned.as_secs_f64(),
            (t.elapsed() - scanned).as_secs_f64(),
            stats.errors
        );
        for e in &stats.error_samples {
            eprintln!("  {e}");
        }
    }
    if let Some(path) = &cli.save {
        persist::write(&g, path)
            .map_err(|e| (exit::IO_ERROR, format!("{}: {e}", path.display())))?;
    }
    Ok(g)
}

fn run(cli: &Cli) -> Result<u8, Failure> {
    let field = SortField::parse(&cli.sort)
        .ok_or_else(|| (2, format!("unknown sort field {:?}", cli.sort)))?;
    let sort = Sort {
        field,
        descending: cli.desc,
    };
    let generation = load_generation(cli)?;
    let options = SearchOptions {
        match_case: cli.case,
        whole_word: cli.whole_word,
        match_path: cli.match_path,
        regex: cli.regex,
        match_diacritics: !cli.ignore_diacritics,
        ..SearchOptions::default()
    };
    let mut macros = Macros::default();
    for m in &cli.macros {
        let (name, query) = m
            .split_once('=')
            .ok_or_else(|| (2, format!("--macro expects NAME=QUERY, got {m:?}")))?;
        macros.user.insert(name.trim().to_owned(), query.to_owned());
    }
    let users = users::SystemUsers;
    let ctx = CompileContext {
        options,
        clock: Clock::system(),
        macros: &macros,
        users: &users,
    };
    let query = cli.query.join(" ");
    let compiled = compile_str(&query, &ctx).map_err(|e| (exit::QUERY_ERROR, e.to_string()))?;
    let mime = MimeGlobs::load_system();
    let cancel = AtomicBool::new(false);
    let mut sctx = SearchContext::basic(&cancel);
    sctx.content = Some(&FsContent);
    sctx.mime = mime
        .as_ref()
        .map(|m| m as &dyn fossick_core::search::MimeDb);
    let t = Instant::now();
    if cli.count {
        let n =
            count(&generation, &compiled, &sctx).map_err(|e| (exit::QUERY_ERROR, e.to_string()))?;
        if cli.stats {
            eprintln!(
                "counted {n} results in {:.2} ms",
                t.elapsed().as_secs_f64() * 1e3
            );
        }
        println!("{n}");
        return Ok(if n > 0 { exit::FOUND } else { exit::NOT_FOUND });
    }
    let result = search(&generation, &compiled, sort, &sctx)
        .map_err(|e| (exit::QUERY_ERROR, e.to_string()))?;
    if cli.stats {
        eprintln!(
            "{} results in {:.2} ms (index: {} entries)",
            result.ids.len(),
            result.elapsed.as_secs_f64() * 1e3,
            generation.len()
        );
    }
    let ids = result
        .ids
        .iter()
        .skip(cli.offset)
        .take(cli.limit.unwrap_or(usize::MAX));
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let written = if cli.json {
        output::json(&mut out, &generation, ids)
    } else {
        output::plain(&mut out, &generation, ids, cli.null, cli.long)
    };
    match written.and_then(|()| out.flush()) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
        Err(e) => return Err((exit::IO_ERROR, e.to_string())),
    }
    Ok(if result.ids.is_empty() {
        exit::NOT_FOUND
    } else {
        exit::FOUND
    })
}
