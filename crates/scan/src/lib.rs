//! Fossick’s universal scanner: a parallel directory walk using raw `getdents64` and `statx`.
//!
//! - Symlinks are recorded as entries and never followed.
//! - Mount points are never crossed (compared by `stx_mnt_id`, so nested btrfs subvolumes, which
//!   have their own device numbers but live in the same mount, are followed).
//! - Each directory’s listing is sent to the consumer before its subdirectories are scanned, so
//!   the consumer always sees a parent before its children.

use std::ffi::OsStr;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use fossick_core::{Attrs, EntryId, EntryMeta, IndexWriter, Timestamp};
use rustix::fs::{AtFlags, Mode, OFlags, StatxAttributes, StatxFlags, openat, statx};

pub mod content;
pub mod mime;
mod priority;

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
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            threads: 0,
            low_priority: false,
            cross_mounts: false,
            include_mount_points: true,
            exclude: Vec::new(),
        }
    }
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
#[derive(Debug)]
pub struct Listing {
    /// Scan id of the listed directory (0 = the scan root).
    pub dir: u32,
    pub entries: Vec<ScanEntry>,
}

/// Counters for one scan.
#[derive(Clone, Debug, Default)]
pub struct ScanStats {
    pub dirs: u64,
    pub entries: u64,
    pub errors: u64,
    pub mount_points: u64,
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

struct Shared {
    tx: mpsc::SyncSender<Listing>,
    next_id: AtomicU32,
    root_mnt: u64,
    root_dev: u64,
    mnt_id_ok: bool,
    opts: ScanOptions,
    dirs: AtomicU64,
    entries: AtomicU64,
    errors: AtomicU64,
    mounts: AtomicU64,
    samples: std::sync::Mutex<Vec<String>>,
}

impl Shared {
    fn error(&self, msg: String) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut s) = self.samples.lock()
            && s.len() < 10
        {
            s.push(msg);
        }
    }
}

fn scan_dir<'s>(scope: &rayon::Scope<'s>, shared: &'s Shared, fd: OwnedFd, path: &Path, id: u32) {
    shared.dirs.fetch_add(1, Ordering::Relaxed);
    let fd = Arc::new(fd);
    let mut buf: Vec<MaybeUninit<u8>> = vec![MaybeUninit::uninit(); 64 * 1024];
    let mut dir = rustix::fs::RawDir::new(fd.as_fd(), &mut buf);
    let mut entries = Vec::new();
    let mut subdirs: Vec<(Box<[u8]>, u32)> = Vec::new();
    while let Some(item) = dir.next() {
        let e = match item {
            Ok(e) => e,
            Err(err) => {
                shared.error(format!("{}: {err}", path.display()));
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
                continue;
            }
        };
        let meta = meta_from_statx(&st);
        let is_dir = meta.kind().is_dir();
        let mut child_id = None;
        if is_dir {
            let other_mount = if shared.mnt_id_ok {
                st.stx_mnt_id != shared.root_mnt
            } else {
                ((u64::from(st.stx_dev_major) << 32) | u64::from(st.stx_dev_minor))
                    != shared.root_dev
            };
            let child_path = path.join(OsStr::from_bytes(name));
            if shared.opts.exclude.contains(&child_path) {
                continue;
            }
            if other_mount && !shared.opts.cross_mounts {
                shared.mounts.fetch_add(1, Ordering::Relaxed);
                if !shared.opts.include_mount_points {
                    continue;
                }
            } else {
                let cid = shared.next_id.fetch_add(1, Ordering::Relaxed);
                child_id = Some(cid);
                subdirs.push((name.into(), cid));
            }
        }
        entries.push(ScanEntry {
            name: name.into(),
            meta,
            dir: child_id,
        });
    }
    shared
        .entries
        .fetch_add(entries.len() as u64, Ordering::Relaxed);
    // Send before scanning children: the consumer must see the parent first.
    if shared.tx.send(Listing { dir: id, entries }).is_err() {
        return;
    }
    for (name, cid) in subdirs {
        let parent = Arc::clone(&fd);
        let child_path = path.join(OsStr::from_bytes(&name));
        scope.spawn(move |s| {
            let flags = OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NOATIME;
            let opened = openat(parent.as_fd(), &*name, flags, Mode::empty()).or_else(|_| {
                openat(
                    parent.as_fd(),
                    &*name,
                    flags - OFlags::NOATIME,
                    Mode::empty(),
                )
            });
            drop(parent);
            match opened {
                Ok(cfd) => scan_dir(s, shared, cfd, &child_path, cid),
                Err(rustix::io::Errno::NOENT) => {
                    let _ = shared.tx.send(Listing {
                        dir: cid,
                        entries: Vec::new(),
                    });
                }
                Err(err) => {
                    shared.error(format!("{}: {err}", child_path.display()));
                    // Still send an (empty) listing so consumers can account for every id.
                    let _ = shared.tx.send(Listing {
                        dir: cid,
                        entries: Vec::new(),
                    });
                }
            }
        });
    }
}

/// Scans the tree under `root`, calling `sink` (on the calling thread) with each directory
/// listing, parents before children. Scan id 0 is `root` itself.
pub fn scan(
    root: &Path,
    opts: &ScanOptions,
    mut sink: impl FnMut(Listing),
) -> Result<ScanStats, ScanError> {
    let start = Instant::now();
    let root_err = |source: std::io::Error| ScanError::Root {
        path: root.to_path_buf(),
        source,
    };
    let (_, root_mnt, root_dev) = stat_path(root).map_err(root_err)?;
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
        tx,
        next_id: AtomicU32::new(1),
        root_mnt,
        root_dev,
        mnt_id_ok,
        opts: opts.clone(),
        dirs: AtomicU64::new(0),
        entries: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        mounts: AtomicU64::new(0),
        samples: std::sync::Mutex::new(Vec::new()),
    };
    let root_path = root.to_path_buf();
    std::thread::scope(|ts| {
        let shared = &shared;
        let pool = &pool;
        let worker = ts.spawn(move || {
            pool.scope(|s| scan_dir(s, shared, fd, &root_path, 0));
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
        elapsed: start.elapsed(),
        error_samples: shared.samples.into_inner().unwrap_or_default(),
    })
}

/// Scans `root` and inserts everything below it under `root_entry` (normally a volume root).
pub fn scan_into(
    root: &Path,
    writer: &mut IndexWriter,
    root_entry: EntryId,
    opts: &ScanOptions,
) -> Result<ScanStats, ScanError> {
    let mut map: Vec<Option<EntryId>> = vec![Some(root_entry)];
    let mut insert_errors = 0u64;
    let mut stats = scan(root, opts, |listing| {
        let Some(parent) = map.get(listing.dir as usize).copied().flatten() else {
            return;
        };
        for e in listing.entries {
            match writer.upsert(parent, &e.name, &e.meta) {
                Ok(id) => {
                    if let Some(d) = e.dir {
                        let d = d as usize;
                        if map.len() <= d {
                            map.resize(d + 1, None);
                        }
                        map[d] = Some(id);
                    }
                }
                Err(_) => insert_errors += 1,
            }
        }
    })?;
    stats.errors += insert_errors;
    Ok(stats)
}

#[cfg(test)]
mod tests;
