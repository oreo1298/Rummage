//! The writer thread: the only code that changes the index.
//!
//! It keeps the volume table in sync with the mount table, scans volumes (pruned after a
//! snapshot load), applies watcher events by re-reading the filesystem, publishes generations
//! (at most every 50 ms under load) and writes snapshots in the background.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, atomic::Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fossick_core::perm::{AclEntry, parse_acl, tag};
use fossick_core::{EntryId, EntryMeta, FileKind, IndexConfig, IndexWriter, VolumeId, persist};
use fossick_proto::{ServerMode, UpdateMethod, VolumeState};
use fossick_scan::handle::{FileHandle, open_by_handle};
use fossick_scan::{
    Applier, IndexPrior, Listing, OpenHook, ScanOptions, Visit, scan_inline, scan_with_prior,
};

use crate::config::WatchMethod;
use crate::events::{Cmd, DirRef, FsEvent};
use crate::identity::{UuidMap, identify};
use crate::inotify::Inotify;
use crate::mounts::{Class, Planned, plan, read_mounts};
use crate::state::{Shared, VolumeInfo};
use crate::{fanotify, log};

/// Publish at most this often while events keep arriving.
const PUBLISH_EVERY: Duration = Duration::from_millis(50);
/// Publish this often during long scans so searches see progress.
const SCAN_PUBLISH_EVERY: Duration = Duration::from_millis(500);
/// Directories modified this close to the snapshot time are always re-listed (coarse
/// filesystem timestamps, e.g. 2 s on FAT).
const GRANULARITY_NS: i64 = 2_000_000_000;

/// `FOSSICK_CHECK_INVARIANTS=1` verifies the index after every publish (slow; for tests).
static CHECK_INVARIANTS: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("FOSSICK_CHECK_INVARIANTS").is_some());

/// How a volume is kept current.
#[derive(Debug)]
enum Watch {
    None,
    Fanotify(Arc<fanotify::Group>),
    Inotify(Arc<Inotify>),
    Poll,
}

/// Runtime state of one volume of the index.
#[derive(Debug)]
struct Vol {
    plan: Planned,
    identity: String,
    /// `stx_mnt_id` of the mount root (0 if unknown).
    mnt_id: u64,
    root_fd: Option<std::fs::File>,
    watch: Watch,
    state: VolumeState,
    method: UpdateMethod,
    note: String,
    next_poll: Option<Instant>,
    polls: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanKind {
    /// List everything (new volume, or a metadata refresh).
    Full,
    /// Skip directories whose timestamps are unchanged.
    Pruned,
}

/// The writer thread’s state.
pub struct Writer {
    shared: Arc<Shared>,
    w: IndexWriter,
    rx: Receiver<Cmd>,
    tx: Sender<Cmd>,
    vols: HashMap<VolumeId, Vol>,
    /// Directory file handles (per volume) → (entry, inode), for fanotify events.
    handles: HashMap<(VolumeId, FileHandle), (EntryId, u64)>,
    /// Names of hard-linked files: (volume, inode) → entries, and whether the volume was
    /// searched for all of them. An event names one path; the other names must follow.
    links: HashMap<(VolumeId, u64), (Vec<EntryId>, bool)>,
    queue: VecDeque<(VolumeId, ScanKind)>,
    /// When the loaded snapshot was written (ns), for pruned rescans; `None` without one.
    snapshot_time_ns: Option<i64>,
    last_publish: Instant,
    dirty: bool,
    last_snapshot: Instant,
    snapshot_job: Option<JoinHandle<()>>,
    /// Volumes whose metadata refresh is still due after startup.
    refresh_due: HashSet<VolumeId>,
    /// Events that couldn’t be decided yet, with their attempt counts.
    retry: Vec<(VolumeId, FsEvent, u8)>,
    /// Directories by (volume, device, inode), for inotify events.
    dir_inodes: HashMap<(VolumeId, u64, u64), EntryId>,
}

/// What an event’s directory is in the index.
#[derive(Debug, Clone, Copy)]
enum Resolved {
    Dir(EntryId),
    /// It no longer exists: the event is moot.
    Gone,
    /// It exists but the index doesn’t have it there yet: retry after this batch’s moves.
    Pending,
}

/// What the filesystem says about one directory entry.
enum ChildState {
    /// It exists: its metadata, mount id and real current path.
    Present(EntryMeta, u64, Vec<u8>),
    Gone,
    /// Can’t tell yet: its directory moved and the move isn’t in the index yet.
    Unknown,
}

fn handle_of(d: &DirRef) -> Option<&FileHandle> {
    match d {
        DirRef::Handle(h) => Some(h),
        DirRef::Entry(_) | DirRef::Inode(..) => None,
    }
}

/// The current path of an open file descriptor.
fn fd_path(fd: &OwnedFd) -> Option<Vec<u8>> {
    let link = std::fs::read_link(format!(
        "/proc/self/fd/{}",
        std::os::fd::AsRawFd::as_raw_fd(fd)
    ))
    .ok()?;
    let b = link.as_os_str().as_bytes().to_vec();
    // A deleted directory reads back as "… (deleted)".
    (!b.ends_with(b" (deleted)")).then_some(b)
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer")
            .field("volumes", &self.vols.len())
            .finish_non_exhaustive()
    }
}

/// The snapshot file.
#[must_use]
pub fn snapshot_path(shared: &Shared) -> PathBuf {
    shared.paths.state_dir.join("index.fsidx")
}

