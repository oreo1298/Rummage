//! Which GUI to start: by the desktop (`XDG_CURRENT_DESKTOP`), unless the settings say otherwise.

use crate::settings::Frontend;

/// Desktops whose native toolkit is Qt.
const QT_DESKTOPS: &[&str] = &["kde", "lxqt", "trinity", "deepin", "ukui"];

/// The GUI to start, given the setting, `XDG_CURRENT_DESKTOP` (a `:`-separated list) and which
/// GUIs are installed. `None` if neither is.
#[must_use]
pub fn pick(setting: Frontend, desktop: &str, qt: bool, gtk: bool) -> Option<Frontend> {
    let preferred = match setting {
        Frontend::Qt | Frontend::Gtk => setting,
        Frontend::Auto => {
            let qt_desktop = desktop
                .split(':')
                .any(|d| QT_DESKTOPS.contains(&d.trim().to_ascii_lowercase().as_str()));
            if qt_desktop || desktop.is_empty() {
                Frontend::Qt
            } else {
                Frontend::Gtk
            }
        }
    };
    let installed = |f| match f {
        Frontend::Qt => qt,
        Frontend::Gtk => gtk,
        Frontend::Auto => false,
    };
    if installed(preferred) {
        return Some(preferred);
    }
    let other = if preferred == Frontend::Qt {
        Frontend::Gtk
    } else {
        Frontend::Qt
    };
    installed(other).then_some(other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_by_desktop_and_availability() {
        use Frontend::{Auto, Gtk, Qt};
        assert_eq!(pick(Auto, "KDE", true, true), Some(Qt));
        assert_eq!(pick(Auto, "ubuntu:GNOME", true, true), Some(Gtk));
        assert_eq!(pick(Auto, "X-Cinnamon", true, true), Some(Gtk));
        assert_eq!(
            pick(Auto, "GNOME", true, false),
            Some(Qt),
            "only Qt installed"
        );
        assert_eq!(
            pick(Auto, "KDE", false, true),
            Some(Gtk),
            "only GTK installed"
        );
        assert_eq!(pick(Gtk, "KDE", true, true), Some(Gtk), "the setting wins");
        assert_eq!(pick(Qt, "GNOME", true, true), Some(Qt));
        assert_eq!(pick(Qt, "GNOME", false, true), Some(Gtk));
        assert_eq!(pick(Auto, "", true, true), Some(Qt));
        assert_eq!(pick(Auto, "KDE", false, false), None);
    }
}
