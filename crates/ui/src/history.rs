//! Recent searches: `$XDG_STATE_HOME/fossick/history` (default `~/.local/state/…`), one search
//! per line, newest first, readable by the user only.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// How many searches are kept.
pub const LIMIT: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    /// Newest first, no duplicates, no empty or multi-line entries.
    pub entries: Vec<String>,
}

impl History {
    /// `$XDG_STATE_HOME/fossick/history` (or `~/.local/state/…`).
    #[must_use]
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
            })?;
        Some(base.join("fossick").join("history"))
    }

    /// Reads a history file; a missing or unreadable file is an empty history.
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        let mut h = Self::default();
        for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
            if h.entries.len() == LIMIT {
                break;
            }
            let line = line.trim();
            if !line.is_empty() && !h.entries.iter().any(|e| e == line) {
                h.entries.push(line.to_owned());
            }
        }
        h
    }

    #[must_use]
    pub fn load() -> Self {
        Self::path().map_or_else(Self::default, |p| Self::load_from(&p))
    }

    /// Puts `search` first (moving it if it was there already). Returns false if it can’t be
    /// stored (empty, or more than one line).
    pub fn add(&mut self, search: &str) -> bool {
        let search = search.trim();
        if search.is_empty() || search.contains(['\n', '\r']) {
            return false;
        }
        self.entries.retain(|e| e != search);
        self.entries.insert(0, search.to_owned());
        self.entries.truncate(LIMIT);
        true
    }

    /// Writes the history (mode 0600: searches can be private).
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("new");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        for e in &self.entries {
            writeln!(f, "{e}")?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    pub fn save(&self) -> std::io::Result<()> {
        match Self::path() {
            Some(p) => self.save_to(&p),
            None => Err(std::io::Error::other("no home directory")),
        }
    }

    /// Forgets every search and removes the file.
    pub fn clear_at(path: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn newest_first_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/fossick/history");
        let mut h = History::load_from(&path);
        assert!(h.entries.is_empty());
        assert!(h.add("report"));
        assert!(h.add(" ext:pdf "));
        assert!(h.add("report"));
        assert!(!h.add("  "));
        assert!(!h.add("a\nb"));
        assert_eq!(h.entries, ["report", "ext:pdf"]);
        h.save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(History::load_from(&path), h);

        for i in 0..2 * LIMIT {
            h.add(&format!("q{i}"));
        }
        assert_eq!(h.entries.len(), LIMIT);
        assert_eq!(h.entries[0], format!("q{}", 2 * LIMIT - 1));

        History::clear_at(&path).unwrap();
        History::clear_at(&path).unwrap();
        assert!(History::load_from(&path).entries.is_empty());
    }
}
