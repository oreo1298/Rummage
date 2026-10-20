//! Runs the Everything-syntax compatibility corpus in `tests/corpus/` against a fixture index.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use fossick_core::meta::{S_IFDIR, S_IFLNK, S_IFREG};
use fossick_core::query::{Clock, CompileContext, Macros, NumericIds, SearchOptions, compile_str};
use fossick_core::search::{SearchContext, Sort, SortField, search};
use fossick_core::{EntryId, EntryMeta, Generation, IndexConfig, IndexWriter, Timestamp};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    entries: Vec<FixtureEntry>,
}

#[derive(Deserialize)]
struct FixtureEntry {
    path: String,
    size: Option<String>,
    mtime: Option<String>,
    btime: Option<String>,
    atime: Option<String>,
    mode: Option<String>,
    uid: Option<u32>,
    gid: Option<u32>,
    kind: Option<String>,
}

#[derive(Deserialize, Default)]
struct Options {
    match_case: Option<bool>,
    whole_word: Option<bool>,
    match_path: Option<bool>,
    regex: Option<bool>,
    match_diacritics: Option<bool>,
}

#[derive(Deserialize)]
struct Case {
    query: String,
    expect: Option<Vec<String>>,
    count: Option<usize>,
    error: Option<String>,
    #[serde(default)]
    ordered: bool,
    sort: Option<String>,
    #[serde(default)]
    descending: bool,
    #[serde(default)]
    options: Options,
    source: String,
}

#[derive(Deserialize)]
struct CaseFile {
    #[serde(default)]
    macros: HashMap<String, String>,
    #[serde(rename = "case")]
    cases: Vec<Case>,
}

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus")
}

