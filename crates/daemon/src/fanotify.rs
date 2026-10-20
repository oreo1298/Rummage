//! fanotify watching: one notification group per volume, marked on the whole filesystem
//! (`FAN_MARK_FILESYSTEM`), reporting the directory file handle and name of each change
//! (`FAN_REPORT_DFID_NAME`) and, where the kernel supports it, both ends of renames
//! (`FAN_RENAME`, Linux ≥ 5.17).

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use fossick_core::VolumeId;
use fossick_scan::handle::FileHandle;

use crate::events::{Cmd, DirRef, FsEvent};
use crate::sys;

// fanotify ABI constants (linux/fanotify.h).
pub const FAN_MODIFY: u64 = 0x0000_0002;
pub const FAN_ATTRIB: u64 = 0x0000_0004;
pub const FAN_CLOSE_WRITE: u64 = 0x0000_0008;
pub const FAN_MOVED_FROM: u64 = 0x0000_0040;
pub const FAN_MOVED_TO: u64 = 0x0000_0080;
pub const FAN_CREATE: u64 = 0x0000_0100;
pub const FAN_DELETE: u64 = 0x0000_0200;
pub const FAN_DELETE_SELF: u64 = 0x0000_0400;
pub const FAN_MOVE_SELF: u64 = 0x0000_0800;
pub const FAN_Q_OVERFLOW: u64 = 0x0000_4000;
pub const FAN_RENAME: u64 = 0x1000_0000;
pub const FAN_ONDIR: u64 = 0x4000_0000;

const FAN_CLOEXEC: u32 = 0x1;
const FAN_REPORT_DIR_FID: u32 = 0x400;
const FAN_REPORT_NAME: u32 = 0x800;
const FAN_MARK_ADD: u32 = 0x1;
const FAN_MARK_FILESYSTEM: u32 = 0x100;

const INFO_DFID_NAME: u8 = 2;
const INFO_DFID: u8 = 3;
const INFO_OLD_DFID_NAME: u8 = 10;
const INFO_NEW_DFID_NAME: u8 = 12;

const METADATA_LEN: usize = 24;

/// Events every group subscribes to.
const BASE_MASK: u64 = FAN_CREATE
    | FAN_DELETE
    | FAN_MOVED_FROM
    | FAN_MOVED_TO
    | FAN_ATTRIB
    | FAN_MODIFY
    | FAN_CLOSE_WRITE
    | FAN_DELETE_SELF
    | FAN_MOVE_SELF
    | FAN_ONDIR;

/// A fanotify group watching one filesystem.
#[derive(Debug)]
pub struct Group {
    fd: OwnedFd,
    /// Renames are reported with both ends (`FAN_RENAME`).
    pub renames: bool,
    stop: AtomicBool,
}

