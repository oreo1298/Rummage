//! Fossick’s universal scanner: a parallel directory walk using raw `getdents64` and `statx`.
//!
//! - Symlinks are recorded as entries and never followed.
//! - Mount points are never crossed (compared by `stx_mnt_id`, so nested btrfs subvolumes, which
//!   have their own device numbers but live in the same mount, are followed).
//! - Each directory’s listing is sent to the consumer before its subdirectories are scanned, so
//!   the consumer always sees a parent before its children.

use std::ffi::OsStr;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use fossick_core::{Attrs, EntryId, EntryMeta, IndexWriter, Timestamp};
use rustix::fs::{AtFlags, Mode, OFlags, StatxAttributes, StatxFlags, openat, statx};

pub mod content;
pub mod handle;
pub mod mime;
mod prior;
mod priority;

pub use prior::IndexPrior;
pub use priority::lower_thread_priority;

/// Scanner configuration.
#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Worker threads (0 = number of CPUs).
    pub threads: usize,
    /// Run workers at idle I/O priority and nice 19.
    pub low_priority: bool,
    /// Descend into other mounts (off: mount points are recorded but not entered).
    pub cross_mounts: bool,
    /// Record mount-point directories themselves.
    pub include_mount_points: bool,
    /// Absolute paths that are skipped entirely.
    pub exclude: Vec<PathBuf>,
    /// Mount ids (`stx_mnt_id`) whose mount points are left out entirely, because they are
    /// indexed as volumes of their own.
    pub skip_mounts: Vec<u64>,
    /// Read each directory’s POSIX access ACL into [`Listing::acl`].
    pub read_acls: bool,
    /// Record each directory’s file handle in [`Listing::handle`] (for fanotify).
    pub capture_handles: bool,
    /// Called with each directory’s path and descriptor just before it is read (on scanner
    /// threads). inotify adds its watch here, so nothing created during the scan is missed.
    pub on_open: Option<OpenHook>,
}

/// The callback type of [`OpenHook`].
pub type OpenFn = dyn Fn(&Path, BorrowedFd<'_>) + Send + Sync;

/// See [`ScanOptions::on_open`].
#[derive(Clone)]
pub struct OpenHook(pub Arc<OpenFn>);

impl std::fmt::Debug for OpenHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenHook")
    }
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            threads: 0,
            low_priority: false,
            cross_mounts: false,
            include_mount_points: true,
            exclude: Vec::new(),
            skip_mounts: Vec::new(),
            read_acls: false,
            capture_handles: false,
            on_open: None,
        }
    }
}

/// What an earlier index knows about directories, for pruned rescans. Keys are opaque `u32`s
/// chosen by the implementation (the daemon uses entry ids).
pub trait Prior: Sync {
    /// Key of the scan root, if known.
    fn root(&self) -> Option<u32>;
    /// Key of the known directory `name` inside the known directory `parent`.
    fn child(&self, parent: u32, name: &[u8]) -> Option<u32>;
    /// If the directory `key` can’t have changed, given its current modification and change
    /// times (ns), returns the names of its known subdirectories. `None` means it must be
    /// listed again.
    fn unchanged(&self, key: u32, mtime_ns: i64, ctime_ns: i64) -> Option<Vec<Box<[u8]>>>;
}

/// One scanned entry.
#[derive(Clone, Debug)]
pub struct ScanEntry {
    pub name: Box<[u8]>,
    pub meta: EntryMeta,
    /// For directories that will be scanned: their scan id.
    pub dir: Option<u32>,
}

