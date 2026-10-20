//! Fossick core: the index data structures, query language, matcher, sorting and persistence.
//!
//! This crate is pure logic. Filesystem access (scanning, watching) lives in `fossick-scan` and
//! the daemon; the only I/O here is reading and writing snapshot files.

pub mod buf;
pub mod fold;
pub mod index;
pub mod meta;
pub mod name;
pub mod perm;
pub mod persist;
pub mod query;
pub mod search;
pub mod util;

pub use index::{
    ChangeSet, EntryId, Generation, IndexConfig, IndexError, IndexHandle, IndexWriter, MemoryStats,
    OrderKind, TimeField, Volume, VolumeId,
};
pub use meta::{Attrs, EntryMeta, FileKind, Timestamp};
