//! The Fossick IPC protocol.
//!
//! - **Transport:** a Unix stream socket (`/run/fossick/socket` for the system daemon,
//!   `$XDG_RUNTIME_DIR/fossick/socket` for user mode).
//! - **Framing:** each message is a little-endian `u32` length followed by that many bytes of
//!   postcard-encoded payload. Frames over [`MAX_FRAME`] are rejected.
//! - **Handshake:** the client’s first message is [`Request::Hello`], and the server answers
//!   [`Response::Welcome`] or [`Response::Error`].
//! - **Compatibility:** postcard encodes enum variants by index, so new variants and new message
//!   kinds are only ever **appended**, and fields are never reordered. Incompatible changes bump
//!   [`PROTOCOL_VERSION`].
//!
//! Results never cross the socket in full: a query yields a total count, and the client asks for
//! row windows ([`Request::Rows`]) as it scrolls.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// Protocol version spoken by this build.
pub const PROTOCOL_VERSION: u32 = 1;
/// Largest accepted frame (16 MiB).
pub const MAX_FRAME: usize = 16 << 20;
/// Largest row window a client may request at once.
pub const MAX_ROWS: u32 = 10_000;

/// Global search options (Everything’s Search menu toggles).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryOptions {
    pub match_case: bool,
    pub whole_word: bool,
    pub match_path: bool,
    pub regex: bool,
    pub match_diacritics: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            match_case: false,
            whole_word: false,
            match_path: false,
            regex: false,
            match_diacritics: true,
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

/// A sort order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SortSpec {
    pub field: SortField,
    pub descending: bool,
}

/// A query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRequest {
    /// Client-chosen id; responses and pushes carry it.
    pub id: u64,
    pub text: String,
    pub options: QueryOptions,
    pub sort: SortSpec,
    /// User macros (`name` → query).
    pub macros: Vec<(String, String)>,
    /// IANA time zone for date constants (`today` …); empty = UTC.
    pub time_zone: String,
    /// Keep the result live: the server pushes [`Response::Changed`] when it changes.
    pub live: bool,
}

/// Requests from client to server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Hello {
        version: u32,
        client: String,
    },
    /// Runs a query; replaces (and cancels) the connection’s previous query.
    Query(QueryRequest),
    /// Cancels the query with this id if it is still running.
    Cancel {
        id: u64,
    },
    /// Fetches rows `[offset, offset + count)` of a finished query.
    Rows {
        id: u64,
        offset: u64,
        count: u32,
    },
    /// Re-sorts the current result set.
    Sort {
        id: u64,
        sort: SortSpec,
    },
    /// Index and volume status.
    Status,
    /// The user opened this file through a frontend (run history).
    ReportRun {
        path: Vec<u8>,
    },
    /// Ends the session.
    Goodbye,
    /// Rescans a volume (by mount path; empty = all volumes). Root only.
    Rescan {
        mount_path: Vec<u8>,
    },
}

/// File kinds as sent over the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Folder,
    Symlink,
    Other,
}

/// One result row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    /// Raw name bytes.
    pub name: Vec<u8>,
    /// Raw path of the containing folder.
    pub parent: Vec<u8>,
    pub kind: Kind,
    /// File size or folder size; `None` when hidden from this user or unknown.
    pub size: Option<u64>,
    /// Unix seconds.
    pub modified: Option<i64>,
    pub created: Option<i64>,
    pub accessed: Option<i64>,
    pub changed: Option<i64>,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub run_count: u32,
    pub last_run: Option<i64>,
    /// Volume is not mounted (shown greyed out).
    pub offline: bool,
    /// Byte ranges to highlight in `name`.
    pub name_highlights: Vec<(u32, u32)>,
    /// Byte ranges to highlight in the full path (`parent` + `/` + `name`).
    pub path_highlights: Vec<(u32, u32)>,
}

/// How a volume is kept up to date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateMethod {
    /// fanotify filesystem mark (live).
    Live,
    Inotify,
    Polling,
    /// Read-only or offline: never changes.
    Static,
}

/// Volume state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeState {
    Scanning,
    Ready,
    Offline,
    Error,
}

/// Status of one volume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeStatus {
    pub mount_path: Vec<u8>,
    pub fstype: String,
    pub identity: String,
    pub label: Option<String>,
    pub state: VolumeState,
    pub method: UpdateMethod,
    pub entries: u64,
    /// Human-readable note (why polling, scan errors …).
    pub note: String,
}

/// Which daemon answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMode {
    /// The system daemon (all filesystems, permission filtered).
    System,
    /// A per-user daemon (directories the user can read).
    User,
}

/// Error categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    Protocol,
    BadQuery,
    NotSupported,
    UnknownQuery,
    Denied,
    Internal,
    /// A newer query on the same connection replaced this one.
    Cancelled,
}

/// Responses and pushes from server to client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Welcome {
        version: u32,
        server: String,
        mode: ServerMode,
        uid: u32,
    },
    /// A query finished.
    Result {
        id: u64,
        total: u64,
        generation: u64,
        elapsed_us: u64,
    },
    Rows {
        id: u64,
        offset: u64,
        rows: Vec<Row>,
    },
    /// Push: a live query’s results changed (re-fetch the visible window).
    Changed {
        id: u64,
        total: u64,
        generation: u64,
    },
    Status {
        entries: u64,
        generation: u64,
        volumes: Vec<VolumeStatus>,
    },
    Error {
        id: Option<u64>,
        kind: ErrorKind,
        message: String,
    },
    Ok,
}