/// The listing of one directory.
#[derive(Debug, Default)]
pub struct Listing {
    /// Scan id of the listed directory (0 = the scan root).
    pub dir: u32,
    /// The directory’s [`Prior`] key, if it was known.
    pub prior: Option<u32>,
    /// The directory was unchanged (see [`Prior::unchanged`]): `entries` holds only its known
    /// subdirectories, freshly stat-ed, instead of a full listing.
    pub unchanged: bool,
    pub entries: Vec<ScanEntry>,
    /// Raw `system.posix_acl_access` value, if the directory has one (with `read_acls`).
    pub acl: Option<Vec<u8>>,
    /// The directory’s file handle (with `capture_handles`, where the filesystem supports it).
    pub handle: Option<handle::FileHandle>,
    /// The directory couldn’t be opened or read completely; `entries` may be partial.
    pub incomplete: bool,
    /// The directory’s (mtime, ctime) in ns, observed before it was read.
    pub times: Option<(i64, i64)>,
    /// The directory’s device and inode numbers (with an `on_open` hook).
    pub dev_ino: Option<(u64, u64)>,
}

/// Counters for one scan.
#[derive(Clone, Debug, Default)]
pub struct ScanStats {
    pub dirs: u64,
    pub entries: u64,
    pub errors: u64,
    pub mount_points: u64,
    /// Directories skipped as unchanged in a pruned rescan.
    pub unchanged_dirs: u64,
    pub elapsed: Duration,
    /// First few error messages.
    pub error_samples: Vec<String>,
}

/// Scanner errors.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("cannot open {path}: {source}")]
    Root {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot create scanner threads: {0}")]
    Threads(String),
}

/// Converts statx results into entry metadata.
#[must_use]
pub fn meta_from_statx(st: &rustix::fs::Statx) -> EntryMeta {
    let ts = |t: &rustix::fs::StatxTimestamp| Some(Timestamp::new(t.tv_sec, t.tv_nsec));
    let has = |f: StatxFlags| st.stx_mask & f.bits() != 0;
    let mut attrs = Attrs::empty();
    let a = st.stx_attributes & st.stx_attributes_mask;
    if a.contains(StatxAttributes::COMPRESSED) {
        attrs |= Attrs::COMPRESSED;
    }
    if a.contains(StatxAttributes::IMMUTABLE) {
        attrs |= Attrs::IMMUTABLE;
    }
    if a.contains(StatxAttributes::APPEND) {
        attrs |= Attrs::APPEND;
    }
    if a.contains(StatxAttributes::NODUMP) {
        attrs |= Attrs::NODUMP;
    }
    if a.contains(StatxAttributes::ENCRYPTED) {
        attrs |= Attrs::ENCRYPTED;
    }
    if a.contains(StatxAttributes::VERITY) {
        attrs |= Attrs::VERITY;
    }
    let mode = u32::from(st.stx_mode);
    if mode & fossick_core::meta::S_IFMT == fossick_core::meta::S_IFREG
        && st.stx_size > 0
        && st.stx_blocks.saturating_mul(512) < st.stx_size
    {
        attrs |= Attrs::SPARSE;
    }
    EntryMeta {
        mode,
        uid: st.stx_uid,
        gid: st.stx_gid,
        size: st.stx_size,
        mtime: if has(StatxFlags::MTIME) {
            ts(&st.stx_mtime)
        } else {
            None
        },
        ctime: if has(StatxFlags::CTIME) {
            ts(&st.stx_ctime)
        } else {
            None
        },
        btime: if has(StatxFlags::BTIME) {
            ts(&st.stx_btime)
        } else {
            None
        },
        atime: if has(StatxFlags::ATIME) {
            ts(&st.stx_atime)
        } else {
            None
        },
        inode: st.stx_ino,
        attrs,
        nlink: st.stx_nlink,
    }
}

const STATX_AT: AtFlags = AtFlags::SYMLINK_NOFOLLOW
    .union(AtFlags::STATX_DONT_SYNC)
    .union(AtFlags::NO_AUTOMOUNT);

fn statx_mask() -> StatxFlags {
    StatxFlags::BASIC_STATS | StatxFlags::BTIME | StatxFlags::MNT_ID
}

