//! Daemon configuration (`/etc/fossick/fossickd.toml`, or `~/.config/fossick/fossickd.toml` in
//! user mode). Every key is optional; see `packaging/fossickd.toml` for the documented defaults.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The whole configuration file.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub index: IndexSection,
    pub server: ServerSection,
}

/// `[index]`: what is indexed and how.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct IndexSection {
    /// Also index access times (refreshed by rescans only).
    pub track_atime: bool,
    /// If not empty, only the filesystems mounted at these paths are indexed.
    pub volumes: Vec<PathBuf>,
    /// Absolute paths never indexed (with everything below them).
    pub exclude: Vec<PathBuf>,
    /// Names never indexed anywhere, with their contents: shell patterns such as
    /// `node_modules`, `.cache` or `*.o` (`*` any characters, `?` one).
    pub exclude_names: Vec<String>,
    /// Index tmpfs and ramfs mounts. Off by default: in system mode the daemon has a private
    /// `/tmp` anyway.
    pub index_tmpfs: bool,
    /// Mount points of network filesystems (nfs, cifs, sshfs …) to index by polling.
    pub network: Vec<PathBuf>,
    /// Index snapshot directories (`.snapshots`, timeshift). Off: they duplicate everything.
    pub include_snapshots: bool,
    /// Seconds between rescans of polled volumes.
    pub poll_interval_secs: u64,
    /// Every Nth poll reads every directory again, which picks up changed file sizes and dates
    /// (they don’t change directory timestamps). The others only re-read changed directories.
    pub poll_full_every: u32,
    /// Seconds between periodic snapshot writes (when the index changed).
    pub snapshot_interval_secs: u64,
    /// Keep the index of unmounted volumes (shown as offline).
    pub keep_offline: bool,
    /// Scanner threads per volume (0 = number of CPUs).
    pub scan_threads: usize,
    /// After startup, refresh file metadata with a full low-priority rescan.
    pub refresh_metadata: bool,
    /// Largest file `content:` reads, in MiB.
    pub content_limit_mib: u64,
    /// How volumes are watched: `auto` (fanotify, else inotify, else polling), or force
    /// `fanotify`, `inotify` or `poll` (for troubleshooting).
    pub watch_method: WatchMethod,
}

/// `watch_method` values.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WatchMethod {
    #[default]
    Auto,
    Fanotify,
    Inotify,
    Poll,
}

impl Default for IndexSection {
    fn default() -> Self {
        Self {
            track_atime: false,
            volumes: Vec::new(),
            exclude: Vec::new(),
            exclude_names: Vec::new(),
            index_tmpfs: false,
            network: Vec::new(),
            include_snapshots: false,
            poll_interval_secs: 300,
            poll_full_every: 6,
            snapshot_interval_secs: 600,
            keep_offline: true,
            scan_threads: 0,
            refresh_metadata: true,
            content_limit_mib: 256,
            watch_method: WatchMethod::Auto,
        }
    }
}

/// `[server]`: the client socket.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    /// Socket path (default: `/run/fossick/socket`, or `$XDG_RUNTIME_DIR/fossick/socket` in
    /// user mode).
    pub socket: Option<PathBuf>,
}

/// Errors reading the configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid configuration in {path}: {message}")]
    Parse { path: PathBuf, message: String },
}

impl Config {
    /// Parses configuration text.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        for p in cfg
            .index
            .exclude
            .iter()
            .chain(&cfg.index.network)
            .chain(&cfg.index.volumes)
        {
            if !p.is_absolute() {
                return Err(ConfigError::Parse {
                    path: path.to_path_buf(),
                    message: format!("{} is not an absolute path", p.display()),
                });
            }
        }
        Ok(cfg)
    }

    /// Loads `path`; a missing file means the defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

/// Where the daemon keeps its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub state_dir: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    /// System-mode defaults.
    #[must_use]
    pub fn system() -> Self {
        Self {
            config: PathBuf::from("/etc/fossick/fossickd.toml"),
            state_dir: PathBuf::from("/var/lib/fossick"),
            socket: PathBuf::from("/run/fossick/socket"),
        }
    }

    /// User-mode defaults from the XDG environment.
    pub fn user() -> anyhow::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        let xdg = |var: &str, fallback: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
        };
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is not set"))?;
        Ok(Self {
            config: xdg("XDG_CONFIG_HOME", ".config").join("fossick/fossickd.toml"),
            state_dir: xdg("XDG_CACHE_HOME", ".cache").join("fossick"),
            socket: runtime.join("fossick/socket"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let p = Path::new("t.toml");
        assert_eq!(Config::parse("", p).unwrap(), Config::default());
        let c = Config::parse(
            "[index]\nexclude = [\"/srv/big\"]\nnetwork = [\"/mnt/nas\"]\ntrack_atime = true\n\
             [server]\nsocket = \"/tmp/s\"\n",
            p,
        )
        .unwrap();
        assert!(c.index.track_atime);
        assert_eq!(c.index.exclude, [PathBuf::from("/srv/big")]);
        assert_eq!(c.server.socket, Some(PathBuf::from("/tmp/s")));
        assert!(Config::parse("[index]\nexclude = [\"rel\"]\n", p).is_err());
        assert!(
            Config::parse("[index]\nbogus = 1\n", p).is_err(),
            "typos are errors"
        );
    }
}
