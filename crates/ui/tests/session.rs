//! The session thread against a scripted daemon: results, row windows, live pushes, errors and
//! reconnecting.

use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use fossick_client::Target;
use fossick_proto::{
    ErrorKind, Kind, PROTOCOL_VERSION, QueryOptions, Request, Response, Row, ServerMode, SortSpec,
    read_message, write_message,
};
use fossick_ui::session::{Event, QuerySpec, Session};

fn row(i: u64) -> Row {
    Row {
        name: format!("f{i}").into_bytes(),
        parent: b"/data".to_vec(),
        kind: Kind::File,
        size: Some(i),
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
        name_highlights: vec![(0, 1)],
        path_highlights: vec![],
    }
}

/// Serves connections one after another. A query’s total is 10 × its length; `live…` queries
/// get a push 50 ms later with 5 more rows; `bad` fails; `drop` closes the connection.
fn mock_daemon(path: &Path) {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            serve(stream);
        }
    });
}

fn serve(stream: UnixStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let mut totals = std::collections::HashMap::new();
    loop {
        let Ok(req) = read_message::<Request>(&mut reader) else {
            return;
        };
        let reply = match req {
            Request::Hello { .. } => Response::Welcome {
                version: PROTOCOL_VERSION,
                server: "mock".into(),
                mode: ServerMode::User,
                uid: 1000,
            },
            Request::Query(q) if q.text == "bad" => Response::Error {
                id: Some(q.id),
                kind: ErrorKind::BadQuery,
                message: "nope".into(),
            },
            Request::Query(q) if q.text == "drop" => return,
            Request::Query(q) => {
                let total = q.text.len() as u64 * 10;
                totals.insert(q.id, total);
                if q.text.starts_with("live") {
                    let mut w = writer.try_clone().unwrap();
                    let id = q.id;
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(50));
                        let _ = write_message(
                            &mut w,
                            &Response::Changed {
                                id,
                                total: total + 5,
                                generation: 2,
                            },
                        );
                    });
                }
                Response::Result {
                    id: q.id,
                    total,
                    generation: 1,
                    elapsed_us: 120,
                }
            }
            Request::Rows { id, offset, count } => {
                let total = totals.get(&id).copied().unwrap_or(0);
                let end = (offset + u64::from(count)).min(total);
                Response::Rows {
                    id,
                    offset,
                    rows: (offset..end).map(row).collect(),
                }
            }
            Request::Status => Response::Status {
                entries: 7,
                generation: 1,
                volumes: Vec::new(),
            },
            Request::ReportRun { .. } => Response::Ok,
            Request::Goodbye => return,
            _ => Response::Ok,
        };
        if write_message(&mut writer, &reply).is_err() {
            return;
        }
    }
}

fn spec(text: &str) -> QuerySpec {
    QuerySpec {
        text: text.into(),
        options: QueryOptions::default(),
        sort: SortSpec::default(),
    }
}

/// The next event that isn’t `Connected`.
fn next(rx: &mpsc::Receiver<Event>) -> Event {
    loop {
        match rx.recv_timeout(Duration::from_secs(5)).expect("an event") {
            Event::Connected { mode, server } => {
                assert_eq!(mode, ServerMode::User);
                assert_eq!(server, "mock");
            }
            e => return e,
        }
    }
}

#[test]
fn session_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("socket");
    mock_daemon(&sock);
    let (tx, rx) = mpsc::channel();
    let session = Session::start(Target::Socket(sock), move |e| {
        let _ = tx.send(e);
    });

    // A query and a window of its rows.
    let seq = session.query(spec("abc"));
    match next(&rx) {
        Event::Result { seq: s, total, .. } => assert_eq!((s, total), (seq, 30)),
        e => panic!("unexpected {e:?}"),
    }
    session.rows(seq, 0, 200);
    match next(&rx) {
        Event::Rows {
            seq: s,
            offset,
            rows,
        } => {
            assert_eq!((s, offset, rows.len()), (seq, 0, 30));
            assert_eq!(rows[3].name, "f3");
            assert_eq!(rows[3].name_html, "<b>f</b>3");
        }
        e => panic!("unexpected {e:?}"),
    }

    // Rows of an older query are not fetched.
    let newer = session.query(spec("abcd"));
    session.rows(seq, 0, 10);
    match next(&rx) {
        Event::Result { seq: s, total, .. } => assert_eq!((s, total), (newer, 40)),
        e => panic!("unexpected {e:?}"),
    }

    // Live results report changes.
    let live = session.query(spec("live"));
    match next(&rx) {
        Event::Result { seq: s, .. } => assert_eq!(s, live),
        e => panic!("unexpected {e:?}"),
    }
    match next(&rx) {
        Event::Changed { seq: s, total } => assert_eq!((s, total), (live, 45)),
        e => panic!("unexpected {e:?}"),
    }

    // Query errors are reported and the connection stays usable.
    let bad = session.query(spec("bad"));
    match next(&rx) {
        Event::QueryError { seq: s, message } => assert_eq!((s, message.as_str()), (bad, "nope")),
        e => panic!("unexpected {e:?}"),
    }
    session.status();
    assert!(matches!(next(&rx), Event::Status(s) if s.entries == 7));

    // A lost connection is reported; the next command reconnects.
    session.query(spec("drop"));
    assert!(matches!(next(&rx), Event::Disconnected(_)));
    let again = session.query(spec("xy"));
    match next(&rx) {
        Event::Result { seq: s, total, .. } => assert_eq!((s, total), (again, 20)),
        e => panic!("unexpected {e:?}"),
    }
}

#[test]
fn typing_quickly_runs_the_newest_query() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("socket");
    mock_daemon(&sock);
    let (tx, rx) = mpsc::channel();
    let session = Session::start(Target::Socket(sock), move |e| {
        let _ = tx.send(e);
    });
    let mut last = 0;
    for text in ["a", "ab", "abc", "abcd", "abcde"] {
        last = session.query(spec(text));
    }
    // Earlier keystrokes may or may not have run, but the newest one always does, last.
    loop {
        match next(&rx) {
            Event::Result { seq, total, .. } if seq == last => {
                assert_eq!(total, 50);
                break;
            }
            Event::Result { seq, .. } => assert!(seq < last),
            e => panic!("unexpected {e:?}"),
        }
    }
}

#[test]
fn no_daemon_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel();
    let session = Session::start(Target::Socket(dir.path().join("none")), move |e| {
        let _ = tx.send(e);
    });
    session.query(spec("x"));
    assert!(matches!(next(&rx), Event::Disconnected(_)));
}
