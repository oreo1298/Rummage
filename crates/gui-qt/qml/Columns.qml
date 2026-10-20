// Result columns: keys (as in the settings file), translated titles and how they sort. The
// order matches the model’s columns (fossick_ui::columns::Column::ALL).
pragma Singleton
import QtQuick

QtObject {
    readonly property var keys: ["name", "path", "size", "modified", "created", "accessed",
        "extension", "type", "permissions", "owner", "run-count", "volume"]
    // The daemon’s sort field for each column ("" = not sortable).
    readonly property var sortFields: ["Name", "Path", "Size", "Modified", "Created", "Accessed",
        "Extension", "Type", "", "", "RunCount", ""]

    function title(column) {
        switch (keys[column]) {
        case "name": return qsTr("Name")
        case "path": return qsTr("Path")
        case "size": return qsTr("Size")
        case "modified": return qsTr("Date Modified")
        case "created": return qsTr("Date Created")
        case "accessed": return qsTr("Date Accessed")
        case "extension": return qsTr("Extension")
        case "type": return qsTr("Type")
        case "permissions": return qsTr("Permissions")
        case "owner": return qsTr("Owner")
        case "run-count": return qsTr("Run Count")
        case "volume": return qsTr("Volume")
        }
        return ""
    }
    function indexOfSort(field) {
        return Math.max(0, sortFields.indexOf(field))
    }
    function sortable(column) {
        return sortFields[column] !== ""
    }
    function isDate(column) {
        return column >= 3 && column <= 5
    }
}
