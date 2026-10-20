//! Editing the daemon’s configuration from a GUI: the `[index]` settings people change, read
//! from and written back to `fossickd.toml`, keeping every other key as it was.
//!
//! The system daemon’s file belongs to root, so it is installed with
//! `pkexec fossick --install-config FILE`, which also reloads the daemon and puts the old file
//! back if the daemon rejects the new one. The user daemon’s file is written directly and the
//! daemon is asked to reload.

use std::path::{Path, PathBuf};
use std::process::Command;

use fossick_client::{Client, Target};
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

/// The system daemon’s configuration file.
pub const SYSTEM_CONFIG: &str = "/etc/fossick/fossickd.toml";

/// The `[index]` keys a settings window shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSettings {
    /// Only these mount points (empty: every suitable mount).
    pub volumes: Vec<String>,
    pub exclude: Vec<String>,
    pub exclude_names: Vec<String>,
    pub network: Vec<String>,
    pub index_tmpfs: bool,
    pub include_snapshots: bool,
    pub keep_offline: bool,
    pub track_atime: bool,
    /// "auto", "fanotify", "inotify" or "poll".
    pub watch_method: String,
}

/// The configuration file of the system daemon (`system`) or of the user’s daemon.
#[must_use]
pub fn path(system: bool) -> Option<PathBuf> {
    if system {
        return Some(PathBuf::from(SYSTEM_CONFIG));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("fossick").join("fossickd.toml"))
}

fn strings(t: &Table, key: &str) -> Vec<String> {
    t.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn boolean(t: &Table, key: &str, default: bool) -> bool {
    t.get(key).and_then(Value::as_bool).unwrap_or(default)
}

impl IndexSettings {
    /// The settings in configuration text (defaults for missing keys).
    pub fn from_text(text: &str) -> Result<Self, String> {
        let table: Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let index = table
            .get("index")
            .and_then(Value::as_table)
            .cloned()
            .unwrap_or_default();
        Ok(Self {
            volumes: strings(&index, "volumes"),
            exclude: strings(&index, "exclude"),
            exclude_names: strings(&index, "exclude_names"),
            network: strings(&index, "network"),
            index_tmpfs: boolean(&index, "index_tmpfs", false),
            include_snapshots: boolean(&index, "include_snapshots", false),
            keep_offline: boolean(&index, "keep_offline", true),
            track_atime: boolean(&index, "track_atime", false),
            watch_method: index
                .get("watch_method")
                .and_then(Value::as_str)
                .unwrap_or("auto")
                .to_owned(),
        })
    }

    /// Reads the settings of the system daemon or of the user’s daemon.
    pub fn load(system: bool) -> Result<Self, String> {
        match path(system) {
            Some(p) => Self::load_from(&p),
            None => Self::from_text(""),
        }
    }

    /// Reads the settings in `file` (defaults if it doesn’t exist).
    pub fn load_from(file: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(file) {
            Ok(text) => Self::from_text(&text).map_err(|e| format!("{}: {e}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::from_text(""),
            Err(e) => Err(format!("{}: {e}", file.display())),
        }
    }

    /// `text` with these settings in its `[index]` table; other keys are kept.
    pub fn apply_to(&self, text: &str) -> Result<String, String> {
        let mut table: Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let index = table
            .entry("index")
            .or_insert_with(|| Value::Table(Table::new()));
        let Some(index) = index.as_table_mut() else {
            return Err("[index] is not a table".into());
        };
        let list = |v: &[String]| {
            Value::Array(
                v.iter()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| Value::String(s.to_owned()))
                    .collect(),
            )
        };
        for (key, value) in [
            ("volumes", list(&self.volumes)),
            ("exclude", list(&self.exclude)),
            ("exclude_names", list(&self.exclude_names)),
            ("network", list(&self.network)),
        ] {
            if value.as_array().is_some_and(Vec::is_empty) {
                index.remove(key);
            } else {
                index.insert(key.into(), value);
            }
        }
        index.insert("index_tmpfs".into(), Value::Boolean(self.index_tmpfs));
        index.insert(
            "include_snapshots".into(),
            Value::Boolean(self.include_snapshots),
        );
        index.insert("keep_offline".into(), Value::Boolean(self.keep_offline));
        index.insert("track_atime".into(), Value::Boolean(self.track_atime));
        let method = match self.watch_method.as_str() {
            m @ ("fanotify" | "inotify" | "poll") => m,
            _ => "auto",
        };
        index.insert("watch_method".into(), Value::String(method.into()));
        let body = toml::to_string_pretty(&table).map_err(|e| e.to_string())?;
        Ok(format!(
            "# Fossick daemon configuration, last saved from the settings window.\n\
             # See /usr/share/doc/fossick/fossickd.toml for every key.\n\n{body}"
        ))
    }

    /// Saves and applies the settings (blocks; for the system daemon it waits for pkexec’s
    /// password prompt). `fossick` is the CLI used to install the system file.
    pub fn save(&self, system: bool, fossick: &Path) -> Result<(), String> {
        let p = path(system).ok_or("no home directory")?;
        if system {
            self.install_system(&p, fossick)
        } else {
            self.save_user(&p, &Target::User)
        }
    }

    fn install_system(&self, file: &Path, fossick: &Path) -> Result<(), String> {
        let text = self.apply_to(&std::fs::read_to_string(file).unwrap_or_default())?;
        let dir =
            std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let tmp = dir.join(format!("fossickd-{}.toml", std::process::id()));
        std::fs::write(&tmp, &text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let out = Command::new("pkexec")
            .arg(fossick)
            .arg("--install-config")
            .arg(&tmp)
            .output();
        let _ = std::fs::remove_file(&tmp);
        let out = out.map_err(|e| format!("cannot run pkexec: {e}"))?;
        match out.status.code() {
            Some(0) => Ok(()),
            // pkexec: the user dismissed the prompt or isn’t authorized.
            Some(126 | 127) => Err("not authorized".into()),
            _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
        }
    }

    /// Writes the user daemon’s configuration `file` and asks the daemon at `daemon` to reload
    /// it; puts the old file back if the daemon rejects the new one.
    pub fn save_user(&self, file: &Path, daemon: &Target) -> Result<(), String> {
        let old = std::fs::read_to_string(file).ok();
        let text = self.apply_to(old.as_deref().unwrap_or_default())?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = file.with_extension("toml.new");
        std::fs::write(&tmp, &text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))?;
        match Client::connect(daemon).and_then(|mut c| c.reload()) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Put the old file back so the next start doesn’t fail.
                let _ = match old {
                    Some(old) => std::fs::write(file, old),
                    None => std::fs::remove_file(file),
                };
                Err(e.to_string())
            }
        }
    }
}

