//! The client socket: one reader thread and one executor thread per connection.
//!
//! The reader decodes requests and cancels the running query when a newer one arrives (every
//! keystroke supersedes the previous query). The executor runs queries against the latest
//! generation, keeps the result list, and serves row windows from it. Live queries are re-run
//! when the index changes, at most ten times a second, and the client is told to re-fetch.

use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fossick_core::perm::{Creds, Visibility};
use fossick_core::query::{
    Clock, CompileContext, Compiled, Macros, QueryError, SearchOptions, UserDb, compile_str,
};
use fossick_core::search::{self, MimeDb, SearchContext, SearchError, Sort, SortField as CoreSort};
use fossick_core::{EntryId, FileKind, Generation, TimeField};
use fossick_proto::{
    ErrorKind, Kind, MAX_ROWS, PROTOCOL_VERSION, QueryRequest, Request, Response, Row, ServerMode,
    SortField, SortSpec, read_message, write_message,
};
use fossick_scan::mime::MimeGlobs;

use crate::events::Cmd;
use crate::log;
use crate::perms::{PermContent, VisCache, peer_creds};
use crate::runs::{ResolvedRuns, RunStore};
use crate::state::Shared;

/// Longest path accepted for the run history.
const MAX_RUNS_PATH: usize = 4096;

/// Resolves user and group names through NSS.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemUsers;

impl UserDb for SystemUsers {
    fn uid_of(&self, name: &str) -> Option<u32> {
        nix::unistd::User::from_name(name)
            .ok()
            .flatten()
            .map(|u| u.uid.as_raw())
    }

    fn gid_of(&self, name: &str) -> Option<u32> {
        nix::unistd::Group::from_name(name)
            .ok()
            .flatten()
            .map(|g| g.gid.as_raw())
    }
}

/// A connection’s executor, as the live-update thread sees it.
#[derive(Debug)]
struct LiveReg {
    tx: Sender<Exec>,
    pending: Arc<AtomicBool>,
}

/// Server-wide state.
#[derive(Debug)]
pub struct Server {
    shared: Arc<Shared>,
    writer: Sender<Cmd>,
    vis: VisCache,
    runs: RunStore,
    mime: Option<MimeGlobs>,
    live: Mutex<Vec<LiveReg>>,
    daemon_uid: u32,
}

/// Work for a connection’s executor.
#[derive(Debug)]
enum Exec {
    Query(QueryRequest, Arc<AtomicBool>),
    Request(Request),
    /// The index changed: re-run a live query.
    Refresh,
}

impl Server {
    #[must_use]
    pub fn new(shared: Arc<Shared>, writer: Sender<Cmd>) -> Arc<Self> {
        let runs = RunStore::new(shared.paths.state_dir.join("runs"));
        Arc::new(Self {
            shared,
            writer,
            vis: VisCache::default(),
            runs,
            mime: MimeGlobs::load_system(),
            live: Mutex::new(Vec::new()),
            daemon_uid: rustix::process::getuid().as_raw(),
        })
    }

    /// Accepts connections forever.
    pub fn serve(self: &Arc<Self>, listener: &UnixListener) {
        let me = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("fossick-live".into())
            .spawn(move || me.live_loop());
        for stream in listener.incoming() {
            if self.shared.shutting_down() {
                break;
            }
            let Ok(stream) = stream else {
                continue;
            };
            let me = Arc::clone(self);
            let _ = std::thread::Builder::new()
                .name("fossick-conn".into())
                .spawn(move || me.connection(stream));
        }
    }

    fn live_loop(&self) {
        let mut seen = self.shared.current().number();
        while !self.shared.shutting_down() {
            let n = self.shared.wait_generation(seen, Duration::from_secs(5));
            if n == seen {
                continue;
            }
            seen = n;
            // At most ten refreshes a second; changes in the meantime are picked up together.
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(mut regs) = self.live.lock() {
                regs.retain(|r| {
                    if r.pending.swap(true, Ordering::AcqRel) {
                        true
                    } else {
                        r.tx.send(Exec::Refresh).is_ok()
                    }
                });
            }
        }
    }

