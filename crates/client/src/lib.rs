//! Client library for `fossickd`.
//!
//! [`Client::connect`] finds a daemon: the system daemon’s socket first, then the user’s own
//! daemon, which it starts (`fossickd --user`) when it isn’t running. Queries return a total;
//! rows are fetched in windows as the caller needs them.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use fossick_proto as proto;
use fossick_proto::{
    ErrorKind, PROTOCOL_VERSION, ProtoError, QueryRequest, Request, Response, Row, ServerMode,
    VolumeStatus, read_message, write_message,
};

/// The system daemon’s socket.
pub const SYSTEM_SOCKET: &str = "/run/fossick/socket";

/// The user daemon’s socket (`$XDG_RUNTIME_DIR/fossick/socket`).
#[must_use]
pub fn user_socket() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("fossick/socket"))
}

/// Client errors.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("no Fossick daemon is running ({0})")]
    NoDaemon(String),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error("{message}")]
    Server { kind: ErrorKind, message: String },
    #[error("unexpected reply from the daemon: {0}")]
    Unexpected(String),
}

/// Which daemon to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The system daemon if it runs, else the user daemon (started if needed).
    Auto,
    /// Only the system daemon.
    System,
    /// Only the user daemon (started if needed).
    User,
    /// A specific socket.
    Socket(PathBuf),
}

/// A finished query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryResult {
    pub id: u64,
    pub total: u64,
    pub generation: u64,
    pub elapsed: Duration,
}

/// Index status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub entries: u64,
    pub generation: u64,
    pub volumes: Vec<VolumeStatus>,
}

/// A connection to a daemon.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
    pub server: String,
    pub mode: ServerMode,
    /// The uid the daemon sees for this connection.
    pub uid: u32,
    next_id: u64,
    /// Pushes received while waiting for a reply.
    pushed: Vec<Response>,
}

impl Client {
    /// Connects to a daemon.
    pub fn connect(target: &Target) -> Result<Self, ClientError> {
        match target {
            Target::Socket(p) => Self::connect_to(p),
            Target::System => Self::connect_to(Path::new(SYSTEM_SOCKET)),
            Target::User => Self::connect_user(),
            Target::Auto => match Self::connect_to(Path::new(SYSTEM_SOCKET)) {
                Ok(c) => Ok(c),
                Err(ClientError::NoDaemon(_)) => Self::connect_user(),
                Err(e) => Err(e),
            },
        }
    }

