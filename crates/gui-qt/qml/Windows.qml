// The open windows, commands from other processes (fossick --toggle, a second launch) and
// the optional tray icon.
pragma Singleton
import QtQuick
import Qt.labs.platform as Platform
import io.github.oreo1298.Fossick

QtObject {
    id: windows

    property var list: []
    // The window commands go to: the one used last.
    property var last: null
    property bool initialTaken: false
    readonly property bool trayShown: tray.visible
    property var settingsWindow: null

    function add(w) {
        list = list.concat([w])
        last = w
        if (App.openSettings !== "" && list.length === 1) {
            const pages = ["general", "search", "index", "shortcut"]
            showSettings(Math.max(0, pages.indexOf(App.openSettings)))
        }
    }
    function remove(w) {
        list = list.filter(x => x !== w)
        if (last === w) last = list.length > 0 ? list[list.length - 1] : null
    }
    function focused(w) {
        last = w
    }
    // `--search TEXT` applies to the first window only.
    function takeInitialSearch() {
        if (initialTaken) return ""
        initialTaken = true
        return App.initialSearch
    }
    function create(text) {
        const c = Qt.createComponent(Qt.resolvedUrl("Main.qml"))
        const w = c.createObject(null, { initialText: text || "" })
        if (w) {
            w.show()
            w.raise()
            w.requestActivate()
        }
        return w
    }
    function raise(w) {
        w.show()
        w.raise()
        w.requestActivate()
        if (w.current) w.current.focusSearch()
    }
    // The settings window, one for all windows.
    function showSettings(page) {
        if (!settingsWindow) {
            const c = Qt.createComponent(Qt.resolvedUrl("SettingsWindow.qml"))
            settingsWindow = c.createObject(null)
            if (!settingsWindow) {
                console.warn(c.errorString())
                return
            }
        }
        if (page !== undefined) settingsWindow.page = page
        settingsWindow.show()
        settingsWindow.raise()
        settingsWindow.requestActivate()
    }
    function toggle() {
        const w = last
        if (!w) {
            create("")
        } else if (w.visible && w.active) {
            w.hide()
        } else {
            raise(w)
        }
    }

    property Connections commands: Connections {
        target: App
        function onCommandReceived(kind, text, token) {
            App.setActivationToken(token)
            if (kind === "settings") {
                if (!windows.last) windows.create("")
                windows.showSettings()
            } else if (kind === "new-window" || !windows.last) {
                windows.create(kind === "search" ? text : "")
            } else if (kind === "toggle") {
                windows.toggle()
            } else {
                if (kind === "search") windows.last.newTab(text)
                windows.raise(windows.last)
            }
        }
    }

    property Platform.SystemTrayIcon tray: Platform.SystemTrayIcon {
        visible: JSON.parse(App.settingsJson).tray
        icon.name: "io.github.oreo1298.Fossick"
        icon.source: "qrc:/fossick/fossick.svg"
        tooltip: "Fossick"
        onVisibleChanged: App.setQuitOnLastWindowClosed(!visible)
        Component.onCompleted: App.setQuitOnLastWindowClosed(!visible)
        onActivated: reason => {
            if (reason === Platform.SystemTrayIcon.Trigger) windows.toggle()
        }
        menu: Platform.Menu {
            Platform.MenuItem { text: qsTr("Show or Hide"); onTriggered: windows.toggle() }
            Platform.MenuItem { text: qsTr("New Window"); onTriggered: windows.create("") }
            Platform.MenuSeparator {}
            Platform.MenuItem { text: qsTr("Quit"); onTriggered: Qt.quit() }
        }
    }
}
