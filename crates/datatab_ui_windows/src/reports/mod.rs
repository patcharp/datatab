//! Report actions shared by the send window and Settings > About.
//!
//! The two surfaces that offer to send a report — the workspace's send window
//! and the pending list in Settings > About — sit in different crates, and
//! this is the layer both can reach: it decides whether reports may be offered
//! (the setting, `DATATAB_DISABLE_ERROR_REPORTS` and the managed file), writes
//! the "Save as file" copy, makes the one send attempt, and makes the one
//! delete attempt for a report that was already sent. Nothing here draws
//! an element: each surface owns its own layout, and both act on the same
//! functions, so a change to what "send", "save" or "delete" means lands in
//! one place.

pub mod actions;
pub mod delete;
pub mod offer;

pub use actions::{
    ReportSender, default_sender, save_report_copy, send_blocking, suggested_file_name,
    suggested_file_name_for,
};
pub use delete::{ReportDeleter, default_deleter, delete_blocking};
pub use offer::{current_switch, install_offer_seam, off_reason, offer};

#[cfg(test)]
mod tests;