    fn connection(self: Arc<Self>, stream: UnixStream) {
        let Ok(creds) = peer_creds(&stream) else {
            return;
        };
        let Ok(read_half) = stream.try_clone() else {
            return;
        };
        let out = Arc::new(Mutex::new(stream));
        let mut reader = BufReader::new(read_half);
        let send = |r: &Response| -> bool {
            out.lock()
                .is_ok_and(|mut s| write_message(&mut *s, r).is_ok())
        };
        match read_message::<Request>(&mut reader) {
            Ok(Request::Hello { version, .. }) if version == PROTOCOL_VERSION => {}
            Ok(Request::Hello { version, .. }) => {
                send(&Response::Error {
                    id: None,
                    kind: ErrorKind::Protocol,
                    message: format!(
                        "protocol version {version} is not supported (server speaks {PROTOCOL_VERSION})"
                    ),
                });
                return;
            }
            _ => {
                send(&Response::Error {
                    id: None,
                    kind: ErrorKind::Protocol,
                    message: "expected Hello".into(),
                });
                return;
            }
        }
        if self.shared.mode == ServerMode::User && creds.uid != self.daemon_uid {
            send(&Response::Error {
                id: None,
                kind: ErrorKind::Denied,
                message: "this is another user’s Fossick daemon".into(),
            });
            return;
        }
        if !send(&Response::Welcome {
            version: PROTOCOL_VERSION,
            server: format!("fossickd {}", env!("CARGO_PKG_VERSION")),
            mode: self.shared.mode,
            uid: creds.uid,
        }) {
            return;
        }
        let (tx, rx) = mpsc::channel::<Exec>();
        let pending = Arc::new(AtomicBool::new(false));
        if let Ok(mut regs) = self.live.lock() {
            regs.push(LiveReg {
                tx: tx.clone(),
                pending: Arc::clone(&pending),
            });
        }
        let latest = Arc::new(AtomicU64::new(u64::MAX));
        let session = Session {
            server: Arc::clone(&self),
            creds,
            out: Arc::clone(&out),
            latest: Arc::clone(&latest),
            pending,
            active: None,
        };
        let executor = std::thread::Builder::new()
            .name("fossick-exec".into())
            .spawn(move || session.run(&rx));
        let mut cancel = Arc::new(AtomicBool::new(false));
        while let Ok(req) = read_message::<Request>(&mut reader) {
            match req {
                Request::Query(q) => {
                    cancel.store(true, Ordering::Relaxed);
                    cancel = Arc::new(AtomicBool::new(false));
                    latest.store(q.id, Ordering::Relaxed);
                    if tx.send(Exec::Query(q, Arc::clone(&cancel))).is_err() {
                        break;
                    }
                }
                Request::Cancel { id } => {
                    if latest.load(Ordering::Relaxed) == id {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
                Request::Goodbye => break,
                other => {
                    if tx.send(Exec::Request(other)).is_err() {
                        break;
                    }
                }
            }
        }
        cancel.store(true, Ordering::Relaxed);
        drop(tx);
        if let Ok(e) = executor {
            let _ = e.join();
        }
    }
}

/// A finished query.
#[derive(Debug)]
struct Active {
    req: QueryRequest,
    compiled: Compiled,
    g: Arc<Generation>,
    vis: Option<Arc<Visibility>>,
    runs: Arc<ResolvedRuns>,
    ids: Vec<EntryId>,
    cancel: Arc<AtomicBool>,
}

/// A connection’s executor state.
struct Session {
    server: Arc<Server>,
    creds: Creds,
    out: Arc<Mutex<UnixStream>>,
    latest: Arc<AtomicU64>,
    pending: Arc<AtomicBool>,
    active: Option<Active>,
}

fn sort_of(s: SortSpec) -> Sort {
    Sort {
        field: match s.field {
            SortField::Name => CoreSort::Name,
            SortField::Path => CoreSort::Path,
            SortField::Size => CoreSort::Size,
            SortField::Extension => CoreSort::Extension,
            SortField::Type => CoreSort::Type,
            SortField::Modified => CoreSort::Modified,
            SortField::Created => CoreSort::Created,
            SortField::Accessed => CoreSort::Accessed,
            SortField::Changed => CoreSort::Changed,
            SortField::RunCount => CoreSort::RunCount,
            SortField::DateRun => CoreSort::DateRun,
        },
        descending: s.descending,
    }
}

impl Session {
    fn send(&self, r: &Response) -> bool {
        self.out
            .lock()
            .is_ok_and(|mut s| write_message(&mut *s, r).is_ok())
    }

    fn error(&self, id: Option<u64>, kind: ErrorKind, message: impl Into<String>) {
        self.send(&Response::Error {
            id,
            kind,
            message: message.into(),
        });
    }

    fn run(mut self, rx: &Receiver<Exec>) {
        while let Ok(job) = rx.recv() {
            match job {
                Exec::Query(q, cancel) => {
                    // Skip queries that a newer one already replaced.
                    if self.latest.load(Ordering::Relaxed) != q.id {
                        self.error(
                            Some(q.id),
                            ErrorKind::Cancelled,
                            "replaced by a newer query",
                        );
                        continue;
                    }
                    self.query(&q, &cancel);
                }
                Exec::Request(r) => self.request(r),
                Exec::Refresh => {
                    self.pending.store(false, Ordering::Release);
                    self.refresh();
                }
            }
        }
    }

    /// The search context for this peer. Root (and the owner in user mode) sees everything.
    fn context<'a>(
        &'a self,
        g: &Generation,
        vis: Option<&'a Visibility>,
        runs: &'a ResolvedRuns,
        content: &'a PermContent,
        cancel: &'a AtomicBool,
    ) -> SearchContext<'a> {
        let _ = g;
        let mut ctx = SearchContext::basic(cancel);
        ctx.runs = runs;
        ctx.content = Some(content);
        ctx.mime = self.server.mime.as_ref().map(|m| m as &dyn MimeDb);
        ctx.visible = vis.map(|v| &v.entries);
        ctx.listable = vis.map(|v| &v.listable);
        ctx.hide_restricted_sizes = vis.is_some();
        ctx.content_limit = self.server.shared.config.index.content_limit_mib << 20;
        ctx
    }

