//! A connection to the daemon on its own thread, so the GUI thread never blocks on the socket.
//!
//! The GUI sends commands (run a query, fetch rows …) and receives [`Event`]s through a
//! callback, which runs on the session thread: frontends forward them to their event loop.
//! Queries are numbered by the session (`seq`); typing quickly sends several, and only the
//! newest one runs. Results are live: when the index changes, the session reports
//! [`Event::Changed`] and the frontend fetches its visible rows again.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use fossick_client::{Client, ClientError, Status, Target};
use fossick_proto::{QueryOptions, QueryRequest, Response, ServerMode, SortSpec};

use crate::rows::{Names, RowData};

/// What to search for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuerySpec {
    pub text: String,
    pub options: QueryOptions,
    pub sort: SortSpec,
}

/// What the session reports.
#[derive(Debug)]
pub enum Event {
    Connected {
        mode: ServerMode,
        server: String,
    },
    /// No daemon (or the connection was lost); the session retries on the next command.
    Disconnected(String),
    Result {
        seq: u64,
        total: u64,
        elapsed: Duration,
    },
    QueryError {
        seq: u64,
        message: String,
    },
    Rows {
        seq: u64,
        offset: u64,
        rows: Vec<RowData>,
    },
    /// Rows could not be fetched (the frontend may ask again).
    RowsFailed {
        seq: u64,
        offset: u64,
    },
    /// The result of `seq` changed; it now has `total` rows.
    Changed {
        seq: u64,
        total: u64,
    },
    Status(Status),
}

enum Cmd {
    Query(u64, QuerySpec),
    Rows(u64, u64, u32),
    Status,
    ReportRun(Vec<u8>),
    Stop,
}

/// A handle to the session thread (dropping it stops the thread).
#[derive(Debug)]
pub struct Session {
    tx: Sender<Cmd>,
    next_seq: AtomicU64,
}

type Emit = Arc<dyn Fn(Event) + Send + Sync>;

impl Session {
    /// Starts a session thread connecting to `target`; `emit` is called on that thread.
    #[must_use]
    pub fn start(target: Target, emit: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let emit: Emit = Arc::new(emit);
        let _ = std::thread::Builder::new()
            .name("fossick-session".into())
            .spawn(move || Worker::new(target, emit).run(&rx));
        Self {
            tx,
            next_seq: AtomicU64::new(1),
        }
    }

    /// Runs a query (replacing the previous one). Returns its number.
    pub fn query(&self, spec: QuerySpec) -> u64 {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Query(seq, spec));
        seq
    }

    /// Fetches rows `[offset, offset + count)` of query `seq`.
    pub fn rows(&self, seq: u64, offset: u64, count: u32) {
        let _ = self.tx.send(Cmd::Rows(seq, offset, count));
    }

    /// Asks for the index status (answered with [`Event::Status`]).
    pub fn status(&self) {
        let _ = self.tx.send(Cmd::Status);
    }

    /// Records that the user opened `path` (run count, “Date Run”).
    pub fn report_run(&self, path: Vec<u8>) {
        let _ = self.tx.send(Cmd::ReportRun(path));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
    }
}

struct Worker {
    target: Target,
    emit: Emit,
    client: Option<Client>,
    /// The query being shown: (seq, spec, the daemon’s id for it).
    current: Option<(u64, QuerySpec, Option<u64>)>,
    names: Names,
}

impl Worker {
    fn new(target: Target, emit: Emit) -> Self {
        Self {
            target,
            emit,
            client: None,
            current: None,
            names: Names::default(),
        }
    }

