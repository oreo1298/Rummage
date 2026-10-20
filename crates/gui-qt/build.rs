use cxx_qt_build::{CxxQtBuilder, QmlFile, QmlModule};

fn main() {
    let qml = [
        "qml/Main.qml",
        "qml/SearchView.qml",
        "qml/ResultsTable.qml",
        "qml/StatusBar.qml",
        "qml/PropertiesDialog.qml",
    ]
    .into_iter()
    .map(QmlFile::from)
    .chain(
        ["qml/Filters.qml", "qml/Columns.qml"]
            .into_iter()
            .map(|f| QmlFile::from(f).singleton(true)),
    );
    CxxQtBuilder::new_qml_module(QmlModule::new("io.github.oreo1298.Fossick").qml_files(qml))
        .qt_module("Widgets")
        .qt_module("Quick")
        .qt_module("QuickControls2")
        .files(["src/app.rs", "src/native.rs", "src/search.rs"])
        .cpp_file("cpp/native.cpp")
        .build();
}
