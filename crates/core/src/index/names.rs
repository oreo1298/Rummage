//! Per-page name arenas.
//!
//! Names of one 4096-slot page are stored back to back, each followed by a NUL byte, in slot
//! order, plus a table of start offsets. NUL never occurs in a Linux file name, so a substring
//! match over the arena can never span two names. Free slots have empty names.

use std::fmt;
use std::sync::OnceLock;

use crate::buf::Buf;
use crate::fold;
use crate::index::{PAGE_SIZE, flags};

/// Which normalized form a side arena holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SideKind {
    pub(crate) fold_case: bool,
    pub(crate) strip_diacritics: bool,
}

impl SideKind {
    fn index(self) -> usize {
        usize::from(self.fold_case) | (usize::from(self.strip_diacritics) << 1)
    }
}

/// Normalized names of the non-ASCII entries of one page (built lazily on first use).
#[derive(Debug, Default)]
pub(crate) struct SideArena {
    pub(crate) slots: Vec<u16>,
    pub(crate) bytes: Vec<u8>,
    pub(crate) offs: Vec<u32>,
}

impl SideArena {
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Normalized name number `i` (not slot `i`).
    pub(crate) fn name(&self, i: usize) -> &[u8] {
        let start = self.offs[i] as usize;
        let end = self.offs[i + 1] as usize - 1;
        &self.bytes[start..end]
    }
}

/// The name arena of one page.
pub(crate) struct NamePage {
    pub(crate) bytes: Buf<u8>,
    /// `PAGE_SIZE + 1` offsets; name `i` is `bytes[offs[i] .. offs[i + 1] - 1]`.
    pub(crate) offs: Buf<u32>,
    side: [OnceLock<SideArena>; 4],
}

impl fmt::Debug for NamePage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NamePage({} bytes)", self.bytes.len())
    }
}

impl NamePage {
    pub(crate) fn from_parts(bytes: Buf<u8>, offs: Buf<u32>) -> Self {
        Self {
            bytes,
            offs,
            side: Default::default(),
        }
    }

    /// An all-free page.
    pub(crate) fn empty() -> Self {
        Self::build((0..PAGE_SIZE).map(|_| &[][..]))
    }

    /// Builds a page from exactly `PAGE_SIZE` names in slot order.
    pub(crate) fn build<'a>(names: impl Iterator<Item = &'a [u8]>) -> Self {
        let mut bytes = Vec::with_capacity(PAGE_SIZE * 16);
        let mut offs = Vec::with_capacity(PAGE_SIZE + 1);
        for name in names.take(PAGE_SIZE) {
            offs.push(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
            bytes.extend_from_slice(name);
            bytes.push(0);
        }
        while offs.len() < PAGE_SIZE {
            offs.push(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
            bytes.push(0);
        }
        offs.push(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        crate::buf::note_copy(bytes.len() + offs.len() * 4);
        Self::from_parts(
            Buf::Owned(bytes.into_boxed_slice()),
            Buf::Owned(offs.into_boxed_slice()),
        )
    }

    /// The name stored in `slot` (empty for free slots or corrupt data).
    #[inline]
    pub(crate) fn name(&self, slot: usize) -> &[u8] {
        let offs = self.offs.as_slice();
        let bytes = self.bytes.as_slice();
        let (Some(&start), Some(&next)) = (offs.get(slot), offs.get(slot + 1)) else {
            return &[];
        };
        let (start, end) = (start as usize, (next as usize).saturating_sub(1));
        bytes.get(start..end).unwrap_or(&[])
    }

    #[inline]
    pub(crate) fn arena(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    #[inline]
    pub(crate) fn offsets(&self) -> &[u32] {
        self.offs.as_slice()
    }

    /// The side arena of the given kind, built on first use from this page’s non-ASCII names.
    pub(crate) fn side(&self, kind: SideKind, page_flags: &[u8]) -> &SideArena {
        self.side[kind.index()].get_or_init(|| {
            let mut side = SideArena::default();
            for (slot, &f) in page_flags.iter().enumerate().take(PAGE_SIZE) {
                if f & flags::FREE != 0 || f & flags::NON_ASCII == 0 {
                    continue;
                }
                side.slots.push(u16::try_from(slot).unwrap_or(u16::MAX));
                side.offs
                    .push(u32::try_from(side.bytes.len()).unwrap_or(u32::MAX));
                let name = self.name(slot);
                if kind.strip_diacritics {
                    fold::strip_diacritics_into(name, kind.fold_case, &mut side.bytes);
                } else {
                    fold::normalize_into(name, kind.fold_case, &mut side.bytes);
                }
                side.bytes.push(0);
            }
            side.offs
                .push(u32::try_from(side.bytes.len()).unwrap_or(u32::MAX));
            side
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_reads_names() {
        let names: Vec<&[u8]> = vec![b"alpha", b"", "Ünïcode".as_bytes(), b"x\xFF"];
        let page = NamePage::build(names.iter().copied());
        assert_eq!(page.name(0), b"alpha");
        assert_eq!(page.name(1), b"");
        assert_eq!(page.name(2), "Ünïcode".as_bytes());
        assert_eq!(page.name(3), b"x\xFF");
        assert_eq!(page.name(4), b"");
        assert_eq!(page.name(PAGE_SIZE - 1), b"");
        assert_eq!(page.name(PAGE_SIZE + 5), b"");
        assert_eq!(page.offsets().len(), PAGE_SIZE + 1);
    }

    #[test]
    fn side_arena_holds_only_non_ascii() {
        let names: Vec<&[u8]> = vec![b"plain", "Café".as_bytes(), "e\u{301}cole".as_bytes()];
        let page = NamePage::build(names.iter().copied());
        let mut fl = vec![flags::FREE; PAGE_SIZE];
        fl[0] = 0;
        fl[1] = flags::NON_ASCII;
        fl[2] = flags::NON_ASCII;
        let side = page.side(
            SideKind {
                fold_case: true,
                strip_diacritics: false,
            },
            &fl,
        );
        assert_eq!(side.slots, vec![1, 2]);
        assert_eq!(side.name(0), "café".as_bytes());
        assert_eq!(side.name(1), "école".as_bytes());
        let stripped = page.side(
            SideKind {
                fold_case: true,
                strip_diacritics: true,
            },
            &fl,
        );
        assert_eq!(stripped.name(0), b"cafe");
        assert_eq!(stripped.name(1), b"ecole");
    }
}
