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
        /// The global shortcut: "off", "pending", "bound", "cancelled" or "unavailable".
        #[qproperty(QString, shortcut_state)]
        /// The keys of a bound shortcut, or why it is unavailable.
        #[qproperty(QString, shortcut_detail)]
        /// An administrative action (applying index settings, rescanning) is running.
        #[qproperty(bool, admin_busy)]
        /// The settings page `--settings PAGE` asked for ("" = none).
        #[qproperty(QString, open_settings, READ, CONSTANT)]
        /// Recent searches, newest first, as a JSON array of strings.
        #[qproperty(QString, history_json)]
        type App = super::AppRust;
    }

    impl cxx_qt::Threading for App {}
    impl cxx_qt::Initialize for App {}

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        /// Another process asked for something: `kind` is "toggle", "activate", "new-window",
        /// "settings" or "search" (with `text`); `token` is its activation token.
        #[qsignal]
        fn command_received(self: Pin<&mut App>, kind: QString, text: QString, token: QString);

        /// An administrative action finished: `action` is "apply" or "rescan".
        #[qsignal]
        fn admin_finished(self: Pin<&mut App>, action: QString, ok: bool, message: QString);
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

        /// The index settings of the system daemon (`system`) or the user’s daemon, as JSON:
        /// `{"settings": {...}, "path": "..."}` or `{"error": "..."}`.
        #[qinvokable]
        fn daemon_config_json(self: &App, system: bool) -> QString;

        /// Saves index settings (JSON as in `daemonConfigJson`) and makes the daemon reload
        /// them; for the system daemon this asks for an administrator password. Reports
        /// through `adminFinished`.
        #[qinvokable]
        fn apply_daemon_config(self: Pin<&mut App>, json: &QString, system: bool);

        /// Asks the daemon to rescan every volume (administrator password for the system
        /// daemon). Reports through `adminFinished`.
        #[qinvokable]
        fn rescan_all(self: Pin<&mut App>, system: bool);

        /// Registers (or removes) the global shortcut that shows or hides the window.
        #[qinvokable]
        fn set_global_shortcut(self: Pin<&mut App>, enabled: bool, keys: &QString);

        /// Opens the desktop’s settings for the global shortcut; false if it can’t.
        #[qinvokable]
        fn configure_global_shortcut(self: &App) -> bool;

        /// Puts `text` on the clipboard.
        #[qinvokable]
        fn copy_text(self: &App, text: &QString);

        /// Adds a search to the recent searches (if the settings keep them).
        #[qinvokable]
        fn remember_search(self: Pin<&mut App>, text: &QString);

        /// Forgets every recent search.
        #[qinvokable]
        fn clear_history(self: Pin<&mut App>);
    }
}

use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::QString;
use fossick_ui::columns::Column;
use fossick_ui::daemon_config::{self, IndexSettings};
use fossick_ui::history::History;
use fossick_ui::instance::{BUS_NAME, Command};
use fossick_ui::settings::{Settings, Theme};
use fossick_ui::shortcuts::{GlobalShortcut, Status};

use crate::native::ffi as native;

pub struct AppRust {
    settings_json: QString,
    version: QString,
    initial_search: QString,
    shortcut_state: QString,
    shortcut_detail: QString,
    admin_busy: bool,
    open_settings: QString,
    history_json: QString,
    history: History,
}

impl Default for AppRust {
    fn default() -> Self {
        let s = Settings::load();
        Self {
            settings_json: QString::from(&*serde_json::to_string(&s).unwrap_or_default()),
            version: QString::from(env!("CARGO_PKG_VERSION")),
            initial_search: QString::from(&*crate::arg_value("--search").unwrap_or_default()),
            shortcut_state: QString::from("off"),
            shortcut_detail: QString::default(),
            admin_busy: false,
            open_settings: QString::from(&*settings_page()),
            history_json: QString::from("[]"),
            history: History::default(),
        }
    }
}

/// The page of `--settings [PAGE]`: "general" without one, "" without the flag.
fn settings_page() -> String {
    if !std::env::args().any(|a| a == "--settings") {
        return String::new();
    }
    crate::arg_value("--settings")
        .filter(|p| !p.starts_with('-'))
        .unwrap_or_else(|| "general".into())
}

