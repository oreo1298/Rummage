//! The in-memory index: paged struct-of-arrays columns, directory records, presorted orders and
//! published generations.

pub(crate) mod children;
pub(crate) mod names;
pub mod order;
mod writer;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

use crate::buf::Column;
use crate::meta::{Attrs, FileKind, Timestamp};
use names::NamePage;
pub use order::{Order, SortKey};
pub use writer::{IndexConfig, IndexError, IndexWriter, PublishStats};

/// log2 of the number of slots per page.
pub const PAGE_BITS: u32 = 12;
/// Slots per page.
pub const PAGE_SIZE: usize = 1 << PAGE_BITS;
/// Sentinel for “no entry” in `u32` columns.
pub(crate) const NONE: u32 = u32::MAX;
/// Upper bound on parent-chain walks (protects against cycles in corrupt snapshots).
pub(crate) const MAX_DEPTH: usize = 4096;

/// Time column sentinels.
pub(crate) const T_UNKNOWN: u32 = u32::MAX - 1;
pub(crate) const T_ESCAPE: u32 = u32::MAX;
/// Size column sentinel for files ≥ 4 GiB − 1.
pub(crate) const SIZE_ESCAPE: u32 = u32::MAX;
/// Owner column sentinel when the owner table is full.
pub(crate) const OWNER_ESCAPE: u16 = u16::MAX;

/// Per-entry flag bits.
pub mod flags {
    pub const FREE: u8 = 1;
    pub const NON_ASCII: u8 = 2;
    pub const FOLD_SPECIAL: u8 = 4;
    pub const HIDDEN: u8 = 8;
    pub const VOLUME_ROOT: u8 = 16;
    pub const HAS_ACL: u8 = 32;
}

/// Identifies one entry (file, directory, …) for its whole lifetime. Internal to the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryId(pub u32);

impl EntryId {
    #[inline]
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
    #[inline]
    #[must_use]
    pub fn page(self) -> usize {
        (self.0 >> PAGE_BITS) as usize
    }
    #[inline]
    #[must_use]
    pub fn slot(self) -> usize {
        (self.0 as usize) & (PAGE_SIZE - 1)
    }
    #[must_use]
    pub fn from_parts(page: usize, slot: usize) -> Self {
        Self(u32::try_from((page << PAGE_BITS) | slot).unwrap_or(NONE))
    }
}

/// Identifies a volume (one indexed filesystem tree).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VolumeId(pub u16);

/// Which timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeField {
    Modified,
    Changed,
    Created,
    Accessed,
}

/// A volume: a mounted filesystem (or directory tree, in local mode) and where it lives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub id: VolumeId,
    /// Stable identity (filesystem UUID, btrfs fsid+subvolume, …).
    pub key: String,
    /// Absolute mount path as raw bytes (`/`, `/home`, `/run/media/me/USB`).
    pub mount_path: Vec<u8>,
    pub fstype: String,
    pub label: Option<String>,
    pub root: EntryId,
    pub online: bool,
}

impl Volume {
    /// Path components of the mount path (`/run/media` → `["run", "media"]`).
    pub(crate) fn mount_components(&self) -> impl Iterator<Item = &[u8]> {
        self.mount_path
            .split(|&b| b == b'/')
            .filter(|c| !c.is_empty())
    }
}

/// Directory records, indexed by directory id.
#[derive(Clone, Debug)]
pub(crate) struct DirStore {
    pub(crate) entry: Column<u32>,
    pub(crate) size: Column<u64>,
    pub(crate) mtime_ns: Column<i64>,
    pub(crate) ctime_ns: Column<i64>,
    pub(crate) dir_key: Column<u32>,
    pub(crate) files: Column<u32>,
    pub(crate) folders: Column<u32>,
    pub(crate) restricted: Column<u32>,
}

