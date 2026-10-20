// A Fossick window: tabs of searches sharing the window’s settings.
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import io.github.oreo1298.Fossick

Kirigami.ApplicationWindow {
    id: root

    // Settings are shared by all windows through App; changes are saved right away.
    property var settings: JSON.parse(App.settingsJson)
    readonly property var current: tabs.itemAt(tabBar.currentIndex)

    function saveSettings() {
        App.saveSettings(JSON.stringify(settings))
    }
    function toggle(key) {
        settings[key] = !settings[key]
        saveSettings()
        for (let i = 0; i < tabs.count; ++i) tabs.itemAt(i).run()
    }
    function newTab(text) {
        tabModel.append({ initial: text || "" })
        tabBar.currentIndex = tabModel.count - 1
    }
    function closeTab(index) {
        if (tabModel.count <= 1) { root.close(); return }
        tabModel.remove(index)
    }
    function newWindow() {
        const c = Qt.createComponent(Qt.resolvedUrl("Main.qml"))
        const w = c.createObject(null)
        if (w) w.show()
    }
    function openMainMenu(item) {
        mainMenu.popup(item, 0, item.height)
    }

    title: current && current.text.length > 0 ? qsTr("%1 – Fossick").arg(current.text) : "Fossick"
    width: settings.window_width
    height: settings.window_height
    minimumWidth: Kirigami.Units.gridUnit * 25
    minimumHeight: Kirigami.Units.gridUnit * 15
    pageStack.globalToolBar.style: Kirigami.ApplicationHeaderStyle.None
    pageStack.initialPage: Kirigami.Page {
        padding: 0
        globalToolBarStyle: Kirigami.ApplicationHeaderStyle.None

        ColumnLayout {
            anchors.fill: parent
            spacing: 0

            QQC2.TabBar {
                id: tabBar
                Layout.fillWidth: true
                visible: tabModel.count > 1
                Repeater {
                    model: tabModel
                    delegate: QQC2.TabButton {
                        required property int index
                        width: Math.max(Kirigami.Units.gridUnit * 8, implicitWidth)
                        text: tabs.itemAt(index) ? tabs.itemAt(index).title : ""
                        contentItem: RowLayout {
                            QQC2.Label {
                                Layout.fillWidth: true
                                text: parent.parent.text
                                elide: Text.ElideRight
                            }
                            QQC2.ToolButton {
                                icon.name: "tab-close"
                                display: QQC2.AbstractButton.IconOnly
                                text: qsTr("Close Tab")
                                onClicked: root.closeTab(index)
                            }
                        }
                    }
                }
            }

            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: tabBar.currentIndex
                onCurrentIndexChanged: if (root.current) root.current.focusSearch()

                Repeater {
                    id: tabs
                    model: ListModel {
                        id: tabModel
                        Component.onCompleted: append({ initial: App.initialSearch })
                    }
                    delegate: SearchView {
                        required property string initial
                        settings: root.settings
                        saveSettings: root.saveSettings
                        Component.onCompleted: {
                            text = initial
                            run()
                            focusSearch()
                        }
                    }
                }
            }
        }
    }

    QQC2.Menu {
        id: mainMenu
        QQC2.MenuItem {
            text: qsTr("Match Case"); checkable: true; checked: root.settings.match_case
            onTriggered: root.toggle("match_case")
        }
        QQC2.MenuItem {
            text: qsTr("Match Whole Word"); checkable: true; checked: root.settings.whole_word
            onTriggered: root.toggle("whole_word")
        }
        QQC2.MenuItem {
            text: qsTr("Match Path"); checkable: true; checked: root.settings.match_path
            onTriggered: root.toggle("match_path")
        }
        QQC2.MenuItem {
            text: qsTr("Match Diacritics"); checkable: true; checked: root.settings.match_diacritics
            onTriggered: root.toggle("match_diacritics")
        }
        QQC2.MenuItem {
            text: qsTr("Regular Expressions"); checkable: true; checked: root.settings.regex
            onTriggered: root.toggle("regex")
        }
        QQC2.MenuItem {
            text: qsTr("Show Hidden Files"); checkable: true; checked: root.settings.show_hidden
            onTriggered: root.toggle("show_hidden")
        }
        QQC2.MenuSeparator {}
        QQC2.MenuItem { text: qsTr("New Tab"); icon.name: "tab-new"; onTriggered: root.newTab() }
        QQC2.MenuItem { text: qsTr("New Window"); icon.name: "window-new"; onTriggered: root.newWindow() }
        QQC2.MenuSeparator {}
        QQC2.MenuItem { text: qsTr("Quit"); icon.name: "application-exit"; onTriggered: Qt.quit() }
    }

    Shortcut { sequence: "Ctrl+I"; onActivated: root.toggle("match_case") }
    Shortcut { sequence: "Ctrl+B"; onActivated: root.toggle("whole_word") }
    Shortcut { sequence: "Ctrl+U"; onActivated: root.toggle("match_path") }
    Shortcut { sequence: "Ctrl+R"; onActivated: root.toggle("regex") }
    Shortcut { sequences: ["Ctrl+F", "F3", "Ctrl+L"]; onActivated: if (root.current) root.current.focusSearch() }
    Shortcut { sequences: [StandardKey.AddTab]; onActivated: root.newTab() }
    Shortcut { sequences: [StandardKey.Close]; onActivated: root.closeTab(tabBar.currentIndex) }
    Shortcut { sequences: [StandardKey.New]; onActivated: root.newWindow() }
    Shortcut { sequences: [StandardKey.Quit]; onActivated: Qt.quit() }
    Shortcut { sequence: "Ctrl+Tab"; onActivated: tabBar.currentIndex = (tabBar.currentIndex + 1) % tabModel.count }
    Shortcut {
        sequence: "Ctrl+Shift+Tab"
        onActivated: tabBar.currentIndex = (tabBar.currentIndex + tabModel.count - 1) % tabModel.count
    }

    onClosing: {
        settings.window_width = width
        settings.window_height = height
        if (current) current.storeColumns()
        saveSettings()
    }
}
