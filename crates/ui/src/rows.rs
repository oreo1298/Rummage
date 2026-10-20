//! Rows as the frontends show them, and a cache of row windows fetched on demand.
//!
//! A result can have millions of rows; only the pages a view actually shows are fetched. When a
//! live result changes, cached pages are kept but marked stale, so the view keeps showing
//! something sensible while fresh pages arrive.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fossick_proto::{Kind, Row};

/// Rows per fetched page.
pub const PAGE: u64 = 200;

/// One result row, prepared for display (strings are lossy UTF-8; `path` keeps the raw bytes
/// for acting on the file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowData {
    pub name: String,
    /// `name` as HTML with the matched parts in `<b>`.
    pub name_html: String,
    /// The containing folder.
    pub parent: String,
    /// `parent` as HTML with the matched parts of a path search in `<b>`.
    pub parent_html: String,
    /// The full path as HTML with the matched parts in `<b>` (for path searches).
    pub path_html: String,
    /// The full path, raw.
    pub path: Vec<u8>,
    pub kind: Kind,
    /// `None`: unknown, or hidden from this user (folder sizes of private subtrees).
    pub size: Option<u64>,
    pub modified: Option<i64>,
    pub created: Option<i64>,
    pub accessed: Option<i64>,
    pub extension: String,
    /// `ls -l` style, e.g. `drwxr-xr-x`.
    pub permissions: String,
    /// `user:group` names (numbers when unknown).
    pub owner: String,
    pub run_count: u32,
    /// The volume isn’t mounted.
    pub offline: bool,
}

/// Looks up user and group names, remembering them.
#[derive(Debug, Default)]
pub struct Names {
    users: HashMap<u32, String>,
    groups: HashMap<u32, String>,
}

impl Names {
    pub fn user(&mut self, uid: u32) -> &str {
        self.users.entry(uid).or_insert_with(|| {
            nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
                .ok()
                .flatten()
                .map_or_else(|| uid.to_string(), |u| u.name)
        })
    }

    /// `user:group`.
    pub fn owner(&mut self, uid: u32, gid: u32) -> String {
        let user = self.user(uid).to_owned();
        format!("{user}:{}", self.group(gid))
    }

    pub fn group(&mut self, gid: u32) -> &str {
        self.groups.entry(gid).or_insert_with(|| {
            nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid))
                .ok()
                .flatten()
                .map_or_else(|| gid.to_string(), |g| g.name)
        })
    }
}

impl RowData {
    /// Prepares a row from the daemon.
    #[must_use]
    pub fn from_row(r: &Row, names: &mut Names) -> Self {
        let path = full_path(&r.parent, &r.name);
        let name_html = highlighted(&r.name, &r.name_highlights);
        let path_html = highlighted(&path, &r.path_highlights);
        let parent_len = u32::try_from(r.parent.len()).unwrap_or(u32::MAX);
        let parent_marks: Vec<(u32, u32)> = r
            .path_highlights
            .iter()
            .filter(|&&(s, _)| s < parent_len)
            .map(|&(s, e)| (s, e.min(parent_len)))
            .collect();
        let parent_html = highlighted(&r.parent, &parent_marks);
        let extension = if r.kind == Kind::Folder {
            String::new()
        } else {
            extension(&r.name).map_or_else(String::new, |e| String::from_utf8_lossy(e).into_owned())
        };
        let owner = names.owner(r.uid, r.gid);
        Self {
            name: String::from_utf8_lossy(&r.name).into_owned(),
            name_html,
            parent: String::from_utf8_lossy(&r.parent).into_owned(),
            parent_html,
            path_html,
            path,
            kind: r.kind,
            size: r.size,
            modified: r.modified,
            created: r.created,
            accessed: r.accessed,
            extension,
            permissions: permissions(r.mode, r.kind),
            owner,
            run_count: r.run_count,
            offline: r.offline,
        }
    }
}

