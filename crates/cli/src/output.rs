//! Result formatting. Paths are written as raw bytes (exactly as on disk) except in JSON, which
//! carries a lossy display string plus the exact bytes in hex when they are not valid UTF-8.

use std::fmt::Write as _;
use std::io::{self, Write};

use fossick_core::meta::FileKind;
use fossick_core::name::display_name;
use fossick_core::{EntryId, Generation, TimeField};
use fossick_proto::{Kind, Row};

/// A result row built from a local generation (the daemon sends the same rows).
pub fn local_row(g: &Generation, id: EntryId) -> Row {
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
        size: Some(g.size(id)),
        modified: secs(TimeField::Modified),
        created: secs(TimeField::Created),
        accessed: secs(TimeField::Accessed),
        changed: secs(TimeField::Changed),
        mode: g.mode(id),
        uid,
        gid,
        run_count: 0,
        last_run: None,
        offline: false,
        name_highlights: Vec::new(),
        path_highlights: Vec::new(),
    }
}

/// The full path of a row.
pub fn full_path(r: &Row) -> Vec<u8> {
    let mut p = r.parent.clone();
    if r.name.is_empty() {
        return p;
    }
    if p.last() != Some(&b'/') {
        p.push(b'/');
    }
    p.extend_from_slice(&r.name);
    p
}

fn format_time(secs: i64) -> String {
    jiff::Timestamp::from_second(secs)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub fn plain(
    out: &mut impl Write,
    rows: impl Iterator<Item = Row>,
    null: bool,
    long: bool,
) -> io::Result<()> {
    for r in rows {
        if long {
            let mtime = r.modified.map(format_time).unwrap_or_default();
            let size = r.size.map_or_else(|| "-".to_owned(), |s| s.to_string());
            write!(out, "{size:>14}\t{mtime:16}\t")?;
        }
        out.write_all(&full_path(&r))?;
        out.write_all(if null { b"\0" } else { b"\n" })?;
    }
    Ok(())
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::File => "file",
        Kind::Folder => "folder",
        Kind::Symlink => "symlink",
        Kind::Other => "other",
    }
}

pub fn json(out: &mut impl Write, rows: impl Iterator<Item = Row>) -> io::Result<()> {
    out.write_all(b"[")?;
    for (i, r) in rows.enumerate() {
        if i > 0 {
            out.write_all(b",")?;
        }
        let path = full_path(&r);
        let mut obj = String::with_capacity(128);
        let _ = write!(
            obj,
            "\n  {{\"name\": {}, \"path\": {}, \"type\": \"{}\", \"size\": {}",
            json_str(&display_name(&r.name)),
            json_str(&display_name(&path)),
            kind_name(r.kind),
            r.size.map_or_else(|| "null".to_owned(), |s| s.to_string())
        );
        for (key, value) in [("date_modified", r.modified), ("date_created", r.created)] {
            if let Some(t) = value {
                let _ = write!(obj, ", \"{key}\": {t}");
            }
        }
        if r.offline {
            obj.push_str(", \"offline\": true");
        }
        if std::str::from_utf8(&path).is_err() {
            obj.push_str(", \"path_hex\": \"");
            for b in &path {
                let _ = write!(obj, "{b:02x}");
            }
            obj.push('"');
        }
        obj.push('}');
        out.write_all(obj.as_bytes())?;
    }
    out.write_all(b"\n]\n")
}
