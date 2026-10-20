//! Running compiled queries against a generation: filtering, sorting, result rows.

mod eval;

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::index::{EntryId, Generation, OrderKind, TimeField, flags};
use crate::meta::FileKind;
use crate::name;
use crate::query::compile::{Compiled, Pred};
use crate::query::matcher::TextMatch;
use crate::util::Bitmap;

/// Run counts and last-run times (per user, maintained by the daemon).
pub trait RunHistory: Sync {
    fn run_count(&self, id: EntryId) -> u32;
    /// Unix seconds of the last run.
    fn last_run(&self, id: EntryId) -> Option<i64>;
}

/// No run history.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRuns;

impl RunHistory for NoRuns {
    fn run_count(&self, _: EntryId) -> u32 {
        0
    }
    fn last_run(&self, _: EntryId) -> Option<i64> {
        None
    }
}

/// Opens file contents for `content:` (the caller decides how permissions are enforced).
pub trait ContentSource: Sync {
    fn open(&self, path: &[u8]) -> std::io::Result<Box<dyn Read + Send>>;
}

/// Maps names to MIME types for `type:` and the Type column.
pub trait MimeDb: Sync {
    fn mime_type(&self, name: &[u8]) -> Option<&str>;
}

/// Inputs to a search besides the query and the generation.
pub struct SearchContext<'a> {
    pub runs: &'a dyn RunHistory,
    pub content: Option<&'a dyn ContentSource>,
    pub mime: Option<&'a dyn MimeDb>,
    /// Entries the caller may see (`None` = all). The daemon passes the peer’s visibility map.
    pub visible: Option<&'a Bitmap>,
    /// Set to cancel the search (checked per page).
    pub cancel: &'a AtomicBool,
    /// Largest file `content:` reads, in bytes.
    pub content_limit: u64,
}

impl std::fmt::Debug for SearchContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchContext").finish_non_exhaustive()
    }
}

impl<'a> SearchContext<'a> {
    /// A context with no run history, no content access and no MIME data.
    #[must_use]
    pub fn basic(cancel: &'a AtomicBool) -> Self {
        Self {
            runs: &NoRuns,
            content: None,
            mime: None,
            visible: None,
            cancel,
            content_limit: 256 << 20,
        }
    }
}

/// Sortable columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SortField {
    #[default]
    Name,
    Path,
    Size,
    Extension,
    Type,
    Modified,
    Created,
    Accessed,
    Changed,
    RunCount,
    DateRun,
}

impl SortField {
    /// Parses CLI-style names (`name`, `path`, `size`, `ext`, `type`, `dm`, `dc`, `da`, `dch`,
    /// `runcount`, `dr`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "name" => Self::Name,
            "path" => Self::Path,
            "size" => Self::Size,
            "ext" | "extension" => Self::Extension,
            "type" => Self::Type,
            "dm" | "modified" | "date-modified" | "datemodified" => Self::Modified,
            "dc" | "created" | "date-created" | "datecreated" => Self::Created,
            "da" | "accessed" | "date-accessed" | "dateaccessed" => Self::Accessed,
            "dch" | "changed" | "date-changed" | "datechanged" => Self::Changed,
            "runcount" | "run-count" => Self::RunCount,
            "dr" | "daterun" | "date-run" => Self::DateRun,
            _ => return None,
        })
    }

    fn presorted(self) -> Option<OrderKind> {
        match self {
            Self::Name => Some(OrderKind::Name),
            Self::Path => Some(OrderKind::Path),
            Self::Size => Some(OrderKind::Size),
            Self::Modified => Some(OrderKind::Modified),
            _ => None,
        }
    }
}

/// Sort order of a result list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Sort {
    pub field: SortField,
    pub descending: bool,
}

/// Search failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("search cancelled")]
    Cancelled,
}

/// The ids matching a query, in display order.
#[derive(Clone)]
pub struct SearchResult {
    pub generation: Arc<Generation>,
    pub ids: Vec<EntryId>,
    pub sort: Sort,
    pub elapsed: Duration,
}

