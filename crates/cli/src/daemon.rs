//! Searching through the daemon.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;

use fossick_client::{Client, ClientError, Target};
use fossick_core::search::{Sort, SortField};
use fossick_proto::{
    ErrorKind, QueryOptions, QueryRequest, SortField as Field, SortSpec, UpdateMethod, VolumeState,
};

use crate::{Cli, Failure, exit, macros_of, write_rows};

/// Rows fetched per request.
const WINDOW: u32 = 2000;

fn fail(e: &ClientError) -> Failure {
    let code = match e {
        ClientError::NoDaemon(_) => exit::NO_DAEMON,
        ClientError::Server {
            kind: ErrorKind::BadQuery | ErrorKind::NotSupported,
            ..
        } => exit::QUERY_ERROR,
        _ => exit::IO_ERROR,
    };
    let hint = if code == exit::NO_DAEMON {
        "\nStart it with `systemctl enable --now fossickd`, or search a directory in-process \
         with --local DIR."
    } else {
        ""
    };
    (code, format!("{e}{hint}"))
}

fn proto_sort(s: Sort) -> SortSpec {
    SortSpec {
        field: match s.field {
            SortField::Name => Field::Name,
            SortField::Path => Field::Path,
            SortField::Size => Field::Size,
            SortField::Extension => Field::Extension,
            SortField::Type => Field::Type,
            SortField::Modified => Field::Modified,
            SortField::Created => Field::Created,
            SortField::Accessed => Field::Accessed,
            SortField::Changed => Field::Changed,
            SortField::RunCount => Field::RunCount,
            SortField::DateRun => Field::DateRun,
        },
        descending: s.descending,
    }
}

pub fn run(cli: &Cli, sort: Sort) -> Result<u8, Failure> {
    let target = match (&cli.socket, cli.user_daemon) {
        (Some(p), _) => Target::Socket(p.clone()),
        (None, true) => Target::User,
        (None, false) => Target::Auto,
    };
    let mut client = Client::connect(&target).map_err(|e| fail(&e))?;
    if cli.status {
        return status(&mut client);
    }
    if let Some(m) = &cli.rescan {
        client
            .rescan(m.as_os_str().as_bytes())
            .map_err(|e| fail(&e))?;
        return Ok(exit::FOUND);
    }
    let req = QueryRequest {
        id: 0,
        text: cli.query.join(" "),
        options: QueryOptions {
            match_case: cli.case,
            whole_word: cli.whole_word,
            match_path: cli.match_path,
            regex: cli.regex,
            match_diacritics: !cli.ignore_diacritics,
        },
        sort: proto_sort(sort),
        macros: macros_of(cli)?,
        time_zone: jiff::tz::TimeZone::system()
            .iana_name()
            .unwrap_or("UTC")
            .to_owned(),
        live: false,
    };
    let result = client.query(req).map_err(|e| fail(&e))?;
    if cli.stats {
        eprintln!(
            "{} results in {:.2} ms ({})",
            result.total,
            result.elapsed.as_secs_f64() * 1e3,
            client.server
        );
    }
    if cli.count {
        println!("{}", result.total);
        return Ok(if result.total > 0 {
            exit::FOUND
        } else {
            exit::NOT_FOUND
        });
    }
    let start = cli.offset as u64;
    let end = cli
        .limit
        .map_or(result.total, |l| start.saturating_add(l as u64))
        .min(result.total);
    // Fetch windows lazily while writing.
    let mut next = start;
    let mut buffered: std::vec::IntoIter<fossick_proto::Row> = Vec::new().into_iter();
    let mut error = None;
    let rows = std::iter::from_fn(|| {
        if let Some(r) = buffered.next() {
            return Some(r);
        }
        if next >= end || error.is_some() {
            return None;
        }
        let count = u32::try_from(end - next).unwrap_or(u32::MAX).min(WINDOW);
        match client.rows(result.id, next, count) {
            Ok(rows) if !rows.is_empty() => {
                next += rows.len() as u64;
                buffered = rows.into_iter();
                buffered.next()
            }
            Ok(_) => None,
            Err(e) => {
                error = Some(e);
                None
            }
        }
    });
    write_rows(cli, rows)?;
    if let Some(e) = error {
        return Err(fail(&e));
    }
    Ok(if result.total > 0 {
        exit::FOUND
    } else {
        exit::NOT_FOUND
    })
}

fn status(client: &mut Client) -> Result<u8, Failure> {
    let s = client.status().map_err(|e| fail(&e))?;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "{} ({}), {} entries visible to uid {}, generation {}",
        client.server,
        match client.mode {
            fossick_proto::ServerMode::System => "system daemon",
            fossick_proto::ServerMode::User => "user daemon",
        },
        s.entries,
        client.uid,
        s.generation
    );
    for v in &s.volumes {
        let state = match v.state {
            VolumeState::Scanning => "scanning",
            VolumeState::Ready => "ready",
            VolumeState::Offline => "offline",
            VolumeState::Error => "error",
        };
        let method = match v.method {
            UpdateMethod::Live => "fanotify",
            UpdateMethod::Inotify => "inotify",
            UpdateMethod::Polling => "polling",
            UpdateMethod::Static => "static",
        };
        let _ = writeln!(
            out,
            "  {:<32} {:<8} {:<9} {:<9} {:>10}  {}",
            String::from_utf8_lossy(&v.mount_path),
            v.fstype,
            state,
            method,
            v.entries,
            v.note
        );
    }
    Ok(exit::FOUND)
}
