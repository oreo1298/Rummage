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
    }
}

use std::pin::Pin;

use cxx_qt_lib::QString;
use fossick_ui::columns::Column;
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

impl qobject::App {
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