    fn visibility(&self, g: &Generation) -> Option<Arc<Visibility>> {
        match self.server.shared.mode {
            // A user daemon only indexes what its user can read, and only that user connects.
            ServerMode::User => None,
            ServerMode::System => self.server.vis.get(g, &self.creds),
        }
    }

    fn execute(
        &self,
        req: &QueryRequest,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(Active, Duration), (ErrorKind, String)> {
        let start = Instant::now();
        let g = self.server.shared.current();
        let tz = jiff::tz::TimeZone::get(&req.time_zone).unwrap_or(jiff::tz::TimeZone::UTC);
        let options = SearchOptions {
            match_case: req.options.match_case,
            whole_word: req.options.whole_word,
            match_path: req.options.match_path,
            regex: req.options.regex,
            match_diacritics: req.options.match_diacritics,
            ..SearchOptions::default()
        };
        let macros = Macros {
            user: req.macros.iter().cloned().collect(),
        };
        let cctx = CompileContext {
            options,
            clock: Clock::at(jiff::Timestamp::now().as_second(), tz),
            macros: &macros,
            users: &SystemUsers,
        };
        let compiled = compile_str(&req.text, &cctx).map_err(|e| {
            let kind = if matches!(e, QueryError::NotSupported(_)) {
                ErrorKind::NotSupported
            } else {
                ErrorKind::BadQuery
            };
            (kind, e.to_string())
        })?;
        let vis = self.visibility(&g);
        let runs = Arc::new(ResolvedRuns::new(
            &g,
            &self.server.runs.user(self.creds.uid),
        ));
        let content = PermContent {
            creds: self.creds.clone(),
        };
        let ctx = self.context(&g, vis.as_deref(), &runs, &content, cancel);
        let result =
            search::search(&g, &compiled, sort_of(req.sort), &ctx).map_err(|e| match e {
                SearchError::Cancelled => (ErrorKind::Cancelled, "cancelled".to_owned()),
            })?;
        Ok((
            Active {
                req: req.clone(),
                compiled,
                g,
                vis,
                runs,
                ids: result.ids,
                cancel: Arc::clone(cancel),
            },
            start.elapsed(),
        ))
    }

    fn query(&mut self, req: &QueryRequest, cancel: &Arc<AtomicBool>) {
        let id = req.id;
        match self.execute(req, cancel) {
            Ok((active, elapsed)) => {
                let total = active.ids.len() as u64;
                let generation = active.g.number();
                self.active = Some(active);
                self.send(&Response::Result {
                    id,
                    total,
                    generation,
                    elapsed_us: u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
                });
            }
            Err((kind, message)) => {
                self.active = None;
                self.error(Some(id), kind, message);
            }
        }
    }

    fn refresh(&mut self) {
        let Some(active) = &self.active else {
            return;
        };
        if !active.req.live || active.g.number() == self.server.shared.current().number() {
            return;
        }
        let req = active.req.clone();
        let cancel = Arc::clone(&active.cancel);
        if let Ok((new, _)) = self.execute(&req, &cancel) {
            let changed = self.active.as_ref().is_none_or(|a| a.ids != new.ids);
            let (total, generation) = (new.ids.len() as u64, new.g.number());
            self.active = Some(new);
            if changed {
                self.send(&Response::Changed {
                    id: req.id,
                    total,
                    generation,
                });
            }
        }
    }

    fn request(&mut self, r: Request) {
        match r {
            Request::Rows { id, offset, count } => self.rows(id, offset, count),
            Request::Sort { id, sort } => {
                let Some(active) = self.active.as_mut().filter(|a| a.req.id == id) else {
                    self.error(Some(id), ErrorKind::UnknownQuery, "no such query");
                    return;
                };
                active.req.sort = sort;
                let content = PermContent {
                    creds: self.creds.clone(),
                };
                let cancel = AtomicBool::new(false);
                let start = Instant::now();
                let mut ids = std::mem::take(&mut active.ids);
                {
                    let active = self.active.as_ref().unwrap_or_else(|| unreachable!());
                    let ctx = self.context(
                        &active.g,
                        active.vis.as_deref(),
                        &active.runs,
                        &content,
                        &cancel,
                    );
                    search::sort_ids(&active.g, &mut ids, sort_of(sort), &ctx);
                }
                let Some(active) = self.active.as_mut() else {
                    return;
                };
                active.ids = ids;
                let (total, generation) = (active.ids.len() as u64, active.g.number());
                self.send(&Response::Result {
                    id,
                    total,
                    generation,
                    elapsed_us: u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
                });
            }
            Request::Status => {
                let g = self.server.shared.current();
                let vis = self.visibility(&g);
                let counts = g.volume_counts(vis.as_ref().map(|v| &v.entries));
                let volumes = self.server.shared.volume_status(&g, &counts);
                self.send(&Response::Status {
                    entries: counts.iter().sum(),
                    generation: g.number(),
                    volumes,
                });
            }
            Request::ReportRun { path } => {
                if path.len() > MAX_RUNS_PATH || !path.starts_with(b"/") {
                    self.error(None, ErrorKind::BadQuery, "invalid path");
                    return;
                }
                match self.server.runs.record(self.creds.uid, &path) {
                    Ok(()) => {
                        self.send(&Response::Ok);
                    }
                    Err(e) => self.error(None, ErrorKind::Internal, e.to_string()),
                }
            }
            Request::Rescan { mount_path } => {
                if !self.creds.is_root() && self.server.shared.mode == ServerMode::System {
                    self.error(None, ErrorKind::Denied, "only root may start a rescan");
                    return;
                }
                if self.server.writer.send(Cmd::Rescan(mount_path)).is_ok() {
                    self.send(&Response::Ok);
                }
            }
            Request::Hello { .. } => self.error(None, ErrorKind::Protocol, "already greeted"),
            Request::Query(_) | Request::Cancel { .. } | Request::Goodbye => {}
        }
    }

    fn rows(&self, id: u64, offset: u64, count: u32) {
        let Some(active) = self.active.as_ref().filter(|a| a.req.id == id) else {
            self.error(Some(id), ErrorKind::UnknownQuery, "no such query");
            return;
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(active.ids.len());
        let end = start
            .saturating_add(count.min(MAX_ROWS) as usize)
            .min(active.ids.len());
        let rows = active.ids[start..end]
            .iter()
            .map(|&id| self.row(active, id))
            .collect();
        self.send(&Response::Rows {
            id,
            offset: start as u64,
            rows,
        });
    }

    fn row(&self, a: &Active, id: EntryId) -> Row {
        let g = &a.g;
        let hidden = a.vis.is_some() && g.restricted_count(id).is_some_and(|r| r > 0);
        let h = search::highlights(g, &a.compiled, id);
        let pair = |v: Vec<(usize, usize)>| {
            v.into_iter()
                .map(|(s, e)| {
                    (
                        u32::try_from(s).unwrap_or(u32::MAX),
                        u32::try_from(e).unwrap_or(u32::MAX),
                    )
                })
                .collect()
        };
        let secs = |f| g.time(id, f).map(|t| t.secs);
        let (uid, gid) = g.owner(id);
        Row {
            name: g.name(id).to_vec(),
            parent: g.parent_path(id),
            kind: match g.kind(id) {
                FileKind::File => Kind::File,
                FileKind::Dir => Kind::Folder,
                FileKind::Symlink => Kind::Symlink,
                _ => Kind::Other,
            },
            size: (!hidden).then(|| g.size(id)),
            modified: secs(TimeField::Modified),
            created: secs(TimeField::Created),
            accessed: secs(TimeField::Accessed),
            changed: secs(TimeField::Changed),
            mode: g.mode(id),
            uid,
            gid,
            run_count: fossick_core::search::RunHistory::run_count(&*a.runs, id),
            last_run: fossick_core::search::RunHistory::last_run(&*a.runs, id),
            offline: g.volume(id).is_some_and(|v| !v.online),
            name_highlights: pair(h.name),
            path_highlights: pair(h.path),
        }
    }
}

/// Logs and drops a failed accept.
pub fn log_listen_error(e: &std::io::Error) {
    log::warn(&format!("socket error: {e}"));
}
