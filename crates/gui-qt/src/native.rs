//! Bindings to the small C++ helpers in `cpp/native.cpp`.

#[cxx::bridge(namespace = "fossick")]
pub mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        #[namespace = ""]
        type QString = cxx_qt_lib::QString;

        include!("fossick-qt/cpp/native.h");
        fn create_application(app_name: &QString, desktop_file: &QString, version: &QString);
        fn exec_application() -> i32;
        fn choose_style();
        fn set_color_scheme(scheme: i32);
        fn set_clipboard_text(text: &QString);
        fn mime_icon_name(file_name: &QString, is_dir: bool) -> QString;
        fn mime_comment(file_name: &QString, is_dir: bool) -> QString;
        fn install_translations();
        fn schedule_screenshot(path: &QString, delay_ms: i32);
    }
}
