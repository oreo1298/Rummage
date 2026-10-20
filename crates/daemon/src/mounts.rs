//! Mount discovery: parsing `/proc/self/mountinfo`, deciding which mounts become volumes, and
//! watching for mount changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::IndexSection;

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountInfo {
    pub mount_id: u64,
    pub parent_id: u64,
    pub major: u32,
    pub minor: u32,
    /// Path of the mounted subtree inside its filesystem (`/` unless a bind mount or btrfs
    /// subvolume).
    pub root: Vec<u8>,
    pub mount_point: Vec<u8>,
    pub options: String,
    pub fstype: String,
    pub source: Vec<u8>,
    pub super_options: String,
}

impl MountInfo {
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.options.split(',').any(|o| o == "ro")
    }

    #[must_use]
    pub fn mount_path(&self) -> &Path {
        use std::os::unix::ffi::OsStrExt;
        Path::new(std::ffi::OsStr::from_bytes(&self.mount_point))
    }

    /// A `key=value` super option (`subvolid`, `subvol` …).
    #[must_use]
    pub fn super_option(&self, key: &str) -> Option<&str> {
        self.super_options
            .split(',')
            .find_map(|o| o.strip_prefix(key)?.strip_prefix('='))
    }
}

/// Decodes the octal escapes mountinfo uses for space, tab, newline and backslash.
fn unescape(field: &str) -> Vec<u8> {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Parses mountinfo text. Malformed lines are skipped.
#[must_use]
pub fn parse_mountinfo(text: &str) -> Vec<MountInfo> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<MountInfo> {
    let mut f = line.split(' ');
    let mount_id = f.next()?.parse().ok()?;
    let parent_id = f.next()?.parse().ok()?;
    let (major, minor) = f.next()?.split_once(':')?;
    let root = unescape(f.next()?);
    let mount_point = unescape(f.next()?);
    let options = f.next()?.to_owned();
    // Optional fields end at a lone "-".
    for x in f.by_ref() {
        if x == "-" {
            break;
        }
    }
    let fstype = f.next()?.to_owned();
    let source = unescape(f.next()?);
    let super_options = f.next().unwrap_or("").to_owned();
    Some(MountInfo {
        mount_id,
        parent_id,
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
        root,
        mount_point,
        options,
        fstype,
        source,
        super_options,
    })
}

/// How a mount is treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Never indexed (pseudo filesystems, container layers, …).
    Excluded,
    /// Indexed and watched live by default.
    Local,
    /// Read-only media: indexed once, never watched.
    Static,
    /// tmpfs / ramfs: indexed only with `index_tmpfs`.
    Memory,
    /// Network or remote filesystem: indexed by polling only when listed in `network`.
    Network,
}

const PSEUDO: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "cgroup",
    "cgroup2",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "bpf",
    "configfs",
    "fusectl",
    "mqueue",
    "hugetlbfs",
    "autofs",
    "binfmt_misc",
    "efivarfs",
    "rpc_pipefs",
    "nsfs",
    "selinuxfs",
    "ramfs_internal",
    "nfsd",
    "fuse.gvfsd-fuse",
    "fuse.portal",
    "fuse.lxcfs",
    "fuse.xdg-document-portal",
    "fuse.doc",
    "zonefs",
];

const NETWORK: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "9p",
    "ceph",
    "afs",
    "lustre",
    "glusterfs",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.davfs2",
    "davfs",
    "fuse.glusterfs",
    "fuse.s3fs",
    "fuse.gcsfuse",
];

const LOCAL: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "vfat", "msdos", "exfat", "ntfs3", "ntfs",
    "fuseblk", "jfs", "reiserfs", "nilfs2", "bcachefs", "zfs", "hfsplus", "hfs", "apfs", "ufs",
];

const READ_ONLY_MEDIA: &[&str] = &["iso9660", "udf", "squashfs", "erofs", "cramfs"];

/// Mount points under these never become volumes (except `/run/media`, where desktops mount
/// removable drives).
const SYSTEM_PREFIXES: &[&[u8]] = &[
    b"/proc",
    b"/sys",
    b"/dev",
    b"/run",
    b"/snap",
    b"/var/lib/docker",
    b"/var/lib/containers",
    b"/var/lib/snapd",
    b"/var/snap",
];