/// Stats the scan root itself.
pub fn stat_path(path: &Path) -> std::io::Result<(EntryMeta, u64, u64)> {
    let st = statx(rustix::fs::CWD, path, STATX_AT, statx_mask())?;
    let dev = (u64::from(st.stx_dev_major) << 32) | u64::from(st.stx_dev_minor);
    Ok((meta_from_statx(&st), st.stx_mnt_id, dev))
}

/// Stats `name` inside the directory `dir` (symlinks not followed), or `dir` itself when
/// `name` is empty. Returns the metadata and the mount id.
pub fn stat_at(dir: BorrowedFd<'_>, name: &[u8]) -> std::io::Result<(EntryMeta, u64)> {
    let flags = if name.is_empty() {
        STATX_AT | AtFlags::EMPTY_PATH
    } else {
        STATX_AT
    };
    let st = statx(dir, name, flags, statx_mask())?;
    Ok((meta_from_statx(&st), st.stx_mnt_id))
}

struct Shared<'p> {
    /// Listings go here in parallel scans; inline scans hand them to the sink directly.
    tx: Option<mpsc::SyncSender<Listing>>,
    next_id: AtomicU32,
    root_mnt: u64,
    root_dev: u64,
    mnt_id_ok: bool,
    opts: ScanOptions,
    prior: Option<&'p dyn Prior>,
    dirs: AtomicU64,
    entries: AtomicU64,
    errors: AtomicU64,
    mounts: AtomicU64,
    skipped: AtomicU64,
    samples: std::sync::Mutex<Vec<String>>,
}

impl Shared<'_> {
    fn error(&self, msg: String) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut s) = self.samples.lock()
            && s.len() < 10
        {
            s.push(msg);
        }
    }

    fn other_mount(&self, st: &rustix::fs::Statx) -> bool {
        if self.mnt_id_ok {
            st.stx_mnt_id != self.root_mnt
        } else {
            ((u64::from(st.stx_dev_major) << 32) | u64::from(st.stx_dev_minor)) != self.root_dev
        }
    }
}

/// A directory’s (mtime, ctime) in nanoseconds.
type Times = Option<(i64, i64)>;

/// A directory waiting to be scanned.
struct DirJob {
    fd: OwnedFd,
    path: PathBuf,
    id: u32,
    prior: Option<u32>,
    /// Current times, for the prior check.
    times: Times,
}

fn times_of(meta: &EntryMeta) -> Times {
    Some((meta.mtime?.as_nanos(), meta.ctime?.as_nanos()))
}

fn read_acl(fd: BorrowedFd<'_>) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 512];
    loop {
        match rustix::fs::fgetxattr(fd, fossick_core::perm::ACL_XATTR, &mut buf[..]) {
            Ok(n) => {
                buf.truncate(n);
                return Some(buf);
            }
            Err(rustix::io::Errno::RANGE) if buf.len() < 64 * 1024 => {
                let n = buf.len() * 4;
                buf.resize(n, 0);
            }
            // No ACL, or the filesystem has no ACL support.
            Err(_) => return None,
        }
    }
}

/// Stats the known subdirectories of an unchanged directory. `None` if anything is off (a
/// subdirectory vanished or isn’t a directory any more), in which case the caller lists it.
fn known_subdirs(
    shared: &Shared<'_>,
    fd: BorrowedFd<'_>,
    names: Vec<Box<[u8]>>,
) -> Option<Vec<ScanEntry>> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let st = statx(fd, &*name, STATX_AT, statx_mask()).ok()?;
        let meta = meta_from_statx(&st);
        if !meta.kind().is_dir() || shared.other_mount(&st) {
            return None;
        }
        out.push(ScanEntry {
            name,
            meta,
            dir: None,
        });
    }
    Some(out)
}

/// A subdirectory found by [`list_dir`], not opened yet.
struct Pending {
    parent: Arc<OwnedFd>,
    name: Box<[u8]>,
    path: PathBuf,
    id: u32,
    prior: Option<u32>,
    times: Times,
}

