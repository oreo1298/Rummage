//! fossickd: the Fossick indexing daemon.
//!
//! One writer thread owns the index. It scans volumes, applies filesystem events and publishes
//! immutable generations. Client connections search the latest generation without locks, filtered
//! by each peer’s credentials. See `docs/ARCHITECTURE.md` §4.

pub mod config;
pub mod events;
pub mod fanotify;
pub mod identity;
pub mod inotify;
pub mod log;
pub mod mounts;
pub mod perms;
pub mod runs;
pub mod server;
pub mod state;
pub mod sys;
pub mod writer;
