//! The result table of one tab: a `QAbstractTableModel` over a daemon session. Rows are
//! fetched a page at a time as the view asks for them, and refetched when the live result
//! changes.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++Qt" {
        include!(<QtCore/QAbstractTableModel>);
        #[qobject]
        type QAbstractTableModel;
    }

    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qvector.h");
        type QVector_i32 = cxx_qt_lib::QVector<i32>;
        include!("cxx-qt-lib/qt.h");
        #[namespace = "Qt"]
        type Orientation = cxx_qt_lib::Orientation;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[base = QAbstractTableModel]
        #[qml_element]
        #[qproperty(i32, total)]
        #[qproperty(f64, elapsed_ms)]
        #[qproperty(bool, busy)]
        #[qproperty(QString, error)]
        #[qproperty(bool, connected)]
        #[qproperty(bool, user_mode)]
        #[qproperty(QString, server)]
        #[qproperty(i32, sort_column)]
        #[qproperty(bool, sort_descending)]
        #[qproperty(i32, selected_count)]
        type SearchModel = super::SearchModelRust;
    }

    impl cxx_qt::Threading for SearchModel {}
    impl cxx_qt::Initialize for SearchModel {}

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[inherit]
        #[qsignal]
        fn data_changed(
            self: Pin<&mut SearchModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QVector_i32,
        );

        /// An action on files failed (message for the user).
        #[qsignal]
        fn action_failed(self: Pin<&mut SearchModel>, message: QString);

        /// The index status arrived (JSON; see `status_json`).
        #[qsignal]
        fn status_changed(self: Pin<&mut SearchModel>, json: QString);
    }

    #[auto_cxx_name]
    extern "RustQt" {
        /// Runs a search: `flags` is a bit set (1 match case, 2 whole word, 4 match path,
        /// 8 regex, 16 match diacritics).
        #[qinvokable]
        fn search(self: Pin<&mut SearchModel>, text: &QString, filter: &QString, flags: i32);

        /// Sorts by a column (model column index).
        #[qinvokable]
        fn sort_by(self: Pin<&mut SearchModel>, column: i32, descending: bool);

        /// Asks for the index status (answered by `statusChanged`).
        #[qinvokable]
        fn request_status(self: &SearchModel);

        #[qinvokable]
        fn name_at(self: &SearchModel, row: i32) -> QString;
        #[qinvokable]
        fn path_at(self: &SearchModel, row: i32) -> QString;
        #[qinvokable]
        fn is_folder_at(self: &SearchModel, row: i32) -> bool;

        /// Changes the selection by a click on `row`: mode 0 replace, 1 toggle (Ctrl),
        /// 2 extend from the anchor (Shift), 3 add a range (Ctrl+Shift).
        #[qinvokable]
        fn select(self: Pin<&mut SearchModel>, row: i32, mode: i32);
        #[qinvokable]
        fn select_all(self: Pin<&mut SearchModel>);
        #[qinvokable]
        fn clear_selection(self: Pin<&mut SearchModel>);
        #[qinvokable]
        fn is_selected(self: &SearchModel, row: i32) -> bool;

        /// Opens the selection with the default applications (`with`: let the user choose).
        #[qinvokable]
        fn open_selected(self: Pin<&mut SearchModel>, with: bool);
        /// Shows the selection in the file manager.
        #[qinvokable]
        fn show_selected_in_folder(self: Pin<&mut SearchModel>);
        /// Copies names (`full_path` false) or full paths to the clipboard, one per line.
        #[qinvokable]
        fn copy_selected(self: &SearchModel, full_path: bool);
        /// Moves the selection to the trash.
        #[qinvokable]
        fn trash_selected(self: Pin<&mut SearchModel>);
        /// Deletes the selection permanently (the view asks for confirmation first).
        #[qinvokable]
        fn delete_selected(self: Pin<&mut SearchModel>);
        /// `text/uri-list` of the selection, for drag and drop.
        #[qinvokable]
        fn uri_list(self: &SearchModel) -> QString;
        /// Renames a row’s file within its folder.
        #[qinvokable]
        fn rename_row(self: Pin<&mut SearchModel>, row: i32, new_name: &QString);
        /// Properties of a row’s file, as JSON (empty on error).
        #[qinvokable]
        fn properties_json(self: &SearchModel, row: i32) -> QString;
    }

    // QAbstractItemModel API.
    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut SearchModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut SearchModel>);
        #[inherit]
        #[cxx_name = "beginInsertRows"]
        unsafe fn begin_insert_rows(
            self: Pin<&mut SearchModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        #[cxx_name = "endInsertRows"]
        unsafe fn end_insert_rows(self: Pin<&mut SearchModel>);
        #[inherit]
        #[cxx_name = "beginRemoveRows"]
        unsafe fn begin_remove_rows(
            self: Pin<&mut SearchModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        #[cxx_name = "endRemoveRows"]
        unsafe fn end_remove_rows(self: Pin<&mut SearchModel>);
        #[inherit]
        fn index(self: &SearchModel, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;
    }

    extern "RustQt" {
        #[qinvokable]
        #[cxx_override]
        fn data(self: &SearchModel, index: &QModelIndex, role: i32) -> QVariant;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &SearchModel, parent: &QModelIndex) -> i32;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "columnCount"]
        fn column_count(self: &SearchModel, parent: &QModelIndex) -> i32;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &SearchModel) -> QHash_i32_QByteArray;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "headerData"]
        fn header_data(
            self: &SearchModel,
            section: i32,
            orientation: Orientation,
            role: i32,
        ) -> QVariant;
    }
}

