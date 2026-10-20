// An editable list of strings (paths, patterns): one field per entry, plus Add and, for
// folders, a folder picker. Changes are reported as a whole new list through `edited`.
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Dialogs
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: editor

    property var values: []
    property string placeholder: ""
    // Name for screen readers (the form label isn’t attached to the fields).
    property string accessibleName: ""
    // Offer a folder picker next to Add.
    property bool folders: false

    signal edited(var values)

    function replaceAt(i, v) {
        const copy = values.slice()
        copy[i] = v
        edited(copy)
    }
    function removeAt(i) {
        const copy = values.slice()
        copy.splice(i, 1)
        edited(copy)
    }

    spacing: Kirigami.Units.smallSpacing

    Repeater {
        model: editor.values
        delegate: RowLayout {
            required property int index
            required property string modelData
            Layout.fillWidth: true
            QQC2.TextField {
                Layout.fillWidth: true
                text: modelData
                placeholderText: editor.placeholder
                Accessible.name: qsTr("%1, entry %2").arg(editor.accessibleName).arg(index + 1)
                onEditingFinished: if (text !== modelData) editor.replaceAt(index, text)
            }
            QQC2.ToolButton {
                icon.name: "list-remove"
                text: qsTr("Remove")
                display: QQC2.AbstractButton.IconOnly
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.text: text
                onClicked: editor.removeAt(index)
            }
        }
    }
    QQC2.Label {
        visible: editor.values.length === 0
        text: qsTr("None")
        color: Kirigami.Theme.disabledTextColor
    }
    RowLayout {
        QQC2.Button {
            icon.name: "list-add"
            text: qsTr("Add")
            Accessible.name: qsTr("Add to %1").arg(editor.accessibleName)
            onClicked: editor.edited(editor.values.concat([""]))
        }
        QQC2.Button {
            visible: editor.folders
            icon.name: "folder-open"
            text: qsTr("Choose Folder…")
            onClicked: folderDialog.open()
        }
    }

    FolderDialog {
        id: folderDialog
        onAccepted: {
            const url = selectedFolder.toString()
            if (url.startsWith("file://"))
                editor.edited(editor.values.concat([decodeURIComponent(url.substring(7))]))
        }
    }
}
