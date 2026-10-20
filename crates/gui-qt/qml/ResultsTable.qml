// The results table: sortable, resizable and reorderable columns over a lazily loaded model,
// with Everything’s keyboard shortcuts, a context menu and drag and drop to other apps.
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import io.github.oreo1298.Fossick

FocusScope {
    id: root

    required property var model
    required property var settings
    required property var saveSettings
    readonly property int selectedCount: model.selectedCount
    property int currentRow: -1

    signal searchFocusRequested()
    // A result was opened, shown or copied (the search goes into the recent searches).
    signal resultUsed()

    function open(withChooser) {
        root.model.openSelected(withChooser)
        resultUsed()
    }
    function showInFolder() {
        root.model.showSelectedInFolder()
        resultUsed()
    }
    function copy(fullPath) {
        root.model.copySelected(fullPath)
        resultUsed()
    }

    function focusTable() {
        if (root.model.total > 0 && currentRow < 0) setCurrent(0, 0)
        table.forceActiveFocus()
    }
    function setCurrent(row, mode) {
        if (row < 0 || row >= root.model.total) return
        const moved = row !== currentRow
        currentRow = row
        if (mode >= 0) root.model.select(row, mode)
        table.positionViewAtRow(row, TableView.Contain)
        if (moved && table.activeFocus) Qt.callLater(announceCurrent)
    }
    // Focus stays on the table while the current row moves, so tell screen readers which row
    // it is (Qt 6.8 and later).
    function announceCurrent() {
        const name = root.model.nameAt(currentRow)
        if (name === "" || typeof table.Accessible.announce !== "function") return
        const selected = root.model.isSelected(currentRow) ? qsTr("selected") : qsTr("not selected")
        table.Accessible.announce(qsTr("%1, %2, row %3 of %4").arg(name).arg(selected)
            .arg(currentRow + 1).arg(root.model.total))
    }
    function openContextMenu() {
        if (currentRow < 0) return
        const rowHeight = Kirigami.Units.gridUnit * 1.5
        contextMenu.popup(table, Kirigami.Units.gridUnit,
                          Math.max(0, (currentRow + 1) * rowHeight - table.contentY))
    }
    function columnState(column) {
        return root.settings.columns.find(c => c.column === Columns.keys[column])
    }
    // Visual order of the model columns (TableView reports moves but has no getter).
    property var order: Columns.keys.map((k, i) => i)
    onOrderChanged: Qt.callLater(table.forceLayout)
    // Column visibility can change in the settings window.
    onSettingsChanged: Qt.callLater(table.forceLayout)
    property bool restored: false

    // Saves widths and the visual order of the columns.
    function storeColumns() {
        const cols = []
        for (const logical of order) {
            const c = columnState(logical)
            if (!c) continue
            const w = table.explicitColumnWidth(logical)
            if (w > 0) c.width = Math.round(w)
            cols.push(c)
        }
        if (cols.length === root.settings.columns.length) root.settings.columns = cols
        root.saveSettings()
    }
    function restoreOrder() {
        if (restored) return
        restored = true
        const want = root.settings.columns.map(c => Columns.keys.indexOf(c.column)).filter(i => i >= 0)
        for (let v = 0; v < want.length; ++v) {
            const from = order.indexOf(want[v])
            if (from >= 0 && from !== v) table.moveColumn(from, v)
        }
    }
    function setColumnVisible(column, visible) {
        const c = columnState(column)
        if (!c || c.column === "name") return
        c.visible = visible
        root.saveSettings()
        Qt.callLater(table.forceLayout)
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        QQC2.HorizontalHeaderView {
            id: header
            Layout.fillWidth: true
            syncView: table
            clip: true
            resizableColumns: true
            movableColumns: true
            delegate: QQC2.ItemDelegate {
                id: headerCell
                required property int index
                implicitHeight: Kirigami.Units.gridUnit * 1.6
                text: Columns.title(index)
                icon.name: root.model.sortColumn === index
                    ? (root.model.sortDescending ? "view-sort-descending" : "view-sort-ascending")
                    : ""
                Accessible.role: Accessible.ColumnHeader
                onClicked: {
                    if (!Columns.sortable(index)) return
                    const desc = root.model.sortColumn === index ? !root.model.sortDescending : false
                    root.model.sortBy(index, desc)
                    root.settings.sort = Columns.sortFields[index]
                    root.settings.sort_descending = desc
                    root.saveSettings()
                }
                TapHandler {
                    acceptedButtons: Qt.RightButton
                    onTapped: columnMenu.popup()
                }
            }
        }

        // No ScrollView around the table: a ScrollView’s implicit size is its content size, and a
        // TableView laid out at its content height (rows × row height) creates a delegate for
        // every cell of the result set.
        TableView {
            id: table
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            focus: true
            model: root.model
            boundsBehavior: Flickable.StopAtBounds
            keyNavigationEnabled: false
            pointerNavigationEnabled: false
            resizableColumns: true
            reuseItems: true
            Accessible.role: Accessible.Table
            Accessible.name: qsTr("Search results")
            function widthOf(column) {
                const c = root.columnState(column)
                if (!c || !c.visible) return 0
                const w = explicitColumnWidth(column)
                return w > 0 ? w : c.width
            }
            // The last visible column takes the remaining width.
            columnWidthProvider: column => {
                const w = widthOf(column)
                if (w === 0) return 0
                const visible = root.order.filter(l => widthOf(l) > 0)
                if (visible[visible.length - 1] !== column) return w
                const others = visible.slice(0, -1).reduce((sum, l) => sum + widthOf(l), 0)
                return Math.max(w, table.width - others)
            }
            onWidthChanged: Qt.callLater(forceLayout)
            onColumnsChanged: if (columns > 0) Qt.callLater(root.restoreOrder)
            onColumnMoved: (logical, oldVisual, newVisual) => {
                const o = root.order.slice()
                o.splice(oldVisual, 1)
                o.splice(newVisual, 0, logical)
                root.order = o
            }

            delegate: Rectangle {
                id: cell
                required property int row
                required property int column
                required property var model
                readonly property bool isSelected: model.selected === true
                implicitHeight: Kirigami.Units.gridUnit * 1.5
                color: isSelected ? Kirigami.Theme.highlightColor
                     : (row % 2 ? Kirigami.Theme.alternateBackgroundColor : Kirigami.Theme.backgroundColor)
                Accessible.role: Accessible.Cell
                // Plain text: the name and path labels carry highlight markup.
                Accessible.name: cell.column <= 1 ? (cell.model.display || "") : label.text
                Accessible.selected: isSelected

                Rectangle {
                    anchors.fill: parent
                    visible: cell.row === root.currentRow && table.activeFocus
                    color: "transparent"
                    border.color: Kirigami.Theme.focusColor
                    border.width: cell.column === 0 ? 1 : 0
                }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: Kirigami.Units.smallSpacing
                    anchors.rightMargin: Kirigami.Units.smallSpacing
                    spacing: Kirigami.Units.smallSpacing
                    Kirigami.Icon {
                        visible: cell.column === 0
                        source: cell.model.iconName || "text-x-generic"
                        Layout.preferredWidth: Kirigami.Units.iconSizes.small
                        Layout.preferredHeight: Kirigami.Units.iconSizes.small
                    }
                    QQC2.Label {
                        id: label
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        textFormat: cell.column <= 1 ? Text.StyledText : Text.PlainText
                        horizontalAlignment: cell.column === 2 || cell.column === 10 ? Text.AlignRight : Text.AlignLeft
                        color: cell.isSelected ? Kirigami.Theme.highlightedTextColor
                             : (cell.model.offline ? Kirigami.Theme.disabledTextColor : Kirigami.Theme.textColor)
                        text: {
                            if (!cell.model.loaded) return ""
                            const v = cell.model.value
                            if (cell.column <= 1) return cell.model.html || ""
                            if (cell.column === 2) return v < 0 ? "" : Qt.locale().formattedDataSize(v, 1, Locale.DataSizeTraditionalFormat)
                            if (Columns.isDate(cell.column))
                                return v < 0 ? "" : new Date(v * 1000).toLocaleString(Qt.locale(), Locale.ShortFormat)
                            return cell.model.display || ""
                        }
                    }
                }

                // Drag the selection (or this row) out to other applications.
                Drag.dragType: Drag.Automatic
                Drag.supportedActions: Qt.CopyAction | Qt.LinkAction
                Drag.mimeData: ({ "text/uri-list": dragUris })
                property string dragUris: ""
                DragHandler {
                    id: drag
                    target: null
                    onActiveChanged: {
                        if (active) {
                            if (!cell.isSelected) root.setCurrent(cell.row, 0)
                            cell.dragUris = root.model.uriList()
                            cell.Drag.active = true
                        } else {
                            cell.Drag.active = false
                        }
                    }
                }

                TapHandler {
                    acceptedButtons: Qt.LeftButton
                    onTapped: (point, button) => {
                        table.forceActiveFocus()
                        const ctrl = point.modifiers & Qt.ControlModifier
                        const shift = point.modifiers & Qt.ShiftModifier
                        root.setCurrent(cell.row, ctrl && shift ? 3 : shift ? 2 : ctrl ? 1 : 0)
                    }
                    onDoubleTapped: root.open(false)
                }
                TapHandler {
                    acceptedButtons: Qt.RightButton
                    onTapped: {
                        table.forceActiveFocus()
                        if (!cell.isSelected) root.setCurrent(cell.row, 0)
                        else root.currentRow = cell.row
                        contextMenu.popup()
                    }
                }
            }

            QQC2.ScrollBar.vertical: QQC2.ScrollBar {}
            QQC2.ScrollBar.horizontal: QQC2.ScrollBar {}

            Keys.onPressed: event => {
                const ctrl = event.modifiers & Qt.ControlModifier
                const shift = event.modifiers & Qt.ShiftModifier
                const alt = event.modifiers & Qt.AltModifier
                const page = Math.max(1, Math.floor(table.height / (Kirigami.Units.gridUnit * 1.5)) - 1)
                // Moving: Shift extends the selection, Ctrl moves without selecting.
                const mode = shift ? 2 : (ctrl ? -1 : 0)
                let target = -1
                switch (event.key) {
                case Qt.Key_Up: target = Math.max(0, root.currentRow - 1); break
                case Qt.Key_Down: target = Math.min(root.model.total - 1, root.currentRow + 1); break
                case Qt.Key_PageUp: target = Math.max(0, root.currentRow - page); break
                case Qt.Key_PageDown: target = Math.min(root.model.total - 1, root.currentRow + page); break
                case Qt.Key_Home: target = 0; break
                case Qt.Key_End: target = root.model.total - 1; break
                }
                if (target >= 0) {
                    if (event.key === Qt.Key_Up && root.currentRow === 0 && !shift) {
                        root.searchFocusRequested()
                    } else {
                        root.setCurrent(target, mode)
                    }
                    event.accepted = true
                    return
                }
                if (event.key === Qt.Key_Space && ctrl) {
                    root.model.select(root.currentRow, 1)
                } else if (event.key === Qt.Key_A && ctrl) {
                    root.model.selectAll()
                } else if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && alt) {
                    propertiesDialog.show(root.currentRow)
                } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                    if (ctrl) root.showInFolder()
                    else root.open(false)
                } else if (event.key === Qt.Key_C && ctrl && shift) {
                    root.copy(true)
                } else if (event.key === Qt.Key_C && ctrl) {
                    root.copy(false)
                } else if (event.key === Qt.Key_F2) {
                    renameDialog.show(root.currentRow)
                } else if (event.key === Qt.Key_Delete && shift) {
                    deleteDialog.open()
                } else if (event.key === Qt.Key_Delete) {
                    root.model.trashSelected()
                } else if (event.key === Qt.Key_Menu || (event.key === Qt.Key_F10 && shift)) {
                    root.openContextMenu()
                } else if (event.key === Qt.Key_Escape) {
                    root.model.clearSelection()
                    root.searchFocusRequested()
                } else {
                    return
                }
                event.accepted = true
            }
        }
    }

    Connections {
        target: root.model
        function onModelReset() { root.currentRow = -1 }
    }

    QQC2.Menu {
        id: contextMenu
        QQC2.MenuItem { text: qsTr("Open"); icon.name: "document-open"; onTriggered: root.open(false) }
        QQC2.MenuItem { text: qsTr("Open With…"); icon.name: "document-open"; onTriggered: root.open(true) }
        QQC2.MenuItem { text: qsTr("Show in Folder"); icon.name: "document-open-folder"; onTriggered: root.showInFolder() }
        QQC2.MenuSeparator {}
        QQC2.MenuItem { text: qsTr("Copy Name"); icon.name: "edit-copy"; onTriggered: root.copy(false) }
        QQC2.MenuItem { text: qsTr("Copy Full Path"); icon.name: "edit-copy-path"; onTriggered: root.copy(true) }
        QQC2.MenuSeparator {}
        QQC2.MenuItem { text: qsTr("Rename…"); icon.name: "edit-rename"; enabled: root.selectedCount === 1; onTriggered: renameDialog.show(root.currentRow) }
        QQC2.MenuItem { text: qsTr("Move to Trash"); icon.name: "edit-delete"; onTriggered: root.model.trashSelected() }
        QQC2.MenuItem { text: qsTr("Delete Permanently…"); icon.name: "edit-delete-shred"; onTriggered: deleteDialog.open() }
        QQC2.MenuSeparator {}
        QQC2.MenuItem { text: qsTr("Properties"); icon.name: "document-properties"; onTriggered: propertiesDialog.show(root.currentRow) }
    }

    QQC2.Menu {
        id: columnMenu
        Repeater {
            model: Columns.keys.length
            delegate: QQC2.MenuItem {
                required property int index
                text: Columns.title(index)
                checkable: true
                enabled: index !== 0
                checked: { const c = root.columnState(index); return c ? c.visible : false }
                onTriggered: root.setColumnVisible(index, checked)
            }
        }
    }

    Kirigami.PromptDialog {
        id: renameDialog
        property int row: -1
        function show(r) {
            if (r < 0) return
            row = r
            nameField.text = root.model.nameAt(r)
            open()
            nameField.forceActiveFocus()
            const dot = nameField.text.lastIndexOf(".")
            nameField.select(0, dot > 0 ? dot : nameField.text.length)
        }
        title: qsTr("Rename")
        standardButtons: Kirigami.Dialog.Ok | Kirigami.Dialog.Cancel
        QQC2.TextField {
            id: nameField
            Accessible.name: qsTr("New name")
            onAccepted: renameDialog.accept()
        }
        onAccepted: root.model.renameRow(row, nameField.text)
    }

    Kirigami.PromptDialog {
        id: deleteDialog
        title: qsTr("Delete Permanently")
        subtitle: qsTr("Delete %n selected item(s) permanently? This cannot be undone.", "", root.selectedCount)
        standardButtons: Kirigami.Dialog.Cancel
        customFooterActions: [
            Kirigami.Action {
                text: qsTr("Delete")
                icon.name: "edit-delete-shred"
                onTriggered: { root.model.deleteSelected(); deleteDialog.close() }
            }
        ]
    }

    PropertiesDialog {
        id: propertiesDialog
        model: root.model
    }
}