use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant, QVector,
};
use fossick_client::Target;
use fossick_proto::{Kind, QueryOptions, ServerMode, SortField, SortSpec};
use fossick_ui::columns::Column;
use fossick_ui::rows::{Lookup, PAGE, RowCache, RowData};
use fossick_ui::selection::{Mode, Selection};
use fossick_ui::session::{Event, QuerySpec, Session};
use fossick_ui::{desktop, filters};

use crate::native::ffi as native;

const ROLE_DISPLAY: i32 = 0;
const ROLE_TOOLTIP: i32 = 3;
const ROLE_VALUE: i32 = 256;
const ROLE_HTML: i32 = 257;
const ROLE_ICON: i32 = 258;
const ROLE_FOLDER: i32 = 259;
const ROLE_LOADED: i32 = 260;
const ROLE_OFFLINE: i32 = 261;
const ROLE_SELECTED: i32 = 262;

/// Actions on more rows than this only act on the first ones.
const ACTION_LIMIT: u64 = 100_000;

/// Pages kept per tab (×200 rows).
const CACHE_PAGES: usize = 64;

/// Where to connect: `FOSSICK_SOCKET` for tests and development, else the system daemon or the
/// user daemon.
fn target() -> Target {
    std::env::var_os("FOSSICK_SOCKET").map_or(Target::Auto, |s| Target::Socket(s.into()))
}

pub struct SearchModelRust {
    total: i32,
    elapsed_ms: f64,
    busy: bool,
    error: QString,
    connected: bool,
    user_mode: bool,
    server: QString,
    sort_column: i32,
    sort_descending: bool,
    selected_count: i32,

    session: Option<Arc<Session>>,
    selection: Selection,
    seq: u64,
    spec: Option<QuerySpec>,
    cache: RefCell<RowCache>,
    /// MIME (icon name, description) by extension (`/` for folders).
    mime: RefCell<HashMap<String, (QString, QString)>>,
    /// (mount path, label) of each volume, longest mount path first.
    volumes: Vec<(Vec<u8>, String)>,
}

impl Default for SearchModelRust {
    fn default() -> Self {
        Self {
            total: 0,
            elapsed_ms: 0.0,
            busy: false,
            error: QString::default(),
            connected: false,
            user_mode: false,
            server: QString::default(),
            sort_column: 0,
            sort_descending: false,
            selected_count: 0,
            session: None,
            selection: Selection::default(),
            seq: 0,
            spec: None,
            cache: RefCell::new(RowCache::new(CACHE_PAGES)),
            mime: RefCell::new(HashMap::new()),
            volumes: Vec::new(),
        }
    }
}

