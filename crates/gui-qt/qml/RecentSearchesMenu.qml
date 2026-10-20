// The recent searches, newest first, with an entry to forget them.
import QtQuick
import QtQuick.Controls as QQC2
import io.github.oreo1298.Fossick

QQC2.Menu {
    id: menu

    signal chosen(string search)

    readonly property var entries: JSON.parse(App.historyJson).slice(0, 20)

    title: qsTr("Recent Searches")

    Instantiator {
        model: menu.entries
        delegate: QQC2.MenuItem {
            required property string modelData
            // "&" marks mnemonics in menu texts.
            text: modelData.replace(/&/g, "&&")
            onTriggered: menu.chosen(modelData)
        }
        onObjectAdded: (index, object) => menu.insertItem(index, object)
        onObjectRemoved: (index, object) => menu.removeItem(object)
    }
    QQC2.MenuItem {
        text: qsTr("No Recent Searches")
        enabled: false
        visible: menu.entries.length === 0
        height: visible ? implicitHeight : 0
    }
    QQC2.MenuSeparator {}
    QQC2.MenuItem {
        text: qsTr("Clear Recent Searches")
        icon.name: "edit-clear-history"
        enabled: menu.entries.length > 0
        onTriggered: App.clearHistory()
    }
}
