//! inotify watching, for volumes where fanotify isn’t available (no `CAP_SYS_ADMIN`, as in user
//! mode, or a filesystem without file-id support). inotify watches one directory per watch, so
//! the writer adds a watch for every directory it indexes, up to the user’s watch limit.

use std::collections::HashMap;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fossick_core::VolumeId;
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

use crate::events::{Cmd, DirRef, FsEvent};

/// An inotify instance for one volume.
#[derive(Debug)]
pub struct Inotify {
    fd: OwnedFd,
    /// Watch descriptor → the watched directory’s (device, inode).
    wds: Mutex<HashMap<i32, (u64, u64)>>,
    /// The watch limit was reached: some directories are not watched.
    pub full: AtomicBool,
    stop: AtomicBool,
}

const MASK: WatchFlags = WatchFlags::CREATE
    .union(WatchFlags::DELETE)
    .union(WatchFlags::MODIFY)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::ONLYDIR)
    .union(WatchFlags::EXCL_UNLINK);

impl Inotify {
    pub fn new() -> io::Result<Arc<Self>> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        Ok(Arc::new(Self {
            fd,
            wds: Mutex::new(HashMap::new()),
            full: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        }))
    }

    /// Watches the directory open as `dir` (through `/proc/self/fd`, so exactly that
    /// directory even if its path changes meanwhile).
    pub fn add(&self, dir: BorrowedFd<'_>) -> io::Result<()> {
        let st = rustix::fs::fstat(dir)?;
        let path = format!("/proc/self/fd/{}", dir.as_raw_fd());
        // Hold the map while adding: the reader may see the new watch’s first events before
        // `add_watch` returns, and must not drop them as belonging to an unknown watch.
        let mut wds = self
            .wds
            .lock()
            .map_err(|_| io::Error::other("watch map poisoned"))?;
        match inotify::add_watch(self.fd.as_fd(), path.as_str(), MASK) {
            Ok(wd) => {
                wds.insert(wd, (st.st_dev, st.st_ino));
                Ok(())
            }
            Err(rustix::io::Errno::NOSPC) => {
                self.full.store(true, Ordering::Relaxed);
                Err(io::Error::from_raw_os_error(libc::ENOSPC))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Number of active watches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.wds.lock().map_or(0, |m| m.len())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Makes [`Inotify::run`] return.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Reads events until stopped or the writer is gone.
    pub fn run(&self, volume: VolumeId, tx: &Sender<Cmd>) {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut buf = vec![MaybeUninit::<u8>::uninit(); 256 * 1024];
        let timeout = Timespec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        while !self.stop.load(Ordering::Relaxed) {
            let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) | Err(rustix::io::Errno::INTR) => continue,
                Ok(_) => {}
                Err(_) => return,
            }
            let mut events = Vec::new();
            let mut overflow = false;
            // A move out is held until the matching move in (same cookie) is seen.
            let mut moved_from: Option<(u32, DirRef, Vec<u8>)> = None;
            let mut reader = inotify::Reader::new(&self.fd, &mut buf);
            loop {
                let ev = match reader.next() {
                    Ok(ev) => ev,
                    Err(rustix::io::Errno::WOULDBLOCK | rustix::io::Errno::INTR) => break,
                    Err(_) => return,
                };
                let flags = ev.events();
                if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                    overflow = true;
                    continue;
                }
                let wd = ev.wd();
                if flags.contains(ReadFlags::IGNORED) {
                    if let Ok(mut m) = self.wds.lock() {
                        m.remove(&wd);
                    }
                    continue;
                }
                let Some((dev, ino)) = self.wds.lock().ok().and_then(|m| m.get(&wd).copied())
                else {
                    continue;
                };
                let Some(name) = ev.file_name().map(|n| n.to_bytes().to_vec()) else {
                    continue;
                };
                let dir = DirRef::Inode(dev, ino);
                if flags.contains(ReadFlags::MOVED_FROM) {
                    if let Some((_, d, n)) = moved_from.take() {
                        events.push(FsEvent::Child { dir: d, name: n });
                    }
                    moved_from = Some((ev.cookie(), dir, name));
                    continue;
                }
                if flags.contains(ReadFlags::MOVED_TO)
                    && let Some((cookie, from_dir, from_name)) = moved_from.take()
                {
                    if cookie == ev.cookie() {
                        events.push(FsEvent::Rename {
                            from_dir,
                            from_name,
                            to_dir: dir,
                            to_name: name,
                        });
                        continue;
                    }
                    events.push(FsEvent::Child {
                        dir: from_dir,
                        name: from_name,
                    });
                }
                events.push(FsEvent::Child { dir, name });
            }
            if let Some((_, dir, name)) = moved_from.take() {
                events.push(FsEvent::Child { dir, name });
            }
            if overflow && tx.send(Cmd::Overflow(volume)).is_err() {
                return;
            }
            if !events.is_empty() && tx.send(Cmd::Events(volume, events)).is_err() {
                return;
            }
        }
    }
}

/// Sleeps in short steps until `stop` is set or `d` passes.
pub fn sleep_unless(stop: &AtomicBool, d: Duration) {
    let step = Duration::from_millis(200);
    let mut left = d;
    while !stop.load(Ordering::Relaxed) && !left.is_zero() {
        let s = left.min(step);
        std::thread::sleep(s);
        left -= s;
    }
}