impl cxx_qt::Initialize for qobject::SearchModel {
    fn initialize(mut self: Pin<&mut Self>) {
        let thread = self.qt_thread();
        let session = Session::start(target(), move |event| {
            let _ = thread.queue(move |this| this.on_event(event));
        });
        session.status();
        self.as_mut().rust_mut().session = Some(Arc::new(session));
    }
}

fn qs(s: &str) -> QString {
    QString::from(s)
}

fn lossy(b: &[u8]) -> QString {
    QString::from(&*String::from_utf8_lossy(b))
}

impl qobject::SearchModel {
    fn on_event(mut self: Pin<&mut Self>, event: Event) {
        match event {
            Event::Connected { mode, server } => {
                self.as_mut().set_connected(true);
                self.as_mut().set_user_mode(mode == ServerMode::User);
                self.as_mut().set_server(qs(&server));
                self.as_mut().set_error(QString::default());
            }
            Event::Disconnected(message) => {
                self.as_mut().set_connected(false);
                self.as_mut().set_busy(false);
                self.as_mut().set_error(qs(&message));
            }
            Event::Result {
                seq,
                total,
                elapsed,
            } if seq == self.seq => {
                // SAFETY: begin/end reset are paired.
                unsafe { self.as_mut().begin_reset_model() };
                self.cache.borrow_mut().reset(seq, total);
                self.as_mut().rust_mut().selection.clear();
                // SAFETY: paired with begin_reset_model above.
                unsafe { self.as_mut().end_reset_model() };
                self.as_mut().set_selected_count(0);
                self.as_mut()
                    .set_total(i32::try_from(total).unwrap_or(i32::MAX));
                self.as_mut().set_elapsed_ms(elapsed.as_secs_f64() * 1e3);
                self.as_mut().set_busy(false);
                self.as_mut().set_error(QString::default());
            }
            Event::QueryError { seq, message } if seq == self.seq => {
                // SAFETY: paired with end_reset_model below.
                unsafe { self.as_mut().begin_reset_model() };
                self.cache.borrow_mut().reset(seq, 0);
                self.as_mut().rust_mut().selection.clear();
                // SAFETY: paired with begin_reset_model above.
                unsafe { self.as_mut().end_reset_model() };
                self.as_mut().set_selected_count(0);
                self.as_mut().set_total(0);
                self.as_mut().set_busy(false);
                self.as_mut().set_error(qs(&message));
            }
            Event::Rows { seq, offset, rows } => {
                let range = self.cache.borrow_mut().insert(seq, offset, rows);
                if let Some((first, last)) = range {
                    self.as_mut().rows_changed(first, last);
                }
            }
            Event::RowsFailed { offset, .. } => {
                self.cache.borrow_mut().forget_request(offset);
            }
            Event::Changed { seq, total } if seq == self.seq => {
                self.as_mut().apply_changed(total);
            }
            Event::Status(status) => {
                let mut vols: Vec<(Vec<u8>, String)> = status
                    .volumes
                    .iter()
                    .map(|v| {
                        let label = v
                            .label
                            .clone()
                            .unwrap_or_else(|| String::from_utf8_lossy(&v.mount_path).into_owned());
                        (v.mount_path.clone(), label)
                    })
                    .collect();
                vols.sort_by_key(|(m, _)| std::cmp::Reverse(m.len()));
                self.as_mut().rust_mut().volumes = vols;
                let json = crate::status::status_json(&status);
                self.as_mut().status_changed(qs(&json));
            }
            _ => {}
        }
    }

    fn rows_changed(self: Pin<&mut Self>, first: u64, last: u64) {
        let cols = i32::try_from(Column::ALL.len()).unwrap_or(1);
        let top = self.index(
            i32::try_from(first).unwrap_or(i32::MAX),
            0,
            &QModelIndex::default(),
        );
        let bottom = self.index(
            i32::try_from(last).unwrap_or(i32::MAX),
            cols - 1,
            &QModelIndex::default(),
        );
        self.data_changed(&top, &bottom, &QVector::default());
    }

