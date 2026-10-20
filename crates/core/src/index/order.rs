//! Presorted orders: persistent sequences of id blocks sorted by `(SortKey, id)`.
//!
//! Blocks are immutable `Arc<[u32]>`; batched inserts and removals rebuild only the touched blocks,
//! so publishing a new generation copies kilobytes, not the whole 4-bytes-per-entry order.

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use rayon::prelude::*;

/// Primary and secondary sort key; the entry id is the final tie-break.
pub type SortKey = (u64, u64);

pub(crate) const BLOCK_MAX: usize = 512;
const BLOCK_MIN: usize = BLOCK_MAX / 4;

#[inline]
fn cmp_full(a: (SortKey, u32), b: (SortKey, u32)) -> Ordering {
    a.cmp(&b)
}

/// One presorted order.
#[derive(Clone, Default)]
pub struct Order {
    blocks: Vec<Arc<[u32]>>,
    /// `(key, id)` of the first element of each block, so finding a block needs no key lookups.
    /// Must be refreshed (`refresh_fences`) whenever keys of ids in the order change in place
    /// (monotone relabelling).
    fences: Vec<(SortKey, u32)>,
    /// `starts[i]` = number of ids before block `i`.
    starts: Vec<usize>,
    len: usize,
}

impl fmt::Debug for Order {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Order({} ids, {} blocks)", self.len, self.blocks.len())
    }
}

impl Order {
    /// Builds an order from ids already sorted by `(key, id)`.
    pub(crate) fn from_sorted(ids: &[u32], key: &(impl Fn(u32) -> SortKey + Sync)) -> Self {
        let blocks = ids
            .chunks(BLOCK_MAX / 2)
            .map(Arc::<[u32]>::from)
            .collect::<Vec<_>>();
        let mut order = Self {
            blocks,
            fences: Vec::new(),
            starts: Vec::new(),
            len: 0,
        };
        order.reindex();
        order.refresh_fences(key);
        order
    }

    /// Builds an order from sorted blocks and their fences (snapshot loading).
    pub(crate) fn from_parts(blocks: Vec<Arc<[u32]>>, fences: Vec<(SortKey, u32)>) -> Self {
        let mut order = Self {
            blocks,
            fences,
            starts: Vec::new(),
            len: 0,
        };
        order.reindex();
        order
    }

    pub(crate) fn fences(&self) -> &[(SortKey, u32)] {
        &self.fences
    }

    /// Recomputes all block fences from current keys (after monotone relabelling).
    pub(crate) fn refresh_fences(&mut self, key: &(impl Fn(u32) -> SortKey + Sync)) {
        self.fences = self
            .blocks
            .par_iter()
            .map(|b| b.first().map_or(((0, 0), 0), |&id| (key(id), id)))
            .collect();
    }

    fn fence_of(blk: &[u32], key: &impl Fn(u32) -> SortKey) -> (SortKey, u32) {
        blk.first().map_or(((0, 0), 0), |&id| (key(id), id))
    }

    /// Index of the last block whose first element is `<= target` (0 if none).
    #[inline]
    fn block_for(&self, target: (SortKey, u32)) -> usize {
        self.fences
            .partition_point(|&f| f <= target)
            .saturating_sub(1)
    }

    /// Sorts `ids` by `(key(id), id)` in parallel and builds the order.
    pub(crate) fn build(mut ids: Vec<u32>, key: impl Fn(u32) -> SortKey + Sync) -> Self {
        let mut keyed: Vec<(SortKey, u32)> = ids.par_iter().map(|&id| (key(id), id)).collect();
        keyed.par_sort_unstable();
        ids.clear();
        ids.extend(keyed.iter().map(|&(_, id)| id));
        Self::from_sorted(&ids, &key)
    }