/// Framing and encoding errors.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("frame of {0} bytes exceeds the limit")]
    TooLarge(usize),
    #[error("malformed message: {0}")]
    Decode(String),
    #[error("connection closed")]
    Closed,
}

/// Encodes a message into one frame.
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    let payload = postcard::to_stdvec(msg).map_err(|e| ProtoError::Decode(e.to_string()))?;
    if payload.len() > MAX_FRAME {
        return Err(ProtoError::TooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decodes a payload (without the length prefix).
pub fn decode<'a, T: Deserialize<'a>>(payload: &'a [u8]) -> Result<T, ProtoError> {
    postcard::from_bytes(payload).map_err(|e| ProtoError::Decode(e.to_string()))
}

/// Writes one framed message.
pub fn write_message<T: Serialize>(w: &mut impl Write, msg: &T) -> Result<(), ProtoError> {
    w.write_all(&encode(msg)?)?;
    w.flush()?;
    Ok(())
}

/// Reads one frame’s payload. Returns [`ProtoError::Closed`] on a clean end of stream.
pub fn read_frame(r: &mut impl Read) -> Result<Vec<u8>, ProtoError> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(ProtoError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(ProtoError::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Reads and decodes one message.
pub fn read_message<T: for<'a> Deserialize<'a>>(r: &mut impl Read) -> Result<T, ProtoError> {
    let frame = read_frame(r)?;
    decode(&frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_messages() {
        let msgs = vec![
            Request::Hello {
                version: PROTOCOL_VERSION,
                client: "test".into(),
            },
            Request::Query(QueryRequest {
                id: 7,
                text: "ext:pdf café".into(),
                options: QueryOptions::default(),
                sort: SortSpec {
                    field: SortField::Size,
                    descending: true,
                },
                macros: vec![("m".into(), "audio:".into())],
                time_zone: "Europe/Berlin".into(),
                live: true,
            }),
            Request::Rows {
                id: 7,
                offset: 100,
                count: 50,
            },
            Request::ReportRun {
                path: b"/a/\xFF".to_vec(),
            },
        ];
        let mut wire = Vec::new();
        for m in &msgs {
            write_message(&mut wire, m).unwrap();
        }
        let mut r = io::Cursor::new(wire);
        for m in &msgs {
            let got: Request = read_message(&mut r).unwrap();
            assert_eq!(&got, m);
        }
        assert!(matches!(
            read_message::<Request>(&mut r),
            Err(ProtoError::Closed)
        ));
    }

    #[test]
    fn rejects_oversized_and_garbage_frames() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&(u32::MAX).to_le_bytes());
        assert!(matches!(
            read_frame(&mut io::Cursor::new(wire)),
            Err(ProtoError::TooLarge(_))
        ));
        let garbage = [3u8, 0, 0, 0, 0xFF, 0xFF, 0xFF];
        assert!(matches!(
            read_message::<Request>(&mut io::Cursor::new(garbage)),
            Err(ProtoError::Decode(_))
        ));
    }
}

/// Writes one payload per message kind as fuzzing seeds:
/// `FOSSICK_SEED_DIR=fuzz/seeds/ipc_decoder cargo test -p fossick-proto -- --ignored`.
#[cfg(test)]
#[test]
#[ignore = "writes files for fuzzing"]
fn write_fuzz_seeds() {
    let dir =
        std::path::PathBuf::from(std::env::var("FOSSICK_SEED_DIR").expect("FOSSICK_SEED_DIR"));
    let row = Row {
        name: b"r\xFFeport.pdf".to_vec(),
        parent: b"/home/a".to_vec(),
        kind: Kind::File,
        size: Some(1 << 40),
        modified: Some(1_700_000_000),
        created: None,
        accessed: Some(-5),
        changed: Some(0),
        mode: 0o100_644,
        uid: 1000,
        gid: 1000,
        run_count: 3,
        last_run: Some(1),
        offline: false,
        name_highlights: vec![(0, 3)],
        path_highlights: vec![(8, 11)],
    };
    let requests = [
        Request::Hello {
            version: PROTOCOL_VERSION,
            client: "seed".into(),
        },
        Request::Query(QueryRequest {
            id: 1,
            text: "ext:pdf <a|b> !c dm:today".into(),
            options: QueryOptions::default(),
            sort: SortSpec::default(),
            macros: vec![("m".into(), "x".into())],
            time_zone: "Europe/Berlin".into(),
            live: true,
        }),
        Request::Rows {
            id: 1,
            offset: 0,
            count: 100,
        },
        Request::Status,
        Request::Rescan {
            mount_path: b"/".to_vec(),
        },
    ];
    let responses = [
        Response::Rows {
            id: 1,
            offset: 0,
            rows: vec![row],
        },
        Response::Status {
            entries: 5,
            generation: 2,
            volumes: vec![VolumeStatus {
                mount_path: b"/".to_vec(),
                fstype: "btrfs".into(),
                identity: "uuid".into(),
                label: Some("root".into()),
                state: VolumeState::Ready,
                method: UpdateMethod::Live,
                entries: 5,
                note: String::new(),
            }],
        },
        Response::Error {
            id: Some(1),
            kind: ErrorKind::BadQuery,
            message: "x".into(),
        },
    ];
    for (i, r) in requests.iter().enumerate() {
        std::fs::write(
            dir.join(format!("request{i}")),
            postcard::to_stdvec(r).unwrap(),
        )
        .unwrap();
    }
    for (i, r) in responses.iter().enumerate() {
        std::fs::write(
            dir.join(format!("response{i}")),
            postcard::to_stdvec(r).unwrap(),
        )
        .unwrap();
    }
}