    /// A live result changed size and content: grow or shrink the model at the end, then
    /// let the views refetch what they show.
    fn apply_changed(mut self: Pin<&mut Self>, total: u64) {
        {
            let cache = self.cache.borrow();
            let path_of = |r: u64| cache.peek(r).map(|d| d.path.clone());
            let mut sel = self.selection.clone();
            sel.pin(&path_of);
            drop(cache);
            self.as_mut().rust_mut().selection = sel;
        }
        let old = i64::from(self.total);
        let new = i64::try_from(total.min(i32::MAX as u64)).unwrap_or(0);
        let parent = QModelIndex::default();
        if new > old {
            // SAFETY: paired with end_insert_rows below.
            unsafe {
                self.as_mut().begin_insert_rows(
                    &parent,
                    i32::try_from(old).unwrap_or(0),
                    i32::try_from(new - 1).unwrap_or(0),
                );
            }
            self.cache.borrow_mut().invalidate(total);
            self.as_mut().rust_mut().total = i32::try_from(new).unwrap_or(0);
            // SAFETY: paired with begin_insert_rows above.
            unsafe { self.as_mut().end_insert_rows() };
        } else if new < old {
            // SAFETY: paired with end_remove_rows below.
            unsafe {
                self.as_mut().begin_remove_rows(
                    &parent,
                    i32::try_from(new).unwrap_or(0),
                    i32::try_from(old - 1).unwrap_or(0),
                );
            }
            self.cache.borrow_mut().invalidate(total);
            self.as_mut().rust_mut().total = i32::try_from(new).unwrap_or(0);
            // SAFETY: paired with begin_remove_rows above.
            unsafe { self.as_mut().end_remove_rows() };
        } else {
            self.cache.borrow_mut().invalidate(total);
        }
        self.as_mut().total_changed();
        if new > 0 {
            self.as_mut()
                .rows_changed(0, u64::try_from(new - 1).unwrap_or(0));
        }
    }

    pub fn search(mut self: Pin<&mut Self>, text: &QString, filter: &QString, flags: i32) {
        let options = QueryOptions {
            match_case: flags & 1 != 0,
            whole_word: flags & 2 != 0,
            match_path: flags & 4 != 0,
            regex: flags & 8 != 0,
            match_diacritics: flags & 16 != 0,
        };
        let spec = QuerySpec {
            text: filters::combine(&filter.to_string(), &text.to_string()),
            options,
            sort: self.sort_spec(),
        };
        self.as_mut().run(spec);
    }

    fn sort_spec(&self) -> SortSpec {
        SortSpec {
            field: usize::try_from(self.sort_column)
                .ok()
                .and_then(Column::from_index)
                .and_then(Column::sort_field)
                .unwrap_or(SortField::Name),
            descending: self.sort_descending,
        }
    }

