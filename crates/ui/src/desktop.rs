//! Acting on results the way the desktop expects: open through the portal, show in the file
//! manager, move to the trash, rename, delete.
//!
//! Every function blocks (D-Bus calls, file operations); frontends call them off their GUI
//! thread.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use zbus::blocking::Connection;
use zbus::zvariant::{Fd, Value};

/// Why an action failed (a message for the user).
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
}

fn path_of(p: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(p))
}

/// A `file://` URI for a raw path (everything but unreserved characters and `/` escaped).
#[must_use]
pub fn file_uri(path: &[u8]) -> String {
    let mut s = String::from("file://");
    for &b in path {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            s.push(char::from(b));
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn portal_open(path: &[u8], ask: bool, token: Option<&str>) -> Result<(), ActionError> {
    let conn = Connection::session().map_err(|e| ActionError::Message(e.to_string()))?;
    let file = std::fs::File::open(path_of(path))?;
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("ask", Value::from(ask));
    if let Some(t) = token {
        options.insert("activation_token", Value::from(t));
    }
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.OpenURI"),
        "OpenFile",
        &("", Fd::from(&file), options),
    )
    .map_err(|e| ActionError::Message(e.to_string()))?;
    Ok(())
}

fn spawn_detached(program: &str, arg: &Path) -> Result<(), ActionError> {
    Command::new(program)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| ActionError::Message(format!("cannot run {program}: {e}")))
}

/// Opens a file or folder with its default application: the OpenURI portal, or `xdg-open` where
/// no portal runs.
pub fn open(path: &[u8], activation_token: Option<&str>) -> Result<(), ActionError> {
    portal_open(path, false, activation_token)
        .or_else(|_| spawn_detached("xdg-open", path_of(path)))
}

/// Lets the user choose the application (the portal’s chooser).
pub fn open_with(path: &[u8], activation_token: Option<&str>) -> Result<(), ActionError> {
    portal_open(path, true, activation_token)
}

/// Opens the containing folders with the items selected (`org.freedesktop.FileManager1`), or
/// opens the folder of the first item where no file manager implements it.
pub fn show_in_folder(paths: &[Vec<u8>], startup_id: &str) -> Result<(), ActionError> {
    let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
    let shown = Connection::session().and_then(|conn| {
        conn.call_method(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            Some("org.freedesktop.FileManager1"),
            "ShowItems",
            &(uris, startup_id),
        )
    });
    match shown {
        Ok(_) => Ok(()),
        Err(_) => {
            let first = paths
                .first()
                .ok_or_else(|| ActionError::Message("nothing selected".into()))?;
            let dir = path_of(first).parent().unwrap_or_else(|| Path::new("/"));
            spawn_detached("xdg-open", dir)
        }
    }
}

/// Moves files and folders to the trash (freedesktop.org trash specification).
pub fn trash(paths: &[Vec<u8>]) -> Result<(), ActionError> {
    trash::delete_all(paths.iter().map(|p| path_of(p)))
        .map_err(|e| ActionError::Message(e.to_string()))
}

