//! Kernel file handles (`name_to_handle_at`, `open_by_handle_at`). fanotify identifies the
//! directory of an event by its handle, so scans record each directory’s handle.
//!
//! This is the only module of the scanner that uses `unsafe`: the C structure has a flexible
//! array member, which the `libc` bindings model with raw pointers.
#![allow(unsafe_code)]

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// Largest handle the kernel produces (`MAX_HANDLE_SZ`).
pub const MAX_HANDLE_SZ: usize = 128;

/// An opaque file handle: its type and bytes, as the kernel encodes them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileHandle {
    pub handle_type: i32,
    pub bytes: Box<[u8]>,
}

/// `struct file_handle` with room for the largest handle.
#[repr(C)]
struct RawHandle {
    handle_bytes: u32,
    handle_type: i32,
    f_handle: [u8; MAX_HANDLE_SZ],
}

impl RawHandle {
    fn empty() -> Self {
        Self {
            handle_bytes: MAX_HANDLE_SZ as u32,
            handle_type: 0,
            f_handle: [0; MAX_HANDLE_SZ],
        }
    }

    fn from_handle(h: &FileHandle) -> io::Result<Self> {
        if h.bytes.len() > MAX_HANDLE_SZ {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let mut raw = Self::empty();
        raw.handle_bytes = h.bytes.len() as u32;
        raw.handle_type = h.handle_type;
        raw.f_handle[..h.bytes.len()].copy_from_slice(&h.bytes);
        Ok(raw)
    }
}

fn handle_impl(dirfd: BorrowedFd<'_>, name: &CStr, flags: libc::c_int) -> io::Result<FileHandle> {
    let mut raw = RawHandle::empty();
    let mut mount_id: libc::c_int = 0;
    // SAFETY: `raw` is a `#[repr(C)]` `struct file_handle` whose `handle_bytes` (128) matches the
    // space that follows it, `name` is NUL-terminated, and both out-pointers are valid for the
    // duration of the call.
    let rc = unsafe {
        libc::name_to_handle_at(
            dirfd.as_raw_fd(),
            name.as_ptr(),
            std::ptr::from_mut(&mut raw).cast::<libc::file_handle>(),
            &raw mut mount_id,
            flags,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let n = (raw.handle_bytes as usize).min(MAX_HANDLE_SZ);
    Ok(FileHandle {
        handle_type: raw.handle_type,
        bytes: raw.f_handle[..n].into(),
    })
}

/// The handle of the object open as `fd`.
pub fn handle_of_fd(fd: BorrowedFd<'_>) -> io::Result<FileHandle> {
    handle_impl(fd, c"", libc::AT_EMPTY_PATH)
}

/// The handle of `name` inside the directory `dirfd` (symlinks are not followed).
pub fn handle_at(dirfd: BorrowedFd<'_>, name: &CStr) -> io::Result<FileHandle> {
    handle_impl(dirfd, name, 0)
}

/// Opens the object a handle refers to (needs `CAP_DAC_READ_SEARCH`). `mount_fd` is any open
/// file on the same filesystem. Fails with `ESTALE` if the object no longer exists.
pub fn open_by_handle(
    mount_fd: BorrowedFd<'_>,
    handle: &FileHandle,
    flags: libc::c_int,
) -> io::Result<OwnedFd> {
    let mut raw = RawHandle::from_handle(handle)?;
    // SAFETY: `raw` is a valid `struct file_handle` (its `handle_bytes` never exceeds the
    // buffer), and the kernel only reads it.
    let fd = unsafe {
        libc::open_by_handle_at(
            mount_fd.as_raw_fd(),
            std::ptr::from_mut(&mut raw).cast::<libc::file_handle>(),
            flags | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `open_by_handle_at` returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use super::*;

    #[test]
    fn handles_identify_directories() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        let root = std::fs::File::open(tmp.path()).unwrap();
        let d = std::fs::File::open(tmp.path().join("d")).unwrap();
        let (a, b) = match (handle_of_fd(d.as_fd()), handle_at(root.as_fd(), c"d")) {
            (Ok(a), Ok(b)) => (a, b),
            // Some filesystems (e.g. overlayfs without index, some FUSE) can’t encode handles.
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("file handles not supported here: {e}");
                return;
            }
        };
        assert_eq!(a, b);
        assert_ne!(handle_of_fd(root.as_fd()).unwrap(), a);
        // Opening needs CAP_DAC_READ_SEARCH; skip that half when unprivileged.
        if let Ok(fd) = open_by_handle(root.as_fd(), &a, libc::O_PATH) {
            assert_eq!(handle_of_fd(fd.as_fd()).unwrap(), a);
        }
    }
}
