//! The single-threaded index writer: applies mutations to a private copy of the index and
//! publishes immutable generations.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use arc_swap::ArcSwap;
use rayon::prelude::*;

use super::children::ChildList;
use super::names::NamePage;
use super::order::{Order, SortKey};
use super::{
    ChangeSet, EntryId, Generation, IndexData, IndexHandle, MAX_DEPTH, NONE, OWNER_ESCAPE,
    OrderKind, Orders, PAGE_SIZE, SIZE_ESCAPE, T_ESCAPE, T_UNKNOWN, TimeField, Volume, VolumeId,
    flags, order_key, time_index,
};
use crate::fold;
use crate::meta::{EntryMeta, FileKind, Timestamp};
use crate::name::is_valid_entry_name;
use crate::util::{Bitmap, IdMap, IdSet};

/// Labels are spread over `LABEL_LO..=LABEL_HI`; 0 is reserved (path-order position of volume
/// roots) and `u32::MAX` is the open upper bound.
const LABEL_LO: u32 = 1;
const LABEL_HI: u32 = u32::MAX - 1;

const ALL_ORDERS: u8 = 0b1111;

/// Index configuration.
#[derive(Clone, Copy, Debug, Default)]
pub struct IndexConfig {
    /// Keep an access-time column (off by default, like Everything).
    pub track_atime: bool,
}

/// Errors from index mutations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IndexError {
    #[error("entry not found")]
    NotFound,
    #[error("parent is not a directory")]
    NotADirectory,
    #[error("invalid entry name")]
    InvalidName,
    #[error("an entry with that name already exists")]
    Exists(EntryId),
    #[error("cannot move a directory into its own subtree")]
    WouldCycle,
    #[error("cannot change an entry between directory and non-directory; remove and re-insert")]
    KindChange,
    #[error("volume roots cannot be moved or removed this way")]
    VolumeRoot,
    #[error("index is full")]
    Full,
}

/// Names of a page created in the current batch, filled in slot order.
#[derive(Debug, Default)]
struct FreshPage {
    bytes: Vec<u8>,
    offs: Vec<u32>,
}

impl FreshPage {
    fn name(&self, slot: usize) -> &[u8] {
        let start = self.offs[slot] as usize;
        let end = self
            .offs
            .get(slot + 1)
            .map_or(self.bytes.len(), |&o| o as usize);
        &self.bytes[start..end]
    }
}

/// Unpublished name changes of one page.
#[derive(Debug, Default)]
struct PendingPage {
    fresh: Option<FreshPage>,
    overlay: BTreeMap<u16, Box<[u8]>>,
}

#[derive(Clone, Copy, Debug)]
struct Dirty {
    /// Keys the entry had in the published orders (`None` if it was created in this batch).
    old: Option<[SortKey; 4]>,
    /// Which orders need a remove + re-insert.
    mask: u8,
}

#[derive(Debug, Default)]
struct Batch {
    dirty: IdMap<u32, Dirty>,
    touched: IdSet<u32>,
    needs_dir_key: HashSet<u32>,
    freed: Vec<u32>,
    freed_dirs: Vec<u32>,
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.dirty.is_empty()
            && self.touched.is_empty()
            && self.needs_dir_key.is_empty()
            && self.freed.is_empty()
    }
}

/// Owns the mutable index state. Not `Sync`: use it from one thread and share [`IndexHandle`]s
/// with readers.
pub struct IndexWriter {
    data: IndexData,
    orders: Orders,
    pending: HashMap<u32, PendingPage>,
    children: Vec<ChildList>,
    free_slots: Vec<Vec<u32>>,
    /// Per volume: the page being filled sequentially and its next slot.
    open_page: Vec<Option<(u32, u32)>>,
    free_dirs: Vec<u32>,
    next_dir: u32,
    owner_ids: HashMap<(u32, u32), u16>,
    /// Directory ids sorted by `dir_key` (depth-first path order).
    dir_seq: Vec<u32>,
    batch: Batch,
    handle: IndexHandle,
    number: u64,
    config: IndexConfig,
    last_publish: PublishStats,
    /// Set when labels of entries already in the orders changed (fences must be refreshed).
    relabeled: bool,
}

/// Per-phase cost of one publish: (duration, bytes copied) for remove-from-orders, names,
/// labels, re-insert/rebuild orders, finish.
#[derive(Clone, Copy, Debug, Default)]
pub struct PublishStats {
    pub dirty: usize,
    pub rebuilt_orders: usize,
    pub phases: [(std::time::Duration, u64); 5],
}

struct Lap {
    t: std::time::Instant,
    c: u64,
}

impl Lap {
    fn start() -> Self {
        Self {
            t: std::time::Instant::now(),
            c: crate::buf::cow_bytes_copied(),
        }
    }

    fn next(&mut self) -> (std::time::Duration, u64) {
        let now = Self::start();
        let r = (now.t - self.t, now.c - self.c);
        *self = now;
        r
    }
}

impl std::fmt::Debug for IndexWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexWriter")
            .field("live", &self.data.live)
            .field("number", &self.number)
            .finish_non_exhaustive()
    }
}

/// Name of `id` as the writer sees it (including unpublished changes).
fn wname<'a>(data: &'a IndexData, pending: &'a HashMap<u32, PendingPage>, id: u32) -> &'a [u8] {
    let e = EntryId(id);
    #[allow(clippy::cast_possible_truncation)]
    if let Some(p) = pending.get(&(e.page() as u32)) {
        if let Some(n) = p.overlay.get(&(e.slot() as u16)) {
            return n;
        }
        if let Some(f) = &p.fresh {
            return if e.slot() < f.offs.len() {
                f.name(e.slot())
            } else {
                &[]
            };
        }
    }
    data.name_of(id)
}

fn entry_flags(name: &[u8], root: bool) -> u8 {
    let mut f = 0;
    if !name.is_ascii() {
        f |= flags::NON_ASCII;
        if fold::has_fold_special(name) {
            f |= flags::FOLD_SPECIAL;
        }
    }
    if name.first() == Some(&b'.') {
        f |= flags::HIDDEN;
    }
    if root {
        f |= flags::VOLUME_ROOT;
    }
    f
}

/// A directory counts as restricted when “others” cannot list it (by its mode bits).
fn is_restricted(mode: u32) -> bool {
    mode & 0o005 != 0o005
}

/// Restricted by mode bits, or carrying an ACL (which may lock individual users out).
fn self_restricted(mode: u32, entry_flags: u8) -> bool {
    is_restricted(mode) || entry_flags & flags::HAS_ACL != 0
}

/// Evenly spreads `m` labels strictly between `lo` and `hi`, or `None` if they do not fit.
fn spread(lo: u32, hi: u32, m: usize) -> Option<Vec<u32>> {
    let room = u64::from(hi)
        .saturating_sub(u64::from(lo))
        .saturating_sub(1);
    if (m as u64) > room || m == 0 {
        return (m == 0).then(Vec::new);
    }
    let step = (u64::from(hi) - u64::from(lo)) / (m as u64 + 1);
    Some(
        (1..=m as u64)
            .map(|i| u32::try_from(u64::from(lo) + i * step.max(1)).unwrap_or(LABEL_HI))
            .collect(),
    )
}

impl IndexWriter {
    /// Creates an empty index.
    #[must_use]
    pub fn new(config: IndexConfig) -> Self {
        let empty = Arc::new(Generation::empty(config.track_atime));
        Self {
            data: IndexData::new(config.track_atime),
            orders: Orders::default(),
            pending: HashMap::new(),
            children: Vec::new(),
            free_slots: Vec::new(),
            open_page: Vec::new(),
            free_dirs: Vec::new(),
            next_dir: 0,
            owner_ids: HashMap::new(),
            dir_seq: Vec::new(),
            batch: Batch::default(),
            handle: IndexHandle {
                current: Arc::new(ArcSwap::new(empty)),
            },
            number: 0,
            config,
            last_publish: PublishStats::default(),
            relabeled: false,
        }
    }

