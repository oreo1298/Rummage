use std::path::{Path, PathBuf};
use std::process::Command;

use cxx_qt_build::{CxxQtBuilder, QmlFile, QmlModule};

/// `lrelease` from Qt 6: on `PATH`, or where `qmake6` says Qt’s tools are.
fn lrelease() -> Option<PathBuf> {
    for name in ["lrelease-qt6", "lrelease6", "lrelease"] {
        if Command::new(name).arg("-version").output().is_ok() {
            return Some(name.into());
        }
    }
    let out = Command::new("qmake6")
        .args(["-query", "QT_HOST_LIBEXECS"])
        .output()
        .ok()?;
    let p = Path::new(String::from_utf8_lossy(&out.stdout).trim()).join("lrelease");
    p.is_file().then_some(p)
}

/// Compiles `translations/fossick_<lang>.ts` into a resource file served at `:/i18n`.
/// `translations/fossick.ts` is the template for translators and is not compiled.
fn translations() -> Option<PathBuf> {
    println!("cargo::rerun-if-changed=translations");
    let mut ts: Vec<PathBuf> = std::fs::read_dir("translations")
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == "ts")
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("fossick_"))
        })
        .collect();
    if ts.is_empty() {
        return None;
    }
    ts.sort();
    let Some(tool) = lrelease() else {
        println!("cargo::warning=lrelease not found: the GUI is built without translations");
        return None;
    };
    let out = PathBuf::from(std::env::var_os("OUT_DIR")?);
    let mut qrc = String::from("<RCC>\n  <qresource prefix=\"/i18n\">\n");
    for file in &ts {
        let qm = out.join(file.file_stem()?).with_extension("qm");
        let ok = Command::new(&tool)
            .arg(file)
            .arg("-qm")
            .arg(&qm)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            println!("cargo::warning=lrelease failed on {}", file.display());
            continue;
        }
        qrc.push_str(&format!(
            "    <file alias=\"{}\">{}</file>\n",
            qm.file_name()?.to_str()?,
            qm.display()
        ));
    }
    qrc.push_str("  </qresource>\n</RCC>\n");
    let path = out.join("translations.qrc");
    std::fs::write(&path, qrc).ok()?;
    Some(path)
}

fn main() {
    let qml = [
        "qml/Main.qml",
        "qml/SearchView.qml",
        "qml/ResultsTable.qml",
        "qml/StatusBar.qml",
        "qml/PropertiesDialog.qml",
        "qml/SettingsWindow.qml",
        "qml/StringListEditor.qml",
        "qml/RecentSearchesMenu.qml",
        "qml/BookmarksMenu.qml",
    ]
    .into_iter()
    .map(QmlFile::from)
    .chain(
        ["qml/Filters.qml", "qml/Columns.qml", "qml/Windows.qml"]
            .into_iter()
            .map(|f| QmlFile::from(f).singleton(true)),
    );
    let mut builder =
        CxxQtBuilder::new_qml_module(QmlModule::new("io.github.oreo1298.Fossick").qml_files(qml))
            .qt_module("Widgets")
            .qt_module("Quick")
            .qt_module("QuickControls2")
            .files(["src/app.rs", "src/native.rs", "src/search.rs"])
            .cpp_file("cpp/native.cpp")
            .qrc("resources.qrc");
    if let Some(qrc) = translations() {
        builder = builder.qrc(qrc);
    }
    builder.build();
}
