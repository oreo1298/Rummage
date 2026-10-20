//! Messages to the writer thread.

use fossick_core::{EntryId, VolumeId};
use fossick_scan::handle::FileHandle;

/// A directory named by a watcher.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DirRef {
    /// A fanotify directory file handle.
    Handle(FileHandle),
    /// An indexed entry.
    Entry(EntryId),
    /// A directory by device and inode number (inotify watches, which are added before the
    /// directory is indexed).
    Inode(u64, u64),
}

/// A filesystem change. Events say *where* something changed; the writer re-reads the
/// filesystem to learn *what* changed, so lost or merged event details don’t matter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FsEvent {
    /// `name` in `dir` was created, deleted, modified or moved in or out.
    Child { dir: DirRef, name: Vec<u8> },
    /// The directory itself changed (mode, owner, ACL, times).
    Itself { dir: DirRef },
    /// A rename with both ends known.
    Rename {
        from_dir: DirRef,
        from_name: Vec<u8>,
        to_dir: DirRef,
        to_name: Vec<u8>,
    },
}

/// Commands for the writer thread.
#[derive(Debug)]
pub enum Cmd {
    /// Changes on a volume.
    Events(VolumeId, Vec<FsEvent>),
    /// A watcher lost events: rescan the volume (pruned).
    Overflow(VolumeId),
    /// A watcher stopped working: fall back to polling.
    WatchFailed(VolumeId, String),
    /// The mount table changed.
    MountsChanged,
    /// Rescan the volume mounted at this path (empty: all volumes).
    Rescan(Vec<u8>),
    /// Use this configuration (re-read after an edit).
    Reload(Box<crate::config::Config>),
    /// Write a snapshot and exit.
    Shutdown,
}