impl DirStore {
    fn new() -> Self {
        Self {
            entry: Column::new(PAGE_SIZE),
            size: Column::new(PAGE_SIZE),
            mtime_ns: Column::new(PAGE_SIZE),
            ctime_ns: Column::new(PAGE_SIZE),
            dir_key: Column::new(PAGE_SIZE),
            files: Column::new(PAGE_SIZE),
            folders: Column::new(PAGE_SIZE),
            restricted: Column::new(PAGE_SIZE),
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.entry.page_count() * PAGE_SIZE
    }

    pub(crate) fn push_page(&mut self) {
        self.entry.push_page(NONE);
        self.size.push_page(0);
        self.mtime_ns.push_page(i64::MIN);
        self.ctime_ns.push_page(i64::MIN);
        self.dir_key.push_page(0);
        self.files.push_page(0);
        self.folders.push_page(0);
        self.restricted.push_page(0);
    }
}

/// Sparse side tables for values that do not fit the compact columns.
#[derive(Clone, Debug, Default)]
pub(crate) struct SideTables {
    /// File sizes ≥ `SIZE_ESCAPE`.
    pub(crate) big_size: Arc<HashMap<u32, u64>>,
    /// Out-of-range times: (entry, field index) → nanoseconds.
    pub(crate) odd_time: Arc<HashMap<(u32, u8), i64>>,
    /// Owners that did not fit the owner table.
    pub(crate) odd_owner: Arc<HashMap<u32, (u32, u32)>>,
}

/// All column data of one index state. Cloning is cheap (page tables of `Arc`s).
#[derive(Clone, Debug)]
pub(crate) struct IndexData {
    pub(crate) names: Vec<Arc<NamePage>>,
    pub(crate) parent: Column<u32>,
    /// Files: size (or `SIZE_ESCAPE`); directories: directory id.
    pub(crate) size: Column<u32>,
    pub(crate) mtime: Column<u32>,
    pub(crate) ctime: Column<u32>,
    pub(crate) btime: Column<u32>,
    pub(crate) atime: Option<Column<u32>>,
    pub(crate) name_key: Column<u32>,
    pub(crate) mode: Column<u16>,
    pub(crate) owner: Column<u16>,
    pub(crate) inode: Column<u64>,
    pub(crate) flags: Column<u8>,
    pub(crate) attrs: Column<u8>,
    pub(crate) dirs: DirStore,
    pub(crate) side: SideTables,
    pub(crate) owners: Arc<Vec<(u32, u32)>>,
    pub(crate) volumes: Arc<Vec<Volume>>,
    /// Volume of each page.
    pub(crate) page_volume: Arc<Vec<u16>>,
    /// Number of live entries.
    pub(crate) live: usize,
}

pub(crate) fn time_index(field: TimeField) -> u8 {
    match field {
        TimeField::Modified => 0,
        TimeField::Changed => 1,
        TimeField::Created => 2,
        TimeField::Accessed => 3,
    }
}

impl IndexData {
    pub(crate) fn new(track_atime: bool) -> Self {
        Self {
            names: Vec::new(),
            parent: Column::new(PAGE_SIZE),
            size: Column::new(PAGE_SIZE),
            mtime: Column::new(PAGE_SIZE),
            ctime: Column::new(PAGE_SIZE),
            btime: Column::new(PAGE_SIZE),
            atime: track_atime.then(|| Column::new(PAGE_SIZE)),
            name_key: Column::new(PAGE_SIZE),
            mode: Column::new(PAGE_SIZE),
            owner: Column::new(PAGE_SIZE),
            inode: Column::new(PAGE_SIZE),
            flags: Column::new(PAGE_SIZE),
            attrs: Column::new(PAGE_SIZE),
            dirs: DirStore::new(),
            side: SideTables::default(),
            owners: Arc::new(Vec::new()),
            volumes: Arc::new(Vec::new()),
            page_volume: Arc::new(Vec::new()),
            live: 0,
        }
    }

    pub(crate) fn page_count(&self) -> usize {
        self.flags.page_count()
    }

    /// Number of slots (live or free).
    pub(crate) fn capacity(&self) -> usize {
        self.page_count() * PAGE_SIZE
    }

    pub(crate) fn push_page(&mut self, volume: VolumeId) {
        self.names.push(Arc::new(NamePage::empty()));
        self.parent.push_page(NONE);
        self.size.push_page(0);
        self.mtime.push_page(T_UNKNOWN);
        self.ctime.push_page(T_UNKNOWN);
        self.btime.push_page(T_UNKNOWN);
        if let Some(a) = self.atime.as_mut() {
            a.push_page(T_UNKNOWN);
        }
        self.name_key.push_page(0);
        self.mode.push_page(0);
        self.owner.push_page(0);
        self.inode.push_page(0);
        self.flags.push_page(flags::FREE);
        self.attrs.push_page(0);
        Arc::make_mut(&mut self.page_volume).push(volume.0);
    }