impl Writer {
    /// Loads the snapshot (serving it right away) and prepares the writer.
    #[must_use]
    pub fn new(shared: Arc<Shared>, rx: Receiver<Cmd>, tx: Sender<Cmd>) -> Self {
        let cfg = IndexConfig {
            track_atime: shared.config.index.track_atime,
        };
        let path = snapshot_path(&shared);
        let mut snapshot_time_ns = None;
        let w = match persist::load(&path, persist::Verify::All) {
            Ok(snap) => {
                let g = Arc::new(snap.generation);
                log::info(&format!(
                    "loaded snapshot {} ({} entries)",
                    path.display(),
                    g.len()
                ));
                shared.publish(Arc::clone(&g));
                snapshot_time_ns = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|d| i64::try_from(d.as_nanos()).ok());
                if g.tracks_atime() == cfg.track_atime {
                    IndexWriter::from_generation(&g, cfg)
                } else {
                    log::info("access-time setting changed: rebuilding the index");
                    snapshot_time_ns = None;
                    IndexWriter::new(cfg)
                }
            }
            Err(persist::PersistError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                IndexWriter::new(cfg)
            }
            Err(e) => {
                log::warn(&format!("ignoring snapshot {}: {e}", path.display()));
                IndexWriter::new(cfg)
            }
        };
        Self {
            shared,
            w,
            rx,
            tx,
            vols: HashMap::new(),
            handles: HashMap::new(),
            links: HashMap::new(),
            queue: VecDeque::new(),
            snapshot_time_ns,
            last_publish: Instant::now(),
            dirty: false,
            last_snapshot: Instant::now(),
            snapshot_job: None,
            refresh_due: HashSet::new(),
            retry: Vec::new(),
            dir_inodes: HashMap::new(),
        }
    }

    /// Runs until [`Cmd::Shutdown`].
    pub fn run(mut self) {
        self.sync_mounts();
        self.publish();
        loop {
            if let Some((vid, kind)) = self.queue.pop_front() {
                self.scan_volume(vid, kind);
                self.publish();
                self.handle_pending();
                if self.queue.is_empty() {
                    self.after_scans();
                }
                continue;
            }
            let timeout = self.next_timeout();
            match self.rx.recv_timeout(timeout) {
                Ok(Cmd::Shutdown) => break,
                Ok(cmd) => {
                    self.handle(cmd);
                    self.drain_batch();
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.on_timers();
        }
        self.shutdown();
    }

    /// Applies commands that queued up (during a scan), without blocking.
    fn handle_pending(&mut self) {
        while let Ok(cmd) = self.rx.try_recv() {
            if matches!(cmd, Cmd::Shutdown) {
                // Re-queue so the main loop sees it.
                let _ = self.tx.send(Cmd::Shutdown);
                break;
            }
            self.handle(cmd);
        }
        self.publish();
    }

    /// Keeps applying events for up to [`PUBLISH_EVERY`], then publishes.
    fn drain_batch(&mut self) {
        let start = Instant::now();
        while start.elapsed() < PUBLISH_EVERY {
            match self.rx.recv_timeout(Duration::from_millis(5)) {
                Ok(Cmd::Shutdown) => {
                    let _ = self.tx.send(Cmd::Shutdown);
                    break;
                }
                Ok(cmd) => self.handle(cmd),
                Err(_) => break,
            }
        }
        self.publish();
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Events(vid, events) => self.apply_events(vid, events),
            Cmd::Overflow(vid) => {
                log::warn(&format!(
                    "event queue overflow on {}: rescanning",
                    self.mount_display(vid)
                ));
                self.enqueue(vid, ScanKind::Pruned);
            }
            Cmd::WatchFailed(vid, why) => {
                if let Some(v) = self.vols.get_mut(&vid) {
                    v.watch = Watch::Poll;
                    v.method = UpdateMethod::Polling;
                    v.note = why;
                    v.next_poll = Some(Instant::now());
                }
                self.update_status();
            }
            Cmd::MountsChanged => self.sync_mounts(),
            Cmd::Rescan(path) => {
                let vids: Vec<VolumeId> = self
                    .vols
                    .iter()
                    .filter(|(_, v)| path.is_empty() || v.plan.mount.mount_point == path)
                    .map(|(&vid, _)| vid)
                    .collect();
                for vid in vids {
                    self.enqueue(vid, ScanKind::Full);
                }
            }
            Cmd::Shutdown => {}
        }
    }

    fn enqueue(&mut self, vid: VolumeId, kind: ScanKind) {
        if !self.queue.iter().any(|&(v, k)| v == vid && k == kind) {
            self.queue.push_back((vid, kind));
        }
        if let Some(v) = self.vols.get_mut(&vid) {
            v.state = VolumeState::Scanning;
        }
        self.update_status();
    }

    fn next_timeout(&self) -> Duration {
        let snap = Duration::from_secs(self.shared.config.index.snapshot_interval_secs.max(10));
        let mut t = snap.saturating_sub(self.last_snapshot.elapsed());
        for v in self.vols.values() {
            if let Some(next) = v.next_poll {
                t = t.min(next.saturating_duration_since(Instant::now()));
            }
        }
        t.max(Duration::from_millis(50))
    }

    fn on_timers(&mut self) {
        let interval = Duration::from_secs(self.shared.config.index.snapshot_interval_secs.max(10));
        if self.last_snapshot.elapsed() >= interval {
            self.snapshot(false);
        }
        let now = Instant::now();
        let poll = Duration::from_secs(self.shared.config.index.poll_interval_secs.max(10));
        let due: Vec<VolumeId> = self
            .vols
            .iter()
            .filter(|(_, v)| v.next_poll.is_some_and(|t| t <= now))
            .map(|(&vid, _)| vid)
            .collect();
        let full_every = self.shared.config.index.poll_full_every.max(1);
        for vid in due {
            let mut kind = ScanKind::Pruned;
            if let Some(v) = self.vols.get_mut(&vid) {
                v.next_poll = Some(now + poll);
                v.polls += 1;
                if v.polls % full_every == 0 {
                    kind = ScanKind::Full;
                }
            }
            self.enqueue(vid, kind);
        }
    }

    /// After the startup scans: snapshot, then (optionally) refresh file metadata.
    fn after_scans(&mut self) {
        self.snapshot(false);
        crate::sys::trim_heap();
        if self.shared.config.index.refresh_metadata {
            let due: Vec<VolumeId> = self.refresh_due.drain().collect();
            for vid in due {
                self.enqueue(vid, ScanKind::Full);
            }
        }
    }

    fn publish(&mut self) {
        if self.w.has_pending() {
            let g = self.w.publish();
            self.shared.publish(g);
            self.dirty = true;
            if *CHECK_INVARIANTS && let Err(e) = self.w.check_invariants() {
                log::error(&format!("index invariant broken: {e}"));
            }
        }
        self.last_publish = Instant::now();
    }

    fn mount_display(&self, vid: VolumeId) -> String {
        self.vols.get(&vid).map_or_else(
            || format!("volume {}", vid.0),
            |v| v.plan.mount.mount_path().display().to_string(),
        )
    }

    // ----- volumes -----

    fn sync_mounts(&mut self) {
        let mounts = match read_mounts() {
            Ok(m) => m,
            Err(e) => {
                log::warn(&format!("cannot read the mount table: {e}"));
                return;
            }
        };
        let planned = match self.shared.mode {
            ServerMode::System => plan(&mounts, &self.shared.config.index),
            ServerMode::User => user_plan(&mounts),
        };
        let uuids = UuidMap::load();
        let mut seen: HashSet<VolumeId> = HashSet::new();
        for mut p in planned {
            // The daemon’s own state never needs indexing.
            p.exclude.push(self.shared.paths.state_dir.clone());
            let ident = match self.shared.mode {
                ServerMode::System => identify(&p.mount, p.class, &uuids),
                ServerMode::User => crate::identity::Identity {
                    key: format!("home:{}", p.mount.mount_path().display()),
                    stable: true,
                },
            };
            let mount_path = p.mount.mount_point.clone();
            let Ok((root_meta, mnt_id, _)) = fossick_scan::stat_path(p.mount.mount_path()) else {
                continue;
            };
            let existing = match_volume(
                self.w.volumes(),
                &ident.key,
                &p.mount.fstype,
                &mount_path,
                &seen,
            );
            if let Some(vid) = existing
                && let Some(v) = self.w.volumes().get(vid.0 as usize)
                && v.key != ident.key
                && is_fallback(&v.key)
            {
                log::info(&format!("volume {} is now known as {}", v.key, ident.key));
                self.w.set_volume_key(vid, &ident.key);
            }
            let existing = existing
                .and_then(|vid| self.w.volumes().get(vid.0 as usize))
                .map(|v| (v.id, v.online, v.mount_path.clone()));
            let (vid, kind) = match existing {
                Some((vid, online, old_path)) => {
                    if old_path != mount_path {
                        log::info(&format!(
                            "volume {} moved from {} to {}",
                            ident.key,
                            String::from_utf8_lossy(&old_path),
                            String::from_utf8_lossy(&mount_path)
                        ));
                        let _ = self.w.set_volume_mount(vid, &mount_path);
                    }
                    if !online {
                        self.w.set_volume_online(vid, true);
                    }
                    let known = self.vols.contains_key(&vid);
                    let kind = if known {
                        None
                    } else if self.snapshot_time_ns.is_some() {
                        Some(ScanKind::Pruned)
                    } else {
                        Some(ScanKind::Full)
                    };
                    (vid, kind)
                }
                None => match self.w.add_volume(
                    &ident.key,
                    &mount_path,
                    &p.mount.fstype,
                    uuids.label(p.mount.major, p.mount.minor),
                    &root_meta,
                ) {
                    Ok((vid, _)) => {
                        log::info(&format!(
                            "new volume {} ({}, {})",
                            p.mount.mount_path().display(),
                            p.mount.fstype,
                            ident.key
                        ));
                        (vid, Some(ScanKind::Full))
                    }
                    Err(e) => {
                        log::warn(&format!(
                            "cannot add volume {}: {e}",
                            p.mount.mount_path().display()
                        ));
                        continue;
                    }
                },
            };
            seen.insert(vid);
            if let Some(root) = self.w.volumes().get(vid.0 as usize).map(|v| v.root) {
                let _ = self.w.update(root, &root_meta);
            }
            if let Some(kind) = kind {
                let class = p.class;
                let root_fd = std::fs::File::open(p.mount.mount_path()).ok();
                let mut vol = Vol {
                    plan: p,
                    identity: ident.key.clone(),
                    mnt_id,
                    root_fd,
                    watch: Watch::None,
                    state: VolumeState::Scanning,
                    method: UpdateMethod::Static,
                    note: String::new(),
                    next_poll: None,
                    polls: 0,
                };
                self.start_watch(vid, &mut vol, class);
                self.vols.insert(vid, vol);
                if kind == ScanKind::Pruned {
                    self.refresh_due.insert(vid);
                }
                self.enqueue(vid, kind);
            } else if let Some(v) = self.vols.get_mut(&vid) {
                v.plan = p;
                v.mnt_id = mnt_id;
            }
        }
        // Volumes that went away.
        let gone: Vec<VolumeId> = self
            .w
            .volumes()
            .iter()
            .filter(|v| v.online && !seen.contains(&v.id))
            .map(|v| v.id)
            .collect();
        for vid in gone {
            log::info(&format!("volume {} is offline", self.mount_display(vid)));
            self.w.set_volume_online(vid, false);
            if let Some(v) = self.vols.remove(&vid) {
                stop_watch(&v.watch);
            }
            self.handles.retain(|(v, _), _| *v != vid);
            self.queue.retain(|&(v, _)| v != vid);
            if !self.shared.config.index.keep_offline
                && let Some(root) = self.w.volumes().get(vid.0 as usize).map(|v| v.root)
            {
                for child in self.w.children(root) {
                    let _ = self.w.remove(child);
                }
            }
        }
        // Mounts inside other volumes are separate volumes: their mount points are skipped.
        self.update_status();
    }

    fn start_watch(&mut self, vid: VolumeId, vol: &mut Vol, class: Class) {
        match class {
            Class::Static => {
                vol.method = UpdateMethod::Static;
                vol.note = "read-only: indexed once".into();
                return;
            }
            Class::Network => {
                vol.watch = Watch::Poll;
                vol.method = UpdateMethod::Polling;
                vol.note = "network filesystem: polled".into();
                vol.next_poll = Some(
                    Instant::now()
                        + Duration::from_secs(self.shared.config.index.poll_interval_secs),
                );
                return;
            }
            _ => {}
        }
        let method = self.shared.config.index.watch_method;
        if method == WatchMethod::Poll {
            vol.watch = Watch::Poll;
            vol.method = UpdateMethod::Polling;
            vol.note = "polling (watch_method = \"poll\")".into();
            vol.next_poll = Some(
                Instant::now() + Duration::from_secs(self.shared.config.index.poll_interval_secs),
            );
            return;
        }
        if self.shared.mode == ServerMode::System
            && matches!(method, WatchMethod::Auto | WatchMethod::Fanotify)
        {
            match fanotify::Group::new(vol.plan.mount.mount_path()) {
                Ok(group) => {
                    let group = Arc::new(group);
                    let reader = Arc::clone(&group);
                    let tx = self.tx.clone();
                    let spawned = std::thread::Builder::new()
                        .name(format!("fossick-fan-{}", vid.0))
                        .spawn(move || {
                            if let Err(e) = reader.run(vid, &tx) {
                                let _ = tx.send(Cmd::WatchFailed(
                                    vid,
                                    format!("fanotify stopped ({e}): polled"),
                                ));
                            }
                        });
                    if spawned.is_ok() {
                        vol.note = if group.renames {
                            String::new()
                        } else {
                            "kernel without FAN_RENAME: renamed folders are rescanned".into()
                        };
                        vol.watch = Watch::Fanotify(group);
                        vol.method = UpdateMethod::Live;
                        return;
                    }
                }
                Err(e) => {
                    vol.note = format!("fanotify unavailable ({e}); using inotify");
                }
            }
        }
        match Inotify::new() {
            Ok(ino) => {
                let reader = Arc::clone(&ino);
                let tx = self.tx.clone();
                let spawned = std::thread::Builder::new()
                    .name(format!("fossick-ino-{}", vid.0))
                    .spawn(move || reader.run(vid, &tx));
                if spawned.is_ok() {
                    vol.watch = Watch::Inotify(ino);
                    vol.method = UpdateMethod::Inotify;
                    return;
                }
            }
            Err(e) => vol.note = format!("inotify unavailable ({e}); polled"),
        }
        vol.watch = Watch::Poll;
        vol.method = UpdateMethod::Polling;
        vol.next_poll =
            Some(Instant::now() + Duration::from_secs(self.shared.config.index.poll_interval_secs));
    }

    fn update_status(&self) {
        let infos = self
            .vols
            .iter()
            .map(|(&vid, v)| VolumeInfo {
                vid,
                identity: v.identity.clone(),
                state: v.state,
                method: v.method,
                note: v.note.clone(),
            })
            .collect();
        self.shared.set_volumes(infos);
    }

    // ----- scanning -----

    fn scan_options(&self, vid: VolumeId, low_priority: bool) -> ScanOptions {
        let Some(vol) = self.vols.get(&vid) else {
            return ScanOptions::default();
        };
        let skip_mounts = self
            .vols
            .iter()
            .filter(|(v, _)| **v != vid)
            .map(|(_, v)| v.mnt_id)
            .filter(|&m| m != 0)
            .collect();
        let on_open = match &vol.watch {
            Watch::Inotify(ino) => {
                let ino = Arc::clone(ino);
                Some(OpenHook(Arc::new(move |_: &std::path::Path, fd| {
                    let _ = ino.add(fd);
                })))
            }
            _ => None,
        };
        ScanOptions {
            threads: self.shared.config.index.scan_threads,
            low_priority,
            cross_mounts: false,
            include_mount_points: true,
            exclude: vol.plan.exclude.clone(),
            skip_mounts,
            read_acls: true,
            capture_handles: matches!(vol.watch, Watch::Fanotify(_)),
            on_open,
        }
    }

    fn scan_volume(&mut self, vid: VolumeId, kind: ScanKind) {
        let Some(vol) = self.vols.get(&vid) else {
            return;
        };
        // FAT-family directory times can’t prove a directory unchanged: the root directory has
        // none, and the rest have 2-second resolution. Every rescan there reads everything.
        let kind = if prunable(&vol.plan.mount.fstype) {
            kind
        } else {
            ScanKind::Full
        };
        let path = vol.plan.mount.mount_path().to_path_buf();
        let Some(root) = self.w.volumes().get(vid.0 as usize).map(|v| v.root) else {
            return;
        };
        let opts = self.scan_options(vid, true);
        let g = self.shared.current();
        let prior = match (kind, self.snapshot_time_ns) {
            (ScanKind::Pruned, Some(t)) => Some(IndexPrior::new(
                Arc::clone(&g),
                root,
                t.saturating_sub(GRANULARITY_NS),
            )),
            (ScanKind::Pruned, None) => {
                // Without a snapshot time, compare against the last publish (rescans).
                let now = jiff::Timestamp::now().as_nanosecond();
                Some(IndexPrior::new(
                    Arc::clone(&g),
                    root,
                    i64::try_from(now).unwrap_or(i64::MAX) - GRANULARITY_NS,
                ))
            }
            (ScanKind::Full, _) => None,
        };
        drop(g);
        let started = Instant::now();
        log::info(&format!(
            "scanning {} ({})",
            path.display(),
            if prior.is_some() { "pruned" } else { "full" }
        ));
        let mut applier = Applier::new(root, true);
        let mut last = Instant::now();
        let result = scan_with_prior(
            &path,
            &opts,
            prior.as_ref().map(|p| p as &dyn fossick_scan::Prior),
            |listing| {
                self.apply_listing(vid, &mut applier, listing);
                if last.elapsed() >= SCAN_PUBLISH_EVERY {
                    self.publish();
                    last = Instant::now();
                }
            },
        );
        match result {
            Ok(stats) => {
                log::info(&format!(
                    "scanned {}: {} directories ({} unchanged), {} entries, {} errors in {:.2} s",
                    path.display(),
                    stats.dirs,
                    stats.unchanged_dirs,
                    stats.entries,
                    stats.errors,
                    started.elapsed().as_secs_f64()
                ));
                if let Some(v) = self.vols.get_mut(&vid) {
                    v.state = VolumeState::Ready;
                    if stats.errors > 0 && v.note.is_empty() {
                        v.note = format!("{} entries could not be read", stats.errors);
                    }
                }
                self.check_watch_limit(vid);
            }
            Err(e) => {
                log::warn(&format!("cannot scan {}: {e}", path.display()));
                if let Some(v) = self.vols.get_mut(&vid) {
                    v.state = VolumeState::Error;
                    v.note = e.to_string();
                }
            }
        }
        self.update_status();
    }

    /// Applies one listing and records what the watchers need about the directory.
    fn apply_listing(&mut self, vid: VolumeId, applier: &mut Applier, mut listing: Listing) {
        let handle = listing.handle.take();
        let dev_ino = listing.dev_ino;
        let (entries, unchanged) = (listing.entries.len(), listing.unchanged);
        let dir = applier.apply(&mut self.w, listing);
        for (_, entry, inode) in applier.mismatched.drain(..) {
            log::debug(&format!(
                "not applying a listing of inode {inode} to {} (inode {})",
                String::from_utf8_lossy(&self.w.path(entry)),
                self.w.meta(entry).map_or(0, |m| m.inode)
            ));
        }
        for (id, inode) in applier.multi_links.drain(..) {
            let (ids, _) = self.links.entry((vid, inode)).or_default();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let Some(dir) = dir else {
            return;
        };
        if log::debugging() {
            log::debug(&format!(
                "listed {}: {} entries{}",
                String::from_utf8_lossy(&self.w.path(dir)),
                entries,
                if unchanged { " (unchanged)" } else { "" }
            ));
        }
        if let Some(h) = handle {
            let inode = self.w.meta(dir).map_or(0, |m| m.inode);
            self.handles.insert((vid, h), (dir, inode));
        }
        if let Some((dev, ino)) = dev_ino {
            self.dir_inodes.insert((vid, dev, ino), dir);
        }
    }

    /// Switches a volume to polling when inotify ran out of watches.
    fn check_watch_limit(&mut self, vid: VolumeId) {
        let full = matches!(
            self.vols.get(&vid),
            Some(Vol { watch: Watch::Inotify(ino), .. }) if ino.full.load(Ordering::Relaxed)
        );
        if full {
            let limit = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
                .unwrap_or_default();
            log::warn(&format!(
                "{}: inotify watch limit reached (fs.inotify.max_user_watches = {}); polling",
                self.mount_display(vid),
                limit.trim()
            ));
            if let Some(v) = self.vols.get(&vid) {
                stop_watch(&v.watch);
            }
            self.handle(Cmd::WatchFailed(
                vid,
                format!(
                    "inotify watch limit reached (fs.inotify.max_user_watches = {}): polled",
                    limit.trim()
                ),
            ));
        }
    }

    // ----- events -----

    fn apply_events(&mut self, vid: VolumeId, events: Vec<FsEvent>) {
        if !self.vols.contains_key(&vid) {
            return;
        }
        // Many events name the same entry (every write to a file): handle each once.
        let mut seen = HashSet::new();
        let mut deferred = Vec::new();
        for ev in events {
            if seen.insert(ev.clone()) && !self.apply_event(vid, &ev) {
                deferred.push(ev);
            }
        }
        // Events whose directory had moved: the renames of this batch are applied now.
        let mut retry: Vec<(FsEvent, u8)> = std::mem::take(&mut self.retry)
            .into_iter()
            .filter(|(v, _, _)| *v == vid)
            .map(|(_, e, n)| (e, n))
            .collect();
        retry.extend(deferred.into_iter().map(|e| (e, 0)));
        let mut rescan = false;
        for (ev, tries) in retry {
            if self.apply_event(vid, &ev) {
                continue;
            }
            if tries >= 3 {
                rescan = true;
            } else {
                self.retry.push((vid, ev, tries + 1));
            }
        }
        if rescan {
            log::debug("events stayed ambiguous: rescanning");
            self.enqueue(vid, ScanKind::Pruned);
        }
    }

    /// Applies one event. Returns false if it can’t be decided yet (its directory moved and the
    /// move hasn’t been applied).
    fn apply_event(&mut self, vid: VolumeId, ev: &FsEvent) -> bool {
        match ev {
            FsEvent::Child { dir, name } => match self.resolve(vid, dir) {
                Resolved::Dir(d) => self.refresh_child(vid, d, name, handle_of(dir)),
                Resolved::Gone => {
                    log::debug(&format!(
                        "dropped event for {} (its directory is gone)",
                        String::from_utf8_lossy(name)
                    ));
                    true
                }
                Resolved::Pending => {
                    log::debug(&format!(
                        "directory of {} not indexed yet",
                        String::from_utf8_lossy(name)
                    ));
                    false
                }
            },
            FsEvent::Itself { dir } => match self.resolve(vid, dir) {
                Resolved::Dir(d) => self.refresh_itself(vid, d, handle_of(dir)),
                Resolved::Gone => true,
                Resolved::Pending => false,
            },
            FsEvent::Rename {
                from_dir,
                from_name,
                to_dir,
                to_name,
            } => {
                // `None` (wait) if a side’s directory isn’t indexed yet.
                fn side(r: Resolved, d: &DirRef) -> Option<Option<(EntryId, Option<&FileHandle>)>> {
                    match r {
                        Resolved::Dir(id) => Some(Some((id, handle_of(d)))),
                        Resolved::Gone => Some(None),
                        Resolved::Pending => None,
                    }
                }
                let from = self.resolve(vid, from_dir);
                let to = self.resolve(vid, to_dir);
                match (side(from, from_dir), side(to, to_dir)) {
                    (Some(from), Some(to)) => self.apply_rename(vid, from, from_name, to, to_name),
                    _ => false,
                }
            }
        }
    }

    /// Finds the indexed directory an event refers to.
    fn resolve(&mut self, vid: VolumeId, dir: &DirRef) -> Resolved {
        let is_dir = |w: &IndexWriter, id: EntryId, inode: u64| {
            w.is_live(id)
                && w.kind(id) == FileKind::Dir
                && w.meta(id).is_some_and(|m| m.inode == inode)
        };
        match dir {
            DirRef::Entry(id) => {
                if self.w.is_live(*id) && self.w.kind(*id) == FileKind::Dir {
                    Resolved::Dir(*id)
                } else {
                    Resolved::Gone
                }
            }
            // inotify: a watched directory whose listing was never applied, or that was
            // removed from the index, is gone (or replaced, which its parent’s events cover).
            DirRef::Inode(dev, ino) => match self.dir_inodes.get(&(vid, *dev, *ino)) {
                Some(&id) if is_dir(&self.w, id, *ino) => Resolved::Dir(id),
                other => {
                    log::debug(&format!(
                        "event for unindexed directory {dev}:{ino} (mapped: {})",
                        other.is_some()
                    ));
                    Resolved::Gone
                }
            },
            DirRef::Handle(h) => {
                let key = (vid, h.clone());
                if let Some(&(id, inode)) = self.handles.get(&key) {
                    if is_dir(&self.w, id, inode) {
                        return Resolved::Dir(id);
                    }
                    self.handles.remove(&key);
                }
                // Not seen during a scan: open the handle and look its path up.
                let fd = match self.open_dir_handle(vid, h) {
                    Ok(fd) => fd,
                    // The directory no longer exists.
                    Err(e) if matches!(e.raw_os_error(), Some(libc::ESTALE | libc::ENOENT)) => {
                        return Resolved::Gone;
                    }
                    Err(_) => return Resolved::Pending,
                };
                let inode = rustix::fs::fstat(&fd).map_or(0, |st| st.st_ino);
                // It exists, so if the index doesn’t have it where it is (a move or a creation
                // not applied yet), its events must wait rather than be dropped.
                match fd_path(&fd).and_then(|p| self.w.lookup_path(&p)) {
                    Some(id) if is_dir(&self.w, id, inode) => {
                        self.handles.insert(key, (id, inode));
                        Resolved::Dir(id)
                    }
                    _ => Resolved::Pending,
                }
            }
        }
    }

    fn open_dir_handle(&self, vid: VolumeId, h: &FileHandle) -> std::io::Result<OwnedFd> {
        let root_fd = self
            .vols
            .get(&vid)
            .and_then(|v| v.root_fd.as_ref())
            .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EBADF))?;
        open_by_handle(root_fd.as_fd(), h, libc::O_PATH | libc::O_DIRECTORY)
    }

    /// What the filesystem says about `name` in the indexed directory `dir`. With the event’s
    /// directory handle, the real directory is examined even if it was renamed and the index
    /// doesn’t know yet; otherwise the index’s path is used, and a missing entry only counts as
    /// gone if its directory is still where the index thinks.
    fn child_state(
        &self,
        vid: VolumeId,
        dir: EntryId,
        via: Option<&FileHandle>,
        name: &[u8],
    ) -> ChildState {
        let gone = |e: &std::io::Error| {
            matches!(
                e.raw_os_error(),
                Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP)
            )
        };
        if let Some(h) = via {
            match self.open_dir_handle(vid, h) {
                Ok(fd) => {
                    let Some(mut path) = fd_path(&fd) else {
                        return ChildState::Unknown;
                    };
                    path.push(b'/');
                    path.extend_from_slice(name);
                    return match fossick_scan::stat_at(fd.as_fd(), name) {
                        Ok((meta, mnt)) => ChildState::Present(meta, mnt, path),
                        Err(e) if gone(&e) => ChildState::Gone,
                        Err(_) => ChildState::Unknown,
                    };
                }
                // The directory itself no longer exists, so neither does the entry.
                Err(e) if e.raw_os_error() == Some(libc::ESTALE) => return ChildState::Gone,
                Err(_) => {}
            }
        }
        let dir_path = self.w.path(dir);
        let mut path = dir_path.clone();
        path.push(b'/');
        path.extend_from_slice(name);
        match fossick_scan::stat_path(&bytes_path(&path)) {
            Ok((meta, mnt, _)) => ChildState::Present(meta, mnt, path),
            Err(e) if gone(&e) => {
                let dir_inode = self.w.meta(dir).map(|m| m.inode);
                match fossick_scan::stat_path(&bytes_path(&dir_path)) {
                    Ok((m, _, _)) if m.kind().is_dir() && Some(m.inode) == dir_inode => {
                        ChildState::Gone
                    }
                    _ => ChildState::Unknown,
                }
            }
            Err(_) => ChildState::Unknown,
        }
    }

    /// Brings the index entry for `dir/name` in line with the filesystem. Returns false if that
    /// can’t be decided yet.
    fn refresh_child(
        &mut self,
        vid: VolumeId,
        dir: EntryId,
        name: &[u8],
        via: Option<&FileHandle>,
    ) -> bool {
        if name.is_empty() || name.contains(&b'/') {
            return true;
        }
        let existing = self.w.lookup(dir, name);
        let state = self.child_state(vid, dir, via, name);
        if log::debugging() {
            log::debug(&format!(
                "refresh {}/{} (indexed: {}, on disk: {})",
                String::from_utf8_lossy(&self.w.path(dir)),
                String::from_utf8_lossy(name),
                existing.is_some(),
                match &state {
                    ChildState::Present(m, _, _) => format!("{:?} size {}", m.kind(), m.size),
                    ChildState::Gone => "gone".into(),
                    ChildState::Unknown => "unknown".into(),
                }
            ));
        }
        match state {
            ChildState::Unknown => false,
            ChildState::Gone => {
                if let Some(id) = existing {
                    let _ = self.w.remove(id);
                }
                true
            }
            ChildState::Present(meta, mnt_id, path) => {
                let vol_mnt = self.vols.get(&vid).map_or(0, |v| v.mnt_id);
                if meta.kind().is_dir() && mnt_id != 0 && vol_mnt != 0 && mnt_id != vol_mnt {
                    // A mount point of another filesystem: indexed as its own volume, or
                    // recorded without its contents.
                    if self.vols.values().any(|v| v.mnt_id == mnt_id) {
                        if let Some(id) = existing {
                            let _ = self.w.remove(id);
                        }
                    } else {
                        let _ = self.w.upsert(dir, name, &meta);
                    }
                    return true;
                }
                if self.is_excluded(vid, &path) {
                    return true;
                }
                match existing {
                    Some(id)
                        if self.w.kind(id) == meta.kind()
                            && (!meta.kind().is_dir()
                                || self.w.meta(id).is_some_and(|m| m.inode == meta.inode)) =>
                    {
                        let _ = self.w.update(id, &meta);
                        if meta.kind().is_dir() {
                            self.refresh_acl(id, &path);
                        } else if meta.nlink > 1 {
                            self.sync_links(vid, id, &meta);
                        }
                    }
                    Some(id) => {
                        let _ = self.w.remove(id);
                        self.insert_new(vid, dir, name, &meta, &path);
                    }
                    None => self.insert_new(vid, dir, name, &meta, &path),
                }
                true
            }
        }
    }

    /// Re-reads a directory’s own metadata (through its parent, or directly for a volume root).
    fn refresh_itself(&mut self, vid: VolumeId, dir: EntryId, via: Option<&FileHandle>) -> bool {
        if let Some(h) = via
            && let Ok(fd) = self.open_dir_handle(vid, h)
            && let (Ok((meta, _)), Some(path)) =
                (fossick_scan::stat_at(fd.as_fd(), b""), fd_path(&fd))
        {
            let _ = self.w.update(dir, &meta);
            self.refresh_acl(dir, &path);
            return true;
        }
        match self.w.parent(dir) {
            Some(parent) => {
                let name = self.w.name(dir).to_vec();
                self.refresh_child(vid, parent, &name, None)
            }
            None => {
                let path = self.w.path(dir);
                if let Ok((meta, _, _)) = fossick_scan::stat_path(&bytes_path(&path)) {
                    let _ = self.w.update(dir, &meta);
                    self.refresh_acl(dir, &path);
                }
                true
            }
        }
    }

    fn is_excluded(&self, vid: VolumeId, path: &[u8]) -> bool {
        self.vols.get(&vid).is_some_and(|v| {
            v.plan.exclude.iter().any(|e| {
                let e = e.as_os_str().as_bytes();
                path == e || (path.starts_with(e) && path.get(e.len()) == Some(&b'/'))
            })
        })
    }

    /// Copies a hard-linked file’s fresh metadata to its other names.
    fn sync_links(&mut self, vid: VolumeId, id: EntryId, meta: &EntryMeta) {
        let key = (vid, meta.inode);
        let (mut ids, mut searched) = self.links.remove(&key).unwrap_or_default();
        let w = &self.w;
        ids.retain(|&x| {
            w.is_live(x) && !w.kind(x).is_dir() && w.meta(x).is_some_and(|m| m.inode == meta.inode)
        });
        if !ids.contains(&id) {
            ids.push(id);
        }
        if ids.len() < 2 && !searched {
            // First time we see this file with several names: find the others.
            ids = self.w.entries_with_inode(vid, meta.inode);
            searched = true;
        }
        for &other in &ids {
            if other != id {
                let _ = self.w.update(other, meta);
            }
        }
        self.links.insert(key, (ids, searched));
    }

    /// Inserts a new entry; a new directory is scanned right away (it may already have
    /// contents: moved in, or filled before we got here). `path` is its real current path.
    fn insert_new(
        &mut self,
        vid: VolumeId,
        dir: EntryId,
        name: &[u8],
        meta: &EntryMeta,
        path: &[u8],
    ) {
        let Ok(id) = self.w.insert(dir, name, meta) else {
            return;
        };
        if !meta.kind().is_dir() {
            if meta.nlink > 1 {
                self.sync_links(vid, id, meta);
            }
            return;
        }
        let opts = ScanOptions {
            threads: 1,
            low_priority: false,
            ..self.scan_options(vid, false)
        };
        let mut applier = Applier::new(id, true);
        // (inotify watches are added by the scan options’ hook before each directory is read.)
        let _ = scan_inline(&bytes_path(path), &opts, None, |visit| {
            if let Visit::Listed(listing) = visit {
                self.apply_listing(vid, &mut applier, listing);
            }
        });
        self.check_watch_limit(vid);
    }

    fn refresh_acl(&mut self, id: EntryId, path: &[u8]) {
        let mut buf = vec![0u8; 64 * 1024];
        let raw = rustix::fs::lgetxattr(
            bytes_path(path),
            fossick_core::perm::ACL_XATTR,
            &mut buf[..],
        );
        let acl = match raw {
            Ok(n) => Some(parse_acl(&buf[..n]).unwrap_or_else(|| {
                vec![AclEntry {
                    tag: tag::OTHER,
                    perm: 0,
                    id: 0,
                }]
            })),
            Err(_) => None,
        };
        let _ = self.w.set_acl(id, acl.as_deref());
    }

    /// Applies a rename. Returns false if part of it can’t be decided yet.
    fn apply_rename(
        &mut self,
        vid: VolumeId,
        from: Option<(EntryId, Option<&FileHandle>)>,
        from_name: &[u8],
        to: Option<(EntryId, Option<&FileHandle>)>,
        to_name: &[u8],
    ) -> bool {
        if log::debugging() {
            let show = |d: Option<(EntryId, Option<&FileHandle>)>, n: &[u8]| {
                d.map_or_else(
                    || format!("?/{}", String::from_utf8_lossy(n)),
                    |(d, _)| {
                        format!(
                            "{}/{}",
                            String::from_utf8_lossy(&self.w.path(d)),
                            String::from_utf8_lossy(n)
                        )
                    },
                )
            };
            log::debug(&format!(
                "rename {} -> {}",
                show(from, from_name),
                show(to, to_name)
            ));
        }
        if let (Some((fd, _)), Some((td, to_h))) = (from, to)
            && let Some(id) = self.w.lookup(fd, from_name)
        {
            match self.child_state(vid, td, to_h, to_name) {
                ChildState::Present(meta, _, path)
                    if self.w.meta(id).is_some_and(|m| m.inode == meta.inode)
                        && !self.is_excluded(vid, &path) =>
                {
                    if let Some(victim) = self.w.lookup(td, to_name)
                        && victim != id
                    {
                        let _ = self.w.remove(victim);
                    }
                    if self.w.rename(id, td, to_name).is_ok() {
                        let _ = self.w.update(id, &meta);
                        return true;
                    }
                }
                ChildState::Unknown => return false,
                _ => {}
            }
        }
        let mut done = true;
        if let Some((fd, h)) = from {
            done &= self.refresh_child(vid, fd, from_name, h);
        }
        if let Some((td, h)) = to {
            done &= self.refresh_child(vid, td, to_name, h);
        }
        done
    }

    // ----- snapshots -----

    fn snapshot(&mut self, wait: bool) {
        if let Some(job) = &self.snapshot_job {
            if !job.is_finished() && !wait {
                return;
            }
            if let Some(job) = self.snapshot_job.take() {
                let _ = job.join();
            }
        }
        self.last_snapshot = Instant::now();
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let g = self.shared.current();
        let path = snapshot_path(&self.shared);
        let job = std::thread::Builder::new()
            .name("fossick-snapshot".into())
            .spawn(move || {
                let t = Instant::now();
                match persist::write(&g, &path) {
                    Ok(()) => log::info(&format!(
                        "snapshot written ({} entries, {:.2} s)",
                        g.len(),
                        t.elapsed().as_secs_f64()
                    )),
                    Err(e) => log::warn(&format!("cannot write {}: {e}", path.display())),
                }
            });
        match job {
            Ok(j) if wait => {
                let _ = j.join();
            }
            Ok(j) => self.snapshot_job = Some(j),
            Err(e) => log::warn(&format!("cannot start the snapshot thread: {e}")),
        }
        // Drop handles of directories that no longer exist, and dead link records.
        let w = &self.w;
        self.handles
            .retain(|_, (id, _)| w.is_live(*id) && w.kind(*id) == FileKind::Dir);
        self.links.retain(|_, (ids, _)| {
            ids.retain(|&x| w.is_live(x));
            !ids.is_empty()
        });
        self.dir_inodes
            .retain(|_, id| w.is_live(*id) && w.kind(*id) == FileKind::Dir);
    }

    fn shutdown(&mut self) {
        self.publish();
        for v in self.vols.values() {
            stop_watch(&v.watch);
        }
        self.snapshot(true);
        log::info("writer stopped");
    }
}

