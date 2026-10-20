//! Frontend settings, shared by both GUIs: `$XDG_CONFIG_HOME/fossick/gui.toml`.

use std::path::{Path, PathBuf};

use fossick_proto::{QueryOptions, SortField, SortSpec};
use serde::{Deserialize, Serialize};

use crate::columns::Column;
use crate::filters::Filter;

/// One column’s state, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnState {
    pub column: Column,
    pub visible: bool,
    pub width: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// Which GUI `fossick` (without arguments) starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Frontend {
    /// By desktop: Qt on KDE Plasma and LXQt, GTK elsewhere.
    #[default]
    Auto,
    Qt,
    Gtk,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Columns in display order (every column appears once).
    pub columns: Vec<ColumnState>,
    pub sort: SortField,
    pub sort_descending: bool,
    pub match_case: bool,
    pub whole_word: bool,
    pub match_path: bool,
    pub regex: bool,
    /// Accents must match (`é` doesn’t find `e`).
    pub match_diacritics: bool,
    /// Name of the selected filter.
    pub filter: String,
    pub custom_filters: Vec<Filter>,
    /// Show hidden files (names starting with `.`, and their contents).
    pub show_hidden: bool,
    pub theme: Theme,
    pub frontend: Frontend,
    /// Show a tray icon.
    pub tray: bool,
    /// Closing the window hides it to the tray (when the tray icon is shown).
    pub close_to_tray: bool,
    pub window_width: u32,
    pub window_height: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            columns: Column::ALL
                .into_iter()
                .map(|c| ColumnState {
                    column: c,
                    visible: c.visible_by_default(),
                    width: c.default_width(),
                })
                .collect(),
            sort: SortField::Name,
            sort_descending: false,
            match_case: false,
            whole_word: false,
            match_path: false,
            regex: false,
            match_diacritics: false,
            filter: "Everything".into(),
            custom_filters: Vec::new(),
            show_hidden: true,
            theme: Theme::System,
            frontend: Frontend::Auto,
            tray: false,
            close_to_tray: false,
            window_width: 1100,
            window_height: 700,
        }
    }
}

impl Settings {
    /// `$XDG_CONFIG_HOME/fossick/gui.toml` (or `~/.config/…`).
    #[must_use]
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("fossick").join("gui.toml"))
    }

    /// Loads settings; missing or unreadable files give the defaults.
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        let mut s: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        s.normalize();
        s
    }

    #[must_use]
    pub fn load() -> Self {
        Self::path().map_or_else(Self::default, |p| Self::load_from(&p))
    }

    /// Writes the settings (to a temporary file, then renamed over the old one).
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    pub fn save(&self) -> std::io::Result<()> {
        match Self::path() {
            Some(p) => self.save_to(&p),
            None => Err(std::io::Error::other("no home directory")),
        }
    }

    /// Every column exactly once, Name always visible, sane widths.
    pub fn normalize(&mut self) {
        let mut seen = Vec::new();
        self.columns.retain(|c| {
            let new = !seen.contains(&c.column);
            seen.push(c.column);
            new
        });
        for c in Column::ALL {
            if !seen.contains(&c) {
                self.columns.push(ColumnState {
                    column: c,
                    visible: false,
                    width: c.default_width(),
                });
            }
        }
        for c in &mut self.columns {
            c.width = c.width.clamp(40, 4000);
            if c.column == Column::Name {
                c.visible = true;
            }
        }
        self.window_width = self.window_width.clamp(400, 10_000);
        self.window_height = self.window_height.clamp(300, 10_000);
    }

    #[must_use]
    pub fn options(&self) -> QueryOptions {
        QueryOptions {
            match_case: self.match_case,
            whole_word: self.whole_word,
            match_path: self.match_path,
            regex: self.regex,
            match_diacritics: self.match_diacritics,
        }
    }

    #[must_use]
    pub fn sort_spec(&self) -> SortSpec {
        SortSpec {
            field: self.sort,
            descending: self.sort_descending,
        }
    }

    /// All filters: the built-in ones, then the user’s.
    #[must_use]
    pub fn filters(&self) -> Vec<Filter> {
        let mut f = crate::filters::builtin();
        f.extend(self.custom_filters.iter().cloned());
        f
    }

    /// The query of the selected filter, plus `!hidden:` when hidden files are not shown.
    #[must_use]
    pub fn filter_query(&self) -> String {
        let f = self
            .filters()
            .into_iter()
            .find(|f| f.name == self.filter)
            .map(|f| f.query)
            .unwrap_or_default();
        if self.show_hidden {
            f
        } else {
            crate::filters::combine(&f, "!hidden:")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_repair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/gui.toml");
        assert_eq!(Settings::load_from(&path), Settings::default());
        let mut s = Settings {
            match_case: true,
            filter: "Audio".into(),
            show_hidden: false,
            ..Settings::default()
        };
        s.columns.swap(0, 3);
        s.save_to(&path).unwrap();
        let back = Settings::load_from(&path);
        assert_eq!(back, s);
        assert_eq!(back.filter_query(), "audio: !hidden:");
        assert!(back.options().match_case);

        // Hand-edited files are repaired: duplicates dropped, missing columns added, Name shown.
        std::fs::write(
            &path,
            "columns = [{ column = \"name\", visible = false, width = 1 },\
             { column = \"name\", visible = true, width = 300 }]\nfilter = \"Nope\"\n",
        )
        .unwrap();
        let s = Settings::load_from(&path);
        assert_eq!(s.columns.len(), Column::ALL.len());
        assert_eq!(s.columns[0].column, Column::Name);
        assert!(s.columns[0].visible);
        assert_eq!(s.columns[0].width, 40);
        assert_eq!(s.filter_query(), "");
        // Garbage gives the defaults.
        std::fs::write(&path, "columns = 7").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
    }
}
