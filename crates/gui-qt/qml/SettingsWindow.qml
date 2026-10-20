// Settings: the window’s own options (applied right away) and the index service’s settings
// (applied with the Apply button; for the system service after an administrator password).
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import io.github.oreo1298.Fossick

QQC2.ApplicationWindow {
    id: win

    // Whether the search windows use the system index service (else the user’s own).
    readonly property bool system: {
        const w = Windows.last
        return !(w && w.current && w.current.model.userMode)
    }
    property alias page: tabs.currentIndex
    readonly property var settings: JSON.parse(App.settingsJson)

    // The index settings being edited, as loaded from the service’s configuration file.
    property var index: null
    property string indexPath: ""
    property string indexError: ""
    property bool indexDirty: false
    property string adminMessage: ""
    property bool adminOk: true

    function save(key, value) {
        const s = JSON.parse(App.settingsJson)
        s[key] = value
        App.saveSettings(JSON.stringify(s))
    }
    function loadIndex() {
        const r = JSON.parse(App.daemonConfigJson(system))
        index = r.settings || null
        indexPath = r.path || ""
        indexError = r.error || ""
        indexDirty = false
    }
    function editIndex(key, value) {
        const copy = Object.assign({}, index)
        copy[key] = value
        index = copy
        indexDirty = true
        adminMessage = ""
    }
    function setFilters(list) {
        save("custom_filters", list)
    }

    title: qsTr("Fossick Settings")
    width: Kirigami.Units.gridUnit * 38
    height: Kirigami.Units.gridUnit * 32
    minimumWidth: Kirigami.Units.gridUnit * 24
    minimumHeight: Kirigami.Units.gridUnit * 18
    color: Kirigami.Theme.backgroundColor

    onSystemChanged: loadIndex()
    Component.onCompleted: loadIndex()

    Connections {
        target: App
        function onAdminFinished(action, ok, message) {
            win.adminOk = ok
            if (ok) {
                win.adminMessage = action === "apply"
                    ? qsTr("The index service uses the new settings.")
                    : qsTr("Rescanning all volumes.")
                if (action === "apply") win.loadIndex()
            } else {
                win.adminMessage = message
            }
        }
    }

    header: QQC2.TabBar {
        id: tabs
        QQC2.TabButton { text: qsTr("General") }
        QQC2.TabButton { text: qsTr("Search") }
        QQC2.TabButton { text: qsTr("Index") }
        QQC2.TabButton { text: qsTr("Shortcut") }
    }

    StackLayout {
        anchors.fill: parent
        currentIndex: tabs.currentIndex

        // General
        QQC2.ScrollView {
            contentWidth: availableWidth
            Kirigami.FormLayout {
                width: parent.width
                QQC2.ComboBox {
                    implicitContentWidthPolicy: QQC2.ComboBox.WidestText
                    Kirigami.FormData.label: qsTr("Colors:")
                    readonly property var values: ["system", "light", "dark"]
                    model: [qsTr("Follow the system"), qsTr("Light"), qsTr("Dark")]
                    currentIndex: Math.max(0, values.indexOf(win.settings.theme))
                    onActivated: i => {
                        win.save("theme", values[i])
                        App.applyTheme(values[i])
                    }
                }
                QQC2.ComboBox {
                    implicitContentWidthPolicy: QQC2.ComboBox.WidestText
                    Kirigami.FormData.label: qsTr("Interface for “fossick --gui”:")
                    readonly property var values: ["auto", "qt", "gtk"]
                    model: [qsTr("Match the desktop"), qsTr("Qt (KDE)"), qsTr("GTK (GNOME)")]
                    currentIndex: Math.max(0, values.indexOf(win.settings.frontend))
                    onActivated: i => win.save("frontend", values[i])
                }
                Item { Kirigami.FormData.isSection: true }
                QQC2.CheckBox {
                    Kirigami.FormData.label: qsTr("Tray:")
                    text: qsTr("Show an icon in the system tray")
                    checked: win.settings.tray
                    onToggled: win.save("tray", checked)
                }
                QQC2.CheckBox {
                    text: qsTr("Closing the window keeps Fossick in the tray")
                    enabled: win.settings.tray
                    checked: win.settings.close_to_tray
                    onToggled: win.save("close_to_tray", checked)
                }
                Item { Kirigami.FormData.isSection: true }
                QQC2.CheckBox {
                    Kirigami.FormData.label: qsTr("Results:")
                    text: qsTr("Show hidden files and folders")
                    checked: win.settings.show_hidden
                    onToggled: win.save("show_hidden", checked)
                }
                Item { Kirigami.FormData.isSection: true }
                // Every column but Name, which is always shown.
                GridLayout {
                    Kirigami.FormData.label: qsTr("Columns:")
                    Kirigami.FormData.labelAlignment: Qt.AlignTop
                    columns: 3
                    columnSpacing: Kirigami.Units.largeSpacing
                    rowSpacing: 0
                    Repeater {
                        model: Columns.keys.length - 1
                        delegate: QQC2.CheckBox {
                            required property int index
                            readonly property int logical: index + 1
                            readonly property var column: win.settings.columns.find(c => c.column === Columns.keys[logical])
                            text: Columns.title(logical)
                            checked: column ? column.visible : false
                            onToggled: {
                                const s = JSON.parse(App.settingsJson)
                                const c = s.columns.find(c => c.column === Columns.keys[logical])
                                if (c) c.visible = checked
                                App.saveSettings(JSON.stringify(s))
                            }
                        }
                    }
                }
                Item { Kirigami.FormData.isSection: true }
                RowLayout {
                    Kirigami.FormData.label: qsTr("About:")
                    spacing: Kirigami.Units.largeSpacing
                    Image {
                        source: "qrc:/fossick/fossick.svg"
                        sourceSize.width: Kirigami.Units.iconSizes.large
                        sourceSize.height: Kirigami.Units.iconSizes.large
                        Accessible.ignored: true
                    }
                    ColumnLayout {
                        spacing: 0
                        QQC2.Label {
                            text: qsTr("Fossick %1").arg(App.version)
                            font.bold: true
                        }
                        QQC2.Label {
                            text: qsTr("Free software under the GNU GPL, version 3 or later.")
                        }
                        Kirigami.UrlButton {
                            url: "https://github.com/oreo1298/Rummage"
                        }
                    }
                }
            }
        }

        // Search
        QQC2.ScrollView {
            contentWidth: availableWidth
            Kirigami.FormLayout {
                width: parent.width
                QQC2.CheckBox {
                    Kirigami.FormData.label: qsTr("Matching:")
                    text: qsTr("Match case")
                    checked: win.settings.match_case
                    onToggled: win.save("match_case", checked)
                }
                QQC2.CheckBox {
                    text: qsTr("Match whole words")
                    checked: win.settings.whole_word
                    onToggled: win.save("whole_word", checked)
                }
                QQC2.CheckBox {
                    text: qsTr("Match the whole path")
                    checked: win.settings.match_path
                    onToggled: win.save("match_path", checked)
                }
                QQC2.CheckBox {
                    text: qsTr("Match diacritics (é doesn’t find e)")
                    checked: win.settings.match_diacritics
                    onToggled: win.save("match_diacritics", checked)
                }
                QQC2.CheckBox {
                    text: qsTr("Regular expressions")
                    checked: win.settings.regex
                    onToggled: win.save("regex", checked)
                }
                Item { Kirigami.FormData.isSection: true }
                ColumnLayout {
                    Kirigami.FormData.label: qsTr("Filters:")
                    Kirigami.FormData.labelAlignment: Qt.AlignTop
                    Layout.fillWidth: true
                    QQC2.Label {
                        Layout.fillWidth: true
                        wrapMode: Text.Wrap
                        text: qsTr("Your own entries for the filter list, each a name and a search such as “ext:rs;toml” or “size:>1gb”.")
                    }
                    Repeater {
                        model: win.settings.custom_filters
                        delegate: RowLayout {
                            required property int index
                            required property var modelData
                            Layout.fillWidth: true
                            function update(key, value) {
                                const list = win.settings.custom_filters.map(f => Object.assign({}, f))
                                list[index][key] = value
                                win.setFilters(list)
                            }
                            QQC2.TextField {
                                Layout.preferredWidth: Kirigami.Units.gridUnit * 8
                                text: modelData.name
                                placeholderText: qsTr("Name")
                                Accessible.name: qsTr("Filter %1 name").arg(index + 1)
                                onEditingFinished: if (text !== modelData.name) update("name", text)
                            }
                            QQC2.TextField {
                                Layout.fillWidth: true
                                text: modelData.query
                                placeholderText: qsTr("Search")
                                Accessible.name: qsTr("Filter %1 search").arg(index + 1)
                                onEditingFinished: if (text !== modelData.query) update("query", text)
                            }
                            QQC2.ToolButton {
                                icon.name: "list-remove"
                                text: qsTr("Remove")
                                display: QQC2.AbstractButton.IconOnly
                                onClicked: win.setFilters(win.settings.custom_filters.filter((f, i) => i !== index))
                            }
                        }
                    }
                    QQC2.Button {
                        icon.name: "list-add"
                        text: qsTr("Add Filter")
                        onClicked: {
                            let n = 1
                            const names = win.settings.custom_filters.map(f => f.name)
                            while (names.indexOf(qsTr("Filter %1").arg(n)) >= 0) ++n
                            win.setFilters(win.settings.custom_filters.concat([{ name: qsTr("Filter %1").arg(n), query: "" }]))
                        }
                    }
                }
                Item { Kirigami.FormData.isSection: true }
                ColumnLayout {
                    Kirigami.FormData.label: qsTr("Bookmarks:")
                    Kirigami.FormData.labelAlignment: Qt.AlignTop
                    Layout.fillWidth: true
                    QQC2.Label {
                        visible: win.settings.bookmarks.length === 0
                        Layout.fillWidth: true
                        wrapMode: Text.Wrap
                        color: Kirigami.Theme.disabledTextColor
                        text: qsTr("None yet. Ctrl+D in a search window saves the current search.")
                    }
                    Repeater {
                        model: win.settings.bookmarks
                        delegate: RowLayout {
                            required property int index
                            required property var modelData
                            Layout.fillWidth: true
                            QQC2.TextField {
                                Layout.preferredWidth: Kirigami.Units.gridUnit * 8
                                text: modelData.name
                                placeholderText: qsTr("Name")
                                Accessible.name: qsTr("Bookmark %1 name").arg(index + 1)
                                onEditingFinished: if (text !== modelData.name) {
                                    const list = win.settings.bookmarks.map(b => Object.assign({}, b))
                                    list[index].name = text
                                    win.save("bookmarks", list)
                                }
                            }
                            QQC2.Label {
                                Layout.fillWidth: true
                                text: modelData.search
                                textFormat: Text.PlainText
                                elide: Text.ElideRight
                                color: Kirigami.Theme.disabledTextColor
                            }
                            QQC2.ToolButton {
                                icon.name: "list-remove"
                                text: qsTr("Remove")
                                display: QQC2.AbstractButton.IconOnly
                                onClicked: win.save("bookmarks", win.settings.bookmarks.filter((b, i) => i !== index))
                            }
                        }
                    }
                }
                Item { Kirigami.FormData.isSection: true }
                QQC2.CheckBox {
                    Kirigami.FormData.label: qsTr("History:")
                    text: qsTr("Remember recent searches (Alt+Down in the search field)")
                    checked: win.settings.remember_history
                    onToggled: win.save("remember_history", checked)
                }
                QQC2.Button {
                    icon.name: "edit-clear-history"
                    text: qsTr("Clear Recent Searches")
                    enabled: JSON.parse(App.historyJson).length > 0
                    onClicked: App.clearHistory()
                }
            }
        }

        // Index
        QQC2.ScrollView {
            contentWidth: availableWidth
            ColumnLayout {
                width: parent.width
                spacing: 0

                Kirigami.InlineMessage {
                    Layout.fillWidth: true
                    visible: true
                    type: Kirigami.MessageType.Information
                    position: Kirigami.InlineMessage.Position.Header
                    text: win.system
                        ? qsTr("These settings belong to the system index service and apply to every user. Applying them asks for an administrator password.")
                        : qsTr("These settings belong to your own index service, which indexes what you can read.")
                }
                Kirigami.InlineMessage {
                    Layout.fillWidth: true
                    visible: win.indexError.length > 0
                    type: Kirigami.MessageType.Error
                    text: win.indexError
                }
                Kirigami.InlineMessage {
                    Layout.fillWidth: true
                    visible: win.adminMessage.length > 0
                    type: win.adminOk ? Kirigami.MessageType.Positive : Kirigami.MessageType.Error
                    text: win.adminMessage
                }

                Kirigami.FormLayout {
                    Layout.fillWidth: true
                    enabled: win.index !== null && !App.adminBusy

                    QQC2.Label {
                        Kirigami.FormData.label: qsTr("File:")
                        text: win.indexPath
                        textFormat: Text.PlainText
                        elide: Text.ElideMiddle
                        Layout.maximumWidth: Kirigami.Units.gridUnit * 22
                    }
                    StringListEditor {
                        Kirigami.FormData.label: qsTr("Only these volumes:")
                        Kirigami.FormData.labelAlignment: Qt.AlignTop
                        Layout.fillWidth: true
                        accessibleName: qsTr("Only these volumes")
                        placeholder: qsTr("Mount point, such as /home")
                        folders: true
                        values: win.index ? win.index.volumes : []
                        onEdited: v => win.editIndex("volumes", v)
                    }
                    QQC2.Label {
                        Layout.fillWidth: true
                        wrapMode: Text.Wrap
                        font: Kirigami.Theme.smallFont
                        color: Kirigami.Theme.disabledTextColor
                        text: qsTr("Leave empty to index every local file system.")
                    }
                    StringListEditor {
                        Kirigami.FormData.label: qsTr("Excluded folders:")
                        Kirigami.FormData.labelAlignment: Qt.AlignTop
                        Layout.fillWidth: true
                        accessibleName: qsTr("Excluded folders")
                        placeholder: qsTr("Absolute path")
                        folders: true
                        values: win.index ? win.index.exclude : []
                        onEdited: v => win.editIndex("exclude", v)
                    }
                    StringListEditor {
                        Kirigami.FormData.label: qsTr("Excluded names:")
                        Kirigami.FormData.labelAlignment: Qt.AlignTop
                        Layout.fillWidth: true
                        accessibleName: qsTr("Excluded names")
                        placeholder: qsTr("Pattern, such as node_modules or *.o")
                        values: win.index ? win.index.exclude_names : []
                        onEdited: v => win.editIndex("exclude_names", v)
                    }
                    StringListEditor {
                        Kirigami.FormData.label: qsTr("Network folders:")
                        Kirigami.FormData.labelAlignment: Qt.AlignTop
                        Layout.fillWidth: true
                        accessibleName: qsTr("Network folders")
                        placeholder: qsTr("Mount point of an NFS, SMB or SSHFS share")
                        folders: true
                        values: win.index ? win.index.network : []
                        onEdited: v => win.editIndex("network", v)
                    }
                    QQC2.Label {
                        Layout.fillWidth: true
                        wrapMode: Text.Wrap
                        font: Kirigami.Theme.smallFont
                        color: Kirigami.Theme.disabledTextColor
                        text: qsTr("Network shares aren’t indexed unless listed; they are checked for changes every few minutes.")
                    }
                    Item { Kirigami.FormData.isSection: true }
                    QQC2.CheckBox {
                        Kirigami.FormData.label: qsTr("Also index:")
                        text: qsTr("Memory file systems (tmpfs)")
                        checked: win.index ? win.index.index_tmpfs : false
                        onToggled: win.editIndex("index_tmpfs", checked)
                    }
                    QQC2.CheckBox {
                        text: qsTr("Snapshots (.snapshots, Timeshift)")
                        checked: win.index ? win.index.include_snapshots : false
                        onToggled: win.editIndex("include_snapshots", checked)
                    }
                    QQC2.CheckBox {
                        text: qsTr("Access times (updated by rescans only)")
                        checked: win.index ? win.index.track_atime : false
                        onToggled: win.editIndex("track_atime", checked)
                    }
                    QQC2.CheckBox {
                        Kirigami.FormData.label: qsTr("Removed drives:")
                        text: qsTr("Keep their files searchable while unplugged")
                        checked: win.index ? win.index.keep_offline : true
                        onToggled: win.editIndex("keep_offline", checked)
                    }
                    QQC2.ComboBox {
                    implicitContentWidthPolicy: QQC2.ComboBox.WidestText
                        Kirigami.FormData.label: qsTr("Watch for changes with:")
                        readonly property var values: ["auto", "fanotify", "inotify", "poll"]
                        model: [qsTr("Best available"), "fanotify", "inotify", qsTr("Periodic checks")]
                        currentIndex: win.index ? Math.max(0, values.indexOf(win.index.watch_method)) : 0
                        onActivated: i => win.editIndex("watch_method", values[i])
                    }
                }

                RowLayout {
                    Layout.fillWidth: true
                    Layout.margins: Kirigami.Units.largeSpacing
                    QQC2.Button {
                        icon.name: "view-refresh"
                        text: qsTr("Rescan Everything")
                        enabled: !App.adminBusy
                        onClicked: App.rescanAll(win.system)
                    }
                    Item { Layout.fillWidth: true }
                    QQC2.BusyIndicator {
                        running: App.adminBusy
                        visible: running
                        Layout.preferredHeight: Kirigami.Units.iconSizes.medium
                    }
                    QQC2.Button {
                        icon.name: "edit-undo"
                        text: qsTr("Revert")
                        enabled: win.indexDirty && !App.adminBusy
                        onClicked: win.loadIndex()
                    }
                    QQC2.Button {
                        icon.name: "dialog-ok-apply"
                        text: qsTr("Apply")
                        enabled: win.indexDirty && !App.adminBusy && win.index !== null
                        onClicked: App.applyDaemonConfig(JSON.stringify(win.index), win.system)
                    }
                }
            }
        }

        // Shortcut
        QQC2.ScrollView {
            contentWidth: availableWidth
            Kirigami.FormLayout {
                width: parent.width
                QQC2.CheckBox {
                    id: shortcutBox
                    Kirigami.FormData.label: qsTr("Global shortcut:")
                    text: qsTr("Show or hide the search window from any application")
                    checked: win.settings.global_shortcut
                    onToggled: {
                        win.save("global_shortcut", checked)
                        App.setGlobalShortcut(checked, win.settings.shortcut)
                    }
                }
                QQC2.TextField {
                    Kirigami.FormData.label: qsTr("Suggested keys:")
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 12
                    text: win.settings.shortcut
                    placeholderText: "CTRL+ALT+space"
                    Accessible.name: qsTr("Suggested keys")
                    onEditingFinished: if (text !== win.settings.shortcut) {
                        win.save("shortcut", text)
                        if (shortcutBox.checked) App.setGlobalShortcut(true, text)
                    }
                }
                QQC2.Label {
                    Kirigami.FormData.label: qsTr("State:")
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    text: {
                        switch (App.shortcutState) {
                        case "pending": return qsTr("Waiting for the desktop…")
                        case "bound": return App.shortcutDetail.length > 0
                            ? qsTr("Active: %1").arg(App.shortcutDetail)
                            : qsTr("Registered; no keys assigned yet")
                        case "cancelled": return qsTr("Declined in the desktop’s dialog")
                        case "unavailable": return qsTr("Not available: %1").arg(App.shortcutDetail)
                        default: return qsTr("Off")
                        }
                    }
                }
                QQC2.Button {
                    visible: App.shortcutState === "bound"
                    icon.name: "configure-shortcuts"
                    text: qsTr("Change Keys…")
                    onClicked: if (!App.configureGlobalShortcut())
                        win.adminMessage = qsTr("Change the keys in your desktop’s keyboard settings.")
                }
                Item { Kirigami.FormData.isSection: true }
                QQC2.Label {
                    Layout.fillWidth: true
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 26
                    wrapMode: Text.Wrap
                    text: qsTr("The desktop asks you to confirm the shortcut and has the final say on its keys. If it doesn’t offer global shortcuts to applications, add a custom shortcut in its keyboard settings that runs this command:")
                }
                RowLayout {
                    QQC2.TextField {
                        id: toggleCommand
                        readOnly: true
                        text: "fossick --toggle"
                        Accessible.name: qsTr("Command for a custom shortcut")
                    }
                    QQC2.Button {
                        icon.name: "edit-copy"
                        text: qsTr("Copy")
                        onClicked: App.copyText(toggleCommand.text)
                    }
                }
                QQC2.Label {
                    Layout.fillWidth: true
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 26
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    text: qsTr("KDE Plasma: System Settings → Keyboard → Shortcuts → Add New → Command or Script.\nGNOME: Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts.")
                }
            }
        }
    }

    footer: QQC2.DialogButtonBox {
        standardButtons: QQC2.DialogButtonBox.Close
        onRejected: win.close()
    }

    Shortcut { sequences: [StandardKey.Close, "Escape"]; onActivated: win.close() }
}