/// Deletes permanently (folders with their contents; symlinks are removed, not followed).
pub fn delete(paths: &[Vec<u8>]) -> Result<(), ActionError> {
    for p in paths {
        let path = path_of(p);
        let md = std::fs::symlink_metadata(path)?;
        if md.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// Renames `path` within its folder. Refuses names with `/`, `.`/`..` and existing targets.
/// Returns the new path.
pub fn rename(path: &[u8], new_name: &[u8]) -> Result<Vec<u8>, ActionError> {
    if new_name.is_empty()
        || new_name.contains(&b'/')
        || new_name.contains(&0)
        || new_name == b"."
        || new_name == b".."
    {
        return Err(ActionError::Message("that is not a valid name".into()));
    }
    let from = path_of(path);
    let dir = from.parent().unwrap_or_else(|| Path::new("/"));
    let to: PathBuf = dir.join(OsStr::from_bytes(new_name));
    if to == from {
        return Ok(path.to_vec());
    }
    match rustix::fs::renameat_with(
        rustix::fs::CWD,
        from,
        rustix::fs::CWD,
        &to,
        rustix::fs::RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {}
        Err(rustix::io::Errno::EXIST) => {
            return Err(ActionError::Message(format!(
                "{} already exists",
                to.display()
            )));
        }
        // Filesystems without RENAME_NOREPLACE.
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS) => {
            if std::fs::symlink_metadata(&to).is_ok() {
                return Err(ActionError::Message(format!(
                    "{} already exists",
                    to.display()
                )));
            }
            std::fs::rename(from, &to)?;
        }
        Err(e) => return Err(ActionError::Io(e.into())),
    }
    Ok(to.as_os_str().as_bytes().to_vec())
}

/// What a Properties dialog shows (read from the filesystem, not the index).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Properties {
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub changed: Option<i64>,
    pub created: Option<i64>,
    pub permissions: String,
    pub mode_octal: String,
    pub owner: String,
    pub inode: u64,
    pub links: u64,
    pub symlink_target: Option<String>,
}

/// Reads properties of `path` (without following a final symlink).
pub fn properties(path: &[u8]) -> Result<Properties, ActionError> {
    use std::os::unix::fs::MetadataExt;
    let p = path_of(path);
    let md = std::fs::symlink_metadata(p)?;
    let ft = md.file_type();
    let kind = if ft.is_dir() {
        fossick_proto::Kind::Folder
    } else if ft.is_symlink() {
        fossick_proto::Kind::Symlink
    } else if ft.is_file() {
        fossick_proto::Kind::File
    } else {
        fossick_proto::Kind::Other
    };
    let secs = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok())
    };
    let mut names = crate::rows::Names::default();
    let owner = names.owner(md.uid(), md.gid());
    Ok(Properties {
        path: String::from_utf8_lossy(path).into_owned(),
        kind: match kind {
            fossick_proto::Kind::Folder => "folder",
            fossick_proto::Kind::Symlink => "symlink",
            fossick_proto::Kind::File => "file",
            fossick_proto::Kind::Other => "special file",
        }
        .into(),
        size: md.len(),
        modified: secs(md.modified()),
        accessed: secs(md.accessed()),
        changed: Some(md.ctime()),
        created: secs(md.created()),
        permissions: crate::rows::permissions(md.mode(), kind),
        mode_octal: format!("{:04o}", md.mode() & 0o7777),
        owner,
        inode: md.ino(),
        links: md.nlink(),
        symlink_target: std::fs::read_link(p)
            .ok()
            .map(|t| t.to_string_lossy().into_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris() {
        assert_eq!(file_uri(b"/a b/c\xff.txt"), "file:///a%20b/c%FF.txt");
        assert_eq!(file_uri("/é".as_bytes()), "file:///%C3%A9");
    }

    #[test]
    fn rename_delete_properties() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, b"hello").unwrap();
        std::fs::write(dir.path().join("taken"), b"").unwrap();
        let ab = a.as_os_str().as_bytes();
        assert!(rename(ab, b"x/y").is_err());
        assert!(rename(ab, b"..").is_err());
        assert!(rename(ab, b"taken").is_err(), "no overwriting");
        let b = rename(ab, b"b.txt").unwrap();
        assert!(path_of(&b).exists() && !a.exists());
        let props = properties(&b).unwrap();
        assert_eq!(props.size, 5);
        assert_eq!(props.kind, "file");
        assert!(props.permissions.starts_with('-'));
        std::fs::create_dir_all(dir.path().join("d/e")).unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("link")).unwrap();
        let victims: Vec<Vec<u8>> = ["d", "link", "b.txt"]
            .iter()
            .map(|n| dir.path().join(n).as_os_str().as_bytes().to_vec())
            .collect();
        delete(&victims).unwrap();
        assert!(
            dir.path().join("taken").exists(),
            "the symlink was not followed"
        );
        assert!(!dir.path().join("d").exists());
    }
}
