//! System calls the safe wrappers (`rustix`, `std`) don’t cover. This is the daemon’s only
//! `unsafe` code; each block states why it is sound.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Supplementary groups of a Unix socket peer (`SO_PEERGROUPS`, Linux ≥ 4.13).
pub fn peer_groups(sock: BorrowedFd<'_>) -> io::Result<Vec<u32>> {
    let mut groups: Vec<libc::gid_t> = vec![0; 64];
    loop {
        let mut len = libc::socklen_t::try_from(groups.len() * size_of::<libc::gid_t>())
            .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: the buffer is valid for `len` bytes and the kernel writes at most that many,
        // reporting the size it needs (or wrote) back in `len`.
        let rc = unsafe {
            libc::getsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERGROUPS,
                groups.as_mut_ptr().cast(),
                &raw mut len,
            )
        };
        let n = len as usize / size_of::<libc::gid_t>();
        if rc == 0 {
            groups.truncate(n);
            return Ok(groups);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ERANGE) && n > groups.len() && n <= 65_536 {
            groups.resize(n, 0);
            continue;
        }
        return Err(err);
    }
}

/// `fanotify_init(2)`.
pub fn fanotify_init(flags: u32, event_f_flags: u32) -> io::Result<OwnedFd> {
    // SAFETY: plain syscall with integer arguments.
    let fd = unsafe { libc::fanotify_init(flags, event_f_flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fanotify_init` returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `fanotify_mark(2)` on a path.
pub fn fanotify_mark(group: BorrowedFd<'_>, flags: u32, mask: u64, path: &Path) -> io::Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `c` is a valid NUL-terminated string for the duration of the call.
    let rc =
        unsafe { libc::fanotify_mark(group.as_raw_fd(), flags, mask, libc::AT_FDCWD, c.as_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Caps glibc’s malloc arenas. The parallel scan allocates in per-thread arenas that glibc
/// can’t give back, which inflates the resident size (see docs/BENCHMARKS.md).
pub fn limit_malloc_arenas(n: i32) {
    #[cfg(target_env = "gnu")]
    // SAFETY: `mallopt` only adjusts allocator tuning parameters.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, n);
    }
    #[cfg(not(target_env = "gnu"))]
    let _ = n;
}

/// Returns freed heap memory to the system (glibc only).
pub fn trim_heap() {
    #[cfg(target_env = "gnu")]
    // SAFETY: `malloc_trim` only releases free memory.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Sockets passed by systemd socket activation (`LISTEN_PID`/`LISTEN_FDS`), if any.
pub fn listen_fds() -> Vec<OwnedFd> {
    const SD_LISTEN_FDS_START: RawFd = 3;
    let pid_ok = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|p| p.parse::<u32>().ok())
        == Some(std::process::id());
    let n: RawFd = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    // SAFETY: changing the environment is unsound only with concurrent readers; this runs at
    // startup, before any thread is spawned.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    if !pid_ok || !(1..=16).contains(&n) {
        return Vec::new();
    }
    (SD_LISTEN_FDS_START..SD_LISTEN_FDS_START + n)
        .filter_map(|fd| {
            // SAFETY: systemd passed these descriptors to this process (LISTEN_PID matched)
            // and nothing else in the process has taken ownership of them yet.
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            rustix::io::fcntl_setfd(owned.as_fd(), rustix::io::FdFlags::CLOEXEC).ok()?;
            Some(owned)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_groups_of_own_socket() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let groups = peer_groups(a.as_fd()).unwrap();
        let mut ours: Vec<u32> = rustix::process::getgroups()
            .unwrap()
            .into_iter()
            .map(rustix::fs::Gid::as_raw)
            .collect();
        let mut theirs = groups.clone();
        ours.sort_unstable();
        theirs.sort_unstable();
        assert_eq!(theirs, ours);
    }
}
