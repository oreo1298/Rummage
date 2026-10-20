// Properties of one result, read from the filesystem.
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.Dialog {
    id: dialog

    required property var model
    property var info: ({})

    function show(row) {
        if (row < 0) return
        info = JSON.parse(model.propertiesJson(row) || "{}")
        open()
    }
    function date(secs) {
        return secs === null || secs === undefined ? qsTr("Unknown")
            : new Date(secs * 1000).toLocaleString(Qt.locale(), Locale.LongFormat)
    }

    title: qsTr("Properties")
    standardButtons: Kirigami.Dialog.Close
    preferredWidth: Kirigami.Units.gridUnit * 28

    Kirigami.FormLayout {
        Kirigami.InlineMessage {
            Kirigami.FormData.isSection: true
            visible: dialog.info.error !== undefined
            type: Kirigami.MessageType.Error
            text: dialog.info.error || ""
        }
        QQC2.Label {
            Kirigami.FormData.label: qsTr("Location:")
            text: dialog.info.path || ""
            wrapMode: Text.WrapAnywhere
            Layout.maximumWidth: Kirigami.Units.gridUnit * 20
            textFormat: Text.PlainText
        }
        QQC2.Label {
            Kirigami.FormData.label: qsTr("Type:")
            text: dialog.info.kind || ""
        }
        QQC2.Label {
            Kirigami.FormData.label: qsTr("Points to:")
            visible: !!dialog.info.target
            text: dialog.info.target || ""
            textFormat: Text.PlainText
        }
        QQC2.Label {
            Kirigami.FormData.label: qsTr("Size:")
            text: dialog.info.sizeKnown === false ? qsTr("Unknown")
                : qsTr("%1 (%L2 bytes)").arg(Qt.locale().formattedDataSize(dialog.info.size || 0)).arg(dialog.info.size || 0)
        }
        QQC2.Label { Kirigami.FormData.label: qsTr("Modified:"); text: dialog.date(dialog.info.modified) }
        QQC2.Label { Kirigami.FormData.label: qsTr("Created:"); text: dialog.date(dialog.info.created) }
        QQC2.Label { Kirigami.FormData.label: qsTr("Accessed:"); text: dialog.date(dialog.info.accessed) }
        QQC2.Label { Kirigami.FormData.label: qsTr("Changed:"); text: dialog.date(dialog.info.changed) }
        QQC2.Label {
            Kirigami.FormData.label: qsTr("Permissions:")
            text: (dialog.info.permissions || "") + "  (" + (dialog.info.mode || "") + ")"
            font.family: "monospace"
        }
        QQC2.Label { Kirigami.FormData.label: qsTr("Owner:"); text: dialog.info.owner || "" }
        QQC2.Label { Kirigami.FormData.label: qsTr("Inode:"); text: String(dialog.info.inode || "") }
        QQC2.Label { Kirigami.FormData.label: qsTr("Links:"); text: String(dialog.info.links || "") }
    }
}
