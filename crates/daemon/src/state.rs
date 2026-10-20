//! State shared between the writer thread and client connections.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use fossick_core::{Generation, IndexConfig, IndexWriter, VolumeId};
use fossick_proto::{ServerMode, UpdateMethod, VolumeState, VolumeStatus};

use crate::config::{Config, Paths};

/// Runtime status of one volume, as reported to clients.
#[derive(Clone, Debug)]
pub struct VolumeInfo {
    pub vid: VolumeId,
    pub identity: String,
    pub state: VolumeState,
    pub method: UpdateMethod,
    pub note: String,
}

/// Everything the writer publishes and connections read.
#[derive(Debug)]
pub struct Shared {
    current: ArcSwap<Generation>,
    generation: (Mutex<u64>, Condvar),
    volumes: Mutex<Vec<VolumeInfo>>,
    pub mode: ServerMode,
    config: ArcSwap<Config>,
    pub paths: Paths,
    shutdown: AtomicBool,
}

impl Shared {
    #[must_use]
    pub fn new(mode: ServerMode, config: Config, paths: Paths) -> Arc<Self> {
        let empty = IndexWriter::new(IndexConfig::default()).handle().load();
        Arc::new(Self {
            current: ArcSwap::new(empty),
            generation: (Mutex::new(0), Condvar::new()),
            volumes: Mutex::new(Vec::new()),
            mode,
            config: ArcSwap::from_pointee(config),
            paths,
            shutdown: AtomicBool::new(false),
        })
    }

    /// The configuration in effect.
    #[must_use]
    pub fn config(&self) -> Arc<Config> {
        self.config.load_full()
    }

    /// Replaces the configuration (after a reload).
    pub fn set_config(&self, config: Config) {
        self.config.store(Arc::new(config));
    }

    /// The latest generation.
    #[must_use]
    pub fn current(&self) -> Arc<Generation> {
        self.current.load_full()
    }

    /// Makes `g` the generation searches use and wakes everyone waiting for a change.
    pub fn publish(&self, g: Arc<Generation>) {
        let number = g.number();
        self.current.store(g);
        let (lock, cv) = &self.generation;
        if let Ok(mut n) = lock.lock() {
            *n = number;
        }
        cv.notify_all();
    }

    /// Waits until the generation number differs from `seen` (or the timeout passes, or the
    /// daemon shuts down). Returns the current number.
    pub fn wait_generation(&self, seen: u64, timeout: Duration) -> u64 {
        let (lock, cv) = &self.generation;
        let Ok(guard) = lock.lock() else {
            return seen;
        };
        let result = cv.wait_timeout_while(guard, timeout, |n| {
            *n == seen && !self.shutdown.load(Ordering::Relaxed)
        });
        match result {
            Ok((n, _)) => *n,
            Err(_) => seen,
        }
    }

    pub fn set_volumes(&self, v: Vec<VolumeInfo>) {
        if let Ok(mut g) = self.volumes.lock() {
            *g = v;
        }
    }

    /// Volume status for clients. `counts` are the entries per volume the client may see.
    #[must_use]
    pub fn volume_status(&self, g: &Generation, counts: &[u64]) -> Vec<VolumeStatus> {
        let infos = self.volumes.lock().map(|v| v.clone()).unwrap_or_default();
        g.volumes()
            .iter()
            .map(|v| {
                let info = infos.iter().find(|i| i.vid == v.id);
                VolumeStatus {
                    mount_path: v.mount_path.clone(),
                    fstype: v.fstype.clone(),
                    identity: info.map_or_else(|| v.key.clone(), |i| i.identity.clone()),
                    label: v.label.clone(),
                    state: if v.online {
                        info.map_or(VolumeState::Ready, |i| i.state)
                    } else {
                        VolumeState::Offline
                    },
                    method: if v.online {
                        info.map_or(UpdateMethod::Static, |i| i.method)
                    } else {
                        UpdateMethod::Static
                    },
                    entries: counts.get(v.id.0 as usize).copied().unwrap_or(0),
                    note: info.map(|i| i.note.clone()).unwrap_or_default(),
                }
            })
            .collect()
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.generation.1.notify_all();
    }

    #[must_use]
    pub fn shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}
