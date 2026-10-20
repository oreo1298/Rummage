//! Permissions: POSIX mode bits and ACLs, and which index entries a user may see.
//!
//! The daemon sees every name on the system, so every result is filtered per client. An entry is
//! **visible** to a user iff every directory from its volume root down to its parent can be listed
//! (read and search permission) by that user, and the volume’s mount point is itself visible.
//! That is stricter than “read + execute on the parent, execute on the ancestors”: with that rule
//! a user could learn the name of a listable directory inside a directory they can only traverse.
//! Root sees everything.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::index::{Generation, NONE, PAGE_SIZE, Volume, flags};
use crate::util::Bitmap;

/// Read permission bit.
pub const R: u32 = 4;
/// Write permission bit.
pub const W: u32 = 2;
/// Execute / search permission bit.
pub const X: u32 = 1;

/// ACL entry tags, as in the `system.posix_acl_access` xattr.
pub mod tag {
    pub const USER_OBJ: u16 = 0x01;
    pub const USER: u16 = 0x02;
    pub const GROUP_OBJ: u16 = 0x04;
    pub const GROUP: u16 = 0x08;
    pub const MASK: u16 = 0x10;
    pub const OTHER: u16 = 0x20;
}

/// One POSIX ACL entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AclEntry {
    pub tag: u16,
    /// `rwx` bits (4, 2, 1).
    pub perm: u16,
    /// User or group id for `USER` and `GROUP` entries.
    pub id: u32,
}

/// The xattr holding a file’s access ACL.
pub const ACL_XATTR: &str = "system.posix_acl_access";

/// Parses a `system.posix_acl_access` value (version 2: a `u32` header, then 8-byte entries).
/// Qualifiers of entries other than `USER` and `GROUP` (the kernel stores `ACL_UNDEFINED_ID`)
/// become 0. Returns `None` if the value is malformed; callers must then treat the object as
/// inaccessible.
#[must_use]
pub fn parse_acl(bytes: &[u8]) -> Option<Vec<AclEntry>> {
    let (head, rest) = bytes.split_first_chunk::<4>()?;
    if u32::from_le_bytes(*head) != 2 || rest.len() % 8 != 0 {
        return None;
    }
    let entries: Vec<AclEntry> = rest
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| {
            let tag = u16::from_le_bytes([c[0], c[1]]);
            let qualified = tag == tag::USER || tag == tag::GROUP;
            AclEntry {
                tag,
                perm: u16::from_le_bytes([c[2], c[3]]),
                id: if qualified {
                    u32::from_le_bytes([c[4], c[5], c[6], c[7]])
                } else {
                    0
                },
            }
        })
        .collect();
    let known = [
        tag::USER_OBJ,
        tag::USER,
        tag::GROUP_OBJ,
        tag::GROUP,
        tag::MASK,
        tag::OTHER,
    ];
    entries
        .iter()
        .all(|e| known.contains(&e.tag) && e.perm <= 7)
        .then_some(entries)
}

/// Encodes ACL entries in the xattr format (the inverse of [`parse_acl`]).
#[must_use]
pub fn encode_acl(acl: &[AclEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + acl.len() * 8);
    out.extend_from_slice(&2u32.to_le_bytes());
    for e in acl {
        out.extend_from_slice(&e.tag.to_le_bytes());
        out.extend_from_slice(&e.perm.to_le_bytes());
        out.extend_from_slice(&e.id.to_le_bytes());
    }
    out
}

/// A client’s credentials.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Creds {
    pub uid: u32,
    pub gid: u32,
    /// Primary and supplementary groups, sorted and deduplicated.
    groups: Vec<u32>,
}

impl Creds {
    /// Credentials with a primary group and supplementary groups.
    #[must_use]
    pub fn new(uid: u32, gid: u32, supplementary: &[u32]) -> Self {
        let mut groups: Vec<u32> = supplementary.to_vec();
        groups.push(gid);
        groups.sort_unstable();
        groups.dedup();
        Self { uid, gid, groups }
    }

    /// Superuser credentials.
    #[must_use]
    pub fn root() -> Self {
        Self::new(0, 0, &[])
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.uid == 0
    }

    /// All groups (primary included).
    #[must_use]
    pub fn groups(&self) -> &[u32] {
        &self.groups
    }

    #[must_use]
    pub fn in_group(&self, gid: u32) -> bool {
        self.groups.binary_search(&gid).is_ok()
    }
}

/// True if `creds` hold all permission bits in `want` on an object with this mode, owner and
/// (optional) access ACL. Follows the kernel’s POSIX ACL algorithm. Root is always allowed.
#[must_use]
pub fn allowed(
    creds: &Creds,
    want: u32,
    mode: u32,
    uid: u32,
    gid: u32,
    acl: Option<&[AclEntry]>,
) -> bool {
    if creds.is_root() {
        return true;
    }
    let has = |bits: u32| bits & want == want;
    if creds.uid == uid {
        return has((mode >> 6) & 7);
    }
    let Some(acl) = acl.filter(|a| !a.is_empty()) else {
        if creds.in_group(gid) {
            return has((mode >> 3) & 7);
        }
        return has(mode & 7);
    };
    let mask = acl
        .iter()
        .find(|e| e.tag == tag::MASK)
        .map_or(7, |e| u32::from(e.perm));
    if let Some(e) = acl.iter().find(|e| e.tag == tag::USER && e.id == creds.uid) {
        return has(u32::from(e.perm) & mask);
    }
    // Group class: granted if any matching entry grants everything; denied if some matched.
    let mut matched = false;
    for e in acl {
        let applies = match e.tag {
            tag::GROUP_OBJ => creds.in_group(gid),
            tag::GROUP => creds.in_group(e.id),
            _ => false,
        };
        if applies {
            if has(u32::from(e.perm) & mask) {
                return true;
            }
            matched = true;
        }
    }
    if matched {
        return false;
    }
    let other = acl
        .iter()
        .find(|e| e.tag == tag::OTHER)
        .map_or(mode & 7, |e| u32::from(e.perm));
    has(other)
}

