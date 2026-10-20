// Saved searches: add the current one, organize them, or run one.
import QtQuick
import QtQuick.Controls as QQC2
import io.github.oreo1298.Fossick

QQC2.Menu {
    id: menu

    signal addRequested()
    signal chosen(var bookmark)

    readonly property var bookmarks: JSON.parse(App.settingsJson).bookmarks

    title: qsTr("Bookmarks")

    QQC2.MenuItem {
        text: qsTr("Add Bookmark…")
        icon.name: "bookmark-new"
        onTriggered: menu.addRequested()
    }
    QQC2.MenuItem {
        text: qsTr("Organize Bookmarks…")
        icon.name: "bookmarks-organize"
        enabled: menu.bookmarks.length > 0
        onTriggered: Windows.showSettings(1)
    }
    QQC2.MenuSeparator {
        visible: menu.bookmarks.length > 0
        height: visible ? implicitHeight : 0
    }
    Instantiator {
        model: menu.bookmarks
        delegate: QQC2.MenuItem {
            required property var modelData
            text: modelData.name.replace(/&/g, "&&")
            icon.name: "bookmarks"
            onTriggered: menu.chosen(modelData)
        }
        onObjectAdded: (index, object) => menu.insertItem(3 + index, object)
        onObjectRemoved: (index, object) => menu.removeItem(object)
    }
}