fn stop_watch(w: &Watch) {
    match w {
        Watch::Fanotify(g) => g.stop(),
        Watch::Inotify(i) => i.stop(),
        Watch::None | Watch::Poll => {}
    }
}

/// True for identity keys that only name a mount point (no filesystem UUID was found).
fn is_fallback(key: &str) -> bool {
    key.starts_with("path:")
}

/// The indexed volume a mount is: the one with the same identity key or, when one of the two
/// keys is only a fallback, the one of the same type at the same mount point. udev may not
/// have published a new disk’s UUID yet when the daemon first sees it (or may on one start and
/// not the next); without this the same filesystem would be indexed twice. Volumes already
/// matched to another mount (`taken`) are skipped.
fn match_volume(
    volumes: &[fossick_core::Volume],
    key: &str,
    fstype: &str,
    mount_path: &[u8],
    taken: &HashSet<VolumeId>,
) -> Option<VolumeId> {
    let free = |v: &&fossick_core::Volume| !taken.contains(&v.id);
    volumes
        .iter()
        .filter(free)
        .find(|v| v.key == key)
        .or_else(|| {
            volumes.iter().filter(free).find(|v| {
                (is_fallback(key) || is_fallback(&v.key))
                    && v.fstype == fstype
                    && v.mount_path == mount_path
            })
        })
        .map(|v| v.id)
}