/// Reads one directory. Returns its listing and the subdirectories to descend into.
fn list_dir(shared: &Shared<'_>, job: DirJob) -> (Listing, Vec<Pending>) {
    let DirJob {
        fd,
        path,
        id,
        prior,
        times,
    } = job;
    shared.dirs.fetch_add(1, Ordering::Relaxed);
    let fd = Arc::new(fd);
    if let Some(hook) = &shared.opts.on_open {
        (hook.0)(&path, fd.as_fd());
    }
    let dev_ino = shared
        .opts
        .on_open
        .as_ref()
        .and_then(|_| rustix::fs::fstat(&*fd).ok())
        .map(|st| (st.st_dev, st.st_ino));
    let acl = if shared.opts.read_acls {
        read_acl(fd.as_fd())
    } else {
        None
    };
    let handle = if shared.opts.capture_handles {
        handle::handle_of_fd(fd.as_fd()).ok()
    } else {
        None
    };
    let mut listing = Listing {
        dir: id,
        prior,
        acl,
        handle,
        times,
        dev_ino,
        ..Listing::default()
    };
    // Pruned rescan: an unchanged directory is not listed, only its known subdirectories.
    let known = match (shared.prior, prior, times) {
        (Some(p), Some(key), Some((m, c))) => p
            .unchanged(key, m, c)
            .and_then(|names| known_subdirs(shared, fd.as_fd(), names)),
        _ => None,
    };
    // (name, scan id, current times) of each subdirectory to descend into.
    let mut subdirs: Vec<(Box<[u8]>, u32, Times)> = Vec::new();
    if let Some(mut entries) = known {
        shared.skipped.fetch_add(1, Ordering::Relaxed);
        listing.unchanged = true;
        for e in &mut entries {
            let cid = shared.next_id.fetch_add(1, Ordering::Relaxed);
            e.dir = Some(cid);
            subdirs.push((e.name.clone(), cid, times_of(&e.meta)));
        }
        listing.entries = entries;
    } else {
        let mut buf: Vec<MaybeUninit<u8>> = vec![MaybeUninit::uninit(); 64 * 1024];
        let mut dir = rustix::fs::RawDir::new(fd.as_fd(), &mut buf);
        while let Some(item) = dir.next() {
            let e = match item {
                Ok(e) => e,
                Err(err) => {
                    shared.error(format!("{}: {err}", path.display()));
                    listing.incomplete = true;
                    break;
                }
            };
            let name = e.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let st = match statx(fd.as_fd(), e.file_name(), STATX_AT, statx_mask()) {
                Ok(st) => st,
                Err(rustix::io::Errno::NOENT) => continue, // deleted while scanning
                Err(err) => {
                    shared.error(format!(
                        "{}: {err}",
                        path.join(OsStr::from_bytes(name)).display()
                    ));
                    listing.incomplete = true;
                    continue;
                }
            };
            let meta = meta_from_statx(&st);
            let mut child_id = None;
            if meta.kind().is_dir() {
                let child_path = path.join(OsStr::from_bytes(name));
                if shared.opts.exclude.contains(&child_path) {
                    continue;
                }
                if shared.other_mount(&st) && !shared.opts.cross_mounts {
                    shared.mounts.fetch_add(1, Ordering::Relaxed);
                    if !shared.opts.include_mount_points
                        || (shared.mnt_id_ok && shared.opts.skip_mounts.contains(&st.stx_mnt_id))
                    {
                        continue;
                    }
                } else {
                    let cid = shared.next_id.fetch_add(1, Ordering::Relaxed);
                    child_id = Some(cid);
                    subdirs.push((name.into(), cid, times_of(&meta)));
                }
            }
            listing.entries.push(ScanEntry {
                name: name.into(),
                meta,
                dir: child_id,
            });
        }
    }
    shared
        .entries
        .fetch_add(listing.entries.len() as u64, Ordering::Relaxed);
    let pending = subdirs
        .into_iter()
        .map(|(name, id, times)| Pending {
            parent: Arc::clone(&fd),
            path: path.join(OsStr::from_bytes(&name)),
            prior: match (shared.prior, prior) {
                (Some(p), Some(key)) => p.child(key, &name),
                _ => None,
            },
            name,
            id,
            times,
        })
        .collect();
    (listing, pending)
}