impl std::fmt::Debug for SearchResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchResult")
            .field("count", &self.ids.len())
            .field("sort", &self.sort)
            .field("elapsed", &self.elapsed)
            .finish_non_exhaustive()
    }
}

/// Runs a compiled query and returns the matches in the requested order.
pub fn search(
    generation: &Arc<Generation>,
    query: &Compiled,
    sort: Sort,
    ctx: &SearchContext<'_>,
) -> Result<SearchResult, SearchError> {
    let start = Instant::now();
    let engine = eval::Engine::new(generation, &query.pred, ctx);
    let hits = engine.evaluate_all(&query.pred);
    if ctx.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(SearchError::Cancelled);
    }
    let ids = sorted_ids(generation, &hits, sort, ctx);
    Ok(SearchResult {
        generation: Arc::clone(generation),
        ids,
        sort,
        elapsed: start.elapsed(),
    })
}

/// Counts matches without sorting.
pub fn count(
    generation: &Arc<Generation>,
    query: &Compiled,
    ctx: &SearchContext<'_>,
) -> Result<usize, SearchError> {
    let engine = eval::Engine::new(generation, &query.pred, ctx);
    let hits = engine.evaluate_all(&query.pred);
    if ctx.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(SearchError::Cancelled);
    }
    Ok(hits.count())
}

/// Turns a hit bitmap into ids ordered by `sort`.
fn sorted_ids(g: &Generation, hits: &Bitmap, sort: Sort, ctx: &SearchContext<'_>) -> Vec<EntryId> {
    let n = hits.count();
    if let Some(kind) = sort.field.presorted()
        && n * 32 >= g.len().max(1)
    {
        return g
            .orders()
            .get(kind)
            .filter(hits.words(), sort.descending)
            .into_iter()
            .map(EntryId)
            .collect();
    }
    let mut ids: Vec<EntryId> = Vec::with_capacity(n);
    for (wi, &w) in hits.words().iter().enumerate() {
        let mut w = w;
        while w != 0 {
            ids.push(EntryId(
                u32::try_from(wi * 64 + w.trailing_zeros() as usize).unwrap_or(u32::MAX),
            ));
            w &= w - 1;
        }
    }
    sort_ids(g, &mut ids, sort, ctx);
    ids
}

/// Sorts ids in place (used for small result sets and non-presorted columns).
pub fn sort_ids(g: &Generation, ids: &mut [EntryId], sort: Sort, ctx: &SearchContext<'_>) {
    let nk = |id: EntryId| u64::from(g.data.name_key.get(id.index()));
    if let Some(kind) = sort.field.presorted() {
        ids.par_sort_unstable_by_key(|&id| (g.order_key(kind, id), id));
    } else {
        match sort.field {
            SortField::Extension => {
                ids.par_sort_by_cached_key(|&id| {
                    let mut k = Vec::new();
                    if !g.data.is_dir(id.0) {
                        crate::fold::collation_key_into(
                            name::extension(g.name(id)).unwrap_or(b""),
                            &mut k,
                        );
                    }
                    (k, nk(id), id)
                });
            }
            SortField::Type => {
                ids.par_sort_by_cached_key(|&id| (type_name(g, ctx, id), nk(id), id));
            }
            SortField::Created | SortField::Accessed | SortField::Changed => {
                let field = match sort.field {
                    SortField::Created => TimeField::Created,
                    SortField::Accessed => TimeField::Accessed,
                    _ => TimeField::Changed,
                };
                ids.par_sort_unstable_by_key(|&id| {
                    (
                        g.data.time_secs(id.0, field).unwrap_or(i64::MIN),
                        nk(id),
                        id,
                    )
                });
            }
            SortField::RunCount => {
                ids.par_sort_unstable_by_key(|&id| (ctx.runs.run_count(id), nk(id), id));
            }
            SortField::DateRun => {
                ids.par_sort_unstable_by_key(|&id| {
                    (ctx.runs.last_run(id).unwrap_or(i64::MIN), nk(id), id)
                });
            }
            _ => {}
        }
    }
    if sort.descending {
        ids.reverse();
    }
}

