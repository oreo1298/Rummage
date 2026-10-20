//! `fossick --gui`, `--toggle` and `--settings`: hand over to the running search window, or start the
//! GUI that suits the desktop (`fossick-qt` on KDE Plasma and LXQt, `fossick-gtk` elsewhere,
//! unless the settings choose).

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};

use fossick_ui::instance::{self, Command};
use fossick_ui::launcher;
use fossick_ui::settings::{Frontend, Settings};

use crate::{Failure, exit};

/// `name` next to this executable, else on `PATH`.
fn find_exe(name: &str) -> Option<PathBuf> {
    let here = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .filter(|p| p.is_file());
    here.or_else(|| {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join(name))
                .find(|p| p.is_file())
        })
    })
}

pub fn run(text: &str, toggle: bool, settings_window: bool) -> Result<u8, Failure> {
    let token = Command::token_from_env();
    let cmd = if toggle {
        Command::Toggle { token }
    } else if settings_window {
        Command::Settings { token }
    } else if text.is_empty() {
        Command::Activate { token }
    } else {
        Command::Search {
            text: text.to_owned(),
            token,
        }
    };
    // Without a session bus there is no running GUI to find: start one.
    if let Ok(true) = instance::send(&cmd) {
        return Ok(exit::FOUND);
    }
    let settings = Settings::load();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let qt = find_exe("fossick-qt");
    let gtk = find_exe("fossick-gtk");
    let exe = match launcher::pick(settings.frontend, &desktop, qt.is_some(), gtk.is_some()) {
        Some(Frontend::Qt) => qt,
        Some(Frontend::Gtk) => gtk,
        _ => None,
    }
    .ok_or_else(|| {
        (
            exit::NO_GUI,
            "no GUI is installed (fossick-qt or fossick-gtk)".to_owned(),
        )
    })?;
    let mut p = Process::new(&exe);
    if settings_window {
        p.arg("--settings");
    } else if !text.is_empty() {
        p.args(["--search", text]);
    }
    p.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| (exit::IO_ERROR, format!("{}: {e}", exe.display())))?;
    Ok(exit::FOUND)
}