    #[inline]
    pub(crate) fn flags_of(&self, id: u32) -> u8 {
        if (id as usize) < self.capacity() {
            self.flags.get(id as usize)
        } else {
            flags::FREE
        }
    }

    #[inline]
    pub(crate) fn is_live(&self, id: u32) -> bool {
        self.flags_of(id) & flags::FREE == 0
    }

    #[inline]
    pub(crate) fn mode_of(&self, id: u32) -> u16 {
        self.mode.get(id as usize)
    }

    #[inline]
    pub(crate) fn kind_of(&self, id: u32) -> FileKind {
        FileKind::from_mode(u32::from(self.mode_of(id)))
    }

    #[inline]
    pub(crate) fn is_dir(&self, id: u32) -> bool {
        self.kind_of(id) == FileKind::Dir
    }

    #[inline]
    pub(crate) fn parent_of(&self, id: u32) -> Option<u32> {
        let p = self.parent.get(id as usize);
        (p != NONE).then_some(p)
    }

    /// Directory record id of a directory entry.
    #[inline]
    pub(crate) fn dir_of(&self, id: u32) -> Option<u32> {
        self.is_dir(id).then(|| self.size.get(id as usize))
    }

    #[inline]
    pub(crate) fn dir_key_of_dir(&self, dir: u32) -> u32 {
        self.dirs.dir_key.get(dir as usize)
    }

    /// `dir_key` of an entry’s parent directory (0 for volume roots).
    #[inline]
    pub(crate) fn parent_dir_key(&self, id: u32) -> u32 {
        self.parent_of(id)
            .and_then(|p| self.dir_of(p))
            .map_or(0, |d| self.dir_key_of_dir(d))
    }

    /// Size: file size, or folder size (sum of descendant file sizes) for directories.
    pub(crate) fn size_of(&self, id: u32) -> u64 {
        let raw = self.size.get(id as usize);
        if self.is_dir(id) {
            return self.dirs.size.get(raw as usize);
        }
        if raw == SIZE_ESCAPE {
            return self
                .side
                .big_size
                .get(&id)
                .copied()
                .unwrap_or(u64::from(raw));
        }
        u64::from(raw)
    }

    pub(crate) fn time_of(&self, id: u32, field: TimeField) -> Option<Timestamp> {
        if self.is_dir(id) {
            let d = self.size.get(id as usize) as usize;
            let ns = match field {
                TimeField::Modified => Some(self.dirs.mtime_ns.get(d)),
                TimeField::Changed => Some(self.dirs.ctime_ns.get(d)),
                _ => None,
            };
            if let Some(ns) = ns {
                return (ns != i64::MIN).then(|| Timestamp::from_nanos(ns));
            }
        }
        let col = match field {
            TimeField::Modified => &self.mtime,
            TimeField::Changed => &self.ctime,
            TimeField::Created => &self.btime,
            TimeField::Accessed => self.atime.as_ref()?,
        };
        match col.get(id as usize) {
            T_UNKNOWN => None,
            T_ESCAPE => self
                .side
                .odd_time
                .get(&(id, time_index(field)))
                .map(|&ns| Timestamp::from_nanos(ns)),
            secs => Some(Timestamp::from_secs(i64::from(secs))),
        }
    }

    /// Seconds since the epoch (for sorting and comparisons); `None` when unknown.
    #[inline]
    pub(crate) fn time_secs(&self, id: u32, field: TimeField) -> Option<i64> {
        if !self.is_dir(id) {
            let col = match field {
                TimeField::Modified => &self.mtime,
                TimeField::Changed => &self.ctime,
                TimeField::Created => &self.btime,
                TimeField::Accessed => self.atime.as_ref()?,
            };
            match col.get(id as usize) {
                T_UNKNOWN => return None,
                T_ESCAPE => {}
                secs => return Some(i64::from(secs)),
            }
        }
        self.time_of(id, field).map(|t| t.secs)
    }

    pub(crate) fn owner_of(&self, id: u32) -> (u32, u32) {
        let o = self.owner.get(id as usize);
        if o == OWNER_ESCAPE {
            return self.side.odd_owner.get(&id).copied().unwrap_or((0, 0));
        }
        self.owners.get(o as usize).copied().unwrap_or((0, 0))
    }

