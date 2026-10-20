//! Writer-private child lists.
//!
//! Each directory keeps its children as `(name hash, id)` pairs sorted by that pair, plus small
//! unsorted add/remove buffers that are merged lazily, so a storm of creates in one huge
//! directory is not quadratic. The stored hash never changes while a pair is in the list, so
//! lazy removal stays consistent even when the entry is renamed before the buffers are merged.

use xxhash_rust::xxh3::xxh3_64;

#[inline]
pub(crate) fn name_hash(name: &[u8]) -> u32 {
    // Truncation is intended: collisions are resolved by comparing names.
    #[allow(clippy::cast_possible_truncation)]
    let h = xxh3_64(name) as u32;
    h
}

type Pair = (u32, u32);

#[derive(Clone, Debug, Default)]
pub(crate) struct ChildList {
    sorted: Vec<Pair>,
    added: Vec<Pair>,
    removed: Vec<Pair>,
}

impl ChildList {
    fn threshold(&self) -> usize {
        // sqrt(n) balances linear buffer scans against O(n) merges; at least 64.
        64usize.max(self.sorted.len().isqrt())
    }

    /// Finds the child named `name`.
    pub(crate) fn find<'a>(&self, name: &[u8], name_of: &impl Fn(u32) -> &'a [u8]) -> Option<u32> {
        let h = name_hash(name);
        let start = self.sorted.partition_point(|&(sh, _)| sh < h);
        for &(sh, id) in &self.sorted[start..] {
            if sh != h {
                break;
            }
            if name_of(id) == name && !self.removed.contains(&(sh, id)) {
                return Some(id);
            }
        }
        self.added
            .iter()
            .find(|&&(ah, id)| ah == h && name_of(id) == name)
            .map(|&(_, id)| id)
    }

    /// Adds `id` under `name`.
    pub(crate) fn insert(&mut self, id: u32, name: &[u8]) {
        let pair = (name_hash(name), id);
        if let Some(pos) = self.removed.iter().position(|&r| r == pair) {
            // The identical pair is still in `sorted`: cancelling the removal is exact.
            self.removed.swap_remove(pos);
            return;
        }
        self.added.push(pair);
        self.maybe_compact();
    }

    /// Removes `id`, which was inserted under `name`. Returns false if it was not present.
    pub(crate) fn remove(&mut self, id: u32, name: &[u8]) -> bool {
        let pair = (name_hash(name), id);
        if let Some(pos) = self.added.iter().position(|&a| a == pair) {
            self.added.swap_remove(pos);
            return true;
        }
        if self.sorted.binary_search(&pair).is_err() || self.removed.contains(&pair) {
            return false;
        }
        self.removed.push(pair);
        self.maybe_compact();
        true
    }

    fn maybe_compact(&mut self) {
        if self.added.len() + self.removed.len() > self.threshold() {
            self.compact();
        }
    }

    fn compact(&mut self) {
        if !self.removed.is_empty() {
            self.removed.sort_unstable();
            let removed = std::mem::take(&mut self.removed);
            self.sorted.retain(|p| removed.binary_search(p).is_err());
        }
        if !self.added.is_empty() {
            let mut added = std::mem::take(&mut self.added);
            added.sort_unstable();
            let mut merged = Vec::with_capacity(self.sorted.len() + added.len());
            let (mut i, mut j) = (0, 0);
            while i < self.sorted.len() && j < added.len() {
                if self.sorted[i] < added[j] {
                    merged.push(self.sorted[i]);
                    i += 1;
                } else {
                    merged.push(added[j]);
                    j += 1;
                }
            }
            merged.extend_from_slice(&self.sorted[i..]);
            merged.extend_from_slice(&added[j..]);
            self.sorted = merged;
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.sorted.len() + self.added.len() - self.removed.len()
    }

    /// All children ids (no particular order).
    pub(crate) fn ids(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .sorted
            .iter()
            .filter(|p| !self.removed.contains(p))
            .map(|&(_, id)| id)
            .collect();
        out.extend(self.added.iter().map(|&(_, id)| id));
        out
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Approximate heap bytes used (for memory accounting).
    pub(crate) fn heap_bytes(&self) -> usize {
        (self.sorted.capacity() + self.added.capacity() + self.removed.capacity()) * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_find_remove_many() {
        let names: Vec<Vec<u8>> = (0..5000).map(|i| format!("f{i:05}").into_bytes()).collect();
        let name_of = |id: u32| names[id as usize].as_slice();
        let mut list = ChildList::default();
        for id in (0..5000u32).rev() {
            list.insert(id, &names[id as usize]);
        }
        assert_eq!(list.len(), 5000);
        for id in (0..5000u32).step_by(7) {
            assert_eq!(list.find(&names[id as usize], &name_of), Some(id));
        }
        for id in (0..5000u32).step_by(2) {
            assert!(list.remove(id, &names[id as usize]));
        }
        assert!(!list.remove(0, &names[0]));
        assert_eq!(list.len(), 2500);
        assert_eq!(list.find(b"f00002", &name_of), None);
        assert_eq!(list.find(b"f00003", &name_of), Some(3));
        let mut ids = list.ids();
        ids.sort_unstable();
        assert_eq!(ids, (1..5000).step_by(2).collect::<Vec<u32>>());
    }

    #[test]
    fn rename_with_lazy_removal_stays_consistent() {
        let mut names: Vec<Vec<u8>> = (0..200).map(|i| format!("n{i}").into_bytes()).collect();
        let mut list = ChildList::default();
        for id in 0..200u32 {
            list.insert(id, &names[id as usize]);
        }
        // Force a compaction so everything is in `sorted`.
        list.compact();
        // Rename id 5: remove under the old name, change it, insert under the new one.
        assert!(list.remove(5, &names[5]));
        names[5] = b"renamed".to_vec();
        list.insert(5, &names[5]);
        let name_of = |id: u32| names[id as usize].as_slice();
        assert_eq!(list.find(b"renamed", &name_of), Some(5));
        assert_eq!(list.find(b"n5", &name_of), None);
        assert_eq!(list.find(b"n6", &name_of), Some(6));
        assert_eq!(list.len(), 200);
    }
}
