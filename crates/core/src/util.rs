//! Small shared utilities.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// A fast, non-cryptographic hasher for integer keys (entry ids), in the style of FxHash.
/// Keys here are not attacker-controlled hash-flooding vectors: they are ids the index assigns.
#[derive(Clone, Copy, Debug, Default)]
pub struct IdHasher(u64);

const K: u64 = 0x517c_c1b7_2722_0a95;

impl Hasher for IdHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(K);
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.0 = (self.0.rotate_left(5) ^ u64::from(i)).wrapping_mul(K);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0 = (self.0.rotate_left(5) ^ u64::from(i)).wrapping_mul(K);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(K);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type IdMap<K, V> = HashMap<K, V, BuildHasherDefault<IdHasher>>;
pub type IdSet<K> = HashSet<K, BuildHasherDefault<IdHasher>>;

/// A growable bitmap over ids.
#[derive(Clone, Debug, Default)]
pub struct Bitmap {
    words: Vec<u64>,
}

impl Bitmap {
    #[must_use]
    pub fn with_capacity(bits: usize) -> Self {
        Self {
            words: vec![0; bits.div_ceil(64)],
        }
    }

    #[inline]
    pub fn set(&mut self, i: u32) {
        let w = (i >> 6) as usize;
        if w >= self.words.len() {
            self.words.resize(w + 1, 0);
        }
        self.words[w] |= 1 << (i & 63);
    }

    #[inline]
    #[must_use]
    pub fn get(&self, i: u32) -> bool {
        self.words
            .get((i >> 6) as usize)
            .is_some_and(|w| w & (1 << (i & 63)) != 0)
    }

    #[must_use]
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    pub fn words_mut(&mut self) -> &mut [u64] {
        &mut self.words
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }
}
