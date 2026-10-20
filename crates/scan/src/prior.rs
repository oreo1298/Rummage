//! A [`Prior`](crate::Prior) backed by a published generation, for pruned rescans after a
//! snapshot load.

use std::collections::HashMap;
use std::sync::Arc;

use fossick_core::{EntryId, FileKind, Generation};

/// Subdirectories of one directory: `(name, entry id)`, sorted by name.
type Subdirs = Vec<(Box<[u8]>, u32)>;

/// What a generation knows about the directories of one volume.
#[derive(Debug)]
pub struct IndexPrior {
    g: Arc<Generation>,
    root: EntryId,
    subdirs: HashMap<u32, Subdirs>,
    cutoff_ns: i64,
}

impl IndexPrior {
    /// Builds the directory map for the volume whose root is `root`. Directories whose
    /// modification or change time is at or after `cutoff_ns` are always listed again: a change
    /// within the filesystem’s timestamp granularity of the snapshot may not have moved them.
    #[must_use]
    pub fn new(g: Arc<Generation>, root: EntryId, cutoff_ns: i64) -> Self {
        let volume = g.volume(root).map(|v| v.id);
        let mut subdirs: HashMap<u32, Subdirs> = HashMap::new();
        for id in g.iter() {
            if g.kind(id) != FileKind::Dir || g.volume(id).map(|v| v.id) != volume {
                continue;
            }
            if let Some(p) = g.parent(id) {
                subdirs
                    .entry(p.0)
                    .or_default()
                    .push((g.name(id).into(), id.0));
            }
        }
        for v in subdirs.values_mut() {
            v.sort_unstable();
        }
        Self {
            g,
            root,
            subdirs,
            cutoff_ns,
        }
    }
}

impl crate::Prior for IndexPrior {
    fn root(&self) -> Option<u32> {
        Some(self.root.0)
    }

    fn child(&self, parent: u32, name: &[u8]) -> Option<u32> {
        let list = self.subdirs.get(&parent)?;
        list.binary_search_by(|(n, _)| (**n).cmp(name))
            .ok()
            .map(|i| list[i].1)
    }

    fn unchanged(&self, key: u32, mtime_ns: i64, ctime_ns: i64) -> Option<Vec<Box<[u8]>>> {
        let (m, c) = self.g.dir_times_ns(EntryId(key))?;
        if (m, c) != (mtime_ns, ctime_ns) || m >= self.cutoff_ns || c >= self.cutoff_ns {
            return None;
        }
        Some(
            self.subdirs
                .get(&key)
                .map(|v| v.iter().map(|(n, _)| n.clone()).collect())
                .unwrap_or_default(),
        )
    }
}