    fn run(mut self: Pin<&mut Self>, spec: QuerySpec) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let seq = session.query(spec.clone());
        self.as_mut().rust_mut().seq = seq;
        self.as_mut().rust_mut().spec = Some(spec);
        self.as_mut().set_busy(true);
    }

    pub fn sort_by(mut self: Pin<&mut Self>, column: i32, descending: bool) {
        let sortable = usize::try_from(column)
            .ok()
            .and_then(Column::from_index)
            .and_then(Column::sort_field)
            .is_some();
        if !sortable {
            return;
        }
        self.as_mut().set_sort_column(column);
        self.as_mut().set_sort_descending(descending);
        if let Some(mut spec) = self.spec.clone() {
            spec.sort = self.sort_spec();
            self.run(spec);
        }
    }

    pub fn request_status(&self) {
        if let Some(s) = &self.session {
            s.status();
        }
    }

    fn row_data(&self, row: i32) -> Option<Arc<RowData>> {
        let row = u64::try_from(row).ok()?;
        self.cache.borrow().peek(row).cloned()
    }

    pub fn name_at(&self, row: i32) -> QString {
        self.row_data(row)
            .map_or_else(QString::default, |r| qs(&r.name))
    }

    pub fn path_at(&self, row: i32) -> QString {
        self.row_data(row)
            .map_or_else(QString::default, |r| lossy(&r.path))
    }

    pub fn is_folder_at(&self, row: i32) -> bool {
        self.row_data(row).is_some_and(|r| r.kind == Kind::Folder)
    }

    fn selection_changed(mut self: Pin<&mut Self>) {
        let count = i32::try_from(self.selection.count()).unwrap_or(i32::MAX);
        self.as_mut().set_selected_count(count);
        let total = u64::try_from(self.total).unwrap_or(0);
        if total > 0 {
            let cols = i32::try_from(Column::ALL.len()).unwrap_or(1);
            let top = self.index(0, 0, &QModelIndex::default());
            let bottom = self.index(self.total - 1, cols - 1, &QModelIndex::default());
            let mut roles = QVector::<i32>::default();
            roles.append(ROLE_SELECTED);
            self.data_changed(&top, &bottom, &roles);
        }
    }

    pub fn select(mut self: Pin<&mut Self>, row: i32, mode: i32) {
        let Ok(row) = u64::try_from(row) else {
            return;
        };
        let mut sel = self.selection.clone();
        {
            let cache = self.cache.borrow();
            sel.click(row, Mode::from_code(mode), &|r| {
                cache.peek(r).map(|d| d.path.clone())
            });
        }
        self.as_mut().rust_mut().selection = sel;
        self.selection_changed();
    }

    pub fn select_all(mut self: Pin<&mut Self>) {
        let total = u64::try_from(self.total).unwrap_or(0);
        self.as_mut().rust_mut().selection.select_all(total);
        self.selection_changed();
    }

    pub fn clear_selection(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().selection.clear();
        self.selection_changed();
    }

    pub fn is_selected(&self, row: i32) -> bool {
        let Ok(r) = u64::try_from(row) else {
            return false;
        };
        let path = self.cache.borrow().peek(r).map(|d| d.path.clone());
        self.selection.contains(r, path.as_deref())
    }

    /// The selected rows that are loaded (pinned selections: the selected files), and how many
    /// selected rows were skipped because they aren’t loaded.
    fn selected(&self) -> (Vec<Arc<RowData>>, u64) {
        let cache = self.cache.borrow();
        if let Some(paths) = self.selection.paths() {
            let mut out: Vec<Arc<RowData>> = Vec::new();
            for p in paths {
                // Pinned files may not be loaded; act on their paths anyway.
                out.push(Arc::new(RowData {
                    name: String::from_utf8_lossy(p.rsplit(|&b| b == b'/').next().unwrap_or(p))
                        .into_owned(),
                    path: p.clone(),
                    ..placeholder()
                }));
            }
            return (out, 0);
        }
        let rows = self.selection.rows(ACTION_LIMIT);
        let mut missing = self.selection.count().saturating_sub(rows.len() as u64);
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            match cache.peek(r) {
                Some(d) => out.push(Arc::clone(d)),
                None => missing += 1,
            }
        }
        (out, missing)
    }

    fn selected_paths(self: Pin<&mut Self>) -> Vec<Vec<u8>> {
        let (rows, missing) = self.selected();
        if missing > 0 {
            let message = format!("{missing} selected items are not loaded yet and were left out");
            self.action_failed(qs(&message));
        }
        rows.iter().map(|r| r.path.clone()).collect()
    }

    /// Runs a file action off the GUI thread; failures come back as `actionFailed`.
    fn act(
        self: Pin<&mut Self>,
        paths: Vec<Vec<u8>>,
        f: impl FnOnce(&[Vec<u8>]) -> Result<(), desktop::ActionError> + Send + 'static,
    ) {
        if paths.is_empty() {
            return;
        }
        let thread = self.qt_thread();
        std::thread::spawn(move || {
            if let Err(e) = f(&paths) {
                let message = e.to_string();
                let _ = thread.queue(move |this| this.action_failed(qs(&message)));
            }
        });
    }

    pub fn open_selected(mut self: Pin<&mut Self>, with: bool) {
        let paths = self.as_mut().selected_paths();
        if let Some(s) = &self.session {
            for p in &paths {
                s.report_run(p.clone());
            }
        }
        let token = std::env::var("XDG_ACTIVATION_TOKEN").ok();
        self.act(paths, move |paths| {
            for p in paths {
                if with {
                    desktop::open_with(p, token.as_deref())?;
                } else {
                    desktop::open(p, token.as_deref())?;
                }
            }
            Ok(())
        });
    }

    pub fn show_selected_in_folder(mut self: Pin<&mut Self>) {
        let paths = self.as_mut().selected_paths();
        self.act(paths, |paths| desktop::show_in_folder(paths, ""));
    }

    pub fn copy_selected(&self, full_path: bool) {
        let (rows, _) = self.selected();
        let text: Vec<String> = rows
            .iter()
            .map(|r| {
                if full_path {
                    String::from_utf8_lossy(&r.path).into_owned()
                } else {
                    r.name.clone()
                }
            })
            .collect();
        native::set_clipboard_text(&qs(&text.join("\n")));
    }

    pub fn trash_selected(mut self: Pin<&mut Self>) {
        let paths = self.as_mut().selected_paths();
        self.act(paths, desktop::trash);
    }

    pub fn delete_selected(mut self: Pin<&mut Self>) {
        let paths = self.as_mut().selected_paths();
        self.act(paths, desktop::delete);
    }

    pub fn uri_list(&self) -> QString {
        let (rows, _) = self.selected();
        let uris: Vec<String> = rows.iter().map(|r| desktop::file_uri(&r.path)).collect();
        qs(&(uris.join("\r\n") + "\r\n"))
    }

    pub fn rename_row(self: Pin<&mut Self>, row: i32, new_name: &QString) {
        let Some(r) = self.row_data(row) else {
            return;
        };
        let name = new_name.to_string().into_bytes();
        self.act(vec![r.path.clone()], move |p| {
            desktop::rename(&p[0], &name).map(drop)
        });
    }

    pub fn properties_json(&self, row: i32) -> QString {
        let Some(r) = self.row_data(row) else {
            return QString::default();
        };
        match desktop::properties(&r.path) {
            Ok(p) => qs(&crate::status::properties_json(&p, r.size)),
            Err(e) => qs(&serde_json::json!({ "error": e.to_string() }).to_string()),
        }
    }

    fn mime_of(&self, r: &RowData) -> (QString, QString) {
        let is_dir = r.kind == Kind::Folder;
        let key = if is_dir {
            "/".to_owned()
        } else {
            r.extension.to_lowercase()
        };
        if let Some(v) = self.mime.borrow().get(&key) {
            return v.clone();
        }
        let file = qs(&r.name);
        let v = (
            native::mime_icon_name(&file, is_dir),
            native::mime_comment(&file, is_dir),
        );
        self.mime.borrow_mut().insert(key, v.clone());
        v
    }

    fn volume_of(&self, path: &[u8]) -> String {
        self.volumes
            .iter()
            .find(|(m, _)| {
                m.as_slice() == b"/"
                    || (path.starts_with(m) && matches!(path.get(m.len()), None | Some(b'/')))
            })
            .map(|(_, l)| l.clone())
            .unwrap_or_default()
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Ok(row) = u64::try_from(index.row()) else {
            return QVariant::default();
        };
        let Some(column) = usize::try_from(index.column())
            .ok()
            .and_then(Column::from_index)
        else {
            return QVariant::default();
        };
        let (data, fetch) = {
            let mut cache = self.cache.borrow_mut();
            let (lookup, fetch) = cache.get(row);
            let data = match lookup {
                Lookup::Ready(r) | Lookup::Stale(r) => Some(Arc::clone(r)),
                Lookup::Missing => None,
            };
            (data, fetch)
        };
        if let (Some(offset), Some(s)) = (fetch, &self.session) {
            s.rows(self.seq, offset, u32::try_from(PAGE).unwrap_or(200));
        }
        let Some(r) = data else {
            return match role {
                ROLE_LOADED => QVariant::from(&false),
                ROLE_SELECTED => QVariant::from(&self.selection.contains(row, None)),
                _ => QVariant::default(),
            };
        };
        match role {
            ROLE_DISPLAY => QVariant::from(&self.display(&r, column)),
            ROLE_TOOLTIP => QVariant::from(&lossy(&r.path)),
            ROLE_VALUE => QVariant::from(&value(&r, column)),
            ROLE_HTML => QVariant::from(&qs(match column {
                Column::Name => &r.name_html,
                Column::Path => &r.parent_html,
                _ => "",
            })),
            ROLE_ICON => QVariant::from(&self.mime_of(&r).0),
            ROLE_FOLDER => QVariant::from(&(r.kind == Kind::Folder)),
            ROLE_LOADED => QVariant::from(&true),
            ROLE_OFFLINE => QVariant::from(&r.offline),
            ROLE_SELECTED => QVariant::from(&self.selection.contains(row, Some(&r.path))),
            _ => QVariant::default(),
        }
    }

    fn display(&self, r: &RowData, column: Column) -> QString {
        match column {
            Column::Name => qs(&r.name),
            Column::Path => qs(&r.parent),
            Column::Extension => qs(&r.extension),
            Column::Type => self.mime_of(r).1,
            Column::Permissions => qs(&r.permissions),
            Column::Owner => qs(&r.owner),
            Column::RunCount if r.run_count > 0 => qs(&r.run_count.to_string()),
            Column::Volume => qs(&self.volume_of(&r.path)),
            _ => QString::default(),
        }
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() { 0 } else { self.total }
    }

    pub fn column_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            0
        } else {
            i32::try_from(Column::ALL.len()).unwrap_or(0)
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        for (role, name) in [
            (ROLE_DISPLAY, "display"),
            (ROLE_TOOLTIP, "toolTip"),
            (ROLE_VALUE, "value"),
            (ROLE_HTML, "html"),
            (ROLE_ICON, "iconName"),
            (ROLE_FOLDER, "isFolder"),
            (ROLE_LOADED, "loaded"),
            (ROLE_OFFLINE, "offline"),
            (ROLE_SELECTED, "selected"),
        ] {
            roles.insert(role, QByteArray::from(name));
        }
        roles
    }

    pub fn header_data(
        &self,
        section: i32,
        _orientation: cxx_qt_lib::Orientation,
        role: i32,
    ) -> QVariant {
        match (
            role,
            usize::try_from(section).ok().and_then(Column::from_index),
        ) {
            (ROLE_DISPLAY, Some(c)) => QVariant::from(&qs(c.title())),
            _ => QVariant::default(),
        }
    }
}

/// The number a column shows (bytes, Unix seconds, count); -1 when unknown.
fn value(r: &RowData, column: Column) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = |v: Option<i64>| v.map_or(-1.0, |v| v as f64);
    match column {
        #[allow(clippy::cast_precision_loss)]
        Column::Size => r.size.map_or(-1.0, |s| s as f64),
        Column::Modified => n(r.modified),
        Column::Created => n(r.created),
        Column::Accessed => n(r.accessed),
        Column::RunCount => f64::from(r.run_count),
        _ => -1.0,
    }
}

/// A row with only a name and path (pinned selections whose rows aren’t loaded).
fn placeholder() -> RowData {
    RowData {
        name: String::new(),
        name_html: String::new(),
        parent: String::new(),
        parent_html: String::new(),
        path_html: String::new(),
        path: Vec::new(),
        kind: Kind::Other,
        size: None,
        modified: None,
        created: None,
        accessed: None,
        extension: String::new(),
        permissions: String::new(),
        owner: String::new(),
        run_count: 0,
        offline: false,
    }
}