    fn run(mut self, rx: &Receiver<Cmd>) {
        loop {
            let first = match rx.recv_timeout(Duration::from_millis(40)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            // Coalesce everything queued: only the newest query matters, and row requests for
            // older queries are moot.
            let mut query = None;
            let mut rows: BTreeSet<(u64, u64, u32)> = BTreeSet::new();
            let mut status = false;
            let mut runs = Vec::new();
            for cmd in first.into_iter().chain(rx.try_iter()) {
                match cmd {
                    Cmd::Query(seq, spec) => {
                        query = Some((seq, spec));
                        rows.clear();
                    }
                    Cmd::Rows(seq, off, n) => {
                        rows.insert((seq, off, n));
                    }
                    Cmd::Status => status = true,
                    Cmd::ReportRun(p) => runs.push(p),
                    Cmd::Stop => return,
                }
            }
            if let Some((seq, spec)) = query {
                self.current = Some((seq, spec, None));
                self.run_query();
            }
            for (seq, off, n) in rows {
                self.fetch(seq, off, n);
            }
            if status && let Some(s) = self.with_client(Client::status) {
                (self.emit)(Event::Status(s));
            }
            for p in runs {
                let _ = self.with_client(|c| c.report_run(&p));
            }
            self.pushes();
        }
    }

    /// Runs `f` with a connected client, connecting first if needed. Connection errors are
    /// reported and drop the client (the next command reconnects).
    fn with_client<T>(
        &mut self,
        f: impl FnOnce(&mut Client) -> Result<T, ClientError>,
    ) -> Option<T> {
        if self.client.is_none() {
            match Client::connect(&self.target) {
                Ok(c) => {
                    (self.emit)(Event::Connected {
                        mode: c.mode,
                        server: c.server.clone(),
                    });
                    self.client = Some(c);
                }
                Err(e) => {
                    (self.emit)(Event::Disconnected(e.to_string()));
                    return None;
                }
            }
        }
        let client = self.client.as_mut()?;
        match f(client) {
            Ok(v) => Some(v),
            Err(ClientError::Server { message, .. }) => {
                // A request failed but the connection is fine.
                if let Some((seq, _, _)) = &self.current {
                    (self.emit)(Event::QueryError { seq: *seq, message });
                }
                None
            }
            Err(e) => {
                self.client = None;
                if let Some((_, _, id)) = &mut self.current {
                    *id = None;
                }
                (self.emit)(Event::Disconnected(e.to_string()));
                None
            }
        }
    }

    fn run_query(&mut self) {
        let Some((seq, spec, _)) = self.current.clone() else {
            return;
        };
        let req = QueryRequest {
            id: 0,
            text: spec.text,
            options: spec.options,
            sort: spec.sort,
            macros: Vec::new(),
            time_zone: jiff::tz::TimeZone::system()
                .iana_name()
                .unwrap_or("UTC")
                .to_owned(),
            live: true,
        };
        if let Some(r) = self.with_client(|c| c.query(req)) {
            if let Some((_, _, id)) = &mut self.current {
                *id = Some(r.id);
            }
            (self.emit)(Event::Result {
                seq,
                total: r.total,
                elapsed: r.elapsed,
            });
        }
    }

    fn fetch(&mut self, seq: u64, offset: u64, count: u32) {
        let id = match &self.current {
            Some((s, _, id)) if *s == seq => *id,
            _ => return,
        };
        // After a reconnect the daemon no longer has the result: run the query again first.
        let id = match id {
            Some(id) => id,
            None => {
                self.run_query();
                match &self.current {
                    Some((_, _, Some(id))) => *id,
                    _ => {
                        (self.emit)(Event::RowsFailed { seq, offset });
                        return;
                    }
                }
            }
        };
        match self.with_client(|c| c.rows(id, offset, count)) {
            Some(rows) => {
                let names = &mut self.names;
                let rows = rows
                    .into_iter()
                    .map(|r| RowData::from_row(&r, names))
                    .collect();
                (self.emit)(Event::Rows { seq, offset, rows });
            }
            None => (self.emit)(Event::RowsFailed { seq, offset }),
        }
    }

    /// Forwards live-result pushes for the current query.
    fn pushes(&mut self) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        let mut pushes = client.take_pushes();
        loop {
            match client.poll_readable(Duration::ZERO) {
                Ok(true) => match client.next_push() {
                    Ok(r) => pushes.push(r),
                    Err(e) => {
                        self.client = None;
                        (self.emit)(Event::Disconnected(e.to_string()));
                        break;
                    }
                },
                Ok(false) => break,
                Err(e) => {
                    self.client = None;
                    (self.emit)(Event::Disconnected(e.to_string()));
                    break;
                }
            }
        }
        let mut latest = None;
        for p in pushes {
            if let Response::Changed { id, total, .. } = p
                && let Some((seq, _, Some(cur))) = &self.current
                && *cur == id
            {
                latest = Some((*seq, total));
            }
        }
        if let Some((seq, total)) = latest {
            (self.emit)(Event::Changed { seq, total });
        }
    }
}
