// One tab: search bar, banners, results table and status bar, with its own daemon session.
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import io.github.oreo1298.Fossick

FocusScope {
    id: view

    // Shared settings object (owned by the window) and a callback to persist it.
    required property var settings
    required property var saveSettings
    property alias text: searchField.text
    readonly property alias model: results
    readonly property string title: searchField.text.length > 0 ? searchField.text : qsTr("Search")

    function flags() {
        return (settings.match_case ? 1 : 0) | (settings.whole_word ? 2 : 0)
             | (settings.match_path ? 4 : 0) | (settings.regex ? 8 : 0)
             | (settings.match_diacritics ? 16 : 0)
    }
    function filterQuery() {
        const all = Filters.builtin.concat(settings.custom_filters)
        const f = all.find(x => x.name === settings.filter)
        let q = f ? f.query : ""
        if (!settings.show_hidden) q = (q + " !hidden:").trim()
        return q
    }
    // What run() searched for last, to rerun only when settings change the search.
    property string lastSearch: ""
    function searchKey() {
        return [searchField.text, filterQuery(), flags()].join("\n")
    }
    function run() {
        lastSearch = searchKey()
        results.search(searchField.text, filterQuery(), flags())
    }
    function refresh() {
        if (searchKey() !== lastSearch) run()
    }
    onSettingsChanged: refresh()
    function focusSearch() {
        searchField.forceActiveFocus()
        searchField.selectAll()
    }
    function openMenu() {
        applicationWindow().openMainMenu(menuButton)
    }
    function setSearch(text) {
        searchField.text = text
        focusSearch()
    }
    function remember() {
        App.rememberSearch(searchField.text)
    }
    function applySort(field, descending) {
        results.sortBy(Columns.indexOfSort(field), descending)
    }
    function openHistory() {
        historyMenu.popup(searchField, 0, searchField.height)
    }
    function storeColumns() {
        table.storeColumns()
    }

    RecentSearchesMenu {
        id: historyMenu
        onChosen: search => view.setSearch(search)
    }

    SearchModel {
        id: results
        onActionFailed: message => applicationWindow().showPassiveNotification(message, "long")
        onStatusChanged: json => statusBar.volumes = JSON.parse(json).volumes
        Component.onCompleted: {
            const col = Columns.indexOfSort(view.settings.sort)
            results.sortBy(col, view.settings.sort_descending)
        }
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        QQC2.ToolBar {
            Layout.fillWidth: true
            RowLayout {
                anchors.fill: parent
                Kirigami.SearchField {
                    id: searchField
                    Layout.fillWidth: true
                    focus: true
                    autoAccept: true
                    delaySearch: false
                    // The window’s own Ctrl+F (also F3, Ctrl+L) focuses the field and selects its text.
                    focusSequence: ""
                    focusSequences: []
                    placeholderText: qsTr("Search")
                    Accessible.name: qsTr("Search file and folder names")
                    onTextChanged: view.run()
                    // Alt+Down lists recent searches, as in a combo box.
                    Keys.onDownPressed: event => {
                        if (event.modifiers & Qt.AltModifier) view.openHistory()
                        else table.focusTable()
                    }
                    Keys.onReturnPressed: {
                        view.remember()
                        table.focusTable()
                    }
                }
                QQC2.ComboBox {
                    id: filterBox
                    Accessible.name: qsTr("Filter")
                    model: Filters.builtin.concat(view.settings.custom_filters).map(f => f.name)
                    displayText: Filters.title(currentText)
                    delegate: QQC2.ItemDelegate {
                        required property string modelData
                        width: filterBox.width
                        text: Filters.title(modelData)
                    }
                    currentIndex: Math.max(0, model.indexOf(view.settings.filter))
                    onActivated: index => {
                        view.settings.filter = model[index]
                        view.saveSettings()
                        view.run()
                    }
                }
                QQC2.ToolButton {
                    id: menuButton
                    icon.name: "application-menu"
                    display: QQC2.AbstractButton.IconOnly
                    text: qsTr("Menu")
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.text: text
                    onClicked: applicationWindow().openMainMenu(menuButton)
                }
            }
        }

        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: results.userMode
            type: Kirigami.MessageType.Information
            position: Kirigami.InlineMessage.Position.Header
            text: qsTr("The system index service isn’t running, so only your home folder is searched.")
        }
        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: results.error.length > 0
            type: results.connected ? Kirigami.MessageType.Warning : Kirigami.MessageType.Error
            position: Kirigami.InlineMessage.Position.Header
            text: results.error
        }

        ResultsTable {
            id: table
            Layout.fillWidth: true
            Layout.fillHeight: true
            model: results
            settings: view.settings
            saveSettings: view.saveSettings
            onSearchFocusRequested: view.focusSearch()
            onResultUsed: view.remember()
        }

        StatusBar {
            id: statusBar
            Layout.fillWidth: true
            model: results
            selectedCount: table.selectedCount
        }
    }

    Timer {
        interval: 3000
        running: view.visible
        repeat: true
        triggeredOnStart: true
        onTriggered: results.requestStatus()
    }
}