/// Decodes `\xNN` escapes into raw bytes.
fn raw(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && b.get(i + 1) == Some(&b'x')
            && i + 4 <= b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 2..i + 4], 16)
        {
            out.push(v);
            i += 4;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn ts(s: &str) -> Timestamp {
    Timestamp::from_secs(
        s.parse::<jiff::Timestamp>()
            .expect("fixture timestamp")
            .as_second(),
    )
}

fn parse_size(s: &str) -> u64 {
    let v = fossick_core::query::value::parse_size(s).expect("fixture size");
    u64::try_from(v.start).expect("non-negative size")
}

fn build_fixture() -> Arc<Generation> {
    let text = std::fs::read_to_string(corpus_dir().join("fixture.toml")).expect("fixture.toml");
    let fixture: Fixture = toml::from_str(&text).expect("valid fixture");
    let mut w = IndexWriter::new(IndexConfig { track_atime: true });
    let default_time = ts("2024-01-15T12:00:00Z");
    let root_meta = EntryMeta {
        mode: S_IFDIR | 0o755,
        mtime: Some(default_time),
        ctime: Some(default_time),
        inode: 2,
        ..EntryMeta::default()
    };
    let (_, root) = w
        .add_volume("corpus", b"/c", "corpusfs", Some("Corpus"), &root_meta)
        .unwrap();
    let mut dirs: HashMap<Vec<u8>, EntryId> = HashMap::new();
    dirs.insert(Vec::new(), root);
    for (i, e) in fixture.entries.iter().enumerate() {
        let is_dir = e.path.ends_with('/');
        let path = raw(e.path.trim_end_matches('/'));
        let (parent, name) = match path.iter().rposition(|&b| b == b'/') {
            Some(p) => (path[..p].to_vec(), path[p + 1..].to_vec()),
            None => (Vec::new(), path.clone()),
        };
        let parent_id = *dirs
            .get(&parent)
            .unwrap_or_else(|| panic!("parent of {} missing", e.path));
        let kind_bits = match (is_dir, e.kind.as_deref()) {
            (true, _) => S_IFDIR,
            (false, Some("symlink")) => S_IFLNK,
            _ => S_IFREG,
        };
        let perm = e
            .mode
            .as_deref()
            .map_or(if is_dir { 0o755 } else { 0o644 }, |m| {
                u32::from_str_radix(m, 8).expect("octal mode")
            });
        let mtime = e.mtime.as_deref().map_or(default_time, ts);
        let meta = EntryMeta {
            mode: kind_bits | perm,
            uid: e.uid.unwrap_or(0),
            gid: e.gid.unwrap_or(0),
            size: e.size.as_deref().map_or(0, parse_size),
            mtime: Some(mtime),
            ctime: Some(mtime),
            btime: e.btime.as_deref().map(ts),
            atime: e.atime.as_deref().map(ts),
            inode: 100 + i as u64,
            ..EntryMeta::default()
        };
        let id = w
            .insert(parent_id, &name, &meta)
            .expect("unique fixture paths");
        if is_dir {
            dirs.insert(path, id);
        }
    }
    let g = w.publish();
    w.check_invariants().expect("fixture index is consistent");
    g
}

fn options(o: &Options) -> SearchOptions {
    let d = SearchOptions::default();
    SearchOptions {
        match_case: o.match_case.unwrap_or(d.match_case),
        whole_word: o.whole_word.unwrap_or(d.whole_word),
        match_path: o.match_path.unwrap_or(d.match_path),
        regex: o.regex.unwrap_or(d.regex),
        match_diacritics: o.match_diacritics.unwrap_or(d.match_diacritics),
        ..d
    }
}

fn show(v: &[Vec<u8>]) -> Vec<String> {
    v.iter()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// Runs one case; returns a failure description.
fn run_case(g: &Arc<Generation>, macros: &Macros, case: &Case, label: &str) -> Option<String> {
    let ctx = CompileContext {
        options: options(&case.options),
        clock: Clock::at(1_718_206_200, jiff::tz::TimeZone::UTC),
        macros,
        users: &NumericIds,
    };
    let compiled = match (compile_str(&case.query, &ctx), &case.error) {
        (Err(e), Some(want)) => {
            return (!format!("{e:?}").starts_with(want.as_str()))
                .then(|| format!("{label}: expected error {want}, got {e:?}"));
        }
        (Err(e), None) => return Some(format!("{label}: unexpected error {e}")),
        (Ok(_), Some(want)) => {
            return Some(format!("{label}: expected error {want}, query compiled"));
        }
        (Ok(c), None) => c,
    };
    let sort = Sort {
        field: case.sort.as_deref().map_or(SortField::Name, |s| {
            SortField::parse(s).expect("sort field")
        }),
        descending: case.descending,
    };
    let cancel = AtomicBool::new(false);
    let result = search(g, &compiled, sort, &SearchContext::basic(&cancel)).expect("search");
    let mut got: Vec<Vec<u8>> = result.ids.iter().map(|&id| g.path(id)).collect();
    if let Some(n) = case.count {
        return (got.len() != n)
            .then(|| format!("{label}: expected {n} results, got {}", got.len()));
    }
    let mut want: Vec<Vec<u8>> = case
        .expect
        .as_ref()
        .expect("each case needs expect, count or error")
        .iter()
        .map(|s| raw(s))
        .collect();
    if !case.ordered {
        got.sort();
        want.sort();
    }
    (got != want).then(|| {
        format!(
            "{label}:\n    expected {:?}\n    got      {:?}",
            show(&want),
            show(&got)
        )
    })
}

#[test]
fn syntax_corpus() {
    let g = build_fixture();
    let mut files: Vec<PathBuf> = std::fs::read_dir(corpus_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml") && !p.ends_with("fixture.toml"))
        .collect();
    files.sort();
    let mut failures = Vec::new();
    let mut total = 0;
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        let cf: CaseFile =
            toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let macros = Macros {
            user: cf.macros.clone(),
        };
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        for case in &cf.cases {
            total += 1;
            let label = format!("{name} :: {:?} ({})", case.query, case.source);
            if let Some(f) = run_case(&g, &macros, case, &label) {
                failures.push(f);
            }
        }
    }
    assert!(total >= 140, "corpus unexpectedly small: {total}");
    assert!(
        failures.is_empty(),
        "{} of {total} corpus cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
