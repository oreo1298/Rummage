//! Snapshot persistence: the `FOSSIDX` file format.
//!
//! Layout (all integers little-endian, every section 64-byte aligned):
//!
//! ```text
//! Header (128 bytes) | section table (32 bytes per section) | sections...
//! ```
//!
//! Each section carries its own xxh3-64 checksum. Snapshots are written to a temporary file in the
//! target directory, `fsync`ed, renamed over the old snapshot and the directory is `fsync`ed, so a
//! crash leaves either the old or the new snapshot, never a torn one. Loading memory-maps the file
//! (`MAP_PRIVATE`, read-only) and builds a [`Generation`] whose column pages point straight into
//! the mapping; pages are copied to owned memory only when the writer first modifies them.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::{Xxh3, xxh3_64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::buf::{Buf, Column, MappedSlice, Pod};
use crate::index::names::NamePage;
use crate::index::{
    DirStore, Generation, IndexData, Order, OrderKind, Orders, PAGE_SIZE, SideTables, SortKey,
    Volume,
};

/// File magic.
pub const MAGIC: [u8; 8] = *b"FOSSIDX\0";
/// Current format version. Bump on any incompatible layout change and add a migration.
pub const FORMAT_VERSION: u32 = 1;
const ENDIAN_MARK: u32 = 0x0102_0304;
const ALIGN: u64 = 64;
const HEADER_LEN: u64 = 128;

/// A read-only mapping (or in-memory copy) of a snapshot file.
pub(crate) enum Mapping {
    Mmap(memmap2::Mmap),
    /// In-memory copy, stored as `u64`s so that section offsets are suitably aligned.
    Owned(Box<[u64]>, usize),
}

impl fmt::Debug for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mapping({} bytes)", self.bytes().len())
    }
}

impl Mapping {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Mmap(m) => m,
            Self::Owned(words, len) => &words.as_bytes()[..*len],
        }
    }
}

/// Errors reading or writing snapshots.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("not a Fossick index snapshot")]
    BadMagic,
    #[error("snapshot format version {found} is not supported (expected {FORMAT_VERSION})")]
    Incompatible { found: u32 },
    #[error("snapshot was written on a machine with a different byte order")]
    Endianness,
    #[error("snapshot is corrupt: {0}")]
    Corrupt(String),
    #[error("checksum mismatch in snapshot section {0}")]
    Checksum(String),
}

#[repr(C)]
#[derive(Clone, Copy, Debug, FromBytes, IntoBytes, Immutable, KnownLayout)]
struct Header {
    magic: [u8; 8],
    version: u32,
    endian: u32,
    page_size: u32,
    flags: u32,
    generation: u64,
    created_unix: i64,
    sections: u32,
    _pad: u32,
    table_offset: u64,
    table_checksum: u64,
    header_checksum: u64,
    _reserved: [u8; 56],
}

const FLAG_ATIME: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, FromBytes, IntoBytes, Immutable, KnownLayout)]
struct SectionEntry {
    kind: u16,
    sub: u16,
    elem_size: u32,
    offset: u64,
    len: u64,
    checksum: u64,
}

/// A fence as stored on disk.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
struct DiskFence {
    k0: u64,
    k1: u64,
    id: u32,
    _pad: u32,
}

