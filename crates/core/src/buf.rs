//! Copy-on-write page buffers: owned memory or zero-copy views into a mapped snapshot.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::persist::Mapping;

/// Plain-old-data element types that can live in a column page.
pub trait Pod:
    Copy + Default + FromBytes + IntoBytes + Immutable + KnownLayout + Send + Sync + 'static
{
}
impl<T> Pod for T where
    T: Copy + Default + FromBytes + IntoBytes + Immutable + KnownLayout + Send + Sync + 'static
{
}

/// Bytes copied by copy-on-write since process start (benchmarks read this).
static COW_BYTES: AtomicU64 = AtomicU64::new(0);

/// Total bytes copied by copy-on-write so far.
#[must_use]
pub fn cow_bytes_copied() -> u64 {
    COW_BYTES.load(Ordering::Relaxed)
}

pub(crate) fn note_copy(bytes: usize) {
    COW_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// A slice of a memory-mapped snapshot, validated for alignment and size when created.
#[derive(Clone)]
pub(crate) struct MappedSlice {
    map: Arc<Mapping>,
    start: usize,
    len: usize,
}

impl MappedSlice {
    pub(crate) fn new(map: Arc<Mapping>, start: usize, len: usize) -> Option<Self> {
        let end = start.checked_add(len)?;
        (end <= map.bytes().len()).then_some(Self { map, start, len })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        self.map
            .bytes()
            .get(self.start..self.start + self.len)
            .unwrap_or(&[])
    }
}

/// Storage for one page of a column.
pub(crate) enum Buf<T: Pod> {
    Owned(Box<[T]>),
    Mapped(MappedSlice),
}

impl<T: Pod> Buf<T> {
    #[inline]
    pub(crate) fn as_slice(&self) -> &[T] {
        match self {
            Self::Owned(b) => b,
            // A misaligned or mis-sized section yields an empty slice: accessors then return
            // defaults instead of panicking. The loader validates sections before mapping.
            Self::Mapped(m) => <[T]>::ref_from_bytes(m.bytes()).unwrap_or(&[]),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.as_slice().len()
    }
}

impl<T: Pod> fmt::Debug for Buf<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Owned(b) => write!(f, "Owned({} elems)", b.len()),
            Self::Mapped(m) => write!(f, "Mapped({} bytes)", m.len),
        }
    }
}

/// Returns a mutable view of an `Arc<Buf<T>>`, copying it first if it is shared or mapped.
pub(crate) fn make_mut<T: Pod>(arc: &mut Arc<Buf<T>>) -> &mut [T] {
    let owned_unique = matches!(Arc::get_mut(arc), Some(Buf::Owned(_)));
    if !owned_unique {
        let copy: Box<[T]> = arc.as_slice().into();
        note_copy(std::mem::size_of_val(&*copy));
        *arc = Arc::new(Buf::Owned(copy));
    }
    match Arc::get_mut(arc) {
        Some(Buf::Owned(b)) => b,
        // We just replaced `arc` with a fresh, uniquely owned `Owned` buffer.
        _ => unreachable!("freshly created buffer is unique and owned"),
    }
}

/// A paged column of `T` with copy-on-write pages of [`crate::index::PAGE_SIZE`] elements.
#[derive(Clone)]
pub(crate) struct Column<T: Pod> {
    pages: Vec<Arc<Buf<T>>>,
    page_size: usize,
}

impl<T: Pod> fmt::Debug for Column<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Column({} pages)", self.pages.len())
    }
}

impl<T: Pod> Column<T> {
    pub(crate) fn new(page_size: usize) -> Self {
        Self {
            pages: Vec::new(),
            page_size,
        }
    }

    pub(crate) fn from_pages(pages: Vec<Arc<Buf<T>>>, page_size: usize) -> Self {
        Self { pages, page_size }
    }

    pub(crate) fn pages(&self) -> &[Arc<Buf<T>>] {
        &self.pages
    }

    pub(crate) fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Appends one page filled with `fill`.
    pub(crate) fn push_page(&mut self, fill: T) {
        self.pages.push(Arc::new(Buf::Owned(
            vec![fill; self.page_size].into_boxed_slice(),
        )));
    }

    #[inline]
    pub(crate) fn page(&self, page: usize) -> &[T] {
        self.pages.get(page).map_or(&[], |p| p.as_slice())
    }

    #[inline]
    pub(crate) fn get(&self, index: usize) -> T {
        let (page, slot) = (index / self.page_size, index % self.page_size);
        self.pages
            .get(page)
            .and_then(|p| p.as_slice().get(slot).copied())
            .unwrap_or_default()
    }

    /// Sets one element, copying its page first if needed. Out-of-range indices are ignored.
    #[inline]
    pub(crate) fn set(&mut self, index: usize, value: T) {
        let (page, slot) = (index / self.page_size, index % self.page_size);
        if let Some(arc) = self.pages.get_mut(page) {
            if arc
                .as_slice()
                .get(slot)
                .is_some_and(|cur| cur.as_bytes() == value.as_bytes())
            {
                return;
            }
            if let Some(v) = make_mut(arc).get_mut(slot) {
                *v = value;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_on_write_isolates_clones() {
        let mut a: Column<u32> = Column::new(4);
        a.push_page(0);
        a.push_page(7);
        a.set(1, 5);
        let b = a.clone();
        a.set(1, 9);
        a.set(6, 1);
        assert_eq!(a.get(1), 9);
        assert_eq!(b.get(1), 5);
        assert_eq!(b.get(6), 7);
        assert_eq!(a.get(6), 1);
        // Out of range reads return the default.
        assert_eq!(a.get(100), 0);
    }

    #[test]
    fn setting_same_value_does_not_copy() {
        let mut a: Column<u16> = Column::new(8);
        a.push_page(3);
        let b = a.clone();
        a.set(2, 3);
        assert!(Arc::ptr_eq(&a.pages()[0], &b.pages()[0]));
    }
}
