//! MIME types from the shared-mime-info glob database (`/usr/share/mime/globs2`).

use std::collections::HashMap;
use std::path::PathBuf;

use fossick_core::search::MimeDb;

/// MIME type lookup by file name.
#[derive(Clone, Debug, Default)]
pub struct MimeGlobs {
    /// Lowercase extension (may contain dots, e.g. `tar.gz`) → (weight, type).
    ext: HashMap<Vec<u8>, (u32, String)>,
    /// Case-sensitive extensions.
    ext_cs: HashMap<Vec<u8>, (u32, String)>,
    /// Literal names (e.g. `Makefile`).
    literal: HashMap<Vec<u8>, String>,
}

impl MimeGlobs {
    /// Parses `globs2` text (`weight:type:glob[:flags]` lines).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut db = Self::default();
        for line in text.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let mut parts = line.splitn(4, ':');
            let (Some(w), Some(t), Some(glob)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let flags = parts.next().unwrap_or("");
            let weight: u32 = w.parse().unwrap_or(50);
            let cs = flags.split(',').any(|f| f == "cs");
            if let Some(ext) = glob.strip_prefix("*.") {
                if ext.contains(['*', '?', '[']) {
                    continue;
                }
                let (map, key) = if cs {
                    (&mut db.ext_cs, ext.as_bytes().to_vec())
                } else {
                    (&mut db.ext, ext.to_ascii_lowercase().into_bytes())
                };
                let e = map.entry(key).or_insert((weight, t.to_owned()));
                if weight > e.0 {
                    *e = (weight, t.to_owned());
                }
            } else if !glob.contains(['*', '?', '[']) {
                db.literal
                    .entry(glob.as_bytes().to_vec())
                    .or_insert_with(|| t.to_owned());
            }
        }
        db
    }

    /// Loads the first `mime/globs2` found in `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`.
    #[must_use]
    pub fn load_system() -> Option<Self> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(h) = std::env::var_os("XDG_DATA_HOME") {
            dirs.push(h.into());
        } else if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join(".local/share"));
        }
        let data_dirs = std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
        dirs.extend(
            data_dirs
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        );
        dirs.into_iter()
            .find_map(|d| std::fs::read_to_string(d.join("mime/globs2")).ok())
            .map(|t| Self::parse(&t))
    }
}

impl MimeDb for MimeGlobs {
    fn mime_type(&self, name: &[u8]) -> Option<&str> {
        if let Some(t) = self.literal.get(name) {
            return Some(t);
        }
        // Longest extension first: for `a.tar.gz` try `tar.gz`, then `gz`.
        let mut start = 0;
        while let Some(dot) = name[start..].iter().position(|&b| b == b'.') {
            let ext = &name[start + dot + 1..];
            if start + dot > 0 || start > 0 {
                if let Some((_, t)) = self.ext_cs.get(ext) {
                    return Some(t);
                }
                if let Some((_, t)) = self.ext.get(&ext.to_ascii_lowercase()) {
                    return Some(t);
                }
            }
            start += dot + 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_globs2() {
        let db = MimeGlobs::parse(
            "# comment\n50:application/pdf:*.pdf\n50:application/x-compressed-tar:*.tar.gz\n\
             50:application/gzip:*.gz\n10:text/x-makefile:Makefile\n50:text/x-c++src:*.C:cs\n",
        );
        assert_eq!(db.mime_type(b"x.PDF"), Some("application/pdf"));
        assert_eq!(
            db.mime_type(b"a.tar.gz"),
            Some("application/x-compressed-tar")
        );
        assert_eq!(db.mime_type(b"a.gz"), Some("application/gzip"));
        assert_eq!(db.mime_type(b"Makefile"), Some("text/x-makefile"));
        assert_eq!(db.mime_type(b"x.C"), Some("text/x-c++src"));
        assert_eq!(db.mime_type(b"x.c"), None);
        assert_eq!(db.mime_type(b".pdf"), None);
        assert_eq!(db.mime_type(b"noext"), None);
    }
}
