//! Per-user run history (`runcount:`, `dr:`, sorting by run count): how often and when each
//! user opened a file through a frontend. Stored by path, one file per uid, in
//! `<state dir>/runs/<uid>`; each record is `count: u32, last: i64, len: u32, path`.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fossick_core::search::RunHistory;
use fossick_core::{EntryId, Generation};

type Runs = HashMap<Vec<u8>, (u32, i64)>;

/// Run histories of all users, loaded on demand.
#[derive(Debug)]
pub struct RunStore {
    dir: PathBuf,
    users: Mutex<HashMap<u32, Arc<Runs>>>,
}

impl RunStore {
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            users: Mutex::new(HashMap::new()),
        }
    }

    fn file(&self, uid: u32) -> PathBuf {
        self.dir.join(uid.to_string())
    }

    fn load(path: &Path) -> Runs {
        let Ok(b) = std::fs::read(path) else {
            return Runs::new();
        };
        let mut out = Runs::new();
        let mut i = 0;
        while i + 16 <= b.len() {
            let count = u32::from_le_bytes(b[i..i + 4].try_into().unwrap_or_default());
            let last = i64::from_le_bytes(b[i + 4..i + 12].try_into().unwrap_or_default());
            let len = u32::from_le_bytes(b[i + 12..i + 16].try_into().unwrap_or_default()) as usize;
            let Some(path) = b.get(i + 16..i + 16 + len) else {
                break;
            };
            out.insert(path.to_vec(), (count, last));
            i += 16 + len;
        }
        out
    }

    /// One user’s runs.
    pub fn user(&self, uid: u32) -> Arc<Runs> {
        let Ok(mut users) = self.users.lock() else {
            return Arc::new(Runs::new());
        };
        Arc::clone(
            users
                .entry(uid)
                .or_insert_with(|| Arc::new(Self::load(&self.file(uid)))),
        )
    }

    /// Records that `uid` opened `path` now.
    pub fn record(&self, uid: u32, path: &[u8]) -> io::Result<()> {
        let mut runs = (*self.user(uid)).clone();
        let now = jiff::Timestamp::now().as_second();
        let e = runs.entry(path.to_vec()).or_insert((0, now));
        e.0 = e.0.saturating_add(1);
        e.1 = now;
        let mut bytes = Vec::new();
        for (p, &(count, last)) in &runs {
            bytes.extend_from_slice(&count.to_le_bytes());
            bytes.extend_from_slice(&last.to_le_bytes());
            bytes.extend_from_slice(&u32::try_from(p.len()).unwrap_or(0).to_le_bytes());
            bytes.extend_from_slice(p);
        }
        std::fs::create_dir_all(&self.dir)?;
        let file = self.file(uid);
        let tmp = file.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &file)?;
        if let Ok(mut users) = self.users.lock() {
            users.insert(uid, Arc::new(runs));
        }
        Ok(())
    }
}

/// A user’s runs resolved against one generation.
#[derive(Debug, Default)]
pub struct ResolvedRuns(HashMap<EntryId, (u32, i64)>);

impl ResolvedRuns {
    #[must_use]
    pub fn new(g: &Generation, runs: &Runs) -> Self {
        Self(
            runs.iter()
                .filter_map(|(p, &v)| Some((g.lookup_path(p)?, v)))
                .collect(),
        )
    }
}

impl RunHistory for ResolvedRuns {
    fn run_count(&self, id: EntryId) -> u32 {
        self.0.get(&id).map_or(0, |r| r.0)
    }

    fn last_run(&self, id: EntryId) -> Option<i64> {
        self.0.get(&id).map(|r| r.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RunStore::new(tmp.path().join("runs"));
        store.record(1000, b"/home/a/x.pdf").unwrap();
        store.record(1000, b"/home/a/x.pdf").unwrap();
        store.record(1000, b"/home/a/\xFF").unwrap();
        store.record(1001, b"/other").unwrap();
        let again = RunStore::new(tmp.path().join("runs"));
        let u = again.user(1000);
        assert_eq!(u.get(&b"/home/a/x.pdf"[..]).map(|r| r.0), Some(2));
        assert_eq!(u.get(&b"/home/a/\xFF"[..]).map(|r| r.0), Some(1));
        assert!(!u.contains_key(&b"/other"[..]));
    }
}