    pub(crate) fn volume_of(&self, id: u32) -> Option<&Volume> {
        let page = (id >> PAGE_BITS) as usize;
        let v = *self.page_volume.get(page)?;
        self.volumes.get(v as usize)
    }

    /// Published name of an entry.
    #[inline]
    pub(crate) fn name_of(&self, id: u32) -> &[u8] {
        let e = EntryId(id);
        self.names.get(e.page()).map_or(&[], |p| p.name(e.slot()))
    }

    /// Chain of ids from the volume root down to `id` (inclusive).
    pub(crate) fn chain(&self, id: u32) -> Vec<u32> {
        let mut chain = Vec::with_capacity(16);
        let mut cur = Some(id);
        while let Some(c) = cur {
            chain.push(c);
            if chain.len() > MAX_DEPTH {
                break;
            }
            cur = self.parent_of(c);
        }
        chain.reverse();
        chain
    }

    /// Full path of `id` using `name` to look up names.
    pub(crate) fn path_with<'a>(&'a self, id: u32, name: impl Fn(u32) -> &'a [u8]) -> Vec<u8> {
        let chain = self.chain(id);
        let mut out = Vec::with_capacity(64);
        let root = chain[0];
        match self.volume_of(root) {
            Some(v) => out.extend_from_slice(&v.mount_path),
            None => out.push(b'/'),
        }
        for &c in &chain[1..] {
            if out.last() != Some(&b'/') {
                out.push(b'/');
            }
            out.extend_from_slice(name(c));
        }
        out
    }
}

/// Inserted, removed and modified entries between two generations.
#[derive(Clone, Debug, Default)]
pub struct ChangeSet {
    pub inserted: Vec<EntryId>,
    pub removed: Vec<EntryId>,
    pub modified: Vec<EntryId>,
}

impl ChangeSet {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inserted.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inserted.len() + self.removed.len() + self.modified.len()
    }
}

/// The always-maintained presorted orders.
#[derive(Clone, Debug, Default)]
pub struct Orders {
    pub name: Order,
    pub path: Order,
    pub size: Order,
    pub mtime: Order,
}

/// Which presorted order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderKind {
    Name,
    Path,
    Size,
    Modified,
}

impl OrderKind {
    pub const ALL: [Self; 4] = [Self::Name, Self::Path, Self::Size, Self::Modified];

    pub(crate) fn bit(self) -> u8 {
        match self {
            Self::Name => 1,
            Self::Path => 2,
            Self::Size => 4,
            Self::Modified => 8,
        }
    }
}

impl Orders {
    #[must_use]
    pub fn get(&self, kind: OrderKind) -> &Order {
        match kind {
            OrderKind::Name => &self.name,
            OrderKind::Path => &self.path,
            OrderKind::Size => &self.size,
            OrderKind::Modified => &self.mtime,
        }
    }

    /// The four orders, indexed like [`OrderKind::ALL`].
    pub(crate) fn as_array_mut(&mut self) -> [&mut Order; 4] {
        [
            &mut self.name,
            &mut self.path,
            &mut self.size,
            &mut self.mtime,
        ]
    }

    pub(crate) fn get_mut(&mut self, kind: OrderKind) -> &mut Order {
        match kind {
            OrderKind::Name => &mut self.name,
            OrderKind::Path => &mut self.path,
            OrderKind::Size => &mut self.size,
            OrderKind::Modified => &mut self.mtime,
        }
    }
}

/// Biases a signed value so that unsigned comparison matches signed order.
#[inline]
pub(crate) fn bias(v: i64) -> u64 {
    (v as u64) ^ (1 << 63)
}