/// True if directory timestamps on this filesystem reliably change with the directory’s
/// contents, so pruned rescans may skip directories whose times didn’t change.
fn prunable(fstype: &str) -> bool {
    !matches!(fstype, "vfat" | "msdos" | "exfat" | "fat")
}

/// User mode indexes the home directory only, as one volume.
fn user_plan(mounts: &[crate::mounts::MountInfo]) -> Vec<Planned> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    // Describe $HOME like a mount so the rest of the writer treats it the same way.
    let containing = mounts
        .iter()
        .filter(|m| {
            let mp = m.mount_point.as_slice();
            let h = home.as_os_str().as_bytes();
            mp == b"/" || h == mp || (h.starts_with(mp) && h.get(mp.len()) == Some(&b'/'))
        })
        .max_by_key(|m| m.mount_point.len());
    let mut mount = containing.cloned().unwrap_or(crate::mounts::MountInfo {
        mount_id: 0,
        parent_id: 0,
        major: 0,
        minor: 0,
        root: b"/".to_vec(),
        mount_point: Vec::new(),
        options: "rw".into(),
        fstype: "unknown".into(),
        source: Vec::new(),
        super_options: String::new(),
    });
    mount.mount_point = home.as_os_str().as_bytes().to_vec();
    vec![Planned {
        mount,
        class: Class::Local,
        exclude: Vec::new(),
    }]
}

