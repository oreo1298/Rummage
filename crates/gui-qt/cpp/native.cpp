#include "native.h"

#include <QtCore/QLibraryInfo>
#include <QtCore/QLocale>
#include <QtCore/QMimeDatabase>
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
    // Without a desktop platform theme (no KDE/GNOME session) Qt falls back to hicolor, which
    // has almost no icons: use Breeze or Adwaita when installed.
    if (QIcon::themeName().isEmpty() || QIcon::themeName() == QStringLiteral("hicolor")) {
        QIcon::setFallbackThemeName(QStringLiteral("breeze"));
        QIcon::setThemeName(QStringLiteral("breeze"));
    }
    QApplication::setWindowIcon(QIcon::fromTheme(QStringLiteral("system-search")));
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
    if (t->load(QLocale(), QStringLiteral("fossick"), QStringLiteral("_"), QStringLiteral(":/i18n"))) {
        QCoreApplication::installTranslator(t);
    } else {
        delete t;
    }
}

void schedule_screenshot(const QString& path, int delay_ms) {
    QTimer::singleShot(delay_ms, [path]() {
        int code = 1;
        for (QWindow* w : QGuiApplication::topLevelWindows()) {
            if (auto* q = qobject_cast<QQuickWindow*>(w)) {
                code = q->grabWindow().save(path) ? 0 : 1;
                break;
            }
        }
        QCoreApplication::exit(code);
    });
}

} // namespace fossick
