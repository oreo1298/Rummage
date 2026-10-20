//! Volume identity: a stable key per filesystem (and btrfs subvolume), so a snapshot can be
//! matched to the right mount after a reboot or when a removable drive is mounted elsewhere.
//!
//! Order of preference:
//! 1. the filesystem UUID from `/dev/disk/by-uuid` (plus the subvolume id on btrfs);
//! 2. for network filesystems: the source and type (`nas:/export nfs4`);
//! 3. otherwise the mount point and type, marked volatile (tmpfs, overlays, odd FUSE mounts).

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;

use crate::mounts::{Class, MountInfo};

/// Maps device numbers (`major`, `minor`) to filesystem UUIDs and labels.
#[derive(Clone, Debug, Default)]
pub struct UuidMap {
    uuids: HashMap<(u32, u32), String>,
    labels: HashMap<(u32, u32), String>,
}

/// Reads a `/dev/disk/by-*` directory: device number → link name (udev escapes decoded).
fn read_links(dir: &str) -> HashMap<(u32, u32), String> {
    let mut map = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let Some(name) = e.file_name().to_str().map(unescape_udev) else {
                continue;
            };
            // Follow the symlink to the device node and take its device number.
            if let Ok(md) = std::fs::metadata(e.path()) {
                let rdev = md.rdev();
                map.insert((libc::major(rdev), libc::minor(rdev)), name);
            }
        }
    }
    map
}

/// Decodes udev’s `\xHH` escapes (`My\x20Disk` → `My Disk`).
fn unescape_udev(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && b.get(i + 1) == Some(&b'x')
            && let Some(v) = s
                .get(i + 2..i + 4)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 4;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl UuidMap {
    /// Reads `/dev/disk/by-uuid` and `/dev/disk/by-label` (empty where they don’t exist, as in
    /// many containers).
    #[must_use]
    pub fn load() -> Self {
        Self {
            uuids: read_links("/dev/disk/by-uuid"),
            labels: read_links("/dev/disk/by-label"),
        }
    }

    #[must_use]
    pub fn get(&self, major: u32, minor: u32) -> Option<&str> {
        self.uuids.get(&(major, minor)).map(String::as_str)
    }

    /// The filesystem label of a device.
    #[must_use]
    pub fn label(&self, major: u32, minor: u32) -> Option<&str> {
        self.labels.get(&(major, minor)).map(String::as_str)
    }

    #[cfg(test)]
    pub fn insert(&mut self, major: u32, minor: u32, uuid: &str) {
        self.uuids.insert((major, minor), uuid.to_owned());
    }
}

/// A volume’s identity key and whether it is stable across mounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub key: String,
    /// False when the key depends on the mount point (it changes if the volume moves).
    pub stable: bool,
}

/// Computes the identity of a mount.
#[must_use]
pub fn identify(m: &MountInfo, class: Class, uuids: &UuidMap) -> Identity {
    let root = String::from_utf8_lossy(&m.root);
    if let Some(uuid) = uuids.get(m.major, m.minor) {
        let mut key = format!("uuid:{uuid}");
        if m.fstype == "btrfs" {
            match m.super_option("subvolid") {
                Some(id) => key.push_str(&format!(":subvol={id}")),
                None => key.push_str(&format!(":root={root}")),
            }
        } else if root != "/" {
            // A bind mount of a subdirectory.
            key.push_str(&format!(":root={root}"));
        }
        return Identity { key, stable: true };
    }
    if class == Class::Network {
        return Identity {
            key: format!(
                "net:{}:{}:{}",
                m.fstype,
                String::from_utf8_lossy(&m.source),
                root
            ),
            stable: true,
        };
    }
    Identity {
        key: format!(
            "path:{}:{}",
            m.fstype,
            String::from_utf8_lossy(&m.mount_point)
        ),
        stable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mounts::parse_mountinfo;

    #[test]
    fn identities() {
        let m = parse_mountinfo(
            "1 0 259:2 /@ / rw - btrfs /dev/nvme0n1p2 rw,subvolid=256,subvol=/@\n\
             2 1 259:1 / /boot rw - vfat /dev/nvme0n1p1 rw\n\
             3 1 0:50 / /mnt/nas rw - nfs4 nas:/export rw\n\
             4 1 0:40 / /tmp rw - tmpfs tmpfs rw\n",
        );
        let mut u = UuidMap::default();
        u.insert(259, 2, "aaaa-bbbb");
        u.insert(259, 1, "1234-ABCD");
        assert_eq!(
            identify(&m[0], Class::Local, &u).key,
            "uuid:aaaa-bbbb:subvol=256"
        );
        assert_eq!(identify(&m[1], Class::Local, &u).key, "uuid:1234-ABCD");
        assert_eq!(unescape_udev("My\\x20Disk"), "My Disk");
        let nas = identify(&m[2], Class::Network, &u);
        assert_eq!(nas.key, "net:nfs4:nas:/export:/");
        assert!(nas.stable);
        let tmp = identify(&m[3], Class::Memory, &u);
        assert_eq!(tmp.key, "path:tmpfs:/tmp");
        assert!(!tmp.stable);
    }
}