/// `parent/name`, without doubling the slash of `/`.
#[must_use]
pub fn full_path(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = parent.to_vec();
    if name.is_empty() {
        return p;
    }
    if p.last() != Some(&b'/') {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// The extension of a name, as the query language sees it (`.bashrc` has none).
#[must_use]
pub fn extension(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    (dot > 0).then(|| &name[dot + 1..])
}

/// Escapes text for Qt’s styled-text subset of HTML.
fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

/// `bytes` as escaped HTML with the byte ranges `marks` in bold. Ranges that are out of order,
/// overlapping or out of bounds are ignored.
#[must_use]
pub fn highlighted(bytes: &[u8], marks: &[(u32, u32)]) -> String {
    let mut out = String::with_capacity(bytes.len() + 16);
    let mut pos = 0usize;
    for &(start, end) in marks {
        let (s, e) = (start as usize, end as usize);
        if s < pos || e <= s || e > bytes.len() {
            continue;
        }
        escape(&String::from_utf8_lossy(&bytes[pos..s]), &mut out);
        out.push_str("<b>");
        escape(&String::from_utf8_lossy(&bytes[s..e]), &mut out);
        out.push_str("</b>");
        pos = e;
    }
    escape(&String::from_utf8_lossy(&bytes[pos..]), &mut out);
    out
}

/// `ls -l` style permissions.
#[must_use]
pub fn permissions(mode: u32, kind: Kind) -> String {
    let mut s = String::with_capacity(10);
    s.push(match kind {
        Kind::Folder => 'd',
        Kind::Symlink => 'l',
        Kind::File => '-',
        Kind::Other => match mode & 0o170_000 {
            0o010_000 => 'p',
            0o020_000 => 'c',
            0o060_000 => 'b',
            0o140_000 => 's',
            _ => '?',
        },
    });
    let special = [(0o4000, 's', 'S'), (0o2000, 's', 'S'), (0o1000, 't', 'T')];
    for (i, (bit, set_x, set_no_x)) in special.into_iter().enumerate() {
        let shift = 6 - 3 * i;
        let triple = (mode >> shift) & 7;
        s.push(if triple & 4 != 0 { 'r' } else { '-' });
        s.push(if triple & 2 != 0 { 'w' } else { '-' });
        let x = triple & 1 != 0;
        s.push(match (mode & bit != 0, x) {
            (true, true) => set_x,
            (true, false) => set_no_x,
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// What the cache has for a row.
#[derive(Debug)]
pub enum Lookup<'a> {
    /// Current data.
    Ready(&'a Arc<RowData>),
    /// Data from before the result last changed (a refresh is on its way).
    Stale(&'a Arc<RowData>),
    /// Not fetched yet.
    Missing,
}

#[derive(Debug)]
struct Page {
    rows: Vec<Arc<RowData>>,
    stale: bool,
    used: u64,
}

/// Pages of one query’s rows, least recently used ones dropped beyond a capacity.
#[derive(Debug)]
pub struct RowCache {
    query: u64,
    total: u64,
    pages: HashMap<u64, Page>,
    requested: HashSet<u64>,
    tick: u64,
    capacity: usize,
}

impl RowCache {
    /// A cache keeping at most `capacity` pages.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            query: 0,
            total: 0,
            pages: HashMap::new(),
            requested: HashSet::new(),
            tick: 0,
            capacity: capacity.max(1),
        }
    }

    #[must_use]
    pub fn query(&self) -> u64 {
        self.query
    }

    #[must_use]
    pub fn total(&self) -> u64 {
        self.total
    }

    /// A new result: forget everything.
    pub fn reset(&mut self, query: u64, total: u64) {
        self.query = query;
        self.total = total;
        self.pages.clear();
        self.requested.clear();
    }

    /// The current result changed: keep rows for display, but fetch them again.
    pub fn invalidate(&mut self, total: u64) {
        self.total = total;
        for p in self.pages.values_mut() {
            p.stale = true;
        }
        self.requested.clear();
    }

    /// The row at `row`, and the offset of the page to fetch for it if it is missing or stale
    /// and not already requested (the caller must then fetch `[offset, offset + PAGE)`).
    pub fn get(&mut self, row: u64) -> (Lookup<'_>, Option<u64>) {
        if row >= self.total {
            return (Lookup::Missing, None);
        }
        let page = row / PAGE;
        self.tick += 1;
        let tick = self.tick;
        let fetch = match self.pages.get(&page) {
            Some(p) if !p.stale => None,
            _ => self.requested.insert(page).then_some(page * PAGE),
        };
        let lookup = match self.pages.get_mut(&page) {
            Some(p) => {
                p.used = tick;
                match p.rows.get((row - page * PAGE) as usize) {
                    Some(r) if p.stale => Lookup::Stale(r),
                    Some(r) => Lookup::Ready(r),
                    None => Lookup::Missing,
                }
            }
            None => Lookup::Missing,
        };
        (lookup, fetch)
    }

    /// The row if it is cached (current or stale), without fetching.
    #[must_use]
    pub fn peek(&self, row: u64) -> Option<&Arc<RowData>> {
        let page = row / PAGE;
        self.pages
            .get(&page)
            .and_then(|p| p.rows.get((row - page * PAGE) as usize))
    }

    /// Stores fetched rows. Returns the row range `[first, last]` they cover, or `None` if they
    /// belong to an older query.
    pub fn insert(&mut self, query: u64, offset: u64, rows: Vec<RowData>) -> Option<(u64, u64)> {
        if query != self.query || !offset.is_multiple_of(PAGE) {
            return None;
        }
        let page = offset / PAGE;
        self.requested.remove(&page);
        if rows.is_empty() {
            self.pages.remove(&page);
            return None;
        }
        let last = offset + rows.len() as u64 - 1;
        self.tick += 1;
        self.pages.insert(
            page,
            Page {
                rows: rows.into_iter().map(Arc::new).collect(),
                stale: false,
                used: self.tick,
            },
        );
        while self.pages.len() > self.capacity {
            let Some((&oldest, _)) = self.pages.iter().min_by_key(|(_, p)| p.used) else {
                break;
            };
            self.pages.remove(&oldest);
        }
        Some((offset, last))
    }

    /// A fetch failed: allow asking again.
    pub fn forget_request(&mut self, offset: u64) {
        self.requested.remove(&(offset / PAGE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str) -> RowData {
        RowData::from_row(
            &Row {
                name: name.as_bytes().to_vec(),
                parent: b"/home/u".to_vec(),
                kind: Kind::File,
                size: Some(1),
                modified: None,
                created: None,
                accessed: None,
                changed: None,
                mode: 0o100_644,
                uid: 0,
                gid: 0,
                run_count: 0,
                last_run: None,
                offline: false,
                name_highlights: vec![],
                path_highlights: vec![],
            },
            &mut Names::default(),
        )
    }

    #[test]
    fn display_helpers() {
        assert_eq!(
            highlighted(b"a<b>c.txt", &[(1, 4)]),
            "a<b>&lt;b&gt;</b>c.txt"
        );
        assert_eq!(highlighted(b"abc", &[(2, 1), (0, 9)]), "abc");
        assert_eq!(highlighted(b"x\xffy", &[(2, 3)]), "x\u{fffd}<b>y</b>");
        assert_eq!(permissions(0o040_755, Kind::Folder), "drwxr-xr-x");
        assert_eq!(permissions(0o104_755, Kind::File), "-rwsr-xr-x");
        assert_eq!(permissions(0o041_777, Kind::Folder), "drwxrwxrwt");
        assert_eq!(permissions(0o102_640, Kind::File), "-rw-r-S---");
        assert_eq!(full_path(b"/", b"etc"), b"/etc");
        assert_eq!(extension(b".bashrc"), None);
        let r = row("Report.PDF");
        assert_eq!(r.extension, "PDF");
        assert_eq!(r.path, b"/home/u/Report.PDF");
        assert_eq!(r.owner, "root:root");
    }

    #[test]
    fn cache_fetches_each_page_once_and_evicts() {
        let mut c = RowCache::new(2);
        c.reset(1, 1000);
        let (l, f) = c.get(5);
        assert!(matches!(l, Lookup::Missing));
        assert_eq!(f, Some(0));
        assert_eq!(c.get(7).1, None, "already requested");
        assert_eq!(c.get(1000).1, None, "past the end");
        let rows: Vec<RowData> = (0..PAGE).map(|i| row(&format!("f{i}"))).collect();
        assert_eq!(c.insert(1, 0, rows.clone()), Some((0, PAGE - 1)));
        assert!(matches!(c.get(5).0, Lookup::Ready(r) if r.name == "f5"));
        assert_eq!(c.insert(0, 0, rows.clone()), None, "older query");
        // Live change: stale rows stay visible and are fetched again.
        c.invalidate(900);
        let (l, f) = c.get(5);
        assert!(matches!(l, Lookup::Stale(r) if r.name == "f5"));
        assert_eq!(f, Some(0));
        c.insert(1, 0, rows.clone());
        c.insert(1, PAGE, rows.clone());
        c.get(3);
        c.insert(1, 2 * PAGE, rows);
        assert!(c.peek(3).is_some(), "recently used page kept");
        assert!(
            c.peek(PAGE + 1).is_none(),
            "least recently used page evicted"
        );
        // A new query starts empty.
        c.reset(2, 10);
        assert!(c.peek(3).is_none());
        assert_eq!(c.get(3).1, Some(0));
        c.forget_request(0);
        assert_eq!(c.get(3).1, Some(0), "requested again after a failure");
    }
}