/// The registered global shortcut, if any.
static SHORTCUT: Mutex<Option<GlobalShortcut>> = Mutex::new(None);
/// Counts registrations, so a late status from a replaced one is ignored.
static SHORTCUT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// The `fossick` command-line tool: next to this program, else from `PATH`. pkexec gets an
/// absolute path, so the polkit policy for the installed tool applies.
fn fossick_cli() -> std::path::PathBuf {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("fossick")));
    if let Some(p) = sibling.filter(|p| p.is_file()) {
        return p;
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("fossick"))
        .find(|p| p.is_file())
        .unwrap_or_else(|| "fossick".into())
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
    fn initialize(mut self: Pin<&mut Self>) {
        let thread = self.qt_thread();
        if let Ok(mut state) = COMMANDS.lock() {
            for cmd in state.1.drain(..) {
                let _ = thread.queue(move |app| app.emit_command(cmd));
            }
            state.0 = Some(thread);
        }
        let s = Settings::load();
        if s.remember_history {
            let history = History::load();
            let json = serde_json::to_string(&history.entries).unwrap_or_default();
            self.as_mut().rust_mut().history = history;
            self.as_mut().set_history_json(QString::from(&*json));
        }
        if s.global_shortcut {
            self.as_mut()
                .set_global_shortcut(true, &QString::from(&*s.shortcut));
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
            Command::Settings { token } => ("settings", String::new(), token),
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

    pub fn daemon_config_json(&self, system: bool) -> QString {
        let value = match IndexSettings::load(system) {
            Ok(s) => serde_json::json!({
                "settings": s,
                "path": daemon_config::path(system).map(|p| p.display().to_string()),
            }),
            Err(e) => serde_json::json!({ "error": e }),
        };
        QString::from(&*value.to_string())
    }

    /// Runs `work` on a thread of its own and reports its outcome through `adminFinished`.
    fn admin(
        mut self: Pin<&mut Self>,
        action: &'static str,
        work: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        if self.admin_busy {
            return;
        }
        self.as_mut().set_admin_busy(true);
        let thread = self.qt_thread();
        std::thread::spawn(move || {
            let result = work();
            let _ = thread.queue(move |mut app| {
                app.as_mut().set_admin_busy(false);
                let (ok, message) = match result {
                    Ok(()) => (true, String::new()),
                    Err(e) => (false, e),
                };
                app.admin_finished(QString::from(action), ok, QString::from(&*message));
            });
        });
    }

    pub fn apply_daemon_config(self: Pin<&mut Self>, json: &QString, system: bool) {
        match serde_json::from_str::<IndexSettings>(&json.to_string()) {
            Ok(settings) => self.admin("apply", move || settings.save(system, &fossick_cli())),
            Err(e) => self.admin_finished(
                QString::from("apply"),
                false,
                QString::from(&*e.to_string()),
            ),
        }
    }

    pub fn rescan_all(self: Pin<&mut Self>, system: bool) {
        self.admin("rescan", move || {
            daemon_config::rescan_all(system, &fossick_cli())
        });
    }

    pub fn set_global_shortcut(mut self: Pin<&mut Self>, enabled: bool, keys: &QString) {
        let generation = SHORTCUT_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        // Drop the old registration first (this waits for its listener to stop).
        let old = SHORTCUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(old);
        if !enabled {
            self.as_mut().set_shortcut_state(QString::from("off"));
            self.set_shortcut_detail(QString::default());
            return;
        }
        self.as_mut().set_shortcut_state(QString::from("pending"));
        self.as_mut().set_shortcut_detail(QString::default());
        let thread = self.qt_thread();
        let report = move |status: Status| {
            let (state, detail) = match status {
                Status::Bound { trigger } => ("bound", trigger),
                Status::Cancelled => ("cancelled", String::new()),
                Status::Unavailable(why) => ("unavailable", why),
            };
            let _ = thread.queue(move |mut app| {
                if SHORTCUT_GENERATION.load(Ordering::Relaxed) != generation {
                    return;
                }
                app.as_mut().set_shortcut_state(QString::from(state));
                app.set_shortcut_detail(QString::from(&*detail));
            });
        };
        let started = GlobalShortcut::start(
            BUS_NAME,
            &keys.to_string(),
            "Show or hide the Fossick search window",
            report,
            |token| deliver(Command::Toggle { token }),
        );
        match started {
            Ok(s) => {
                *SHORTCUT
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(s);
            }
            Err(e) => {
                self.as_mut()
                    .set_shortcut_state(QString::from("unavailable"));
                self.set_shortcut_detail(QString::from(&*e.to_string()));
            }
        }
    }

    pub fn configure_global_shortcut(&self) -> bool {
        SHORTCUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|s| s.configure().is_ok())
    }

    pub fn copy_text(&self, text: &QString) {
        native::set_clipboard_text(text);
    }

    pub fn remember_search(mut self: Pin<&mut Self>, text: &QString) {
        if !Settings::load().remember_history {
            return;
        }
        let text = text.to_string();
        if self.history.entries.first() == Some(&text) {
            return;
        }
        if !self.as_mut().rust_mut().history.add(&text) {
            return;
        }
        if let Err(e) = self.history.save() {
            eprintln!("fossick-qt: cannot save the search history: {e}");
        }
        let json = serde_json::to_string(&self.history.entries).unwrap_or_default();
        self.set_history_json(QString::from(&*json));
    }

    pub fn clear_history(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().history = History::default();
        if let Some(p) = History::path()
            && let Err(e) = History::clear_at(&p)
        {
            eprintln!("fossick-qt: cannot remove {}: {e}", p.display());
        }
        self.set_history_json(QString::from("[]"));
    }

    pub fn column_key(&self, column: i32) -> QString {
        usize::try_from(column)
            .ok()
            .and_then(Column::from_index)
            .map_or_else(QString::default, |c| QString::from(c.key()))
    }
}