fn under(path: &[u8], prefix: &[u8]) -> bool {
    path == prefix || (path.starts_with(prefix) && path.get(prefix.len()) == Some(&b'/'))
}

/// Classifies a mount by its filesystem type and mount point.
#[must_use]
pub fn classify(m: &MountInfo) -> Class {
    let mp = m.mount_point.as_slice();
    let fs = m.fstype.as_str();
    if SYSTEM_PREFIXES.iter().any(|p| under(mp, p)) && !under(mp, b"/run/media") {
        return Class::Excluded;
    }
    if PSEUDO.contains(&fs) {
        return Class::Excluded;
    }
    if NETWORK.contains(&fs) {
        return Class::Network;
    }
    if fs == "tmpfs" || fs == "ramfs" {
        return Class::Memory;
    }
    // Overlays are container layers, except as the root of a live or container system.
    if fs == "overlay" {
        return if mp == b"/" {
            Class::Local
        } else {
            Class::Excluded
        };
    }
    if READ_ONLY_MEDIA.contains(&fs) {
        return Class::Static;
    }
    if LOCAL.contains(&fs) {
        return if m.read_only() {
            Class::Static
        } else {
            Class::Local
        };
    }
    // Unknown types: index them if they are backed by a block device.
    if m.source.starts_with(b"/dev/") {
        Class::Local
    } else {
        Class::Excluded
    }
}

/// A mount chosen to be indexed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub mount: MountInfo,
    pub class: Class,
    /// Paths inside this mount that other volumes cover (bind mounts and btrfs subvolumes
    /// mounted elsewhere), excluded from its scan.
    pub exclude: Vec<PathBuf>,
}

/// Decides which mounts are indexed: classification, the configuration, and deduplication of
/// filesystems mounted more than once.
#[must_use]
pub fn plan(mounts: &[MountInfo], cfg: &IndexSection) -> Vec<Planned> {
    let wanted = |m: &MountInfo| -> Option<Class> {
        if cfg
            .exclude
            .iter()
            .any(|e| under(&m.mount_point, path_bytes(e)))
        {
            return None;
        }
        if !cfg.volumes.is_empty() && !cfg.volumes.iter().any(|v| path_bytes(v) == m.mount_point) {
            return None;
        }
        match classify(m) {
            Class::Excluded => None,
            Class::Memory if !cfg.index_tmpfs => None,
            Class::Network if !cfg.network.iter().any(|n| path_bytes(n) == m.mount_point) => None,
            c => Some(c),
        }
    };
    let mut chosen: Vec<Planned> = Vec::new();
    // The same tree mounted twice (same device and root): keep the shortest mount point.
    let mut by_tree: HashMap<(u32, u32, Vec<u8>), usize> = HashMap::new();
    let mut order: Vec<&MountInfo> = mounts.iter().collect();
    order.sort_by(|a, b| {
        (a.mount_point.len(), &a.mount_point).cmp(&(b.mount_point.len(), &b.mount_point))
    });
    for m in order {
        let Some(class) = wanted(m) else {
            continue;
        };
        // tmpfs instances all share major 0, so they are deduplicated by mount id instead.
        let key = if m.major == 0 {
            (0, u32::try_from(m.mount_id).unwrap_or(u32::MAX), Vec::new())
        } else {
            (m.major, m.minor, m.root.clone())
        };
        if by_tree.contains_key(&key) {
            continue;
        }
        by_tree.insert(key, chosen.len());
        chosen.push(Planned {
            mount: m.clone(),
            class,
            exclude: Vec::new(),
        });
    }
    // A mount whose root lies inside another chosen mount’s root on the same device (a bind
    // mount of a subdirectory, or a btrfs subvolume also reachable from the top-level one) is
    // the primary place for that subtree: exclude it from the other mount’s scan.
    let snapshot: Vec<(u32, u32, Vec<u8>, Vec<u8>)> = chosen
        .iter()
        .map(|p| {
            (
                p.mount.major,
                p.mount.minor,
                p.mount.root.clone(),
                p.mount.mount_point.clone(),
            )
        })
        .collect();
    for p in &mut chosen {
        if p.mount.major == 0 {
            continue;
        }
        for (maj, min, root, _) in &snapshot {
            let (maj, min) = (*maj, *min);
            if (maj, min) != (p.mount.major, p.mount.minor) || *root == p.mount.root {
                continue;
            }
            let base = if p.mount.root == b"/" {
                &b""[..]
            } else {
                &p.mount.root[..]
            };
            if root.starts_with(base) && root.get(base.len()) == Some(&b'/') {
                let rel = &root[base.len()..];
                let mut path = p.mount.mount_point.clone();
                if path != b"/" {
                    path.extend_from_slice(rel);
                } else {
                    path = rel.to_vec();
                }
                p.exclude.push(bytes_path(&path));
            }
        }
    }
    if !cfg.include_snapshots {
        for p in &mut chosen {
            for snap in [&b"/.snapshots"[..], b"/timeshift", b"/timeshift-btrfs"] {
                let mut path = if p.mount.mount_point == b"/" {
                    Vec::new()
                } else {
                    p.mount.mount_point.clone()
                };
                path.extend_from_slice(snap);
                p.exclude.push(bytes_path(&path));
            }
        }
    }
    for p in &mut chosen {
        p.exclude.extend(
            cfg.exclude
                .iter()
                .filter(|e| under(path_bytes(e), &p.mount.mount_point))
                .cloned(),
        );
        p.exclude.sort();
        p.exclude.dedup();
    }
    chosen
}

