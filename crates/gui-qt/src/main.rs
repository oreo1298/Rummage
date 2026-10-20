//! `fossick-qt`: the Qt 6 / Kirigami frontend.
// cxx-qt and cxx generate the FFI glue; the unsafe blocks written by hand (paired model
// reset/insert/remove calls) carry SAFETY comments.
#![allow(unsafe_code)]

mod app;
mod native;
mod search;
mod status;

use cxx_qt_lib::{QQmlApplicationEngine, QString, QUrl};
use fossick_ui::instance::{self, Claim, Command};
use fossick_ui::settings::Settings;

/// The value after `flag` on the command line.
pub fn arg_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("fossick-qt {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: fossick-qt [--search TEXT] [--new-window] [--toggle] [--settings [PAGE]]\n\n\
             Opens the Fossick search window, or passes the request to the one already running.\n\
             --settings also opens the settings at PAGE (general, search, index or shortcut)."
        );
        return;
    }
    let token = Command::token_from_env();
    let first = if let Some(text) = arg_value("--search") {
        Command::Search { text, token }
    } else if args.iter().any(|a| a == "--new-window") {
        Command::NewWindow { token }
    } else if args.iter().any(|a| a == "--toggle") {
        Command::Toggle { token }
    } else if args.iter().any(|a| a == "--settings") {
        Command::Settings { token }
    } else {
        Command::Activate { token }
    };
    // One GUI per session: forward the request if one runs. Tests (--screenshot) and sessions
    // without D-Bus run on their own (FOSSICK_SINGLE_INSTANCE=1 makes tests claim the name).
    let standalone = args.iter().any(|a| a == "--screenshot")
        && std::env::var_os("FOSSICK_SINGLE_INSTANCE").is_none();
    let _instance = if standalone {
        None
    } else {
        match instance::claim(&first, app::deliver) {
            Ok(Claim::Primary(i)) => Some(i),
            Ok(Claim::Forwarded) => return,
            Err(e) => {
                eprintln!("fossick-qt: running without single-instance support: {e}");
                None
            }
        }
    };
    native::ffi::create_application(
        &QString::from("fossick"),
        &QString::from("io.github.oreo1298.Fossick"),
        &QString::from(env!("CARGO_PKG_VERSION")),
    );
    native::ffi::choose_style();
    native::ffi::install_translations();
    native::ffi::set_color_scheme(app::theme_code(Settings::load().theme));
    // For tests: `--screenshot FILE [DELAY_MS]` saves the window to FILE and exits.
    if let Some(i) = args.iter().position(|a| a == "--screenshot") {
        let path = args
            .get(i + 1)
            .cloned()
            .unwrap_or_else(|| "fossick.png".into());
        let delay = args.get(i + 2).and_then(|d| d.parse().ok()).unwrap_or(1500);
        native::ffi::schedule_screenshot(&QString::from(&*path), delay);
    }
    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from(
            "qrc:/qt/qml/io/github/oreo1298/Fossick/qml/Main.qml",
        ));
    }
    let code = native::ffi::exec_application();
    drop(engine);
    std::process::exit(code);
}