/// Sort key of `id` in the given order.
#[inline]
pub(crate) fn order_key(d: &IndexData, kind: OrderKind, id: u32) -> SortKey {
    let nk = u64::from(d.name_key.get(id as usize));
    match kind {
        OrderKind::Name => {
            if d.flags_of(id) & flags::VOLUME_ROOT != 0 {
                let own = d.dir_of(id).map_or(0, |x| d.dir_key_of_dir(x));
                (nk, u64::from(own))
            } else {
                (nk, u64::from(d.parent_dir_key(id)))
            }
        }
        OrderKind::Path => {
            if d.flags_of(id) & flags::VOLUME_ROOT != 0 {
                let own = d.dir_of(id).map_or(0, |x| d.dir_key_of_dir(x));
                (u64::from(own), 0)
            } else {
                (u64::from(d.parent_dir_key(id)), nk)
            }
        }
        OrderKind::Size => (d.size_of(id), nk),
        OrderKind::Modified => (d.time_secs(id, TimeField::Modified).map_or(0, bias), nk),
    }
}

/// One published, immutable state of the index. Searches run against a generation and never
/// block the writer.
pub struct Generation {
    pub(crate) data: IndexData,
    pub(crate) orders: Orders,
    pub(crate) number: u64,
    pub(crate) changes: Arc<ChangeSet>,
}

impl fmt::Debug for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Generation")
            .field("number", &self.number)
            .field("live", &self.data.live)
            .finish_non_exhaustive()
    }
}

impl Generation {
    pub(crate) fn empty(track_atime: bool) -> Self {
        Self {
            data: IndexData::new(track_atime),
            orders: Orders::default(),
            number: 0,
            changes: Arc::default(),
        }
    }

    /// Monotonic generation number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }

    /// Number of live entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.live
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.live == 0
    }

    /// Changes relative to the previous generation.
    #[must_use]
    pub fn changes(&self) -> &ChangeSet {
        &self.changes
    }

    #[must_use]
    pub fn orders(&self) -> &Orders {
        &self.orders
    }

    #[must_use]
    pub fn volumes(&self) -> &[Volume] {
        &self.data.volumes
    }

    /// Number of id slots (live or free); bitmaps over ids need this many bits.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    #[must_use]
    pub fn is_live(&self, id: EntryId) -> bool {
        self.data.is_live(id.0)
    }

    #[must_use]
    pub fn name(&self, id: EntryId) -> &[u8] {
        self.data.name_of(id.0)
    }

    /// Full path as raw bytes.
    #[must_use]
    pub fn path(&self, id: EntryId) -> Vec<u8> {
        self.data.path_with(id.0, |c| self.data.name_of(c))
    }

    /// Path of the containing folder (the “Path” column).
    #[must_use]
    pub fn parent_path(&self, id: EntryId) -> Vec<u8> {
        match self.data.parent_of(id.0) {
            Some(p) => self.path(EntryId(p)),
            None => {
                let full = self.path(id);
                match full.iter().rposition(|&b| b == b'/') {
                    Some(0) | None => b"/".to_vec(),
                    Some(i) => full[..i].to_vec(),
                }
            }
        }
    }

    #[must_use]
    pub fn parent(&self, id: EntryId) -> Option<EntryId> {
        self.data.parent_of(id.0).map(EntryId)
    }

    #[must_use]
    pub fn kind(&self, id: EntryId) -> FileKind {
        self.data.kind_of(id.0)
    }

    /// `st_mode` (type and permission bits).
    #[must_use]
    pub fn mode(&self, id: EntryId) -> u32 {
        u32::from(self.data.mode_of(id.0))
    }

    /// File size, or folder size for directories.
    #[must_use]
    pub fn size(&self, id: EntryId) -> u64 {
        self.data.size_of(id.0)
    }

    #[must_use]
    pub fn time(&self, id: EntryId, field: TimeField) -> Option<Timestamp> {
        self.data.time_of(id.0, field)
    }

    /// `(uid, gid)`.
    #[must_use]
    pub fn owner(&self, id: EntryId) -> (u32, u32) {
        self.data.owner_of(id.0)
    }

    #[must_use]
    pub fn inode(&self, id: EntryId) -> u64 {
        self.data.inode.get(id.index())
    }

    #[must_use]
    pub fn attrs(&self, id: EntryId) -> Attrs {
        Attrs(self.data.attrs.get(id.index()))
    }

    #[must_use]
    pub fn flags(&self, id: EntryId) -> u8 {
        self.data.flags_of(id.0)
    }

    #[must_use]
    pub fn volume(&self, id: EntryId) -> Option<&Volume> {
        self.data.volume_of(id.0)
    }

    /// Direct child counts `(files, folders)` of a directory.
    #[must_use]
    pub fn child_counts(&self, id: EntryId) -> Option<(u32, u32)> {
        let d = self.data.dir_of(id.0)? as usize;
        Some((self.data.dirs.files.get(d), self.data.dirs.folders.get(d)))
    }

    /// Number of directories in the subtree (including itself) that “others” cannot list.
    #[must_use]
    pub fn restricted_count(&self, id: EntryId) -> Option<u32> {
        let d = self.data.dir_of(id.0)? as usize;
        Some(self.data.dirs.restricted.get(d))
    }

    /// Depth: number of path components (`/` = 0, `/home` = 1).
    #[must_use]
    pub fn depth(&self, id: EntryId) -> usize {
        let chain_len = self.data.chain(id.0).len();
        let mount = self.volume(id).map_or(0, |v| v.mount_components().count());
        mount + chain_len - 1
    }

    /// Iterator over all live entry ids.
    pub fn iter(&self) -> impl Iterator<Item = EntryId> + '_ {
        (0..self.data.capacity())
            .filter(|&i| self.data.flags.get(i) & flags::FREE == 0)
            .map(|i| EntryId(u32::try_from(i).unwrap_or(NONE)))
    }

    /// Sort key of an entry in one of the presorted orders.
    #[must_use]
    pub fn order_key(&self, kind: OrderKind, id: EntryId) -> SortKey {
        order_key(&self.data, kind, id.0)
    }
}