/// What one user may see of one generation.
#[derive(Clone, Debug)]
pub struct Visibility {
    /// Entries whose names the user may see.
    pub entries: Bitmap,
    /// Visible directories the user may list. Their children are visible, and only for these
    /// may child counts be shown.
    pub listable: Bitmap,
}

impl Visibility {
    /// Number of visible entries.
    #[must_use]
    pub fn count(&self) -> usize {
        self.entries.count()
    }
}

/// Computes what `creds` may see. `mount_visible(volume)` says whether the volume’s mount path is
/// visible: the daemon checks the directories above the mount point on the live filesystem.
#[must_use]
pub fn visibility(
    g: &Generation,
    creds: &Creds,
    mount_visible: &(dyn Fn(&Volume) -> bool + Sync),
) -> Visibility {
    let d = &g.data;
    let roots: Vec<bool> = d.volumes.iter().map(mount_visible).collect();
    let root_ok = |id: u32| -> bool {
        let page = (id as usize) / PAGE_SIZE;
        d.page_volume
            .get(page)
            .and_then(|&v| roots.get(v as usize))
            .copied()
            .unwrap_or(false)
    };
    let listable_dir = |id: u32| -> bool {
        let (uid, gid) = d.owner_of(id);
        let acl = if d.flags_of(id) & flags::HAS_ACL != 0 {
            // A directory flagged with an ACL that we can’t find is treated as inaccessible.
            match d.side.acl.get(&id) {
                Some(a) => Some(&a[..]),
                None => return false,
            }
        } else {
            None
        };
        allowed(creds, R | X, u32::from(d.mode_of(id)), uid, gid, acl)
    };
    // open[dir] = the directory is visible and listable: 0 unknown, 1 open, 2 closed.
    let ndirs = d.dirs.capacity();
    let mut open = vec![0u8; ndirs];
    let mut chain = Vec::new();
    for dir in 0..ndirs {
        let entry = d.dirs.entry.get(dir);
        if entry == NONE || open[dir] != 0 || !d.is_live(entry) {
            continue;
        }
        // Walk up to a known directory (or the volume root), then resolve top-down.
        chain.clear();
        let mut cur = entry;
        let mut above = None;
        loop {
            chain.push(cur);
            match d.parent_of(cur) {
                Some(p) => {
                    let pd = d.dir_of(p).map_or(usize::MAX, |x| x as usize);
                    match open.get(pd) {
                        Some(&s) if s != 0 => {
                            above = Some(s == 1);
                            break;
                        }
                        Some(_) if chain.len() <= crate::index::MAX_DEPTH => cur = p,
                        _ => {
                            above = Some(false);
                            break;
                        }
                    }
                }
                None => break,
            }
        }
        let mut parent_open = above;
        for &e in chain.iter().rev() {
            let visible = parent_open.unwrap_or_else(|| root_ok(e));
            let is_open = visible && listable_dir(e);
            if let Some(x) = d.dir_of(e) {
                open[x as usize] = if is_open { 1 } else { 2 };
            }
            parent_open = Some(is_open);
        }
    }
    let pages = d.page_count();
    let words: Vec<(Vec<u64>, Vec<u64>)> = (0..pages)
        .into_par_iter()
        .map(|page| {
            let mut vis = vec![0u64; PAGE_SIZE / 64];
            let mut lst = vec![0u64; PAGE_SIZE / 64];
            for slot in 0..PAGE_SIZE {
                let id = u32::try_from(page * PAGE_SIZE + slot).unwrap_or(NONE);
                if !d.is_live(id) {
                    continue;
                }
                let v = match d.parent_of(id) {
                    Some(p) => d.dir_of(p).is_some_and(|x| open[x as usize] == 1),
                    None => root_ok(id),
                };
                if v {
                    vis[slot / 64] |= 1 << (slot % 64);
                }
                if d.dir_of(id).is_some_and(|x| open[x as usize] == 1) {
                    lst[slot / 64] |= 1 << (slot % 64);
                }
            }
            (vis, lst)
        })
        .collect();
    let mut entries = Bitmap::with_capacity(pages * PAGE_SIZE);
    let mut listable = Bitmap::with_capacity(pages * PAGE_SIZE);
    for (page, (vis, lst)) in words.iter().enumerate() {
        let r = page * PAGE_SIZE / 64..(page + 1) * PAGE_SIZE / 64;
        entries.words_mut()[r.clone()].copy_from_slice(vis);
        listable.words_mut()[r].copy_from_slice(lst);
    }
    Visibility { entries, listable }
}

#[cfg(test)]
mod tests;
