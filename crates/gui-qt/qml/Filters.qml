// The built-in search filters and their translated names.
pragma Singleton
import QtQuick

QtObject {
    readonly property var builtin: [
        { name: "Everything", query: "" },
        { name: "Audio", query: "audio:" },
        { name: "Compressed", query: "zip:" },
        { name: "Document", query: "doc:" },
        { name: "Executable", query: "exe:" },
        { name: "Folder", query: "folder:" },
        { name: "Picture", query: "pic:" },
        { name: "Video", query: "video:" }
    ]

    function title(name) {
        switch (name) {
        case "Everything": return qsTr("Everything")
        case "Audio": return qsTr("Audio")
        case "Compressed": return qsTr("Compressed")
        case "Document": return qsTr("Document")
        case "Executable": return qsTr("Executable")
        case "Folder": return qsTr("Folder")
        case "Picture": return qsTr("Picture")
        case "Video": return qsTr("Video")
        default: return name
        }
    }
}