/// One escaped size, as stored.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
struct DiskBigSize {
    id: u32,
    _pad: u32,
    size: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
struct DiskOddTime {
    id: u32,
    field: u32,
    nanos: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
struct DiskOwner {
    id: u32,
    uid: u32,
    gid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
enum Kind {
    Meta = 1,
    PageVolume = 2,
    NameBytes = 3,
    NameIndex = 4,
    NameOffs = 5,
    Parent = 10,
    Size = 11,
    Mtime = 12,
    Ctime = 13,
    Btime = 14,
    Atime = 15,
    NameKey = 16,
    Mode = 17,
    Owner = 18,
    Inode = 19,
    Flags = 20,
    Attrs = 21,
    DirEntry = 30,
    DirSize = 31,
    DirMtime = 32,
    DirCtime = 33,
    DirKey = 34,
    DirFiles = 35,
    DirFolders = 36,
    DirRestricted = 37,
    Owners = 40,
    BigSize = 41,
    OddTime = 42,
    OddOwner = 43,
    OrderIds = 50,
    OrderBlocks = 51,
    OrderFences = 52,
}

/// Sections whose checksums are verified before the snapshot is served.
const ESSENTIAL: &[Kind] = &[
    Kind::Meta,
    Kind::PageVolume,
    Kind::NameBytes,
    Kind::NameIndex,
    Kind::NameOffs,
    Kind::Parent,
    Kind::Size,
    Kind::Mode,
    Kind::Flags,
    Kind::OrderIds,
    Kind::OrderBlocks,
    Kind::OrderFences,
];

#[derive(Serialize, Deserialize)]
struct Meta {
    volumes: Vec<Volume>,
    live: usize,
    pages: usize,
    dir_pages: usize,
}

/// How much to verify before returning from [`load`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verify {
    /// Verify every section now.
    All,
    /// Verify the sections searches touch first now; check the rest later with
    /// [`Snapshot::verify_deferred`].
    Essential,
}

/// A loaded snapshot.
#[derive(Debug)]
pub struct Snapshot {
    pub generation: Generation,
    map: Arc<Mapping>,
    deferred: Vec<(String, u64, u64, u64)>,
}

impl Snapshot {
    /// Verifies the checksums of sections not verified at load time.
    pub fn verify_deferred(&self) -> Result<(), PersistError> {
        verify_sections(self.map.bytes(), &self.deferred)
    }

    /// Total size of the mapped file.
    #[must_use]
    pub fn file_len(&self) -> usize {
        self.map.bytes().len()
    }
}

struct SectionWriter<W: Write> {
    out: W,
    pos: u64,
    table: Vec<SectionEntry>,
}

impl<W: Write> SectionWriter<W> {
    fn pad(&mut self) -> io::Result<()> {
        let rem = self.pos % ALIGN;
        if rem != 0 {
            let zeros = [0u8; ALIGN as usize];
            #[allow(clippy::cast_possible_truncation)]
            self.out.write_all(&zeros[..(ALIGN - rem) as usize])?;
            self.pos += ALIGN - rem;
        }
        Ok(())
    }

    fn section<'a>(
        &mut self,
        kind: Kind,
        sub: u16,
        elem_size: usize,
        parts: impl IntoIterator<Item = &'a [u8]>,
    ) -> io::Result<()> {
        self.pad()?;
        let offset = self.pos;
        let mut h = Xxh3::new();
        for p in parts {
            h.update(p);
            self.out.write_all(p)?;
            self.pos += p.len() as u64;
        }
        self.table.push(SectionEntry {
            kind: kind as u16,
            sub,
            elem_size: u32::try_from(elem_size).unwrap_or(0),
            offset,
            len: self.pos - offset,
            checksum: h.digest(),
        });
        Ok(())
    }