/// Opens a pending subdirectory. On failure returns an incomplete listing to report instead:
/// a directory that vanished may have been renamed during the scan, so its absence says nothing
/// about its contents.
fn open_pending(shared: &Shared<'_>, p: Pending) -> Result<DirJob, Box<Listing>> {
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NOATIME;
    let opened = openat(p.parent.as_fd(), &*p.name, flags, Mode::empty()).or_else(|_| {
        openat(
            p.parent.as_fd(),
            &*p.name,
            flags - OFlags::NOATIME,
            Mode::empty(),
        )
    });
    drop(p.parent);
    match opened {
        Ok(fd) => Ok(DirJob {
            fd,
            path: p.path,
            id: p.id,
            prior: p.prior,
            times: p.times,
        }),
        Err(err) => {
            if err != rustix::io::Errno::NOENT {
                shared.error(format!("{}: {err}", p.path.display()));
            }
            Err(Box::new(Listing {
                dir: p.id,
                prior: p.prior,
                incomplete: true,
                ..Listing::default()
            }))
        }
    }
}

/// Parallel driver: lists a directory, sends its listing, then scans subdirectories as rayon
/// tasks.
fn scan_dir<'s>(scope: &rayon::Scope<'s>, shared: &'s Shared<'s>, job: DirJob) {
    let (listing, pending) = list_dir(shared, job);
    // Send before scanning children: the consumer must see the parent first.
    let Some(tx) = &shared.tx else {
        return;
    };
    if tx.send(listing).is_err() {
        return;
    }
    for p in pending {
        scope.spawn(move |s| match open_pending(shared, p) {
            Ok(job) => scan_dir(s, shared, job),
            Err(listing) => {
                let _ = tx.send(*listing);
            }
        });
    }
}

/// Scans the tree under `root`, calling `sink` (on the calling thread) with each directory
/// listing, parents before children. Scan id 0 is `root` itself.
pub fn scan(
    root: &Path,
    opts: &ScanOptions,
    sink: impl FnMut(Listing),
) -> Result<ScanStats, ScanError> {
    scan_with_prior(root, opts, None, sink)
}

