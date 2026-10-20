//! Result formatting. Paths are written as raw bytes (exactly as on disk) except in JSON, which
//! carries a lossy display string plus the exact bytes in hex when they are not valid UTF-8.

use std::fmt::Write as _;
use std::io::{self, Write};

use fossick_core::meta::FileKind;
use fossick_core::name::display_name;
use fossick_core::{EntryId, Generation, TimeField};

fn format_time(secs: i64) -> String {
    jiff::Timestamp::from_second(secs)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub fn plain<'a>(
    out: &mut impl Write,
    g: &Generation,
    ids: impl Iterator<Item = &'a EntryId>,
    null: bool,
    long: bool,
) -> io::Result<()> {
    for &id in ids {
        if long {
            let mtime = g
                .time(id, TimeField::Modified)
                .map(|t| format_time(t.secs))
                .unwrap_or_default();
            write!(out, "{:>14}\t{mtime:16}\t", g.size(id))?;
        }
        out.write_all(&g.path(id))?;
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

fn kind_name(k: FileKind) -> &'static str {
    match k {
        FileKind::File => "file",
        FileKind::Dir => "folder",
        FileKind::Symlink => "symlink",
        FileKind::CharDevice => "char-device",
        FileKind::BlockDevice => "block-device",
        FileKind::Fifo => "fifo",
        FileKind::Socket => "socket",
        FileKind::Unknown => "unknown",
    }
}

pub fn json<'a>(
    out: &mut impl Write,
    g: &Generation,
    ids: impl Iterator<Item = &'a EntryId>,
) -> io::Result<()> {
    out.write_all(b"[")?;
    for (i, &id) in ids.enumerate() {
        if i > 0 {
            out.write_all(b",")?;
        }
        let path = g.path(id);
        let mut obj = String::with_capacity(128);
        let _ = write!(
            obj,
            "\n  {{\"name\": {}, \"path\": {}, \"type\": \"{}\", \"size\": {}",
            json_str(&display_name(g.name(id))),
            json_str(&display_name(&path)),
            kind_name(g.kind(id)),
            g.size(id)
        );
        for (key, field) in [
            ("date_modified", TimeField::Modified),
            ("date_created", TimeField::Created),
        ] {
            if let Some(t) = g.time(id, field) {
                let _ = write!(obj, ", \"{key}\": {}", t.secs);
            }
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
