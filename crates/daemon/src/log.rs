//! Minimal logging to stderr. Under systemd (`JOURNAL_STREAM` set) each line carries a syslog
//! priority prefix, which journald turns into the message priority.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG: AtomicBool = AtomicBool::new(false);

/// Enables debug messages.
pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::Relaxed);
}

fn emit(priority: u8, label: &str, msg: &str) {
    let journal = std::env::var_os("JOURNAL_STREAM").is_some();
    let mut err = std::io::stderr().lock();
    let _ = if journal {
        writeln!(err, "<{priority}>{msg}")
    } else {
        writeln!(err, "fossickd: {label}: {msg}")
    };
}

/// True if debug messages are on (to skip formatting them otherwise).
pub fn debugging() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

pub fn debug(msg: &str) {
    if DEBUG.load(Ordering::Relaxed) {
        emit(7, "debug", msg);
    }
}

pub fn info(msg: &str) {
    emit(6, "info", msg);
}

pub fn warn(msg: &str) {
    emit(4, "warning", msg);
}

pub fn error(msg: &str) {
    emit(3, "error", msg);
}