/// Like [`scan`], but directories that `prior` reports unchanged are not listed again (their
/// known subdirectories are still descended into).
pub fn scan_with_prior(
    root: &Path,
    opts: &ScanOptions,
    prior: Option<&dyn Prior>,
    mut sink: impl FnMut(Listing),
) -> Result<ScanStats, ScanError> {
    let start = Instant::now();
    let root_err = |source: std::io::Error| ScanError::Root {
        path: root.to_path_buf(),
        source,
    };
    let (root_meta, root_mnt, root_dev) = stat_path(root).map_err(root_err)?;
    let mnt_id_ok = root_mnt != 0;
    let fd = rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| root_err(e.into()))?;
    let threads = if opts.threads == 0 {
        std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
    } else {
        opts.threads
    };
    let low = opts.low_priority;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("fossick-scan-{i}"))
        .start_handler(move |_| {
            if low {
                lower_thread_priority();
            }
        })
        .build()
        .map_err(|e| ScanError::Threads(e.to_string()))?;
    let (tx, rx) = mpsc::sync_channel::<Listing>(4096);
    let shared = Shared {
        tx: Some(tx),
        next_id: AtomicU32::new(1),
        root_mnt,
        root_dev,
        mnt_id_ok,
        opts: opts.clone(),
        prior,
        dirs: AtomicU64::new(0),
        entries: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        mounts: AtomicU64::new(0),
        skipped: AtomicU64::new(0),
        samples: std::sync::Mutex::new(Vec::new()),
    };
    let root_job = DirJob {
        fd,
        path: root.to_path_buf(),
        id: 0,
        prior: prior.and_then(Prior::root),
        times: times_of(&root_meta),
    };
    std::thread::scope(|ts| {
        let shared = &shared;
        let pool = &pool;
        let worker = ts.spawn(move || {
            pool.scope(|s| scan_dir(s, shared, root_job));
        });
        // Receive until all senders are done (the worker holds the only sender via `shared`).
        loop {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(l) => sink(l),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if worker.is_finished() {
                        while let Ok(l) = rx.try_recv() {
                            sink(l);
                        }
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Ok(ScanStats {
        dirs: shared.dirs.load(Ordering::Relaxed),
        entries: shared.entries.load(Ordering::Relaxed),
        errors: shared.errors.load(Ordering::Relaxed),
        mount_points: shared.mounts.load(Ordering::Relaxed),
        unchanged_dirs: shared.skipped.load(Ordering::Relaxed),
        elapsed: start.elapsed(),
        error_samples: shared.samples.into_inner().unwrap_or_default(),
    })
}

/// What [`scan_inline`] reports.
#[derive(Debug)]
pub enum Visit<'a> {
    /// A directory (path, scan id) was opened and is about to be read. Watchers that must
    /// watch a directory before reading it (inotify) add their watch here.
    Opening(&'a Path, u32),
    /// A directory’s listing.
    Listed(Listing),
}

/// Like [`scan_with_prior`], but on the calling thread without a thread pool: for the small
/// directories that appear while watching (a new folder), where starting threads would cost
/// more than the scan.
pub fn scan_inline(
    root: &Path,
    opts: &ScanOptions,
    prior: Option<&dyn Prior>,
    mut sink: impl FnMut(Visit<'_>),
) -> Result<ScanStats, ScanError> {
    let start = Instant::now();
    let root_err = |source: std::io::Error| ScanError::Root {
        path: root.to_path_buf(),
        source,
    };
    let (root_meta, root_mnt, root_dev) = stat_path(root).map_err(root_err)?;
    let fd = rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|e| root_err(e.into()))?;
    let shared = Shared {
        tx: None,
        next_id: AtomicU32::new(1),
        root_mnt,
        root_dev,
        mnt_id_ok: root_mnt != 0,
        opts: opts.clone(),
        prior,
        dirs: AtomicU64::new(0),
        entries: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        mounts: AtomicU64::new(0),
        skipped: AtomicU64::new(0),
        samples: std::sync::Mutex::new(Vec::new()),
    };
    let mut stack = vec![DirJob {
        fd,
        path: root.to_path_buf(),
        id: 0,
        prior: prior.and_then(Prior::root),
        times: times_of(&root_meta),
    }];
    while let Some(job) = stack.pop() {
        sink(Visit::Opening(&job.path, job.id));
        let (listing, pending) = list_dir(&shared, job);
        sink(Visit::Listed(listing));
        // Depth-first; parents are always delivered before their children.
        for p in pending.into_iter().rev() {
            match open_pending(&shared, p) {
                Ok(job) => stack.push(job),
                Err(listing) => sink(Visit::Listed(*listing)),
            }
        }
    }
    Ok(ScanStats {
        dirs: shared.dirs.load(Ordering::Relaxed),
        entries: shared.entries.load(Ordering::Relaxed),
        errors: shared.errors.load(Ordering::Relaxed),
        mount_points: shared.mounts.load(Ordering::Relaxed),
        unchanged_dirs: shared.skipped.load(Ordering::Relaxed),
        elapsed: start.elapsed(),
        error_samples: shared.samples.into_inner().unwrap_or_default(),
    })
}

/// Applies listings to an [`IndexWriter`]: maps scan ids to entries, inserts and updates
/// entries, removes entries a full listing no longer contains, and records directory ACLs.
#[derive(Debug)]
pub struct Applier {
    map: Vec<Option<EntryId>>,
    read_acls: bool,
    /// Entries inserted, updated or removed.
    pub changes: u64,
    /// Files with more than one name seen so far: `(entry, inode)`. Callers drain it.
    pub multi_links: Vec<(EntryId, u64)>,
    /// Index errors (should stay 0).
    pub errors: u64,
}

impl Applier {
    /// An applier for a scan whose root (scan id 0) is `root_entry`.
    #[must_use]
    pub fn new(root_entry: EntryId, read_acls: bool) -> Self {
        Self {
            map: vec![Some(root_entry)],
            read_acls,
            changes: 0,
            multi_links: Vec::new(),
            errors: 0,
        }
    }

    /// The entry a scan id was applied as.
    #[must_use]
    pub fn entry_of(&self, scan_id: u32) -> Option<EntryId> {
        self.map.get(scan_id as usize).copied().flatten()
    }

    /// Applies one listing. Returns the listed directory’s entry, if it is (still) indexed.
    pub fn apply(&mut self, w: &mut IndexWriter, listing: Listing) -> Option<EntryId> {
        let parent = self.map.get(listing.dir as usize).copied().flatten()?;
        if !w.is_live(parent) {
            return None;
        }
        if self.read_acls {
            let acl = listing
                .acl
                .as_deref()
                .and_then(fossick_core::perm::parse_acl);
            let acl = match (&listing.acl, acl) {
                // An ACL we can’t parse: record a deny-all ACL so nobody but root sees inside.
                (Some(_), None) => Some(vec![fossick_core::perm::AclEntry {
                    tag: fossick_core::perm::tag::OTHER,
                    perm: 0,
                    id: 0,
                }]),
                (_, acl) => acl,
            };
            if w.set_acl(parent, acl.as_deref()).is_err() {
                self.errors += 1;
            }
        }
        let full = !listing.unchanged && !listing.incomplete;
        let mut seen: Vec<EntryId> = Vec::with_capacity(listing.entries.len());
        for e in listing.entries {
            // A subdirectory’s recorded times describe its own listing, which comes later:
            // keep the old ones (none for a new directory) until then.
            let prev_times = if e.meta.kind().is_dir() {
                w.lookup(parent, &e.name).and_then(|id| w.dir_times(id))
            } else {
                None
            };
            match w.upsert(parent, &e.name, &e.meta) {
                Ok(id) => {
                    if e.meta.kind().is_dir() {
                        let _ = w.set_dir_times(id, prev_times);
                    }
                    seen.push(id);
                    self.changes += 1;
                    if e.meta.nlink > 1 && !e.meta.kind().is_dir() {
                        self.multi_links.push((id, e.meta.inode));
                    }
                    if let Some(d) = e.dir {
                        let d = d as usize;
                        if self.map.len() <= d {
                            self.map.resize(d + 1, None);
                        }
                        self.map[d] = Some(id);
                    }
                }
                Err(_) => self.errors += 1,
            }
        }
        if full {
            // Remove children the directory no longer has.
            seen.sort_unstable();
            for child in w.children(parent) {
                if seen.binary_search(&child).is_err() && w.remove(child).is_ok() {
                    self.changes += 1;
                }
            }
        }
        if !listing.unchanged {
            let times = if listing.incomplete {
                None
            } else {
                listing.times
            };
            let _ = w.set_dir_times(parent, times);
        }
        Some(parent)
    }
}

/// Scans `root` and inserts everything below it under `root_entry` (normally a volume root).
pub fn scan_into(
    root: &Path,
    writer: &mut IndexWriter,
    root_entry: EntryId,
    opts: &ScanOptions,
) -> Result<ScanStats, ScanError> {
    let mut applier = Applier::new(root_entry, opts.read_acls);
    let mut stats = scan(root, opts, |listing| {
        applier.apply(writer, listing);
    })?;
    stats.errors += applier.errors;
    Ok(stats)
}

#[cfg(test)]
mod tests;