fn bytes_path(b: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vol(id: u16, key: &str, fstype: &str, path: &[u8]) -> fossick_core::Volume {
        fossick_core::Volume {
            id: VolumeId(id),
            key: key.into(),
            mount_path: path.to_vec(),
            fstype: fstype.into(),
            label: None,
            root: EntryId(0),
            online: false,
        }
    }

    #[test]
    fn fallback_identities_match_the_same_mount() {
        let vols = [
            vol(0, "path:ext4:/mnt/a", "ext4", b"/mnt/a"),
            vol(1, "uuid:1111", "ext4", b"/mnt/b"),
            vol(2, "uuid:2222", "xfs", b"/mnt/c"),
        ];
        let none = HashSet::new();
        // The UUID showed up after the first start.
        assert_eq!(
            match_volume(&vols, "uuid:9999", "ext4", b"/mnt/a", &none),
            Some(VolumeId(0))
        );
        // The UUID is missing this time.
        assert_eq!(
            match_volume(&vols, "path:ext4:/mnt/b", "ext4", b"/mnt/b", &none),
            Some(VolumeId(1))
        );
        // Two real UUIDs never match each other, and the type must agree.
        assert_eq!(
            match_volume(&vols, "uuid:3333", "ext4", b"/mnt/b", &none),
            None
        );
        assert_eq!(
            match_volume(&vols, "path:ext4:/mnt/c", "ext4", b"/mnt/c", &none),
            None
        );
        // An exact key wins, and a volume is matched to one mount only.
        assert_eq!(
            match_volume(&vols, "uuid:2222", "xfs", b"/elsewhere", &none),
            Some(VolumeId(2))
        );
        let taken: HashSet<VolumeId> = [VolumeId(0)].into();
        assert_eq!(
            match_volume(&vols, "uuid:9999", "ext4", b"/mnt/a", &taken),
            None
        );
    }
}
