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

    function add(w) {
        list = list.concat([w])
        last = w
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
            if (kind === "new-window" || !windows.last) {
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
        icon.name: "system-search"
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
