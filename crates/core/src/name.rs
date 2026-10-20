//! Raw-byte file name helpers: validation, flags and lossy display.

use std::borrow::Cow;
use std::fmt::Write as _;

/// Returns true if `name` may be stored as a directory entry name: non-empty, no `/`, no NUL.
#[must_use]
pub fn is_valid_entry_name(name: &[u8]) -> bool {
    !name.is_empty() && !name.contains(&b'/') && !name.contains(&0)
}

/// Lossy, unambiguous-enough display form of a raw name: valid UTF-8 is shown as-is, invalid
/// bytes as `\xNN`, and control characters escaped. Use the raw bytes, never this string, when
/// copying names out or opening files.
#[must_use]
pub fn display_name(name: &[u8]) -> Cow<'_, str> {
    if let Ok(s) = std::str::from_utf8(name)
        && !s.chars().any(char::is_control)
    {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(name.len() + 8);
    for chunk in name.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                c if c.is_control() => {
                    let _ = write!(out, "\\u{{{:x}}}", c as u32);
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02X}");
        }
    }
    Cow::Owned(out)
}

/// The extension of a name: the bytes after the last `.`, if that dot is not the first byte.
/// `archive.tar.gz` → `gz`; `.bashrc` → none; `file.` → empty extension.
#[must_use]
pub fn extension(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    (dot > 0).then(|| &name[dot + 1..])
}

/// The name without its extension (`report.pdf` → `report`).
#[must_use]
pub fn stem(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b'.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        assert!(is_valid_entry_name(b"a.txt"));
        assert!(!is_valid_entry_name(b""));
        assert!(!is_valid_entry_name(b"a/b"));
        assert!(!is_valid_entry_name(b"a\0b"));
        assert!(is_valid_entry_name(b"\xFF\xFE"));
    }

    #[test]
    fn displays_lossy() {
        assert_eq!(display_name(b"plain.txt"), "plain.txt");
        assert_eq!(display_name(b"bad\xFFname"), "bad\\xFFname");
        assert_eq!(display_name(b"line\nbreak"), "line\\nbreak");
        assert_eq!(display_name("héllo".as_bytes()), "héllo");
    }

    #[test]
    fn extensions() {
        assert_eq!(extension(b"a.tar.gz"), Some(&b"gz"[..]));
        assert_eq!(extension(b".bashrc"), None);
        assert_eq!(extension(b"noext"), None);
        assert_eq!(extension(b"file."), Some(&b""[..]));
        assert_eq!(stem(b"report.pdf"), b"report");
        assert_eq!(stem(b".hidden"), b".hidden");
    }
}