fn path_bytes(p: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes()
}

fn bytes_path(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(b))
}

/// Reads the current mount table.
pub fn read_mounts() -> std::io::Result<Vec<MountInfo>> {
    Ok(parse_mountinfo(&std::fs::read_to_string(
        "/proc/self/mountinfo",
    )?))
}

/// Blocks until the mount table changes (`POLLPRI` on `/proc/self/mountinfo`), or the timeout
/// passes. Returns `true` on a change.
pub fn wait_for_mount_change(
    file: &std::fs::File,
    timeout: std::time::Duration,
) -> std::io::Result<bool> {
    use rustix::event::{PollFd, PollFlags, poll};
    let mut fds = [PollFd::new(file, PollFlags::PRI | PollFlags::ERR)];
    let ts = rustix::event::Timespec {
        tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(timeout.subsec_nanos()),
    };
    let n = poll(&mut fds, Some(&ts))?;
    if n > 0 {
        // Re-arm: POLLPRI stays set until the file is read again from the start.
        use std::io::{Read, Seek};
        let mut f = file;
        f.seek(std::io::SeekFrom::Start(0))?;
        let mut sink = Vec::new();
        f.read_to_end(&mut sink)?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
22 1 0:21 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
23 1 0:22 / /sys rw,nosuid shared:2 - sysfs sysfs rw
1 0 259:2 /@ / rw,noatime shared:1 - btrfs /dev/nvme0n1p2 rw,ssd,subvolid=256,subvol=/@
30 1 259:2 /@home /home rw,noatime shared:30 - btrfs /dev/nvme0n1p2 rw,ssd,subvolid=257,subvol=/@home
31 1 259:2 / /mnt/pool rw,noatime shared:31 - btrfs /dev/nvme0n1p2 rw,ssd,subvolid=5,subvol=/
32 1 259:1 / /boot rw,relatime shared:32 - vfat /dev/nvme0n1p1 rw,fmask=0022
33 1 0:40 / /tmp rw,nosuid,nodev shared:33 - tmpfs tmpfs rw,size=8G
34 30 259:2 /@home/alice/data /srv/data rw shared:34 - btrfs /dev/nvme0n1p2 rw,subvolid=257
35 1 0:50 / /mnt/nas rw shared:35 - nfs4 nas:/export rw,vers=4.2
36 1 0:60 / /var/lib/docker/overlay2/abc/merged rw - overlay overlay rw,lowerdir=/x
37 1 7:0 / /run/media/alice/USB\\040Stick rw,nosuid shared:37 - exfat /dev/sdb1 rw
38 1 7:1 / /snap/core/123 ro,nodev shared:38 - squashfs /dev/loop1 ro
39 1 11:0 / /mnt/cdrom ro shared:39 - iso9660 /dev/sr0 ro
";

    #[test]
    fn parses_mountinfo_with_escapes() {
        let m = parse_mountinfo(SAMPLE);
        assert_eq!(m.len(), 13);
        let usb = m.iter().find(|x| x.fstype == "exfat").unwrap();
        assert_eq!(usb.mount_point, b"/run/media/alice/USB Stick");
        assert_eq!(usb.source, b"/dev/sdb1");
        let home = &m[3];
        assert_eq!((home.major, home.minor), (259, 2));
        assert_eq!(home.root, b"/@home");
        assert_eq!(home.super_option("subvolid"), Some("257"));
        assert!(parse_mountinfo("garbage\n1 2 x\n").is_empty());
    }

    #[test]
    fn classification() {
        let m = parse_mountinfo(SAMPLE);
        let class = |mp: &str| classify(m.iter().find(|x| x.mount_point == mp.as_bytes()).unwrap());
        assert_eq!(class("/proc"), Class::Excluded);
        assert_eq!(class("/"), Class::Local);
        assert_eq!(class("/boot"), Class::Local);
        assert_eq!(class("/tmp"), Class::Memory);
        assert_eq!(class("/mnt/nas"), Class::Network);
        assert_eq!(
            class("/var/lib/docker/overlay2/abc/merged"),
            Class::Excluded
        );
        assert_eq!(class("/run/media/alice/USB Stick"), Class::Local);
        assert_eq!(class("/snap/core/123"), Class::Excluded);
        assert_eq!(class("/mnt/cdrom"), Class::Static);
    }

    #[test]
    fn planning_dedupes_and_excludes_subtrees() {
        let m = parse_mountinfo(SAMPLE);
        let cfg = IndexSection::default();
        let p = plan(&m, &cfg);
        let points: Vec<&[u8]> = p.iter().map(|x| x.mount.mount_point.as_slice()).collect();
        assert_eq!(
            points,
            [
                &b"/"[..],
                b"/boot",
                b"/home",
                b"/mnt/pool",
                b"/srv/data",
                b"/mnt/cdrom",
                b"/run/media/alice/USB Stick"
            ]
        );
        // The top-level btrfs mount excludes the subvolumes mounted as / and /home.
        let pool = p
            .iter()
            .find(|x| x.mount.mount_point == b"/mnt/pool")
            .unwrap();
        assert!(pool.exclude.contains(&PathBuf::from("/mnt/pool/@")));
        assert!(pool.exclude.contains(&PathBuf::from("/mnt/pool/@home")));
        // /home excludes the bind-mounted subdirectory that /srv/data covers.
        let home = p.iter().find(|x| x.mount.mount_point == b"/home").unwrap();
        assert!(home.exclude.contains(&PathBuf::from("/home/alice/data")));
        assert!(home.exclude.contains(&PathBuf::from("/home/.snapshots")));
        // Opt-ins.
        let cfg = IndexSection {
            index_tmpfs: true,
            network: vec![PathBuf::from("/mnt/nas")],
            exclude: vec![PathBuf::from("/boot"), PathBuf::from("/home/alice/big")],
            ..IndexSection::default()
        };
        let p = plan(&m, &cfg);
        assert!(p.iter().any(|x| x.mount.mount_point == b"/tmp"));
        assert!(
            p.iter()
                .any(|x| x.mount.mount_point == b"/mnt/nas" && x.class == Class::Network)
        );
        assert!(!p.iter().any(|x| x.mount.mount_point == b"/boot"));
        let home = p.iter().find(|x| x.mount.mount_point == b"/home").unwrap();
        assert!(home.exclude.contains(&PathBuf::from("/home/alice/big")));
        // An explicit volume list.
        let cfg = IndexSection {
            volumes: vec![PathBuf::from("/home"), PathBuf::from("/mnt/cdrom")],
            ..IndexSection::default()
        };
        let points: Vec<Vec<u8>> = plan(&m, &cfg)
            .into_iter()
            .map(|p| p.mount.mount_point)
            .collect();
        assert_eq!(points, [b"/home".to_vec(), b"/mnt/cdrom".to_vec()]);
    }
}
