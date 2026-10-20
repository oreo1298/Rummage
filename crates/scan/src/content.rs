//! `content:` support for in-process (local or user-mode) searches: files are opened with the
//! caller’s own credentials, so the kernel enforces permissions.

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;

use fossick_core::search::ContentSource;

/// Opens files directly (no privilege of its own).
#[derive(Clone, Copy, Debug, Default)]
pub struct FsContent;

impl ContentSource for FsContent {
    fn open(&self, path: &[u8]) -> std::io::Result<Box<dyn Read + Send>> {
        let f = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NOATIME)
            .open(OsStr::from_bytes(path))
            .or_else(|_| {
                File::options()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(OsStr::from_bytes(path))
            })?;
        Ok(Box::new(f))
    }
}
