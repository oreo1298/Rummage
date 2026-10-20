//! Entry metadata as delivered by scanners and watchers.

use serde::{Deserialize, Serialize};

/// Minimal bitflags implementation (keeps the dependency list short).
macro_rules! bitflags_lite {
    ($(#[$m:meta])* pub struct $name:ident: $t:ty { $(const $f:ident = $v:expr;)* }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub struct $name(pub $t);
        impl $name {
            $(pub const $f: Self = Self($v);)*
            #[must_use]
            pub const fn empty() -> Self { Self(0) }
            #[must_use]
            pub const fn bits(self) -> $t { self.0 }
            #[must_use]
            pub const fn contains(self, other: Self) -> bool { self.0 & other.0 == other.0 }
            #[must_use]
            pub const fn intersects(self, other: Self) -> bool { self.0 & other.0 != 0 }
        }
        impl std::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
        }
        impl std::ops::BitOrAssign for $name {
            fn bitor_assign(&mut self, rhs: Self) { self.0 |= rhs.0; }
        }
    };
}

/// File type, derived from `st_mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    CharDevice,
    BlockDevice,
    Fifo,
    Socket,
    Unknown,
}

pub const S_IFMT: u32 = 0o170_000;
pub const S_IFREG: u32 = 0o100_000;
pub const S_IFDIR: u32 = 0o040_000;
pub const S_IFLNK: u32 = 0o120_000;
pub const S_IFCHR: u32 = 0o020_000;
pub const S_IFBLK: u32 = 0o060_000;
pub const S_IFIFO: u32 = 0o010_000;
pub const S_IFSOCK: u32 = 0o140_000;

impl FileKind {
    #[must_use]
    pub fn from_mode(mode: u32) -> Self {
        match mode & S_IFMT {
            S_IFREG => Self::File,
            S_IFDIR => Self::Dir,
            S_IFLNK => Self::Symlink,
            S_IFCHR => Self::CharDevice,
            S_IFBLK => Self::BlockDevice,
            S_IFIFO => Self::Fifo,
            S_IFSOCK => Self::Socket,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub fn is_dir(self) -> bool {
        self == Self::Dir
    }
}

/// A point in time with nanosecond precision (seconds may be negative).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Timestamp {
    pub secs: i64,
    pub nanos: u32,
}

impl Timestamp {
    #[must_use]
    pub const fn new(secs: i64, nanos: u32) -> Self {
        Self { secs, nanos }
    }

    #[must_use]
    pub const fn from_secs(secs: i64) -> Self {
        Self { secs, nanos: 0 }
    }

    /// Nanoseconds since the Unix epoch, saturating.
    #[must_use]
    pub fn as_nanos(self) -> i64 {
        self.secs
            .saturating_mul(1_000_000_000)
            .saturating_add(i64::from(self.nanos))
    }

    #[must_use]
    pub fn from_nanos(ns: i64) -> Self {
        Self {
            secs: ns.div_euclid(1_000_000_000),
            nanos: u32::try_from(ns.rem_euclid(1_000_000_000)).unwrap_or(0),
        }
    }
}

bitflags_lite! {
    /// statx attributes Fossick keeps (plus the computed `SPARSE`).
    pub struct Attrs: u8 {
        const COMPRESSED = 1;
        const IMMUTABLE = 2;
        const APPEND = 4;
        const NODUMP = 8;
        const ENCRYPTED = 16;
        const VERITY = 32;
        const SPARSE = 64;
    }
}

/// Metadata for one directory entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryMeta {
    /// Full `st_mode` (file type + permission bits).
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// `st_size` (ignored for directories: folder sizes are computed).
    pub size: u64,
    pub mtime: Option<Timestamp>,
    pub ctime: Option<Timestamp>,
    pub btime: Option<Timestamp>,
    pub atime: Option<Timestamp>,
    pub inode: u64,
    pub attrs: Attrs,
}

impl EntryMeta {
    #[must_use]
    pub fn kind(&self) -> FileKind {
        FileKind::from_mode(self.mode)
    }

    /// Convenience constructor for tests and generators: a regular file.
    #[must_use]
    pub fn file(size: u64, mtime_secs: i64) -> Self {
        Self {
            mode: S_IFREG | 0o644,
            size,
            mtime: Some(Timestamp::from_secs(mtime_secs)),
            ctime: Some(Timestamp::from_secs(mtime_secs)),
            ..Self::default()
        }
    }

    /// Convenience constructor for tests and generators: a directory.
    #[must_use]
    pub fn dir(mtime_secs: i64) -> Self {
        Self {
            mode: S_IFDIR | 0o755,
            mtime: Some(Timestamp::from_secs(mtime_secs)),
            ctime: Some(Timestamp::from_secs(mtime_secs)),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_from_mode() {
        assert_eq!(FileKind::from_mode(S_IFDIR | 0o755), FileKind::Dir);
        assert_eq!(FileKind::from_mode(S_IFREG | 0o644), FileKind::File);
        assert_eq!(FileKind::from_mode(S_IFLNK | 0o777), FileKind::Symlink);
        assert_eq!(FileKind::from_mode(0), FileKind::Unknown);
    }

    #[test]
    fn timestamp_nanos_roundtrip() {
        let t = Timestamp::new(-5, 250);
        assert_eq!(Timestamp::from_nanos(t.as_nanos()), t);
        let t = Timestamp::new(1_700_000_000, 999_999_999);
        assert_eq!(Timestamp::from_nanos(t.as_nanos()), t);
    }

    #[test]
    fn attrs_ops() {
        let a = Attrs::COMPRESSED | Attrs::SPARSE;
        assert!(a.contains(Attrs::SPARSE));
        assert!(!a.contains(Attrs::IMMUTABLE));
    }
}