/// The type string used by `type:` and the Type column: the MIME type, plus “folder”/“file”.
#[must_use]
pub fn type_name(g: &Generation, ctx: &SearchContext<'_>, id: EntryId) -> String {
    match g.kind(id) {
        FileKind::Dir => "inode/directory folder".to_owned(),
        FileKind::Symlink => "inode/symlink".to_owned(),
        FileKind::File => match ctx.mime.and_then(|m| m.mime_type(g.name(id))) {
            Some(t) => format!("{t} file"),
            None => "application/octet-stream file".to_owned(),
        },
        FileKind::CharDevice => "inode/chardevice".to_owned(),
        FileKind::BlockDevice => "inode/blockdevice".to_owned(),
        FileKind::Fifo => "inode/fifo".to_owned(),
        FileKind::Socket => "inode/socket".to_owned(),
        FileKind::Unknown => "unknown".to_owned(),
    }
}

/// True if the file’s contents match (reads at most `content_limit` bytes; cancellable).
pub(crate) fn content_matches(
    g: &Generation,
    ctx: &SearchContext<'_>,
    m: &TextMatch,
    id: EntryId,
) -> bool {
    let Some(src) = ctx.content else {
        return false;
    };
    if g.kind(id) != FileKind::File {
        return false;
    }
    let Ok(mut r) = src.open(&g.path(id)) else {
        return false;
    };
    let overlap = m
        .needle
        .len()
        .saturating_sub(1)
        .max(if m.is_regex() { 4096 } else { 0 });
    let mut buf = vec![0u8; 256 * 1024];
    let mut keep = 0usize;
    let mut total = 0u64;
    loop {
        if ctx.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let n = match r.read(&mut buf[keep..]) {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        total += n as u64;
        let window = &buf[..keep + n];
        if m.is_match(window) {
            return true;
        }
        if total >= ctx.content_limit {
            return false;
        }
        let k = overlap.min(window.len());
        let from = window.len() - k;
        buf.copy_within(from..from + k, 0);
        keep = k;
    }
}

/// Highlight ranges for a result row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Highlights {
    /// Byte ranges in the name.
    pub name: Vec<(usize, usize)>,
    /// Byte ranges in the full path.
    pub path: Vec<(usize, usize)>,
}

/// Computes highlight ranges for one entry.
#[must_use]
pub fn highlights(g: &Generation, query: &Compiled, id: EntryId) -> Highlights {
    let mut names = Vec::new();
    let mut paths = Vec::new();
    query.pred.highlight_matchers(&mut names, &mut paths);
    let mut h = Highlights::default();
    let name = g.name(id);
    for m in names {
        h.name.extend(m.highlight(name));
    }
    if !paths.is_empty() {
        let path = g.path(id);
        for m in paths {
            h.path.extend(m.highlight(&path));
        }
    }
    for v in [&mut h.name, &mut h.path] {
        v.sort_unstable();
        v.dedup();
    }
    h
}

/// Is this entry offline (its volume is not mounted)?
#[must_use]
pub fn is_offline(g: &Generation, id: EntryId) -> bool {
    g.volume(id).is_some_and(|v| !v.online)
}

/// Is this entry hidden (dot name)?
#[must_use]
pub fn is_hidden(g: &Generation, id: EntryId) -> bool {
    g.flags(id) & flags::HIDDEN != 0
}

/// Convenience: true if `pred` is satisfied by `id` (used by live result updates).
#[must_use]
pub fn matches_one(g: &Generation, pred: &Pred, ctx: &SearchContext<'_>, id: EntryId) -> bool {
    let engine = eval::Engine::new(g, pred, ctx);
    let page = id.page();
    let mut cand = [0u64; eval::WORDS];
    cand[id.slot() / 64] |= 1 << (id.slot() % 64);
    let r = engine.eval(pred, page, &cand);
    r[id.slot() / 64] & (1 << (id.slot() % 64)) != 0
}

#[cfg(test)]
mod tests;