    fn column<T: Pod>(&mut self, kind: Kind, col: &Column<T>) -> io::Result<()> {
        self.section(
            kind,
            0,
            std::mem::size_of::<T>(),
            col.pages().iter().map(|p| p.as_slice().as_bytes()),
        )
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(name)
}

/// Writes `generation` to `path` atomically (temp file, fsync, rename, fsync directory). The
/// file is created with mode 0600.
pub fn write(generation: &Generation, path: &Path) -> Result<(), PersistError> {
    let tmp = tmp_path(path);
    let result = write_to(generation, &tmp).and_then(|()| {
        fs::rename(&tmp, path)?;
        if let Some(dir) = path.parent() {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            File::open(dir)?.sync_all()?;
        }
        Ok(())
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_to(g: &Generation, tmp: &Path) -> Result<(), PersistError> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(tmp)?;
    let mut w = SectionWriter {
        out: BufWriter::with_capacity(1 << 20, file),
        pos: 0,
        table: Vec::new(),
    };
    let d = &g.data;
    // Reserve the header and table; they are written at the end.
    let section_estimate = 64u64;
    let reserved = HEADER_LEN + section_estimate * 32;
    #[allow(clippy::cast_possible_truncation)]
    w.out.write_all(&vec![0u8; reserved as usize])?;
    w.pos = reserved;

    let meta = Meta {
        volumes: d.volumes.to_vec(),
        live: d.live,
        pages: d.page_count(),
        dir_pages: d.dirs.entry.page_count(),
    };
    let meta_json = serde_json::to_vec(&meta).map_err(|e| PersistError::Corrupt(e.to_string()))?;
    w.section(Kind::Meta, 0, 1, [meta_json.as_slice()])?;
    w.section(Kind::PageVolume, 0, 2, [d.page_volume.as_bytes()])?;
    // Names: all page arenas back to back, a (start, len) index per page, and the offsets.
    let mut index: Vec<u64> = Vec::with_capacity(d.names.len() * 2);
    let mut start = 0u64;
    for p in &d.names {
        index.push(start);
        index.push(p.arena().len() as u64);
        start += p.arena().len() as u64;
    }
    w.section(Kind::NameBytes, 0, 1, d.names.iter().map(|p| p.arena()))?;
    w.section(Kind::NameIndex, 0, 8, [index.as_bytes()])?;
    w.section(
        Kind::NameOffs,
        0,
        4,
        d.names.iter().map(|p| p.offsets().as_bytes()),
    )?;
    w.column(Kind::Parent, &d.parent)?;
    w.column(Kind::Size, &d.size)?;
    w.column(Kind::Mtime, &d.mtime)?;
    w.column(Kind::Ctime, &d.ctime)?;
    w.column(Kind::Btime, &d.btime)?;
    if let Some(a) = &d.atime {
        w.column(Kind::Atime, a)?;
    }
    w.column(Kind::NameKey, &d.name_key)?;
    w.column(Kind::Mode, &d.mode)?;
    w.column(Kind::Owner, &d.owner)?;
    w.column(Kind::Inode, &d.inode)?;
    w.column(Kind::Flags, &d.flags)?;
    w.column(Kind::Attrs, &d.attrs)?;
    w.column(Kind::DirEntry, &d.dirs.entry)?;
    w.column(Kind::DirSize, &d.dirs.size)?;
    w.column(Kind::DirMtime, &d.dirs.mtime_ns)?;
    w.column(Kind::DirCtime, &d.dirs.ctime_ns)?;
    w.column(Kind::DirKey, &d.dirs.dir_key)?;
    w.column(Kind::DirFiles, &d.dirs.files)?;
    w.column(Kind::DirFolders, &d.dirs.folders)?;
    w.column(Kind::DirRestricted, &d.dirs.restricted)?;
    let owners: Vec<u32> = d.owners.iter().flat_map(|&(u, g)| [u, g]).collect();
    w.section(Kind::Owners, 0, 8, [owners.as_bytes()])?;
    let mut big: Vec<DiskBigSize> = d
        .side
        .big_size
        .iter()
        .map(|(&id, &size)| DiskBigSize { id, _pad: 0, size })
        .collect();
    big.sort_unstable_by_key(|b| b.id);
    w.section(Kind::BigSize, 0, 16, [big.as_bytes()])?;
    let mut odd: Vec<DiskOddTime> = d
        .side
        .odd_time
        .iter()
        .map(|(&(id, field), &nanos)| DiskOddTime {
            id,
            field: u32::from(field),
            nanos,
        })
        .collect();
    odd.sort_unstable_by_key(|o| (o.id, o.field));
    w.section(Kind::OddTime, 0, 16, [odd.as_bytes()])?;
    let mut oo: Vec<DiskOwner> = d
        .side
        .odd_owner
        .iter()
        .map(|(&id, &(uid, gid))| DiskOwner { id, uid, gid })
        .collect();
    oo.sort_unstable_by_key(|o| o.id);
    w.section(Kind::OddOwner, 0, 12, [oo.as_bytes()])?;
    for (i, kind) in OrderKind::ALL.into_iter().enumerate() {
        let order = g.orders.get(kind);
        let sub = u16::try_from(i).unwrap_or(0);
        w.section(
            Kind::OrderIds,
            sub,
            4,
            order.blocks().iter().map(|b| b.as_bytes()),
        )?;
        let lens: Vec<u32> = order
            .blocks()
            .iter()
            .map(|b| u32::try_from(b.len()).unwrap_or(0))
            .collect();
        w.section(Kind::OrderBlocks, sub, 4, [lens.as_bytes()])?;
        let fences: Vec<DiskFence> = order
            .fences()
            .iter()
            .map(|&((k0, k1), id)| DiskFence {
                k0,
                k1,
                id,
                _pad: 0,
            })
            .collect();
        w.section(Kind::OrderFences, sub, 24, [fences.as_bytes()])?;
    }
    w.pad()?;
    let end = w.pos;
    let table = std::mem::take(&mut w.table);
    let table_bytes = table.as_bytes();
    if HEADER_LEN + table_bytes.len() as u64 > reserved {
        return Err(PersistError::Corrupt("section table overflow".into()));
    }
    let mut file = w.out.into_inner().map_err(io::IntoInnerError::into_error)?;
    let mut header = Header {
        magic: MAGIC,
        version: FORMAT_VERSION,
        endian: ENDIAN_MARK,
        page_size: u32::try_from(PAGE_SIZE).unwrap_or(0),
        flags: if d.atime.is_some() { FLAG_ATIME } else { 0 },
        generation: g.number,
        created_unix: now_unix(),
        sections: u32::try_from(table.len()).unwrap_or(0),
        _pad: 0,
        table_offset: HEADER_LEN,
        table_checksum: xxh3_64(table_bytes),
        header_checksum: 0,
        _reserved: [0; 56],
    };
    header.header_checksum = xxh3_64(header.as_bytes());
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    file.write_all(header.as_bytes())?;
    file.write_all(table_bytes)?;
    file.set_len(end)?;
    file.sync_all()?;
    Ok(())
}

fn verify_sections(bytes: &[u8], sections: &[(String, u64, u64, u64)]) -> Result<(), PersistError> {
    sections.par_iter().try_for_each(|(name, off, len, sum)| {
        let s = slice(bytes, *off, *len)?;
        if xxh3_64(s) == *sum {
            Ok(())
        } else {
            Err(PersistError::Checksum(name.clone()))
        }
    })
}

fn slice(bytes: &[u8], off: u64, len: u64) -> Result<&[u8], PersistError> {
    let start = usize::try_from(off).map_err(|_| PersistError::Corrupt("offset".into()))?;
    let len = usize::try_from(len).map_err(|_| PersistError::Corrupt("length".into()))?;
    start
        .checked_add(len)
        .and_then(|end| bytes.get(start..end))
        .ok_or_else(|| PersistError::Corrupt("section out of bounds".into()))
}

/// Opens and memory-maps a snapshot, verifying it according to `verify`.
pub fn load(path: &Path, verify: Verify) -> Result<Snapshot, PersistError> {
    if cfg!(target_endian = "big") {
        return Err(PersistError::Endianness);
    }
    let file = File::open(path)?;
    // SAFETY: the snapshot file is private to Fossick (mode 0600 in a 0700 directory) and is only
    // ever replaced by rename, never modified in place, so the mapped bytes cannot change under
    // us. The mapping is MAP_PRIVATE and read-only. All reads through it are bounds-checked.
    #[allow(unsafe_code)]
    let mmap = unsafe { memmap2::MmapOptions::new().map_copy_read_only(&file)? };
    let _ = mmap.advise(memmap2::Advice::WillNeed);
    from_mapping(Arc::new(Mapping::Mmap(mmap)), verify)
}

/// Loads a snapshot from bytes already in memory (tests, fuzzing).
pub fn load_bytes(bytes: &[u8], verify: Verify) -> Result<Snapshot, PersistError> {
    let mut words = vec![0u64; bytes.len().div_ceil(8)].into_boxed_slice();
    words.as_mut_bytes()[..bytes.len()].copy_from_slice(bytes);
    from_mapping(Arc::new(Mapping::Owned(words, bytes.len())), verify)
}

struct Sections<'a> {
    bytes: &'a [u8],
    table: Vec<SectionEntry>,
}

impl Sections<'_> {
    fn find(&self, kind: Kind, sub: u16) -> Option<&SectionEntry> {
        self.table
            .iter()
            .find(|s| s.kind == kind as u16 && s.sub == sub)
    }

    fn get(&self, kind: Kind, sub: u16) -> Result<&SectionEntry, PersistError> {
        self.find(kind, sub)
            .ok_or_else(|| PersistError::Corrupt(format!("missing section {kind:?}/{sub}")))
    }

    fn bytes(&self, kind: Kind, sub: u16) -> Result<&[u8], PersistError> {
        let s = self.get(kind, sub)?;
        slice(self.bytes, s.offset, s.len)
    }

    /// Typed copy of a small section.
    fn vec<T: Pod>(&self, kind: Kind, sub: u16) -> Result<Vec<T>, PersistError> {
        let b = self.bytes(kind, sub)?;
        <[T]>::ref_from_bytes(b)
            .map(<[T]>::to_vec)
            .map_err(|_| PersistError::Corrupt(format!("bad section {kind:?}")))
    }
}

fn mapped_column<T: Pod>(
    map: &Arc<Mapping>,
    secs: &Sections<'_>,
    kind: Kind,
    pages: usize,
) -> Result<Column<T>, PersistError> {
    let s = secs.get(kind, 0)?;
    let page_bytes = PAGE_SIZE * std::mem::size_of::<T>();
    if s.len != (pages * page_bytes) as u64 || s.elem_size as usize != std::mem::size_of::<T>() {
        return Err(PersistError::Corrupt(format!(
            "section {kind:?} has wrong size"
        )));
    }
    if s.offset % ALIGN != 0 {
        return Err(PersistError::Corrupt(format!(
            "section {kind:?} misaligned"
        )));
    }
    let base = usize::try_from(s.offset).map_err(|_| PersistError::Corrupt("offset".into()))?;
    let mut out = Vec::with_capacity(pages);
    for p in 0..pages {
        let m = MappedSlice::new(Arc::clone(map), base + p * page_bytes, page_bytes)
            .ok_or_else(|| PersistError::Corrupt(format!("section {kind:?} out of bounds")))?;
        out.push(Arc::new(Buf::Mapped(m)));
    }
    Ok(Column::from_pages(out, PAGE_SIZE))
}

fn from_mapping(map: Arc<Mapping>, verify: Verify) -> Result<Snapshot, PersistError> {
    let bytes = map.bytes();
    let header_bytes = bytes
        .get(..HEADER_LEN as usize)
        .ok_or(PersistError::BadMagic)?;
    let header = Header::read_from_bytes(header_bytes).map_err(|_| PersistError::BadMagic)?;
    if header.magic != MAGIC {
        return Err(PersistError::BadMagic);
    }
    if header.endian != ENDIAN_MARK {
        return Err(PersistError::Endianness);
    }
    if header.version != FORMAT_VERSION {
        return Err(PersistError::Incompatible {
            found: header.version,
        });
    }
    let mut check = header;
    check.header_checksum = 0;
    if xxh3_64(check.as_bytes()) != header.header_checksum {
        return Err(PersistError::Checksum("header".into()));
    }
    if header.page_size as usize != PAGE_SIZE {
        return Err(PersistError::Corrupt("page size".into()));
    }
    let table_len = u64::from(header.sections) * 32;
    let table_bytes = slice(bytes, header.table_offset, table_len)?;
    if xxh3_64(table_bytes) != header.table_checksum {
        return Err(PersistError::Checksum("section table".into()));
    }
    let table = <[SectionEntry]>::ref_from_bytes(table_bytes)
        .map_err(|_| PersistError::Corrupt("section table".into()))?
        .to_vec();
    let secs = Sections { bytes, table };

    // Verify checksums: essential sections now, the rest now or later.
    let mut now = Vec::new();
    let mut later = Vec::new();
    for s in &secs.table {
        let entry = (format!("{}/{}", s.kind, s.sub), s.offset, s.len, s.checksum);
        let essential = ESSENTIAL.iter().any(|k| *k as u16 == s.kind);
        if essential || verify == Verify::All {
            now.push(entry);
        } else {
            later.push(entry);
        }
    }
    verify_sections(bytes, &now)?;

    let meta: Meta = serde_json::from_slice(secs.bytes(Kind::Meta, 0)?)
        .map_err(|e| PersistError::Corrupt(format!("meta: {e}")))?;
    let pages = meta.pages;
    let page_volume: Vec<u16> = secs.vec(Kind::PageVolume, 0)?;
    if page_volume.len() != pages {
        return Err(PersistError::Corrupt("page volume table".into()));
    }
    // Names.
    let name_index: Vec<u64> = secs.vec(Kind::NameIndex, 0)?;
    let nb = secs.get(Kind::NameBytes, 0)?;
    let no = secs.get(Kind::NameOffs, 0)?;
    let offs_page_bytes = (PAGE_SIZE + 1) * 4;
    if name_index.len() != pages * 2 || no.len != (pages * offs_page_bytes) as u64 {
        return Err(PersistError::Corrupt("name sections".into()));
    }
    let nb_base = usize::try_from(nb.offset).map_err(|_| PersistError::Corrupt("offset".into()))?;
    let no_base = usize::try_from(no.offset).map_err(|_| PersistError::Corrupt("offset".into()))?;
    let mut names = Vec::with_capacity(pages);
    for p in 0..pages {
        let (start, len) = (name_index[2 * p], name_index[2 * p + 1]);
        if start + len > nb.len {
            return Err(PersistError::Corrupt("name index".into()));
        }
        let bytes_slice = MappedSlice::new(
            Arc::clone(&map),
            nb_base + usize::try_from(start).unwrap_or(usize::MAX),
            usize::try_from(len).unwrap_or(usize::MAX),
        )
        .ok_or_else(|| PersistError::Corrupt("names out of bounds".into()))?;
        let offs_slice = MappedSlice::new(
            Arc::clone(&map),
            no_base + p * offs_page_bytes,
            offs_page_bytes,
        )
        .ok_or_else(|| PersistError::Corrupt("name offsets out of bounds".into()))?;
        names.push(Arc::new(NamePage::from_parts(
            Buf::Mapped(bytes_slice),
            Buf::Mapped(offs_slice),
        )));
    }
    let has_atime = header.flags & FLAG_ATIME != 0;
    let dp = meta.dir_pages;
    let dirs = DirStore {
        entry: mapped_column(&map, &secs, Kind::DirEntry, dp)?,
        size: mapped_column(&map, &secs, Kind::DirSize, dp)?,
        mtime_ns: mapped_column(&map, &secs, Kind::DirMtime, dp)?,
        ctime_ns: mapped_column(&map, &secs, Kind::DirCtime, dp)?,
        dir_key: mapped_column(&map, &secs, Kind::DirKey, dp)?,
        files: mapped_column(&map, &secs, Kind::DirFiles, dp)?,
        folders: mapped_column(&map, &secs, Kind::DirFolders, dp)?,
        restricted: mapped_column(&map, &secs, Kind::DirRestricted, dp)?,
    };
    let owners_flat: Vec<u32> = secs.vec(Kind::Owners, 0)?;
    let owners: Vec<(u32, u32)> = owners_flat.chunks_exact(2).map(|c| (c[0], c[1])).collect();
    let big: Vec<DiskBigSize> = secs.vec(Kind::BigSize, 0)?;
    let odd: Vec<DiskOddTime> = secs.vec(Kind::OddTime, 0)?;
    let oo: Vec<DiskOwner> = secs.vec(Kind::OddOwner, 0)?;
    let side = SideTables {
        big_size: Arc::new(big.iter().map(|b| (b.id, b.size)).collect()),
        odd_time: Arc::new(
            odd.iter()
                .map(|o| ((o.id, u8::try_from(o.field).unwrap_or(0)), o.nanos))
                .collect(),
        ),
        odd_owner: Arc::new(oo.iter().map(|o| (o.id, (o.uid, o.gid))).collect()),
    };
    let data = IndexData {
        names,
        parent: mapped_column(&map, &secs, Kind::Parent, pages)?,
        size: mapped_column(&map, &secs, Kind::Size, pages)?,
        mtime: mapped_column(&map, &secs, Kind::Mtime, pages)?,
        ctime: mapped_column(&map, &secs, Kind::Ctime, pages)?,
        btime: mapped_column(&map, &secs, Kind::Btime, pages)?,
        atime: if has_atime {
            Some(mapped_column(&map, &secs, Kind::Atime, pages)?)
        } else {
            None
        },
        name_key: mapped_column(&map, &secs, Kind::NameKey, pages)?,
        mode: mapped_column(&map, &secs, Kind::Mode, pages)?,
        owner: mapped_column(&map, &secs, Kind::Owner, pages)?,
        inode: mapped_column(&map, &secs, Kind::Inode, pages)?,
        flags: mapped_column(&map, &secs, Kind::Flags, pages)?,
        attrs: mapped_column(&map, &secs, Kind::Attrs, pages)?,
        dirs,
        side,
        owners: Arc::new(owners),
        volumes: Arc::new(meta.volumes),
        page_volume: Arc::new(page_volume),
        live: meta.live,
    };
    // Orders are copied into owned blocks.
    let mut orders = Orders::default();
    for (i, kind) in OrderKind::ALL.into_iter().enumerate() {
        let sub = u16::try_from(i).unwrap_or(0);
        let ids: Vec<u32> = secs.vec(Kind::OrderIds, sub)?;
        let lens: Vec<u32> = secs.vec(Kind::OrderBlocks, sub)?;
        let fences: Vec<DiskFence> = secs.vec(Kind::OrderFences, sub)?;
        if lens.iter().map(|&l| l as usize).sum::<usize>() != ids.len()
            || fences.len() != lens.len()
            || ids.len() != data.live
        {
            return Err(PersistError::Corrupt(format!("order {kind:?}")));
        }
        let cap = u32::try_from(data.capacity()).unwrap_or(u32::MAX);
        if ids.iter().any(|&id| id >= cap) {
            return Err(PersistError::Corrupt(format!(
                "order {kind:?} id out of range"
            )));
        }
        let mut blocks = Vec::with_capacity(lens.len());
        let mut at = 0;
        for &l in &lens {
            let l = l as usize;
            blocks.push(Arc::<[u32]>::from(&ids[at..at + l]));
            at += l;
        }
        let fences: Vec<(SortKey, u32)> = fences.iter().map(|f| ((f.k0, f.k1), f.id)).collect();
        *orders.get_mut(kind) = Order::from_parts(blocks, fences);
    }
    let generation = Generation {
        data,
        orders,
        number: header.generation,
        changes: Arc::default(),
    };
    Ok(Snapshot {
        generation,
        map,
        deferred: later,
    })
}

#[cfg(test)]
mod tests;