    fn connect_user() -> Result<Self, ClientError> {
        let sock = user_socket()
            .ok_or_else(|| ClientError::NoDaemon("XDG_RUNTIME_DIR is not set".into()))?;
        match Self::connect_to(&sock) {
            Err(ClientError::NoDaemon(_)) => {}
            other => return other,
        }
        spawn_user_daemon()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match Self::connect_to(&sock) {
                Err(ClientError::NoDaemon(e)) if Instant::now() < deadline => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(50));
                }
                other => return other,
            }
        }
    }

    /// Connects to the daemon listening on `path`.
    pub fn connect_to(path: &Path) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path)
            .map_err(|e| ClientError::NoDaemon(format!("{}: {e}", path.display())))?;
        let mut writer = BufWriter::new(stream.try_clone().map_err(ProtoError::Io)?);
        let mut reader = BufReader::new(stream);
        write_message(
            &mut writer,
            &Request::Hello {
                version: PROTOCOL_VERSION,
                client: format!("fossick-client {}", env!("CARGO_PKG_VERSION")),
            },
        )?;
        match read_message::<Response>(&mut reader)? {
            Response::Welcome {
                server, mode, uid, ..
            } => Ok(Self {
                reader,
                writer,
                server,
                mode,
                uid,
                next_id: 1,
                pushed: Vec::new(),
            }),
            Response::Error { kind, message, .. } => Err(ClientError::Server { kind, message }),
            other => Err(ClientError::Unexpected(format!("{other:?}"))),
        }
    }

    fn send(&mut self, r: &Request) -> Result<(), ClientError> {
        write_message(&mut self.writer, r)?;
        Ok(())
    }

    /// Reads replies until `want` accepts one; pushes (`Changed`) are kept for
    /// [`Client::take_pushes`].
    fn wait<T>(
        &mut self,
        mut want: impl FnMut(Response) -> Result<Option<T>, ClientError>,
    ) -> Result<T, ClientError> {
        loop {
            let r: Response = read_message(&mut self.reader)?;
            if matches!(r, Response::Changed { .. }) {
                self.pushed.push(r);
                continue;
            }
            if let Some(v) = want(r)? {
                return Ok(v);
            }
        }
    }

    /// Runs a query (its `id` is assigned here) and waits for the result count.
    pub fn query(&mut self, mut q: QueryRequest) -> Result<QueryResult, ClientError> {
        q.id = self.next_id;
        self.next_id += 1;
        let id = q.id;
        self.send(&Request::Query(q))?;
        self.wait(|r| match r {
            Response::Result {
                id: rid,
                total,
                generation,
                elapsed_us,
            } if rid == id => Ok(Some(QueryResult {
                id,
                total,
                generation,
                elapsed: Duration::from_micros(elapsed_us),
            })),
            Response::Error {
                id: Some(rid),
                kind,
                message,
            } if rid == id => Err(ClientError::Server { kind, message }),
            Response::Error {
                id: None,
                kind,
                message,
            } => Err(ClientError::Server { kind, message }),
            _ => Ok(None),
        })
    }

    /// Fetches rows `[offset, offset + count)` of query `id`.
    pub fn rows(&mut self, id: u64, offset: u64, count: u32) -> Result<Vec<Row>, ClientError> {
        self.send(&Request::Rows { id, offset, count })?;
        self.wait(|r| match r {
            Response::Rows { id: rid, rows, .. } if rid == id => Ok(Some(rows)),
            Response::Error { kind, message, .. } => Err(ClientError::Server { kind, message }),
            _ => Ok(None),
        })
    }

    /// Index and volume status.
    pub fn status(&mut self) -> Result<Status, ClientError> {
        self.send(&Request::Status)?;
        self.wait(|r| match r {
            Response::Status {
                entries,
                generation,
                volumes,
            } => Ok(Some(Status {
                entries,
                generation,
                volumes,
            })),
            Response::Error { kind, message, .. } => Err(ClientError::Server { kind, message }),
            _ => Ok(None),
        })
    }

    fn simple(&mut self, r: &Request) -> Result<(), ClientError> {
        self.send(r)?;
        self.wait(|r| match r {
            Response::Ok => Ok(Some(())),
            Response::Error { kind, message, .. } => Err(ClientError::Server { kind, message }),
            _ => Ok(None),
        })
    }

    /// Records that the user opened `path` (run history).
    pub fn report_run(&mut self, path: &[u8]) -> Result<(), ClientError> {
        self.simple(&Request::ReportRun {
            path: path.to_vec(),
        })
    }

    /// Asks the daemon to rescan a volume (empty path: all). Root only for the system daemon.
    pub fn rescan(&mut self, mount_path: &[u8]) -> Result<(), ClientError> {
        self.simple(&Request::Rescan {
            mount_path: mount_path.to_vec(),
        })
    }

    /// Asks the daemon to re-read its configuration file. Root only for the system daemon.
    pub fn reload(&mut self) -> Result<(), ClientError> {
        self.simple(&Request::Reload)
    }

    /// Waits for the next push (a live query changed).
    pub fn next_push(&mut self) -> Result<Response, ClientError> {
        if !self.pushed.is_empty() {
            return Ok(self.pushed.remove(0));
        }
        Ok(read_message(&mut self.reader)?)
    }

    /// True if a message (a push) can be read now, waiting up to `timeout` for one to arrive.
    /// Lets a frontend thread interleave its own requests with live updates.
    pub fn poll_readable(&mut self, timeout: Duration) -> Result<bool, ClientError> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        if !self.pushed.is_empty() || !self.reader.buffer().is_empty() {
            return Ok(true);
        }
        let ts = Timespec {
            tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let mut fds = [PollFd::new(self.reader.get_ref(), PollFlags::IN)];
        match poll(&mut fds, Some(&ts)) {
            Ok(n) => Ok(n > 0),
            Err(rustix::io::Errno::INTR) => Ok(false),
            Err(e) => Err(ClientError::Proto(ProtoError::Io(e.into()))),
        }
    }

    /// Pushes received so far.
    pub fn take_pushes(&mut self) -> Vec<Response> {
        std::mem::take(&mut self.pushed)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = write_message(&mut self.writer, &Request::Goodbye);
    }
}

/// Starts `fossickd --user` in its own process group, so it outlives the caller and doesn’t get
/// the terminal’s Ctrl-C.
fn spawn_user_daemon() -> Result<(), ClientError> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("fossickd")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("fossickd"));
    std::process::Command::new(&exe)
        .arg("--user")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
        .map_err(|e| ClientError::NoDaemon(format!("cannot start {}: {e}", exe.display())))
}
