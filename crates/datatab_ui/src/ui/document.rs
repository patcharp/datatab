//! Compat shim: the document subsystem now lives in `datatab_ui_document`.
pub use datatab_ui_document::*;
// workspace/render.rs:839 uses crate::ui::document::tab_bar::TabBar
pub use datatab_ui_document::tab_bar;
