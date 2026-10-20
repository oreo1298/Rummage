// Result count, query time and the indexing state of the volumes.
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

QQC2.ToolBar {
    id: bar

    required property var model
    required property int selectedCount
    property var volumes: []

    position: QQC2.ToolBar.Footer
    Accessible.role: Accessible.StatusBar

    RowLayout {
        anchors.fill: parent
        QQC2.Label {
            text: bar.model.busy ? qsTr("Searching…")
                : bar.selectedCount > 0
                    ? qsTr("%L1 objects, %L2 selected").arg(bar.model.total).arg(bar.selectedCount)
                    : qsTr("%L1 objects").arg(bar.model.total)
        }
        QQC2.Label {
            visible: !bar.model.busy && bar.model.connected
            color: Kirigami.Theme.disabledTextColor
            text: qsTr("%1 ms").arg(bar.model.elapsedMs.toFixed(1))
        }
        Item { Layout.fillWidth: true }
        QQC2.Label {
            id: volumeState
            color: Kirigami.Theme.disabledTextColor
            elide: Text.ElideLeft
            Layout.maximumWidth: bar.width / 2
            text: {
                const v = bar.volumes
                if (!bar.model.connected) return qsTr("Not connected")
                const scanning = v.filter(x => x.state === "Scanning")
                if (scanning.length > 0)
                    return qsTr("Indexing %1…").arg(scanning.map(x => x.label || x.mount).join(", "))
                const polled = v.filter(x => x.method === "Polling")
                if (polled.length > 0)
                    return qsTr("%n volume(s), %1 checked periodically", "", v.length)
                        .arg(polled.map(x => x.label || x.mount).join(", "))
                return qsTr("%n volume(s) indexed", "", v.length)
            }
            QQC2.ToolTip.visible: hover.hovered && bar.volumes.length > 0
            QQC2.ToolTip.text: bar.volumes.map(x =>
                (x.label ? x.label + " (" + x.mount + ")" : x.mount) + ": " + x.state + ", " + x.method
                + (x.note ? " – " + x.note : "")).join("\n")
            HoverHandler { id: hover }
        }
    }
}
