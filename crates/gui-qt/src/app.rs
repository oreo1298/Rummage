//! Application-wide state for QML: settings and theme.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, settings_json)]
        #[qproperty(QString, version, READ, CONSTANT)]
        /// The text of `--search TEXT` (the first window’s first tab starts with it).
        #[qproperty(QString, initial_search, READ, CONSTANT)]
        type App = super::AppRust;
    }

    impl cxx_qt::Threading for App {}
    impl cxx_qt::Initialize for App {}

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        /// Another process asked for something: `kind` is "toggle", "activate", "new-window" or
        /// "search" (with `text`); `token` is its activation token.
        #[qsignal]
        fn command_received(self: Pin<&mut App>, kind: QString, text: QString, token: QString);
    }

    #[auto_cxx_name]
    extern "RustQt" {
        /// Stores settings given as JSON (same shape as `settingsJson`). Returns false if they
        /// don’t parse or can’t be written.
        #[qinvokable]
        fn save_settings(self: Pin<&mut App>, json: &QString) -> bool;

        /// Applies a theme override: "system", "light" or "dark".
        #[qinvokable]
        fn apply_theme(self: &App, theme: &QString);

        /// English column titles by model column (QML translates them).
        #[qinvokable]
        fn column_key(self: &App, column: i32) -> QString;

        /// Uses `token` for the next window activation (Wayland).
        #[qinvokable]
        fn set_activation_token(self: &App, token: &QString);

        /// Whether closing the last window quits the application.
        #[qinvokable]
        fn set_quit_on_last_window_closed(self: &App, quit: bool);
    }
}

use std::pin::Pin;
use std::sync::Mutex;

use cxx_qt::{CxxQtThread, Threading};
use cxx_qt_lib::QString;
use fossick_ui::columns::Column;
use fossick_ui::instance::Command;
use fossick_ui::settings::{Settings, Theme};

use crate::native::ffi as native;

pub struct AppRust {
    settings_json: QString,
    version: QString,
    initial_search: QString,
}

impl Default for AppRust {
    fn default() -> Self {
        let s = Settings::load();
        Self {
            settings_json: QString::from(&*serde_json::to_string(&s).unwrap_or_default()),
            version: QString::from(env!("CARGO_PKG_VERSION")),
            initial_search: QString::from(&*crate::arg_value("--search").unwrap_or_default()),
        }
    }
}

/// The theme number the C++ side takes.
pub fn theme_code(t: Theme) -> i32 {
    match t {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    }
}

/// Commands from other processes: delivered to the App object once QML created it, queued
/// until then.
static COMMANDS: Mutex<(Option<CxxQtThread<qobject::App>>, Vec<Command>)> =
    Mutex::new((None, Vec::new()));

/// Delivers a command from another process (called on the D-Bus thread).
pub fn deliver(cmd: Command) {
    let Ok(mut state) = COMMANDS.lock() else {
        return;
    };
    match &state.0 {
        Some(thread) => {
            let _ = thread.queue(move |app| app.emit_command(cmd));
        }
        None => state.1.push(cmd),
    }
}

impl cxx_qt::Initialize for qobject::App {
    fn initialize(self: Pin<&mut Self>) {
        let thread = self.qt_thread();
        if let Ok(mut state) = COMMANDS.lock() {
            for cmd in state.1.drain(..) {
                let _ = thread.queue(move |app| app.emit_command(cmd));
            }
            state.0 = Some(thread);
        }
    }
}

impl qobject::App {
    fn emit_command(self: Pin<&mut Self>, cmd: Command) {
        let (kind, text, token) = match cmd {
            Command::Toggle { token } => ("toggle", String::new(), token),
            Command::Activate { token } => ("activate", String::new(), token),
            Command::NewWindow { token } => ("new-window", String::new(), token),
            Command::Search { text, token } => ("search", text, token),
        };
        self.command_received(
            QString::from(kind),
            QString::from(&*text),
            QString::from(&*token),
        );
    }

    pub fn set_activation_token(&self, token: &QString) {
        native::set_activation_token(token);
    }

    pub fn set_quit_on_last_window_closed(&self, quit: bool) {
        native::set_quit_on_last_window_closed(quit);
    }

    pub fn save_settings(self: Pin<&mut Self>, json: &QString) -> bool {
        let Ok(mut s) = serde_json::from_str::<Settings>(&json.to_string()) else {
            return false;
        };
        s.normalize();
        if s.save().is_err() {
            return false;
        }
        let text = serde_json::to_string(&s).unwrap_or_default();
        self.set_settings_json(QString::from(&*text));
        true
    }

    pub fn apply_theme(&self, theme: &QString) {
        let t = match theme.to_string().as_str() {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => Theme::System,
        };
        native::set_color_scheme(theme_code(t));
    }

    pub fn column_key(&self, column: i32) -> QString {
        usize::try_from(column)
            .ok()
            .and_then(Column::from_index)
            .map_or_else(QString::default, |c| QString::from(c.key()))
    }
}
