//! Per-client permission enforcement: who is connected, what they may see, and the files
//! `content:` may read for them.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};

use fossick_core::Generation;
use fossick_core::perm::{ACL_XATTR, AclEntry, Creds, R, Visibility, X, allowed, parse_acl, tag};
use fossick_core::search::ContentSource;

use crate::sys;

/// Credentials of a socket peer: `SO_PEERCRED` for the uid and primary gid, `SO_PEERGROUPS` for
/// supplementary groups. Without `SO_PEERGROUPS` the peer gets no supplementary groups, so
/// group-based access is denied rather than guessed.
pub fn peer_creds(sock: &std::os::unix::net::UnixStream) -> io::Result<Creds> {
    let cred = rustix::net::sockopt::socket_peercred(sock)?;
    let groups = sys::peer_groups(sock.as_fd()).unwrap_or_default();
    Ok(Creds::new(cred.uid.as_raw(), cred.gid.as_raw(), &groups))
}

/// A deny-all ACL for objects whose ACL can’t be parsed.
fn deny_all() -> Vec<AclEntry> {
    vec![AclEntry {
        tag: tag::OTHER,
        perm: 0,
        id: 0,
    }]
}

/// The access ACL of an open file (`None` if it has none).
fn acl_of_fd(f: &File) -> Option<Vec<AclEntry>> {
    let mut buf = vec![0u8; 64 * 1024];
    match rustix::fs::fgetxattr(f, ACL_XATTR, &mut buf[..]) {
        Ok(n) => Some(parse_acl(&buf[..n]).unwrap_or_else(deny_all)),
        Err(_) => None,
    }
}

/// The access ACL of a path, without following symlinks.
fn acl_of_path(p: &Path) -> Option<Vec<AclEntry>> {
    let mut buf = vec![0u8; 64 * 1024];
    match rustix::fs::lgetxattr(p, ACL_XATTR, &mut buf[..]) {
        Ok(n) => Some(parse_acl(&buf[..n]).unwrap_or_else(deny_all)),
        Err(_) => None,
    }
}

/// True if every directory above `mount_path` (not the mount point itself) is listable by
/// `creds`, checked on the live filesystem.
#[must_use]
pub fn mount_visible(mount_path: &[u8], creds: &Creds) -> bool {
    if creds.is_root() {
        return true;
    }
    let mut prefix: Vec<u8> = Vec::with_capacity(mount_path.len());
    let comps: Vec<&[u8]> = mount_path
        .split(|&b| b == b'/')
        .filter(|c| !c.is_empty())
        .collect();
    let mut ancestors = vec![b"/".to_vec()];
    for c in comps.iter().take(comps.len().saturating_sub(1)) {
        prefix.push(b'/');
        prefix.extend_from_slice(c);
        ancestors.push(prefix.clone());
    }
    if comps.is_empty() {
        return true;
    }
    ancestors.iter().all(|a| {
        let p = Path::new(OsStr::from_bytes(a));
        let Ok(md) = std::fs::symlink_metadata(p) else {
            return false;
        };
        md.is_dir()
            && allowed(
                creds,
                R | X,
                md.mode(),
                md.uid(),
                md.gid(),
                acl_of_path(p).as_deref(),
            )
    })
}

#[derive(Debug)]
struct Cached {
    generation: u64,
    vis: Arc<Visibility>,
}

/// Visibility maps per credential set, recomputed when the generation changes.
#[derive(Debug, Default)]
pub struct VisCache {
    map: Mutex<HashMap<Creds, Cached>>,
}

impl VisCache {
    /// What `creds` may see of `g` (`None`: everything, for root).
    pub fn get(&self, g: &Generation, creds: &Creds) -> Option<Arc<Visibility>> {
        if creds.is_root() {
            return None;
        }
        if let Ok(m) = self.map.lock()
            && let Some(c) = m.get(creds)
            && c.generation == g.number()
        {
            return Some(Arc::clone(&c.vis));
        }
        let vis = Arc::new(fossick_core::perm::visibility(g, creds, &|v| {
            mount_visible(&v.mount_path, creds)
        }));
        if let Ok(mut m) = self.map.lock() {
            if m.len() > 64 {
                m.clear();
            }
            m.insert(
                creds.clone(),
                Cached {
                    generation: g.number(),
                    vis: Arc::clone(&vis),
                },
            );
        }
        Some(vis)
    }
}

/// `content:` for one client: opens regular files only, and only if the client could read them
/// (mode bits and ACL, checked on the opened file so it can’t be swapped in between). The
/// entry’s directories were already checked by the visibility filter.
#[derive(Debug)]
pub struct PermContent {
    pub creds: Creds,
}

impl ContentSource for PermContent {
    fn open(&self, path: &[u8]) -> io::Result<Box<dyn Read + Send>> {
        let open = |flags| {
            File::options()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC | flags)
                .open(OsStr::from_bytes(path))
        };
        let f = open(libc::O_NOATIME).or_else(|_| open(0))?;
        let md = f.metadata()?;
        if !md.is_file() {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        if !allowed(
            &self.creds,
            R,
            md.mode(),
            md.uid(),
            md.gid(),
            acl_of_fd(&f).as_deref(),
        ) {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        Ok(Box::new(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_paths_need_listable_ancestors() {
        let nobody = Creds::new(65_534, 65_534, &[]);
        assert!(mount_visible(b"/", &nobody));
        assert!(mount_visible(b"/usr", &nobody), "/ is world-listable");
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::create_dir(locked.join("mnt")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mnt = locked.join("mnt");
        assert!(!mount_visible(mnt.as_os_str().as_bytes(), &nobody));
        assert!(mount_visible(mnt.as_os_str().as_bytes(), &Creds::root()));
    }

    #[test]
    fn content_checks_the_opened_file() {
        let tmp = tempfile::tempdir().unwrap();
        let secret = tmp.path().join("secret");
        std::fs::write(&secret, b"x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        let p = secret.as_os_str().as_bytes();
        let nobody = PermContent {
            creds: Creds::new(65_534, 65_534, &[]),
        };
        assert_eq!(
            nobody.open(p).err().and_then(|e| e.raw_os_error()),
            Some(libc::EACCES)
        );
        let me = PermContent {
            creds: Creds::new(rustix::process::getuid().as_raw(), 0, &[]),
        };
        assert!(me.open(p).is_ok());
        // FIFOs are refused without blocking.
        let fifo = tmp.path().join("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o666),
            0,
        )
        .unwrap();
        assert!(me.open(fifo.as_os_str().as_bytes()).is_err());
    }
}