impl Group {
    /// Creates a group and marks the filesystem containing `mount_path`. Fails (and the caller
    /// falls back to inotify) without `CAP_SYS_ADMIN`, on kernels older than 5.9, or on
    /// filesystems that can’t report file ids (some FUSE and network filesystems).
    pub fn new(mount_path: &Path) -> io::Result<Self> {
        let fd = sys::fanotify_init(
            FAN_CLOEXEC | FAN_REPORT_DIR_FID | FAN_REPORT_NAME,
            (libc::O_RDONLY | libc::O_LARGEFILE) as u32,
        )?;
        let flags = FAN_MARK_ADD | FAN_MARK_FILESYSTEM;
        let renames =
            match sys::fanotify_mark(fd.as_fd(), flags, BASE_MASK | FAN_RENAME, mount_path) {
                Ok(()) => true,
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                    sys::fanotify_mark(fd.as_fd(), flags, BASE_MASK, mount_path)?;
                    false
                }
                Err(e) => return Err(e),
            };
        Ok(Self {
            fd,
            renames,
            stop: AtomicBool::new(false),
        })
    }

    /// Makes [`Group::run`] return.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Reads events until stopped, sending them to the writer. Returns when the writer is gone
    /// or reading fails.
    pub fn run(&self, volume: VolumeId, tx: &Sender<Cmd>) -> io::Result<()> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let own_pid = i32::try_from(std::process::id()).unwrap_or(-1);
        let mut buf = vec![0u8; 256 * 1024];
        let timeout = Timespec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        while !self.stop.load(Ordering::Relaxed) {
            let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) | Err(rustix::io::Errno::INTR) => continue,
                Ok(_) => {}
                Err(e) => return Err(e.into()),
            }
            let n = match rustix::io::read(&self.fd, &mut buf) {
                Ok(n) => n,
                Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
                Err(e) => return Err(e.into()),
            };
            let (events, overflow) = parse(&buf[..n], own_pid);
            if overflow && tx.send(Cmd::Overflow(volume)).is_err() {
                return Ok(());
            }
            if !events.is_empty() && tx.send(Cmd::Events(volume, events)).is_err() {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// One directory-handle record: the handle and, for `*_DFID_NAME` records, the name.
fn parse_fid_record(rec: &[u8]) -> Option<(FileHandle, Option<Vec<u8>>)> {
    // header (4) + fsid (8) + handle_bytes (4) + handle_type (4) + handle + name\0
    let body = rec.get(12..)?;
    let hb = u32::from_le_bytes(body.get(0..4)?.try_into().ok()?) as usize;
    let ht = i32::from_le_bytes(body.get(4..8)?.try_into().ok()?);
    let bytes = body.get(8..8 + hb)?;
    let rest = &body[8 + hb..];
    // Events on a directory itself carry the name "."; DFID records carry no name.
    let name = rest
        .iter()
        .position(|&b| b == 0)
        .map(|end| rest[..end].to_vec())
        .filter(|n| !n.is_empty());
    Some((
        FileHandle {
            handle_type: ht,
            bytes: bytes.into(),
        },
        name,
    ))
}

/// Parses a buffer of events. Returns the events and whether the queue overflowed. Events
/// caused by `own_pid` (the daemon’s snapshot writes) are dropped. Malformed records end the
/// buffer.
#[must_use]
pub fn parse(buf: &[u8], own_pid: i32) -> (Vec<FsEvent>, bool) {
    let mut out = Vec::new();
    let mut overflow = false;
    let mut off = 0;
    while off + METADATA_LEN <= buf.len() {
        let ev = &buf[off..];
        let event_len = u32::from_le_bytes([ev[0], ev[1], ev[2], ev[3]]) as usize;
        let metadata_len = usize::from(u16::from_le_bytes([ev[6], ev[7]]));
        if event_len < METADATA_LEN || metadata_len < METADATA_LEN || off + event_len > buf.len() {
            break;
        }
        let mask = u64::from_le_bytes(ev[8..16].try_into().unwrap_or_default());
        let pid = i32::from_le_bytes(ev[20..24].try_into().unwrap_or_default());
        off += event_len;
        if mask & FAN_Q_OVERFLOW != 0 {
            overflow = true;
            continue;
        }
        if pid == own_pid {
            continue;
        }
        let mut dfid_name = None;
        let mut old = None;
        let mut new = None;
        let mut info = &ev[metadata_len.min(event_len)..event_len];
        while info.len() >= 4 {
            let kind = info[0];
            let len = usize::from(u16::from_le_bytes([info[2], info[3]]));
            if len < 4 || len > info.len() {
                break;
            }
            let rec = &info[..len];
            match kind {
                INFO_DFID_NAME | INFO_DFID => dfid_name = parse_fid_record(rec),
                INFO_OLD_DFID_NAME => old = parse_fid_record(rec),
                INFO_NEW_DFID_NAME => new = parse_fid_record(rec),
                _ => {}
            }
            info = &info[len..];
        }
        if mask & FAN_RENAME != 0 {
            if let (Some((fd, Some(fname))), Some((td, Some(tname)))) = (old, new) {
                out.push(FsEvent::Rename {
                    from_dir: DirRef::Handle(fd),
                    from_name: fname,
                    to_dir: DirRef::Handle(td),
                    to_name: tname,
                });
            }
            continue;
        }
        match dfid_name {
            // A directory itself changed (chmod, chown, ACL). Deleting or moving it is also
            // reported to its parent, which covers those.
            Some((dir, Some(name))) if name == b"." => {
                if mask & (FAN_DELETE_SELF | FAN_MOVE_SELF) == 0 {
                    out.push(FsEvent::Itself {
                        dir: DirRef::Handle(dir),
                    });
                }
            }
            Some((dir, Some(name))) => out.push(FsEvent::Child {
                dir: DirRef::Handle(dir),
                name,
            }),
            Some((_, None)) | None => {}
        }
    }
    (out, overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u8, handle: &[u8], name: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&[0u8; 8]); // fsid
        body.extend_from_slice(&u32::try_from(handle.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&1i32.to_le_bytes());
        body.extend_from_slice(handle);
        body.extend_from_slice(name);
        body.push(0);
        while (body.len() + 4) % 4 != 0 {
            body.push(0);
        }
        let mut rec = vec![kind, 0];
        rec.extend_from_slice(&u16::try_from(body.len() + 4).unwrap().to_le_bytes());
        rec.extend_from_slice(&body);
        rec
    }

    fn event(mask: u64, pid: i32, records: &[Vec<u8>]) -> Vec<u8> {
        let info: Vec<u8> = records.concat();
        let len = METADATA_LEN + info.len();
        let mut ev = Vec::new();
        ev.extend_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
        ev.extend_from_slice(&[3, 0]);
        ev.extend_from_slice(&24u16.to_le_bytes());
        ev.extend_from_slice(&mask.to_le_bytes());
        ev.extend_from_slice(&(-1i32).to_le_bytes());
        ev.extend_from_slice(&pid.to_le_bytes());
        ev.extend_from_slice(&info);
        ev
    }

    #[test]
    fn parses_child_rename_and_overflow_events() {
        let h = |b: u8| FileHandle {
            handle_type: 1,
            bytes: vec![b; 8].into(),
        };
        let mut buf = event(
            FAN_CREATE,
            10,
            &[record(INFO_DFID_NAME, &[7; 8], b"new.txt")],
        );
        buf.extend(event(
            FAN_RENAME,
            10,
            &[
                record(INFO_OLD_DFID_NAME, &[1; 8], b"a"),
                record(INFO_NEW_DFID_NAME, &[2; 8], b"b"),
            ],
        ));
        buf.extend(event(
            FAN_MODIFY,
            99,
            &[record(INFO_DFID_NAME, &[7; 8], b"own")],
        ));
        buf.extend(event(FAN_Q_OVERFLOW, 10, &[]));
        buf.extend(event(
            FAN_DELETE_SELF,
            10,
            &[record(INFO_DFID, &[5; 8], b"")],
        ));
        buf.extend(event(
            FAN_ATTRIB | FAN_ONDIR,
            10,
            &[record(INFO_DFID_NAME, &[6; 8], b".")],
        ));
        buf.extend_from_slice(&[1, 2, 3]); // truncated garbage at the end
        let (events, overflow) = parse(&buf, 99);
        assert!(overflow);
        assert_eq!(
            events,
            [
                FsEvent::Child {
                    dir: DirRef::Handle(h(7)),
                    name: b"new.txt".to_vec()
                },
                FsEvent::Rename {
                    from_dir: DirRef::Handle(h(1)),
                    from_name: b"a".to_vec(),
                    to_dir: DirRef::Handle(h(2)),
                    to_name: b"b".to_vec()
                },
                FsEvent::Itself {
                    dir: DirRef::Handle(h(6))
                },
            ]
        );
    }

    #[test]
    fn garbage_never_panics() {
        let mut seed = 0x1234_5678u32;
        for len in 0..400 {
            let buf: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    (seed >> 16) as u8
                })
                .collect();
            let _ = parse(&buf, 1);
        }
    }
}
