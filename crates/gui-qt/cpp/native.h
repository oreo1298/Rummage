// Small Qt helpers the Rust side calls through cxx: things cxx-qt-lib doesn't bind
// (QApplication, the clipboard, the MIME database, styles and color schemes).
#pragma once

#include <QtCore/QString>
#include <QtCore/QStringList>

#include "rust/cxx.h"

namespace fossick {

// Creates the QApplication (QtWidgets is needed by the KDE desktop style and the tray icon).
void create_application(const QString& app_name, const QString& desktop_file, const QString& version);
// Runs the event loop.
int exec_application();
// Chooses the Qt Quick Controls style: org.kde.desktop on Plasma when available, unless the
// user set QT_QUICK_CONTROLS_STYLE.
void choose_style();
// 0 = follow the system, 1 = light, 2 = dark.
void set_color_scheme(int scheme);
void set_clipboard_text(const QString& text);
// Icon name and localized description of a file's MIME type, guessed from its name.
QString mime_icon_name(const QString& file_name, bool is_dir);
QString mime_comment(const QString& file_name, bool is_dir);
// Loads translations for the current locale from the resources, if there are any.
void install_translations();
// Lets the next window activation use this xdg-activation token (Wayland focus stealing
// prevention lets a window take focus only with the token of the request that raised it).
void set_activation_token(const QString& token);
// Whether closing the last window quits (off while the tray icon is shown).
void set_quit_on_last_window_closed(bool quit);
// For tests: after `delay_ms`, saves an image of the first window to `path` and quits.
void schedule_screenshot(const QString& path, int delay_ms);

} // namespace fossick
