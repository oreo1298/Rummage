//! Frontend logic shared by the Qt and GTK GUIs, free of any toolkit: the session with the
//! daemon, the row cache behind virtualized result lists, columns, filters, settings, desktop
//! actions and the choice of GUI.

pub mod columns;
pub mod desktop;
pub mod filters;
pub mod launcher;
pub mod rows;
pub mod session;
pub mod settings;