    /// Resumes writing on top of a loaded generation (rebuilds writer-private structures).
    #[must_use]
    pub fn from_generation(generation: &Generation, config: IndexConfig) -> Self {
        let mut w = Self::new(config);
        w.data = generation.data.clone();
        w.orders = generation.orders.clone();
        w.number = generation.number;
        w.handle = IndexHandle {
            current: Arc::new(ArcSwap::from(Arc::new(Generation {
                data: generation.data.clone(),
                orders: generation.orders.clone(),
                number: generation.number,
                changes: Arc::default(),
            }))),
        };
        w.rebuild_private();
        w
    }

    fn rebuild_private(&mut self) {
        let vols = self.data.volumes.len();
        self.free_slots = vec![Vec::new(); vols];
        self.open_page = vec![None; vols];
        self.owner_ids = self
            .data
            .owners
            .iter()
            .enumerate()
            .filter_map(|(i, &o)| u16::try_from(i).ok().map(|i| (o, i)))
            .collect();
        let dir_cap = self.data.dirs.capacity();
        self.children = vec![ChildList::default(); dir_cap];
        let mut used_dirs = vec![false; dir_cap];
        let mut max_dir = 0u32;
        for id in 0..u32::try_from(self.data.capacity()).unwrap_or(0) {
            let f = self.data.flags_of(id);
            let page = (id as usize) / PAGE_SIZE;
            let vol = self.data.page_volume.get(page).copied().unwrap_or(0) as usize;
            if f & flags::FREE != 0 {
                if let Some(list) = self.free_slots.get_mut(vol) {
                    list.push(id);
                }
                continue;
            }
            if let Some(d) = self.data.dir_of(id) {
                if let Some(u) = used_dirs.get_mut(d as usize) {
                    *u = true;
                }
                max_dir = max_dir.max(d + 1);
            }
            if let Some(p) = self.data.parent_of(id)
                && let Some(pd) = self.data.dir_of(p)
            {
                let name = self.data.name_of(id);
                if let Some(list) = self.children.get_mut(pd as usize) {
                    list.insert(id, name);
                }
            }
        }
        for list in &mut self.free_slots {
            list.reverse();
        }
        self.next_dir = max_dir;
        self.free_dirs = (0..max_dir).filter(|&d| !used_dirs[d as usize]).collect();
        let mut seq: Vec<u32> = (0..max_dir).filter(|&d| used_dirs[d as usize]).collect();
        seq.sort_unstable_by_key(|&d| self.data.dir_key_of_dir(d));
        self.dir_seq = seq;
    }

    /// The configuration this index was created with.
    #[must_use]
    pub fn config(&self) -> IndexConfig {
        self.config
    }

    /// A handle readers use to get the latest generation.
    #[must_use]
    pub fn handle(&self) -> IndexHandle {
        self.handle.clone()
    }