    fn reindex(&mut self) {
        self.starts.clear();
        let mut n = 0;
        for b in &self.blocks {
            self.starts.push(n);
            n += b.len();
        }
        self.len = n;
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn blocks(&self) -> &[Arc<[u32]>] {
        &self.blocks
    }

    /// The id at position `pos`.
    #[must_use]
    pub fn get(&self, pos: usize) -> Option<u32> {
        if pos >= self.len {
            return None;
        }
        let b = self.starts.partition_point(|&s| s <= pos) - 1;
        self.blocks[b].get(pos - self.starts[b]).copied()
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = u32> + '_ {
        self.blocks.iter().flat_map(|b| b.iter().copied())
    }

    /// Position of the first element whose id compares `>= probe` using an arbitrary comparator
    /// (`cmp(id)` returns how the element at that position compares to the probe).
    pub(crate) fn partition_by(&self, cmp: impl Fn(u32) -> Ordering) -> usize {
        // Find the first block whose last element is not Less than the probe.
        let b = self
            .blocks
            .partition_point(|blk| blk.last().is_some_and(|&id| cmp(id) == Ordering::Less));
        if b == self.blocks.len() {
            return self.len;
        }
        let i = self.blocks[b].partition_point(|&id| cmp(id) == Ordering::Less);
        self.starts[b] + i
    }

    /// (block, index within block) of the first element `>= target`.
    fn locate(&self, target: (SortKey, u32), key: &impl Fn(u32) -> SortKey) -> (usize, usize) {
        if self.blocks.is_empty() {
            return (0, 0);
        }
        let b = self.block_for(target);
        let i =
            self.blocks[b].partition_point(|&id| cmp_full((key(id), id), target) == Ordering::Less);
        if i == self.blocks[b].len() && b + 1 < self.blocks.len() {
            return (b + 1, 0);
        }
        (b, i)
    }

    /// Removes the given `(key, id)` pairs. `key` must give the keys the order was sorted by
    /// (for ids being removed, their old keys). Returns how many were found and removed.
    pub(crate) fn remove_batch(
        &mut self,
        mut items: Vec<(SortKey, u32)>,
        key: &impl Fn(u32) -> SortKey,
    ) -> usize {
        if items.is_empty() || self.blocks.is_empty() {
            return 0;
        }
        items.sort_unstable();
        let mut per_block: Vec<(usize, Vec<usize>)> = Vec::new();
        for &item in &items {
            // The fence picks the block without computing any key; a scan of at most
            // BLOCK_MAX ids finds the entry. Fall back to a keyed search if fences are stale.
            let b = self.block_for(item);
            let found = self
                .blocks
                .get(b)
                .and_then(|blk| blk.iter().position(|&x| x == item.1));
            let (b, i) = match found {
                Some(i) => (b, i),
                None => self.locate(item, key),
            };
            if self.blocks.get(b).and_then(|blk| blk.get(i)) == Some(&item.1) {
                match per_block.last_mut() {
                    Some((lb, v)) if *lb == b => v.push(i),
                    _ => per_block.push((b, vec![i])),
                }
            }
        }
        let mut removed = 0;
        for (b, positions) in &per_block {
            let blk = &self.blocks[*b];
            let mut next = Vec::with_capacity(blk.len().saturating_sub(positions.len()));
            let mut p = positions.iter().peekable();
            for (i, &id) in blk.iter().enumerate() {
                if p.peek() == Some(&&i) {
                    p.next();
                    removed += 1;
                } else {
                    next.push(id);
                }
            }
            crate::buf::note_copy(next.len() * 4);
            self.fences[*b] = Self::fence_of(&next, key);
            self.blocks[*b] = Arc::from(next);
        }
        self.drop_empty();
        self.rebalance();
        self.reindex();
        removed
    }

    /// Inserts `(key, id)` pairs; `key` gives current keys of ids already in the order.
    pub(crate) fn insert_batch(
        &mut self,
        mut items: Vec<(SortKey, u32)>,
        key: &impl Fn(u32) -> SortKey,
    ) {
        if items.is_empty() {
            return;
        }
        items.sort_unstable();
        if self.blocks.is_empty() {
            let ids: Vec<u32> = items.iter().map(|&(_, id)| id).collect();
            let blocks: Vec<Arc<[u32]>> = ids.chunks(BLOCK_MAX / 2).map(Arc::from).collect();
            let fences = blocks.iter().map(|b| Self::fence_of(b, key)).collect();
            *self = Self::from_parts(blocks, fences);
            return;
        }
        // Group items by the block they belong in: the last block whose first element <= item.
        let mut groups: Vec<(usize, Vec<(SortKey, u32)>)> = Vec::new();
        for &item in &items {
            let b = self.block_for(item);
            match groups.last_mut() {
                Some((lb, v)) if *lb == b => v.push(item),
                _ => groups.push((b, vec![item])),
            }
        }
        // Rebuild touched blocks from the back so earlier indices stay valid while splitting.
        // Insertion points are found by binary search, so only O(log B) keys are computed per
        // inserted item instead of one per existing element.
        for (b, group) in groups.into_iter().rev() {
            let blk = &self.blocks[b];
            let mut merged = Vec::with_capacity(blk.len() + group.len());
            let mut from = 0;
            for item in group {
                let at = from
                    + blk[from..]
                        .partition_point(|&id| cmp_full((key(id), id), item) == Ordering::Less);
                merged.extend_from_slice(&blk[from..at]);
                merged.push(item.1);
                from = at;
            }
            merged.extend_from_slice(&blk[from..]);
            crate::buf::note_copy(merged.len() * 4);
            let pieces: Vec<Arc<[u32]>> = if merged.len() > BLOCK_MAX {
                merged.chunks(BLOCK_MAX / 2).map(Arc::from).collect()
            } else {
                vec![Arc::from(merged)]
            };
            let fences: Vec<(SortKey, u32)> =
                pieces.iter().map(|p| Self::fence_of(p, key)).collect();
            self.blocks.splice(b..=b, pieces);
            self.fences.splice(b..=b, fences);
        }
        self.reindex();
    }

    fn drop_empty(&mut self) {
        if self.blocks.iter().any(|b| b.is_empty()) {
            let mut fences = std::mem::take(&mut self.fences).into_iter();
            let blocks = std::mem::take(&mut self.blocks);
            for blk in blocks {
                let f = fences.next().unwrap_or(((0, 0), 0));
                if !blk.is_empty() {
                    self.blocks.push(blk);
                    self.fences.push(f);
                }
            }
        }
    }

    /// Merges undersized neighbouring blocks (only touches blocks that need it).
    fn rebalance(&mut self) {
        let mut i = 1;
        while i < self.blocks.len() {
            let (l, r) = (self.blocks[i - 1].len(), self.blocks[i].len());
            if (l < BLOCK_MIN || r < BLOCK_MIN) && l + r <= BLOCK_MAX {
                let mut v = Vec::with_capacity(l + r);
                v.extend_from_slice(&self.blocks[i - 1]);
                v.extend_from_slice(&self.blocks[i]);
                self.blocks[i - 1] = Arc::from(v);
                self.blocks.remove(i);
                self.fences.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Keeps the ids whose bit is set in `bits` (indexed by id), in order (or reversed),
    /// processing blocks in parallel.
    #[must_use]
    pub fn filter(&self, bits: &[u64], reverse: bool) -> Vec<u32> {
        let test = |id: u32| {
            bits.get((id >> 6) as usize)
                .is_some_and(|w| w & (1u64 << (id & 63)) != 0)
        };
        let parts: Vec<Vec<u32>> = self
            .blocks
            .par_iter()
            .map(|blk| {
                if reverse {
                    blk.iter().rev().copied().filter(|&id| test(id)).collect()
                } else {
                    blk.iter().copied().filter(|&id| test(id)).collect()
                }
            })
            .collect();
        let total = parts.iter().map(Vec::len).sum();
        let mut out = Vec::with_capacity(total);
        if reverse {
            for p in parts.into_iter().rev() {
                out.extend(p);
            }
        } else {
            for p in parts {
                out.extend(p);
            }
        }
        out
    }

    /// Verifies the order is sorted under `key` (tests and debug assertions).
    pub(crate) fn is_sorted_by(&self, key: &impl Fn(u32) -> SortKey) -> bool {
        let mut prev: Option<(SortKey, u32)> = None;
        for id in self.iter() {
            let k = (key(id), id);
            if prev.is_some_and(|p| p >= k) {
                return false;
            }
            prev = Some(k);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyfn(keys: &[u64]) -> impl Fn(u32) -> SortKey + Sync + '_ {
        move |id| (keys[id as usize], 0)
    }

    #[test]
    fn build_insert_remove_keep_sorted() {
        let mut keys: Vec<u64> = (0..20_000u64).map(|i| (i * 7919) % 10_007).collect();
        let ids: Vec<u32> = (0..10_000).collect();
        let mut order = Order::build(ids, keyfn(&keys));
        assert_eq!(order.len(), 10_000);
        assert!(order.is_sorted_by(&keyfn(&keys)));

        // Insert 10_000 more.
        let k = keyfn(&keys);
        let items: Vec<(SortKey, u32)> = (10_000..20_000u32).map(|id| (k(id), id)).collect();
        order.insert_batch(items, &k);
        assert_eq!(order.len(), 20_000);
        assert!(order.is_sorted_by(&k));

        // Remove every third id, then change their keys and re-insert.
        let victims: Vec<u32> = (0..20_000u32).filter(|i| i % 3 == 0).collect();
        let items: Vec<(SortKey, u32)> = victims.iter().map(|&id| (k(id), id)).collect();
        let removed = order.remove_batch(items, &k);
        assert_eq!(removed, victims.len());
        assert!(order.is_sorted_by(&k));
        drop(k);
        for &v in &victims {
            keys[v as usize] = 50_000 - u64::from(v);
        }
        let k = keyfn(&keys);
        let items: Vec<(SortKey, u32)> = victims.iter().map(|&id| (k(id), id)).collect();
        order.insert_batch(items, &k);
        assert_eq!(order.len(), 20_000);
        assert!(order.is_sorted_by(&k));
        let all: Vec<u32> = order.iter().collect();
        assert_eq!(order.get(1234), Some(all[1234]));
    }

    #[test]
    fn filter_by_bitmap() {
        let keys: Vec<u64> = (0..100u64).rev().collect();
        let order = Order::build((0..100).collect(), keyfn(&keys));
        let mut bits = vec![0u64; 2];
        for id in [3u32, 64, 99] {
            bits[(id >> 6) as usize] |= 1 << (id & 63);
        }
        assert_eq!(order.filter(&bits, false), vec![99, 64, 3]);
        assert_eq!(order.filter(&bits, true), vec![3, 64, 99]);
    }

    #[test]
    fn empty_order_operations() {
        let keys = vec![5u64, 1, 3];
        let k = keyfn(&keys);
        let mut order = Order::default();
        assert_eq!(order.remove_batch(vec![((1, 0), 1)], &k), 0);
        order.insert_batch(vec![(k(0), 0), (k(1), 1), (k(2), 2)], &k);
        assert_eq!(order.iter().collect::<Vec<_>>(), vec![1, 2, 0]);
        assert_eq!(
            order.remove_batch(vec![(k(1), 1), (k(0), 0), (k(2), 2)], &k),
            3
        );
        assert!(order.is_empty());
    }
}
