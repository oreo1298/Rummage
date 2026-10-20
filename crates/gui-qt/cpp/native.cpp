#include "native.h"

#include <QtCore/QLibraryInfo>
#include <QtCore/QLocale>
#include <QtCore/QMimeDatabase>
#include <QtCore/QStandardPaths>
#include <QtCore/QTimer>
#include <QtCore/QTranslator>
#include <QtQuick/QQuickWindow>
#include <QtGui/QClipboard>
#include <QtGui/QGuiApplication>
#include <QtGui/QIcon>
#include <QtGui/QStyleHints>
#include <QtQuickControls2/QQuickStyle>
#include <QtWidgets/QApplication>

namespace fossick {

namespace {
int g_argc = 1;
char g_arg0[] = "fossick-qt";
char* g_argv[] = {g_arg0, nullptr};

QMimeType mime_for(const QString& file_name, bool is_dir) {
    static const QMimeDatabase db;
    if (is_dir) {
        return db.mimeTypeForName(QStringLiteral("inode/directory"));
    }
    return db.mimeTypeForFile(file_name, QMimeDatabase::MatchExtension);
}
} // namespace

void create_application(const QString& app_name, const QString& desktop_file, const QString& version) {
    static QApplication* app = nullptr;
    if (app != nullptr) {
        return;
    }
    QApplication::setApplicationName(app_name);
    QApplication::setApplicationDisplayName(QStringLiteral("Fossick"));
    QApplication::setApplicationVersion(version);
    QApplication::setOrganizationDomain(QStringLiteral("io.github.oreo1298"));
    QApplication::setDesktopFileName(desktop_file);
    app = new QApplication(g_argc, g_argv);
    // Some platforms (offscreen, minimal) only look for icon themes in the resources: add the
    // XDG icon directories.
    QStringList paths = QIcon::themeSearchPaths();
    for (const QString& dir : QStandardPaths::locateAll(QStandardPaths::GenericDataLocation,
                                                        QStringLiteral("icons"),
                                                        QStandardPaths::LocateDirectory)) {
        if (!paths.contains(dir)) paths << dir;
    }
    QIcon::setThemeSearchPaths(paths);
    // Icons the desktop's theme lacks (Adwaita has few non-symbolic ones) come from Breeze, a
    // dependency of the packages. Without a desktop theme at all (hicolor), use Breeze outright.
    QIcon::setFallbackThemeName(QStringLiteral("breeze"));
    if (QIcon::themeName().isEmpty() || QIcon::themeName() == QStringLiteral("hicolor")) {
        QIcon::setThemeName(QStringLiteral("breeze"));
    }
    // The installed icon, else the copy built into the program.
    QApplication::setWindowIcon(QIcon::fromTheme(QStringLiteral("io.github.oreo1298.Fossick"),
                                                 QIcon(QStringLiteral(":/fossick/fossick.svg"))));
}

int exec_application() {
    return QApplication::exec();
}

void choose_style() {
    if (!qEnvironmentVariableIsEmpty("QT_QUICK_CONTROLS_STYLE")) {
        return;
    }
    const QByteArray desktop = qgetenv("XDG_CURRENT_DESKTOP").toLower();
    if (desktop.contains("kde") || desktop.contains("lxqt")) {
        QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));
        QQuickStyle::setFallbackStyle(QStringLiteral("Fusion"));
    } else if (QQuickStyle::name().isEmpty()) {
        QQuickStyle::setStyle(QStringLiteral("Fusion"));
    }
}

void set_color_scheme(int scheme) {
#if QT_VERSION >= QT_VERSION_CHECK(6, 8, 0)
    Qt::ColorScheme s = Qt::ColorScheme::Unknown;
    if (scheme == 1) {
        s = Qt::ColorScheme::Light;
    } else if (scheme == 2) {
        s = Qt::ColorScheme::Dark;
    }
    QGuiApplication::styleHints()->setColorScheme(s);
#else
    Q_UNUSED(scheme);
#endif
}

void set_clipboard_text(const QString& text) {
    QGuiApplication::clipboard()->setText(text);
}

QString mime_icon_name(const QString& file_name, bool is_dir) {
    const QMimeType m = mime_for(file_name, is_dir);
    const QString icon = m.iconName();
    if (!icon.isEmpty() && QIcon::hasThemeIcon(icon)) {
        return icon;
    }
    const QString generic = m.genericIconName();
    return generic.isEmpty() ? QStringLiteral("text-x-generic") : generic;
}

QString mime_comment(const QString& file_name, bool is_dir) {
    return mime_for(file_name, is_dir).comment();
}

void install_translations() {
    auto* t = new QTranslator(QCoreApplication::instance());
    // The user's language, else English, whose file holds the plural forms of the source
    // texts ("%n object(s)").
    if (t->load(QLocale(), QStringLiteral("fossick"), QStringLiteral("_"), QStringLiteral(":/i18n"))
        || t->load(QStringLiteral("fossick_en"), QStringLiteral(":/i18n"))) {
        QCoreApplication::installTranslator(t);
    } else {
        delete t;
    }
}

void set_activation_token(const QString& token) {
    if (!token.isEmpty()) {
        qputenv("XDG_ACTIVATION_TOKEN", token.toUtf8());
    }
}

void set_quit_on_last_window_closed(bool quit) {
    QGuiApplication::setQuitOnLastWindowClosed(quit);
}

void schedule_screenshot(const QString& path, int delay_ms) {
    QTimer::singleShot(delay_ms, [path]() {
        // The window opened last (the settings window, when one was asked for).
        QQuickWindow* shot = nullptr;
        for (QWindow* w : QGuiApplication::topLevelWindows()) {
            if (auto* q = qobject_cast<QQuickWindow*>(w); q && q->isVisible()) shot = q;
        }
        QCoreApplication::exit(shot && shot->grabWindow().save(path) ? 0 : 1);
    });
}

} // namespace fossick