    /// Number of live entries (including unpublished changes).
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.live
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.live == 0
    }

    /// True if there are unpublished changes.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.batch.is_empty()
    }

    // ----- reads (writer view, including unpublished changes) -----

    #[must_use]
    pub fn name(&self, id: EntryId) -> &[u8] {
        wname(&self.data, &self.pending, id.0)
    }

    #[must_use]
    pub fn is_live(&self, id: EntryId) -> bool {
        self.data.is_live(id.0)
    }

    #[must_use]
    pub fn kind(&self, id: EntryId) -> FileKind {
        self.data.kind_of(id.0)
    }

    #[must_use]
    pub fn parent(&self, id: EntryId) -> Option<EntryId> {
        self.data.parent_of(id.0).map(EntryId)
    }

    /// File size, or folder size for directories.
    #[must_use]
    pub fn size(&self, id: EntryId) -> u64 {
        self.data.size_of(id.0)
    }

    #[must_use]
    pub fn path(&self, id: EntryId) -> Vec<u8> {
        self.data
            .path_with(id.0, |c| wname(&self.data, &self.pending, c))
    }

    #[must_use]
    pub fn volumes(&self) -> &[Volume] {
        &self.data.volumes
    }

    /// Reconstructs the stored metadata of an entry.
    #[must_use]
    pub fn meta(&self, id: EntryId) -> Option<EntryMeta> {
        if !self.data.is_live(id.0) {
            return None;
        }
        let (uid, gid) = self.data.owner_of(id.0);
        let size = if self.data.is_dir(id.0) {
            0
        } else {
            self.data.size_of(id.0)
        };
        Some(EntryMeta {
            mode: u32::from(self.data.mode_of(id.0)),
            uid,
            gid,
            size,
            mtime: self.data.time_of(id.0, TimeField::Modified),
            ctime: self.data.time_of(id.0, TimeField::Changed),
            btime: self.data.time_of(id.0, TimeField::Created),
            atime: self.data.time_of(id.0, TimeField::Accessed),
            inode: self.data.inode.get(id.index()),
            attrs: crate::meta::Attrs(self.data.attrs.get(id.index())),
            nlink: 0,
        })
    }

    /// The modification and change times (ns) a directory had when it was last listed
    /// completely, which rescans compare with the filesystem to decide whether to read it again.
    /// Independent of the displayed times, which every metadata update changes.
    #[must_use]
    pub fn dir_times(&self, id: EntryId) -> Option<(i64, i64)> {
        let d = self.data.dir_of(id.0)? as usize;
        let (m, c) = (
            self.data.dirs.listed_m.get(d),
            self.data.dirs.listed_c.get(d),
        );
        (m != i64::MIN && c != i64::MIN).then_some((m, c))
    }

    /// Records a directory’s modification and change times (`None`: unknown, so the next
    /// pruned rescan reads it again). Scanners set them from the times observed just before
    /// reading the directory, so they never claim more than the listing saw.
    pub fn set_dir_times(
        &mut self,
        id: EntryId,
        times: Option<(i64, i64)>,
    ) -> Result<(), IndexError> {
        let d = self.dir_record(id.0)? as usize;
        let (m, c) = times.unwrap_or((i64::MIN, i64::MIN));
        self.data.dirs.listed_m.set(d, m);
        self.data.dirs.listed_c.set(d, c);
        // Not visible to searches, but it must reach the next published generation (and so
        // the next snapshot).
        self.batch.touched.insert(id.0);
        Ok(())
    }

    /// Live non-directory entries of `volume` with this inode number (all names of a hard
    /// linked file). Scans the volume’s inode column, so use it sparingly.
    #[must_use]
    pub fn entries_with_inode(&self, volume: VolumeId, inode: u64) -> Vec<EntryId> {
        let mut out = Vec::new();
        for (page, &v) in self.data.page_volume.iter().enumerate() {
            if v != volume.0 {
                continue;
            }
            for (slot, &ino) in self.data.inode.page(page).iter().enumerate() {
                if ino != inode {
                    continue;
                }
                let id = u32::try_from(page * PAGE_SIZE + slot).unwrap_or(NONE);
                if self.data.is_live(id) && !self.data.is_dir(id) {
                    out.push(EntryId(id));
                }
            }
        }
        out
    }

    /// Finds the child of `dir` named `name`.
    #[must_use]
    pub fn lookup(&self, dir: EntryId, name: &[u8]) -> Option<EntryId> {
        let d = self.data.dir_of(dir.0)?;
        let name_of = |id: u32| wname(&self.data, &self.pending, id);
        self.children
            .get(d as usize)?
            .find(name, &name_of)
            .map(EntryId)
    }

    /// Children of a directory (unordered).
    #[must_use]
    pub fn children(&self, dir: EntryId) -> Vec<EntryId> {
        self.data
            .dir_of(dir.0)
            .and_then(|d| self.children.get(d as usize))
            .map(|l| l.ids().into_iter().map(EntryId).collect())
            .unwrap_or_default()
    }

    /// Approximate bytes used by writer-private structures.
    #[must_use]
    pub fn private_heap_bytes(&self) -> usize {
        self.children
            .iter()
            .map(ChildList::heap_bytes)
            .sum::<usize>()
            + self.children.capacity() * std::mem::size_of::<ChildList>()
            + self.dir_seq.capacity() * 4
            + self
                .free_slots
                .iter()
                .map(|v| v.capacity() * 4)
                .sum::<usize>()
    }

    // ----- volumes -----

    /// Adds a volume and its root directory entry.
    pub fn add_volume(
        &mut self,
        key: &str,
        mount_path: &[u8],
        fstype: &str,
        label: Option<&str>,
        root_meta: &EntryMeta,
    ) -> Result<(VolumeId, EntryId), IndexError> {
        if root_meta.kind() != FileKind::Dir {
            return Err(IndexError::NotADirectory);
        }
        let vid = VolumeId(u16::try_from(self.data.volumes.len()).map_err(|_| IndexError::Full)?);
        Arc::make_mut(&mut self.data.volumes).push(Volume {
            id: vid,
            key: key.to_owned(),
            mount_path: mount_path.to_vec(),
            fstype: fstype.to_owned(),
            label: label.map(str::to_owned),
            root: EntryId(NONE),
            online: true,
        });
        self.free_slots.push(Vec::new());
        self.open_page.push(None);
        let name: Vec<u8> = mount_path
            .split(|&b| b == b'/')
            .rfind(|c| !c.is_empty())
            .unwrap_or_default()
            .to_vec();
        let id = self.alloc_slot(vid)?;
        self.write_new(id, NONE, &name, root_meta, true)?;
        Arc::make_mut(&mut self.data.volumes)[vid.0 as usize].root = EntryId(id);
        Ok((vid, EntryId(id)))
    }

    /// Moves a volume to a new mount path (a removable drive mounted somewhere else). Every
    /// path in the volume changes, so its path order is rebuilt.
    pub fn set_volume_mount(
        &mut self,
        volume: VolumeId,
        mount_path: &[u8],
    ) -> Result<(), IndexError> {
        let v = volume.0 as usize;
        let vol = self.data.volumes.get(v).ok_or(IndexError::NotFound)?;
        if vol.mount_path == mount_path {
            return Ok(());
        }
        let root = vol.root.0;
        for e in self.subtree(root) {
            self.mark_dirty(e, OrderKind::Name.bit() | OrderKind::Path.bit());
            if let Some(d) = self.data.dir_of(e) {
                self.batch.needs_dir_key.insert(d);
            }
        }
        Arc::make_mut(&mut self.data.volumes)[v].mount_path = mount_path.to_vec();
        let name: Vec<u8> = mount_path
            .split(|&b| b == b'/')
            .rfind(|c| !c.is_empty())
            .unwrap_or_default()
            .to_vec();
        self.set_name(root, &name);
        let keep = self.data.flags_of(root) & flags::HAS_ACL;
        self.data
            .flags
            .set(root as usize, keep | entry_flags(&name, true));
        self.batch.touched.insert(NONE);
        Ok(())
    }

    /// Finds the entry at an absolute path, starting from the volume whose mount path is the
    /// longest prefix of it.
    #[must_use]
    pub fn lookup_path(&self, path: &[u8]) -> Option<EntryId> {
        let vol = self
            .data
            .volumes
            .iter()
            .filter(|v| {
                let mp = v.mount_path.as_slice();
                mp == b"/"
                    || path == mp
                    || (path.starts_with(mp) && path.get(mp.len()) == Some(&b'/'))
            })
            .max_by_key(|v| v.mount_path.len())?;
        let rest = &path[vol.mount_path.len().min(path.len())..];
        let mut cur = vol.root;
        for comp in rest.split(|&b| b == b'/').filter(|c| !c.is_empty()) {
            cur = self.lookup(cur, comp)?;
        }
        self.is_live(cur).then_some(cur)
    }

    /// Marks a volume online or offline (offline volumes keep their entries).
    pub fn set_volume_online(&mut self, volume: VolumeId, online: bool) {
        if let Some(v) = Arc::make_mut(&mut self.data.volumes).get_mut(volume.0 as usize) {
            v.online = online;
        }
        self.batch.touched.insert(NONE);
    }

    // ----- mutations -----

    fn alloc_slot(&mut self, vol: VolumeId) -> Result<u32, IndexError> {
        let v = vol.0 as usize;
        if let Some(id) = self.free_slots.get_mut(v).and_then(Vec::pop) {
            return Ok(id);
        }
        if let Some(Some((page, next))) = self.open_page.get(v).copied()
            && (next as usize) < PAGE_SIZE
        {
            self.open_page[v] = Some((page, next + 1));
            return Ok(EntryId::from_parts(page as usize, next as usize).0);
        }
        let page = self.data.page_count();
        if page >= (1usize << (32 - super::PAGE_BITS)) - 1 {
            return Err(IndexError::Full);
        }
        self.data.push_page(vol);
        let page32 = u32::try_from(page).map_err(|_| IndexError::Full)?;
        self.pending.insert(
            page32,
            PendingPage {
                fresh: Some(FreshPage::default()),
                overlay: BTreeMap::new(),
            },
        );
        self.open_page[v] = Some((page32, 1));
        Ok(EntryId::from_parts(page, 0).0)
    }

    fn alloc_dir(&mut self) -> u32 {
        if let Some(d) = self.free_dirs.pop() {
            return d;
        }
        let d = self.next_dir;
        self.next_dir += 1;
        while self.data.dirs.capacity() <= d as usize {
            self.data.dirs.push_page();
        }
        while self.children.len() <= d as usize {
            self.children.push(ChildList::default());
        }
        d
    }

    fn set_name(&mut self, id: u32, name: &[u8]) {
        let e = EntryId(id);
        #[allow(clippy::cast_possible_truncation)]
        let (page, slot) = (e.page() as u32, e.slot() as u16);
        let pending = self.pending.entry(page).or_default();
        if let Some(fresh) = pending.fresh.as_mut()
            && usize::from(slot) == fresh.offs.len()
            && !pending.overlay.contains_key(&slot)
        {
            fresh
                .offs
                .push(u32::try_from(fresh.bytes.len()).unwrap_or(u32::MAX));
            fresh.bytes.extend_from_slice(name);
            return;
        }
        pending.overlay.insert(slot, name.into());
    }

    fn set_time(&mut self, id: u32, field: TimeField, ts: Option<Timestamp>) {
        let raw = match ts {
            None => T_UNKNOWN,
            Some(t) if t.secs >= 0 && t.secs < i64::from(T_UNKNOWN) => {
                u32::try_from(t.secs).unwrap_or(T_ESCAPE)
            }
            Some(_) => T_ESCAPE,
        };
        let key = (id, time_index(field));
        if raw == T_ESCAPE {
            if let Some(t) = ts {
                Arc::make_mut(&mut self.data.side.odd_time).insert(key, t.as_nanos());
            }
        } else if self.data.side.odd_time.contains_key(&key) {
            Arc::make_mut(&mut self.data.side.odd_time).remove(&key);
        }
        let col = match field {
            TimeField::Modified => &mut self.data.mtime,
            TimeField::Changed => &mut self.data.ctime,
            TimeField::Created => &mut self.data.btime,
            TimeField::Accessed => match self.data.atime.as_mut() {
                Some(c) => c,
                None => return,
            },
        };
        col.set(id as usize, raw);
    }

    fn set_owner(&mut self, id: u32, uid: u32, gid: u32) {
        let o = if let Some(&o) = self.owner_ids.get(&(uid, gid)) {
            o
        } else if self.data.owners.len() < usize::from(OWNER_ESCAPE) {
            let o = u16::try_from(self.data.owners.len()).unwrap_or(OWNER_ESCAPE);
            Arc::make_mut(&mut self.data.owners).push((uid, gid));
            self.owner_ids.insert((uid, gid), o);
            o
        } else {
            OWNER_ESCAPE
        };
        if o == OWNER_ESCAPE {
            Arc::make_mut(&mut self.data.side.odd_owner).insert(id, (uid, gid));
        } else if self.data.side.odd_owner.contains_key(&id) {
            Arc::make_mut(&mut self.data.side.odd_owner).remove(&id);
        }
        self.data.owner.set(id as usize, o);
    }

    fn set_file_size(&mut self, id: u32, size: u64) {
        match u32::try_from(size) {
            Ok(s) if s != SIZE_ESCAPE => {
                if self.data.side.big_size.contains_key(&id) {
                    Arc::make_mut(&mut self.data.side.big_size).remove(&id);
                }
                self.data.size.set(id as usize, s);
            }
            _ => {
                Arc::make_mut(&mut self.data.side.big_size).insert(id, size);
                self.data.size.set(id as usize, SIZE_ESCAPE);
            }
        }
    }

    /// Records the current order keys of `id` (once per batch) and marks orders to refresh.
    fn mark_dirty(&mut self, id: u32, mask: u8) {
        if let Some(d) = self.batch.dirty.get_mut(&id) {
            d.mask |= mask;
            return;
        }
        let d = &self.data;
        let old = [
            order_key(d, OrderKind::Name, id),
            order_key(d, OrderKind::Path, id),
            order_key(d, OrderKind::Size, id),
            order_key(d, OrderKind::Modified, id),
        ];
        self.batch.dirty.insert(
            id,
            Dirty {
                old: Some(old),
                mask,
            },
        );
    }

    /// Writes all columns of a freshly allocated slot and links it under `parent`.
    fn write_new(
        &mut self,
        id: u32,
        parent: u32,
        name: &[u8],
        meta: &EntryMeta,
        root: bool,
    ) -> Result<(), IndexError> {
        let idx = id as usize;
        self.set_name(id, name);
        self.data.parent.set(idx, parent);
        #[allow(clippy::cast_possible_truncation)]
        self.data.mode.set(idx, meta.mode as u16);
        self.set_owner(id, meta.uid, meta.gid);
        self.data.inode.set(idx, meta.inode);
        self.data.attrs.set(idx, meta.attrs.bits());
        self.data.flags.set(idx, entry_flags(name, root));
        self.set_time(id, TimeField::Modified, meta.mtime);
        self.set_time(id, TimeField::Changed, meta.ctime);
        self.set_time(id, TimeField::Created, meta.btime);
        self.set_time(id, TimeField::Accessed, meta.atime);
        self.data.name_key.set(idx, 0);
        let is_dir = meta.kind() == FileKind::Dir;
        if is_dir {
            let d = self.alloc_dir();
            let du = d as usize;
            self.data.size.set(idx, d);
            let dirs = &mut self.data.dirs;
            dirs.entry.set(du, id);
            dirs.size.set(du, 0);
            dirs.mtime_ns
                .set(du, meta.mtime.map_or(i64::MIN, Timestamp::as_nanos));
            dirs.ctime_ns
                .set(du, meta.ctime.map_or(i64::MIN, Timestamp::as_nanos));
            // Not listed yet.
            dirs.listed_m.set(du, i64::MIN);
            dirs.listed_c.set(du, i64::MIN);
            dirs.dir_key.set(du, 0);
            dirs.files.set(du, 0);
            dirs.folders.set(du, 0);
            dirs.restricted.set(du, u32::from(is_restricted(meta.mode)));
            self.children[du].clear();
            self.batch.needs_dir_key.insert(d);
        } else {
            self.set_file_size(id, meta.size);
        }
        self.data.live += 1;
        self.batch.dirty.insert(
            id,
            Dirty {
                old: None,
                mask: ALL_ORDERS,
            },
        );
        if parent != NONE {
            self.attach(id);
        }
        Ok(())
    }

    /// Directory record of a live directory entry, or an error.
    fn dir_record(&self, id: u32) -> Result<u32, IndexError> {
        if !self.data.is_live(id) {
            return Err(IndexError::NotFound);
        }
        self.data.dir_of(id).ok_or(IndexError::NotADirectory)
    }

    /// Adds `id`’s contribution (counts, sizes, restricted dirs) to its parent and ancestors.
    fn attach(&mut self, id: u32) {
        self.contribute(id, true);
    }

    /// Removes `id`’s contribution from its parent and ancestors and unlinks it from the parent’s
    /// child list (uses the current name).
    fn detach(&mut self, id: u32) {
        self.contribute(id, false);
    }

    fn contribute(&mut self, id: u32, add: bool) {
        let Some(parent) = self.data.parent_of(id) else {
            return;
        };
        let Some(pd) = self.data.dir_of(parent) else {
            return;
        };
        let name = wname(&self.data, &self.pending, id).to_vec();
        if add {
            self.children[pd as usize].insert(id, &name);
        } else {
            self.children[pd as usize].remove(id, &name);
        }
        let is_dir = self.data.is_dir(id);
        let counts = if is_dir {
            &mut self.data.dirs.folders
        } else {
            &mut self.data.dirs.files
        };
        let c = counts.get(pd as usize);
        counts.set(pd as usize, if add { c + 1 } else { c.saturating_sub(1) });
        let size = self.data.size_of(id);
        let restricted = self
            .data
            .dir_of(id)
            .map_or(0, |d| self.data.dirs.restricted.get(d as usize));
        self.propagate(parent, size, restricted, add);
        self.batch.touched.insert(parent);
    }

    /// Adds (or subtracts) a size and restricted-count delta to `dir_entry` and its ancestors.
    fn propagate(&mut self, dir_entry: u32, size: u64, restricted: u32, add: bool) {
        if size == 0 && restricted == 0 {
            return;
        }
        let mut cur = Some(dir_entry);
        let mut depth = 0;
        while let Some(e) = cur {
            let Some(d) = self.data.dir_of(e) else {
                break;
            };
            if size != 0 {
                self.mark_dirty(e, OrderKind::Size.bit());
                let s = self.data.dirs.size.get(d as usize);
                let ns = if add {
                    s.saturating_add(size)
                } else {
                    s.saturating_sub(size)
                };
                self.data.dirs.size.set(d as usize, ns);
            }
            if restricted != 0 {
                let r = self.data.dirs.restricted.get(d as usize);
                let nr = if add {
                    r.saturating_add(restricted)
                } else {
                    r.saturating_sub(restricted)
                };
                self.data.dirs.restricted.set(d as usize, nr);
                self.batch.touched.insert(e);
            }
            depth += 1;
            if depth > MAX_DEPTH {
                break;
            }
            cur = self.data.parent_of(e);
        }
    }

    /// Inserts a new entry named `name` under directory `parent`.
    pub fn insert(
        &mut self,
        parent: EntryId,
        name: &[u8],
        meta: &EntryMeta,
    ) -> Result<EntryId, IndexError> {
        if !is_valid_entry_name(name) {
            return Err(IndexError::InvalidName);
        }
        let pd = self.dir_record(parent.0)?;
        {
            let name_of = |id: u32| wname(&self.data, &self.pending, id);
            if let Some(existing) = self.children[pd as usize].find(name, &name_of) {
                return Err(IndexError::Exists(EntryId(existing)));
            }
        }
        let page = parent.page();
        let vol = VolumeId(self.data.page_volume.get(page).copied().unwrap_or(0));
        let id = self.alloc_slot(vol)?;
        self.write_new(id, parent.0, name, meta, false)?;
        Ok(EntryId(id))
    }

    /// Inserts or updates the child `name` of `parent`. If the existing entry changes between
    /// directory and non-directory, it is removed and re-inserted.
    pub fn upsert(
        &mut self,
        parent: EntryId,
        name: &[u8],
        meta: &EntryMeta,
    ) -> Result<EntryId, IndexError> {
        match self.lookup(parent, name) {
            Some(id) => match self.update(id, meta) {
                Ok(()) => Ok(id),
                Err(IndexError::KindChange) => {
                    self.remove(id)?;
                    self.insert(parent, name, meta)
                }
                Err(e) => Err(e),
            },
            None => self.insert(parent, name, meta),
        }
    }

    /// Updates the metadata of an entry (name and parent unchanged).
    pub fn update(&mut self, id: EntryId, meta: &EntryMeta) -> Result<(), IndexError> {
        let i = id.0;
        if !self.data.is_live(i) {
            return Err(IndexError::NotFound);
        }
        let was_dir = self.data.is_dir(i);
        if was_dir != (meta.kind() == FileKind::Dir) {
            return Err(IndexError::KindChange);
        }
        let old_mtime = self.data.time_secs(i, TimeField::Modified);
        let new_mtime = meta.mtime.map(|t| t.secs);
        if old_mtime != new_mtime {
            self.mark_dirty(i, OrderKind::Modified.bit());
        }
        self.batch.touched.insert(i);
        if was_dir {
            let d = self.data.dir_of(i).unwrap_or(0) as usize;
            let old_mode = u32::from(self.data.mode_of(i));
            let fl = self.data.flags_of(i);
            if self_restricted(old_mode, fl) != self_restricted(meta.mode, fl) {
                let add = self_restricted(meta.mode, fl);
                let r = self.data.dirs.restricted.get(d);
                self.data
                    .dirs
                    .restricted
                    .set(d, if add { r + 1 } else { r.saturating_sub(1) });
                if let Some(p) = self.data.parent_of(i) {
                    self.propagate(p, 0, 1, add);
                }
            }
            self.data
                .dirs
                .mtime_ns
                .set(d, meta.mtime.map_or(i64::MIN, Timestamp::as_nanos));
            self.data
                .dirs
                .ctime_ns
                .set(d, meta.ctime.map_or(i64::MIN, Timestamp::as_nanos));
        } else {
            let old_size = self.data.size_of(i);
            if old_size != meta.size {
                self.mark_dirty(i, OrderKind::Size.bit());
                self.set_file_size(i, meta.size);
                if let Some(p) = self.data.parent_of(i) {
                    if meta.size > old_size {
                        self.propagate(p, meta.size - old_size, 0, true);
                    } else {
                        self.propagate(p, old_size - meta.size, 0, false);
                    }
                }
            }
        }
        let idx = i as usize;
        #[allow(clippy::cast_possible_truncation)]
        self.data.mode.set(idx, meta.mode as u16);
        self.set_owner(i, meta.uid, meta.gid);
        self.data.inode.set(idx, meta.inode);
        self.data.attrs.set(idx, meta.attrs.bits());
        self.set_time(i, TimeField::Modified, meta.mtime);
        self.set_time(i, TimeField::Changed, meta.ctime);
        self.set_time(i, TimeField::Created, meta.btime);
        self.set_time(i, TimeField::Accessed, meta.atime);
        Ok(())
    }

    /// Sets or clears the access ACL of a directory. ACLs of other entries aren’t stored: the
    /// daemon checks file ACLs on the live filesystem when it needs them.
    pub fn set_acl(
        &mut self,
        id: EntryId,
        acl: Option<&[crate::perm::AclEntry]>,
    ) -> Result<(), IndexError> {
        let i = id.0;
        let d = self.dir_record(i)? as usize;
        let acl = acl.filter(|a| !a.is_empty());
        if self.data.side.acl.get(&i).map(|a| &a[..]) == acl {
            return Ok(());
        }
        let mode = u32::from(self.data.mode_of(i));
        let old_flags = self.data.flags_of(i);
        let new_flags = match acl {
            Some(a) => {
                Arc::make_mut(&mut self.data.side.acl).insert(i, a.into());
                old_flags | flags::HAS_ACL
            }
            None => {
                Arc::make_mut(&mut self.data.side.acl).remove(&i);
                old_flags & !flags::HAS_ACL
            }
        };
        self.data.flags.set(i as usize, new_flags);
        let (was, now) = (
            self_restricted(mode, old_flags),
            self_restricted(mode, new_flags),
        );
        if was != now {
            let r = self.data.dirs.restricted.get(d);
            self.data
                .dirs
                .restricted
                .set(d, if now { r + 1 } else { r.saturating_sub(1) });
            if let Some(p) = self.data.parent_of(i) {
                self.propagate(p, 0, 1, now);
            }
        }
        self.batch.touched.insert(i);
        Ok(())
    }

    /// All entries of the subtree rooted at `id` (including `id`), parents before children.
    fn subtree(&self, id: u32) -> Vec<u32> {
        let mut out = vec![id];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i];
            if let Some(d) = self.data.dir_of(cur)
                && let Some(list) = self.children.get(d as usize)
            {
                out.extend(list.ids());
            }
            i += 1;
        }
        out
    }

    /// Removes an entry and, for directories, its whole subtree. Returns the number removed.
    pub fn remove(&mut self, id: EntryId) -> Result<usize, IndexError> {
        let i = id.0;
        if !self.data.is_live(i) {
            return Err(IndexError::NotFound);
        }
        if self.data.flags_of(i) & flags::VOLUME_ROOT != 0 {
            return Err(IndexError::VolumeRoot);
        }
        // Capture order keys before anything changes (children's keys depend on their parents).
        let all = self.subtree(i);
        for &e in &all {
            if !matches!(self.batch.dirty.get(&e), Some(Dirty { old: None, .. })) {
                self.mark_dirty(e, ALL_ORDERS);
            }
        }
        self.detach(i);
        for &e in &all {
            self.free_entry(e);
        }
        Ok(all.len())
    }

    fn free_entry(&mut self, e: u32) {
        let created_now = matches!(self.batch.dirty.get(&e), Some(Dirty { old: None, .. }));
        if !created_now {
            self.mark_dirty(e, ALL_ORDERS);
        }
        if let Some(d) = self.data.dir_of(e) {
            self.children[d as usize].clear();
            self.data.dirs.entry.set(d as usize, NONE);
            self.batch.freed_dirs.push(d);
            self.batch.needs_dir_key.remove(&d);
        }
        if self.data.side.big_size.contains_key(&e) {
            Arc::make_mut(&mut self.data.side.big_size).remove(&e);
        }
        if self.data.side.odd_owner.contains_key(&e) {
            Arc::make_mut(&mut self.data.side.odd_owner).remove(&e);
        }
        if self.data.side.acl.contains_key(&e) {
            Arc::make_mut(&mut self.data.side.acl).remove(&e);
        }
        for f in 0..4u8 {
            if self.data.side.odd_time.contains_key(&(e, f)) {
                Arc::make_mut(&mut self.data.side.odd_time).remove(&(e, f));
            }
        }
        let idx = e as usize;
        self.data.flags.set(idx, flags::FREE);
        self.data.parent.set(idx, NONE);
        self.data.mode.set(idx, 0);
        self.data.size.set(idx, 0);
        self.set_name(e, &[]);
        self.data.live -= 1;
        self.batch.freed.push(e);
    }

    /// Renames and/or moves an entry. Fails if the target name exists in `new_parent`.
    pub fn rename(
        &mut self,
        id: EntryId,
        new_parent: EntryId,
        new_name: &[u8],
    ) -> Result<(), IndexError> {
        let i = id.0;
        if !self.data.is_live(i) {
            return Err(IndexError::NotFound);
        }
        if self.data.flags_of(i) & flags::VOLUME_ROOT != 0 {
            return Err(IndexError::VolumeRoot);
        }
        if !is_valid_entry_name(new_name) {
            return Err(IndexError::InvalidName);
        }
        let npd = self.dir_record(new_parent.0)?;
        let old_parent = self.data.parent_of(i).unwrap_or(NONE);
        let name_changed = wname(&self.data, &self.pending, i) != new_name;
        let parent_changed = old_parent != new_parent.0;
        if !name_changed && !parent_changed {
            return Ok(());
        }
        if self.data.is_dir(i) {
            // Refuse to move a directory under itself.
            let mut cur = Some(new_parent.0);
            let mut depth = 0;
            while let Some(c) = cur {
                if c == i {
                    return Err(IndexError::WouldCycle);
                }
                depth += 1;
                if depth > MAX_DEPTH {
                    break;
                }
                cur = self.data.parent_of(c);
            }
        }
        {
            let name_of = |x: u32| wname(&self.data, &self.pending, x);
            if let Some(existing) = self.children[npd as usize].find(new_name, &name_of)
                && existing != i
            {
                return Err(IndexError::Exists(EntryId(existing)));
            }
        }
        let mask = if name_changed {
            ALL_ORDERS
        } else {
            OrderKind::Name.bit() | OrderKind::Path.bit()
        };
        self.mark_dirty(i, mask);
        self.batch.touched.insert(i);
        // Moving or renaming a directory changes the path of its whole subtree.
        let subtree = if self.data.is_dir(i) {
            self.subtree(i)
        } else {
            Vec::new()
        };
        for &e in subtree.iter().skip(1) {
            self.mark_dirty(e, OrderKind::Name.bit() | OrderKind::Path.bit());
        }
        for &e in &subtree {
            if let Some(d) = self.data.dir_of(e) {
                self.batch.needs_dir_key.insert(d);
            }
        }
        self.detach(i);
        if name_changed {
            self.set_name(i, new_name);
            let old_flags = self.data.flags_of(i);
            let root = old_flags & flags::VOLUME_ROOT != 0;
            let keep = old_flags & flags::HAS_ACL;
            self.data
                .flags
                .set(i as usize, keep | entry_flags(new_name, root));
        }
        self.data.parent.set(i as usize, new_parent.0);
        self.attach(i);
        Ok(())
    }

    // ----- publishing -----

    /// Publishes all changes since the last publish as a new generation.
    pub fn publish(&mut self) -> Arc<Generation> {
        let batch = std::mem::take(&mut self.batch);
        let live = self.data.live;
        let mut stats = PublishStats {
            dirty: batch.dirty.len(),
            ..PublishStats::default()
        };
        let mut lap = Lap::start();

        // 1. Remove dirty entries from the orders, using the keys they were sorted by.
        let mut dirty_bits = Bitmap::with_capacity(self.data.capacity());
        for &id in batch.dirty.keys() {
            dirty_bits.set(id);
        }
        let rebuild_flags: Vec<bool> = {
            let data = &self.data;
            let dirty = &batch.dirty;
            let bits = &dirty_bits;
            self.orders
                .as_array_mut()
                .into_par_iter()
                .enumerate()
                .map(|(oi, order)| {
                    let kind = OrderKind::ALL[oi];
                    let items: Vec<(SortKey, u32)> = dirty
                        .iter()
                        .filter(|(_, d)| d.mask & kind.bit() != 0)
                        .filter_map(|(&id, d)| d.old.map(|k| (k[oi], id)))
                        .collect();
                    let dirty_total = dirty.values().filter(|d| d.mask & kind.bit() != 0).count();
                    if dirty_total * 8 > order.len().max(live) || order.is_empty() {
                        return true;
                    }
                    let old_key = |id: u32| {
                        if bits.get(id)
                            && let Some(Dirty { old: Some(k), .. }) = dirty.get(&id)
                        {
                            return k[oi];
                        }
                        order_key(data, kind, id)
                    };
                    order.remove_batch(items, &old_key);
                    false
                })
                .collect()
        };
        let mut rebuild = [false; 4];
        rebuild.copy_from_slice(&rebuild_flags);

        stats.phases[0] = lap.next();
        // 2. Publish the pending names (labels below compare names directly).
        self.flush_names();

        stats.phases[1] = lap.next();
        // 3. Name labels, then directory labels.
        // Every live entry that is not in the cleaned name order gets a fresh label, so that
        // labels stay consistent with neighbours relabelled in this publish.
        let name_ids: Vec<u32> = batch
            .dirty
            .iter()
            .filter(|(_, d)| d.mask & OrderKind::Name.bit() != 0)
            .map(|(&id, _)| id)
            .filter(|&id| self.data.is_live(id))
            .collect();
        if rebuild[0] || name_ids.len() * 8 > live || self.orders.name.is_empty() {
            self.relabel_all_names();
            rebuild[0] = true;
        } else {
            self.assign_name_keys(name_ids);
        }
        let dir_ids: Vec<u32> = batch
            .needs_dir_key
            .iter()
            .copied()
            .filter(|&d| self.data.dirs.entry.get(d as usize) != NONE)
            .collect();
        let freed_dirs: HashSet<u32> = batch.freed_dirs.iter().copied().collect();
        if dir_ids.len() * 8 > self.dir_seq.len() || self.dir_seq.is_empty() {
            self.relabel_all_dirs();
            rebuild[1] = true;
            rebuild[0] = true;
        } else {
            self.assign_dir_keys(&dir_ids, &freed_dirs);
        }

        stats.phases[2] = lap.next();
        // 4. Re-insert dirty live entries (or rebuild whole orders).
        let all_live: Option<Vec<u32>> = rebuild.iter().any(|&r| r).then(|| self.live_ids());
        let relabeled = std::mem::take(&mut self.relabeled);
        {
            let data = &self.data;
            let dirty = &batch.dirty;
            let all_live = &all_live;
            self.orders
                .as_array_mut()
                .into_par_iter()
                .enumerate()
                .for_each(|(oi, order)| {
                    let kind = OrderKind::ALL[oi];
                    let key = |id: u32| order_key(data, kind, id);
                    if rebuild[oi] {
                        let ids = all_live.clone().unwrap_or_default();
                        *order = Order::build(ids, key);
                        return;
                    }
                    if relabeled {
                        order.refresh_fences(&key);
                    }
                    let items: Vec<(SortKey, u32)> = dirty
                        .iter()
                        .filter(|(_, d)| d.mask & kind.bit() != 0)
                        .map(|(&id, _)| id)
                        .filter(|&id| data.is_live(id))
                        .map(|id| (key(id), id))
                        .collect();
                    order.insert_batch(items, &key);
                });
        }

        stats.phases[3] = lap.next();
        stats.rebuilt_orders = rebuild.iter().filter(|&&r| r).count();
        // 5. Free slots become reusable only now.
        for &e in &batch.freed {
            let page = EntryId(e).page();
            let vol = self.data.page_volume.get(page).copied().unwrap_or(0) as usize;
            if let Some(list) = self.free_slots.get_mut(vol) {
                list.push(e);
            }
        }
        self.free_dirs.extend_from_slice(&batch.freed_dirs);

        // 6. Change set and publication.
        let mut changes = ChangeSet::default();
        let freed: HashSet<u32> = batch.freed.iter().copied().collect();
        for (&id, d) in &batch.dirty {
            let live_now = self.data.is_live(id);
            match (d.old.is_some(), live_now) {
                (false, true) => changes.inserted.push(EntryId(id)),
                (true, false) => changes.removed.push(EntryId(id)),
                (true, true) => changes.modified.push(EntryId(id)),
                (false, false) => {}
            }
        }
        for &id in &batch.touched {
            if id != NONE
                && !batch.dirty.contains_key(&id)
                && self.data.is_live(id)
                && !freed.contains(&id)
            {
                changes.modified.push(EntryId(id));
            }
        }
        for v in [
            &mut changes.inserted,
            &mut changes.removed,
            &mut changes.modified,
        ] {
            v.sort_unstable();
        }
        self.number += 1;
        let generation = Arc::new(Generation {
            data: self.data.clone(),
            orders: self.orders.clone(),
            number: self.number,
            changes: Arc::new(changes),
        });
        stats.phases[4] = lap.next();
        self.handle.current.store(Arc::clone(&generation));
        self.last_publish = stats;
        generation
    }

    /// Timing and copy statistics of the last publish (benchmarks and diagnostics).
    #[must_use]
    pub fn last_publish_stats(&self) -> PublishStats {
        self.last_publish
    }

    fn live_ids(&self) -> Vec<u32> {
        let cap = u32::try_from(self.data.capacity()).unwrap_or(u32::MAX);
        (0..cap)
            .into_par_iter()
            .filter(|&id| self.data.is_live(id))
            .collect()
    }

    fn flush_names(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for (page, p) in pending {
            let page = page as usize;
            let Some(old) = self.data.names.get(page).cloned() else {
                continue;
            };
            let names = (0..PAGE_SIZE).map(|slot| {
                #[allow(clippy::cast_possible_truncation)]
                if let Some(n) = p.overlay.get(&(slot as u16)) {
                    return &n[..];
                }
                match &p.fresh {
                    Some(f) if slot < f.offs.len() => f.name(slot),
                    Some(_) => &[],
                    None => old.name(slot),
                }
            });
            self.data.names[page] = Arc::new(NamePage::build(names));
        }
    }

    /// Assigns evenly spaced name labels to all live entries.
    fn relabel_all_names(&mut self) {
        self.relabeled = true;
        let data = &self.data;
        let mut keyed: Vec<(Vec<u8>, u32)> = self
            .live_ids()
            .into_par_iter()
            .map(|id| {
                let mut k = Vec::with_capacity(24);
                fold::collation_key_into(data.name_of(id), &mut k);
                (k, id)
            })
            .collect();
        keyed.par_sort_unstable_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| data.name_of(a.1).cmp(data.name_of(b.1)))
                .then(a.1.cmp(&b.1))
        });
        let mut distinct = 0usize;
        for i in 0..keyed.len() {
            if i == 0
                || keyed[i].0 != keyed[i - 1].0
                || data.name_of(keyed[i].1) != data.name_of(keyed[i - 1].1)
            {
                distinct += 1;
            }
        }
        let step = (u64::from(LABEL_HI - LABEL_LO)) / (distinct as u64 + 1);
        let mut labels = Vec::with_capacity(keyed.len());
        let mut n = 0u64;
        for i in 0..keyed.len() {
            if i == 0
                || keyed[i].0 != keyed[i - 1].0
                || data.name_of(keyed[i].1) != data.name_of(keyed[i - 1].1)
            {
                n += 1;
            }
            let label = u32::try_from(u64::from(LABEL_LO) + n * step.max(1)).unwrap_or(LABEL_HI);
            labels.push((keyed[i].1, label));
        }
        for (id, label) in labels {
            self.data.name_key.set(id as usize, label);
        }
    }

    /// Assigns name labels to new or renamed entries using the (cleaned) name order.
    fn assign_name_keys(&mut self, mut ids: Vec<u32>) {
        if ids.is_empty() {
            return;
        }
        {
            let data = &self.data;
            ids.sort_unstable_by(|&a, &b| {
                fold::collate(data.name_of(a), data.name_of(b)).then(a.cmp(&b))
            });
            ids.dedup();
        }
        // Group identical names; find each group's position in the name order.
        let mut runs: Vec<(usize, Vec<Vec<u32>>)> = Vec::new();
        let mut gi = 0;
        while gi < ids.len() {
            let mut gj = gi + 1;
            while gj < ids.len() && self.data.name_of(ids[gj]) == self.data.name_of(ids[gi]) {
                gj += 1;
            }
            let group = ids[gi..gj].to_vec();
            gi = gj;
            let name = self.data.name_of(group[0]);
            let order = &self.orders.name;
            let pos = order.partition_by(|e| fold::collate(self.data.name_of(e), name));
            if let Some(at) = order.get(pos)
                && self.data.name_of(at) == name
            {
                let label = self.data.name_key.get(at as usize);
                for &id in &group {
                    self.data.name_key.set(id as usize, label);
                }
                continue;
            }
            match runs.last_mut() {
                Some((p, gs)) if *p == pos => gs.push(group),
                _ => runs.push((pos, vec![group])),
            }
        }
        // Label runs left to right; relabel windows include runs placed earlier.
        let mut placed: BTreeMap<usize, Vec<Vec<u32>>> = BTreeMap::new();
        for (pos, groups) in runs {
            self.fill_name_gap(pos, &groups, &placed);
            placed.insert(pos, groups);
        }
    }

    /// Labels `groups` (sorted, distinct names) that belong at position `pos` of the name order,
    /// relabelling a window of neighbours (including groups already `placed` in this batch) if
    /// the gap is too small.
    fn fill_name_gap(
        &mut self,
        pos: usize,
        groups: &[Vec<u32>],
        placed: &BTreeMap<usize, Vec<Vec<u32>>>,
    ) {
        let order = &self.orders.name;
        let data = &self.data;
        let key_at = |p: usize| order.get(p).map(|id| data.name_key.get(id as usize));
        let lo = if pos == 0 {
            0
        } else {
            key_at(pos - 1).unwrap_or(0)
        };
        let hi = key_at(pos).unwrap_or(u32::MAX);
        if let Some(labels) = spread(lo, hi, groups.len()) {
            for (g, label) in groups.iter().zip(labels) {
                for &id in g {
                    self.data.name_key.set(id as usize, label);
                }
            }
            return;
        }
        // Grow a window [a, b) around `pos` until the labels fit with slack.
        let len = order.len();
        let mut w = 32usize;
        let slots: Vec<Vec<u32>> = loop {
            let mut a = pos.saturating_sub(w);
            let mut b = (pos + w).min(len);
            while a > 0 && key_at(a - 1) == key_at(a) {
                a -= 1;
            }
            while b < len && b > 0 && key_at(b) == key_at(b - 1) {
                b += 1;
            }
            let lo = if a == 0 {
                0
            } else {
                key_at(a - 1).unwrap_or(0)
            };
            let hi = if b >= len {
                u32::MAX
            } else {
                key_at(b).unwrap_or(u32::MAX)
            };
            let mut slots: Vec<Vec<u32>> = Vec::new();
            let mut prev = None;
            for p in a..=b {
                if let Some(gs) = placed.get(&p) {
                    slots.extend(gs.iter().cloned());
                    prev = None;
                }
                if p == pos {
                    slots.extend(groups.iter().cloned());
                    prev = None;
                }
                if p == b {
                    break;
                }
                let id = order.get(p).unwrap_or(NONE);
                let k = key_at(p);
                if k != prev || slots.is_empty() {
                    slots.push(Vec::new());
                    prev = k;
                }
                if let Some(last) = slots.last_mut() {
                    last.push(id);
                }
            }
            let room = u64::from(hi) - u64::from(lo);
            if room >= slots.len() as u64 * 4 || (a == 0 && b >= len) {
                self.relabeled = true;
                match spread(lo, hi, slots.len()) {
                    Some(labels) => {
                        let updates: Vec<(Vec<u32>, u32)> = slots.into_iter().zip(labels).collect();
                        for (ids, label) in updates {
                            for id in ids {
                                if id != NONE {
                                    self.data.name_key.set(id as usize, label);
                                }
                            }
                        }
                        return;
                    }
                    None => break Vec::new(),
                }
            }
            w *= 2;
        };
        debug_assert!(slots.is_empty());
        // The whole label space is exhausted (more than ~4 billion distinct names): relabel all.
        self.relabel_all_names();
    }

    /// Path components of a directory for path comparison: mount components, then entry names.
    fn dir_components(&self, entry: u32) -> (Vec<&[u8]>, Vec<u32>) {
        let chain = self.data.chain(entry);
        let mount: Vec<&[u8]> = self
            .data
            .volume_of(chain[0])
            .map(|v| v.mount_components().collect())
            .unwrap_or_default();
        (mount, chain[1..].to_vec())
    }

    /// Component-wise path comparison of two directory entries.
    fn path_cmp(&self, a: u32, b: u32) -> Ordering {
        let (ma, ca) = self.dir_components(a);
        let (mb, cb) = self.dir_components(b);
        let len_a = ma.len() + ca.len();
        let len_b = mb.len() + cb.len();
        for i in 0..len_a.min(len_b) {
            let ord = match (i < ma.len(), i < mb.len()) {
                (false, false) => {
                    let (x, y) = (ca[i - ma.len()], cb[i - mb.len()]);
                    self.data
                        .name_key
                        .get(x as usize)
                        .cmp(&self.data.name_key.get(y as usize))
                }
                (am, bm) => {
                    let x: &[u8] = if am {
                        ma[i]
                    } else {
                        self.data.name_of(ca[i - ma.len()])
                    };
                    let y: &[u8] = if bm {
                        mb[i]
                    } else {
                        self.data.name_of(cb[i - mb.len()])
                    };
                    fold::collate(x, y)
                }
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        len_a.cmp(&len_b).then(a.cmp(&b))
    }

    /// Depth-first relabelling of every directory.
    fn relabel_all_dirs(&mut self) {
        self.relabeled = true;
        let mut roots: Vec<u32> = self
            .data
            .volumes
            .iter()
            .map(|v| v.root.0)
            .filter(|&r| self.data.is_live(r))
            .collect();
        roots.sort_by(|&a, &b| self.path_cmp(a, b));
        // DFS per volume, children sorted by name label.
        let mut per_volume: Vec<Vec<u32>> = Vec::with_capacity(roots.len());
        for &root in &roots {
            let mut seq = Vec::new();
            let mut stack = vec![root];
            while let Some(e) = stack.pop() {
                let Some(d) = self.data.dir_of(e) else {
                    continue;
                };
                seq.push(d);
                let mut kids: Vec<u32> = self.children[d as usize]
                    .ids()
                    .into_iter()
                    .filter(|&k| self.data.is_dir(k))
                    .collect();
                kids.sort_unstable_by_key(|&k| {
                    std::cmp::Reverse(self.data.name_key.get(k as usize))
                });
                stack.extend(kids);
            }
            per_volume.push(seq);
        }
        // Merge volumes in path order (nested mounts interleave).
        let seq: Vec<u32> = if per_volume.len() == 1 {
            per_volume.pop().unwrap_or_default()
        } else {
            let mut all: Vec<u32> = per_volume.into_iter().flatten().collect();
            let data = &self.data;
            all.sort_by(|&x, &y| {
                self.path_cmp(
                    data.dirs.entry.get(x as usize),
                    data.dirs.entry.get(y as usize),
                )
            });
            all
        };
        let step = u64::from(LABEL_HI - LABEL_LO) / (seq.len() as u64 + 1);
        for (i, &d) in seq.iter().enumerate() {
            let label = u32::try_from(u64::from(LABEL_LO) + (i as u64 + 1) * step.max(1))
                .unwrap_or(LABEL_HI);
            self.data.dirs.dir_key.set(d as usize, label);
        }
        self.dir_seq = seq;
    }

    /// Assigns dir labels to new or moved directories and updates `dir_seq`.
    fn assign_dir_keys(&mut self, dirs: &[u32], freed: &HashSet<u32>) {
        let moving: HashSet<u32> = dirs.iter().copied().collect();
        if !moving.is_empty() || !freed.is_empty() {
            self.dir_seq
                .retain(|d| !moving.contains(d) && !freed.contains(d));
        }
        if dirs.is_empty() {
            return;
        }
        let entry = |s: &Self, d: u32| s.data.dirs.entry.get(d as usize);
        let mut new: Vec<u32> = dirs.to_vec();
        new.sort_by(|&x, &y| self.path_cmp(entry(self, x), entry(self, y)));
        // Merge the new directories into the cleaned sequence.
        let mut merged: Vec<(u32, bool)> = Vec::with_capacity(self.dir_seq.len() + new.len());
        let mut s = 0;
        for &d in &new {
            let e = entry(self, d);
            let pos = s + self.dir_seq[s..]
                .partition_point(|&x| self.path_cmp(entry(self, x), e) == Ordering::Less);
            merged.extend(self.dir_seq[s..pos].iter().map(|&x| (x, false)));
            merged.push((d, true));
            s = pos;
        }
        merged.extend(self.dir_seq[s..].iter().map(|&x| (x, false)));
        // Label runs of new directories, relabelling windows when gaps are exhausted.
        let len = merged.len();
        let key = |s: &Self, i: usize| s.data.dir_key_of_dir(merged[i].0);
        let mut i = 0;
        while i < len {
            if !merged[i].1 {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < len && merged[j].1 {
                j += 1;
            }
            let lo = if i == 0 { 0 } else { key(self, i - 1) };
            let hi = if j >= len { u32::MAX } else { key(self, j) };
            if let Some(labels) = spread(lo, hi, j - i) {
                for (k, l) in (i..j).zip(labels) {
                    self.data.dirs.dir_key.set(merged[k].0 as usize, l);
                }
                i = j;
                continue;
            }
            let mut w = 32usize;
            loop {
                let a = i.saturating_sub(w);
                let mut b = (j + w).min(len);
                // The right bound must be an already-labelled (existing) directory.
                while b < len && merged[b].1 {
                    b += 1;
                }
                let lo = if a == 0 { 0 } else { key(self, a - 1) };
                let hi = if b >= len { u32::MAX } else { key(self, b) };
                let need = (b - a) as u64;
                if u64::from(hi) - u64::from(lo) >= need * 4 || (a == 0 && b >= len) {
                    self.relabeled = true;
                    if let Some(labels) = spread(lo, hi, b - a) {
                        for (k, l) in (a..b).zip(labels) {
                            self.data.dirs.dir_key.set(merged[k].0 as usize, l);
                        }
                        i = b;
                    } else {
                        self.dir_seq = merged.iter().map(|&(d, _)| d).collect();
                        self.relabel_all_dirs();
                        return;
                    }
                    break;
                }
                w *= 2;
            }
        }
        self.dir_seq = merged.into_iter().map(|(d, _)| d).collect();
    }

    /// Checks internal invariants (tests). Returns a description of the first violation.
    #[doc(hidden)]
    pub fn check_invariants(&self) -> Result<(), String> {
        let g = self.handle.load();
        let d = &g.data;
        for kind in OrderKind::ALL {
            let order = g.orders.get(kind);
            if order.len() != d.live {
                return Err(format!(
                    "{kind:?} order has {} ids, live = {}",
                    order.len(),
                    d.live
                ));
            }
            if !order.is_sorted_by(&|id| order_key(d, kind, id)) {
                return Err(format!("{kind:?} order not sorted"));
            }
            if order.iter().any(|id| !d.is_live(id)) {
                return Err(format!("{kind:?} order contains a free id"));
            }
        }
        // Name labels respect collation.
        let mut prev: Option<u32> = None;
        for id in g.orders.name.iter() {
            if let Some(p) = prev {
                let c = fold::collate(d.name_of(p), d.name_of(id));
                let kp = d.name_key.get(p as usize);
                let ki = d.name_key.get(id as usize);
                if c == Ordering::Greater || (c == Ordering::Equal) != (kp == ki) {
                    return Err(format!(
                        "name labels inconsistent: {:?}({kp}) vs {:?}({ki})",
                        crate::name::display_name(d.name_of(p)),
                        crate::name::display_name(d.name_of(id))
                    ));
                }
            }
            prev = Some(id);
        }
        // Every entry but a volume root has a live directory as parent, and child lists hold
        // exactly the entries whose parent is that directory.
        let mut per_parent: HashMap<u32, usize> = HashMap::new();
        for id in g.iter() {
            if d.flags_of(id.0) & flags::VOLUME_ROOT != 0 {
                continue;
            }
            match d.parent_of(id.0) {
                Some(p) if d.is_live(p) && d.is_dir(p) => *per_parent.entry(p).or_default() += 1,
                _ => {
                    return Err(format!(
                        "{:?} (id {}) has no live parent directory",
                        crate::name::display_name(d.name_of(id.0)),
                        id.0
                    ));
                }
            }
        }
        for id in g.iter() {
            if d.dir_of(id.0).is_none() {
                continue;
            }
            let kids = self.children(id);
            let expect = per_parent.get(&id.0).copied().unwrap_or(0);
            if kids.len() != expect
                || kids
                    .iter()
                    .any(|k| !d.is_live(k.0) || d.parent_of(k.0) != Some(id.0))
            {
                return Err(format!(
                    "child list of {:?} has {} ids, {expect} entries name it as parent",
                    crate::name::display_name(&g.path(id)),
                    kids.len()
                ));
            }
        }
        // Folder sizes and counts.
        for id in g.iter() {
            let Some(dir) = d.dir_of(id.0) else { continue };
            let kids = self.children(id);
            let files = kids.iter().filter(|k| !d.is_dir(k.0)).count();
            let folders = kids.len() - files;
            let du = dir as usize;
            if d.dirs.files.get(du) as usize != files || d.dirs.folders.get(du) as usize != folders
            {
                return Err(format!("child counts wrong for {:?}", g.path(id)));
            }
            let sum: u64 = kids.iter().map(|k| d.size_of(k.0)).sum();
            if d.dirs.size.get(du) != sum {
                return Err(format!(
                    "folder size of {:?} is {}, expected {sum}",
                    crate::name::display_name(&g.path(id)),
                    d.dirs.size.get(du)
                ));
            }
        }
        Ok(())
    }
}