/// Approximate memory used by one generation, by component (bytes).
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryStats {
    pub names: usize,
    pub columns: usize,
    pub dirs: usize,
    pub orders: usize,
    pub side_tables: usize,
}

impl MemoryStats {
    #[must_use]
    pub fn total(&self) -> usize {
        self.names + self.columns + self.dirs + self.orders + self.side_tables
    }
}

fn column_bytes<T: crate::buf::Pod>(c: &Column<T>) -> usize {
    c.pages()
        .iter()
        .map(|p| p.len() * std::mem::size_of::<T>())
        .sum()
}

impl Generation {
    /// Approximate heap (or mapped) bytes held by this generation.
    #[must_use]
    pub fn memory_stats(&self) -> MemoryStats {
        let d = &self.data;
        let names = d
            .names
            .iter()
            .map(|p| p.arena().len() + p.offsets().len() * 4)
            .sum();
        let mut columns = column_bytes(&d.parent)
            + column_bytes(&d.size)
            + column_bytes(&d.mtime)
            + column_bytes(&d.ctime)
            + column_bytes(&d.btime)
            + column_bytes(&d.name_key)
            + column_bytes(&d.mode)
            + column_bytes(&d.owner)
            + column_bytes(&d.inode)
            + column_bytes(&d.flags)
            + column_bytes(&d.attrs);
        if let Some(a) = &d.atime {
            columns += column_bytes(a);
        }
        let dirs = column_bytes(&d.dirs.entry)
            + column_bytes(&d.dirs.size)
            + column_bytes(&d.dirs.mtime_ns)
            + column_bytes(&d.dirs.ctime_ns)
            + column_bytes(&d.dirs.dir_key)
            + column_bytes(&d.dirs.files)
            + column_bytes(&d.dirs.folders)
            + column_bytes(&d.dirs.restricted);
        let orders = OrderKind::ALL
            .iter()
            .map(|&k| {
                self.orders
                    .get(k)
                    .blocks()
                    .iter()
                    .map(|b| b.len() * 4)
                    .sum::<usize>()
            })
            .sum();
        let side_tables =
            (d.side.big_size.len() * 16 + d.side.odd_time.len() * 24 + d.side.odd_owner.len() * 16)
                * 2
                + d.owners.len() * 8;
        MemoryStats {
            names,
            columns,
            dirs,
            orders,
            side_tables,
        }
    }
}

/// Shared, lock-free handle to the latest published generation.
#[derive(Clone)]
pub struct IndexHandle {
    pub(crate) current: Arc<ArcSwap<Generation>>,
}

impl fmt::Debug for IndexHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndexHandle")
            .field("generation", &self.current.load().number)
            .finish()
    }
}

impl IndexHandle {
    /// The latest published generation.
    #[must_use]
    pub fn load(&self) -> Arc<Generation> {
        self.current.load_full()
    }
}

#[cfg(test)]
mod tests;
