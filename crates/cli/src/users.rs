//! User and group name resolution through NSS (`getpwnam_r`, `getgrnam_r`).

use fossick_core::query::UserDb;
use nix::unistd::{Group, User};

/// Resolves names with the system’s NSS configuration.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemUsers;

impl UserDb for SystemUsers {
    fn uid_of(&self, name: &str) -> Option<u32> {
        User::from_name(name).ok().flatten().map(|u| u.uid.as_raw())
    }

    fn gid_of(&self, name: &str) -> Option<u32> {
        Group::from_name(name)
            .ok()
            .flatten()
            .map(|g| g.gid.as_raw())
    }
}