/// Asks the daemon to rescan everything: through pkexec for the system daemon.
pub fn rescan_all(system: bool, fossick: &Path) -> Result<(), String> {
    if system {
        let out = Command::new("pkexec")
            .arg(fossick)
            .arg("--rescan")
            .output()
            .map_err(|e| format!("cannot run pkexec: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    } else {
        Client::connect(&Target::User)
            .and_then(|mut c| c.rescan(b""))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_other_keys() {
        let text = "# hand-written\n[index]\nscan_threads = 4\nexclude = [\"/a\"]\n\n[server]\nsocket = \"/run/x\"\n";
        let mut s = IndexSettings::from_text(text).unwrap();
        assert_eq!(s.exclude, vec!["/a"]);
        assert!(s.keep_offline, "default");
        assert_eq!(s.watch_method, "auto");
        s.exclude.push(" /b ".into());
        s.exclude_names = vec!["node_modules".into(), String::new()];
        s.index_tmpfs = true;
        s.watch_method = "nonsense".into();
        let out = s.apply_to(text).unwrap();
        let back = IndexSettings::from_text(&out).unwrap();
        assert_eq!(back.exclude, vec!["/a", "/b"]);
        assert_eq!(back.exclude_names, vec!["node_modules"]);
        assert!(back.index_tmpfs);
        assert_eq!(back.watch_method, "auto");
        let table: Table = out.parse().unwrap();
        assert_eq!(table["index"]["scan_threads"].as_integer(), Some(4));
        assert_eq!(table["server"]["socket"].as_str(), Some("/run/x"));
        assert!(
            table["index"].get("volumes").is_none(),
            "empty lists are left out"
        );
        assert!(
            IndexSettings::from_text("index = 3")
                .unwrap()
                .volumes
                .is_empty()
        );
        assert!(s.apply_to("index = 3").is_err());
    }
}
