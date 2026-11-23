//! The send window, the startup notice and the "Send report…" path.
//!
//! ADR 0008 gives the user one window that shows the exact bytes of a report
//! before anything leaves the machine, with "Don't send" as the action Enter
//! runs. This module is that window ([`send_modal`]), the notice the next
//! start shows for a report a panic left ([`notice`]), and the one function
//! that answers whether reports may be offered at all ([`report_offer`]).

pub mod notice;
pub mod send_modal;

#[cfg(test)]
mod tests;

use gpui::App;

pub use notice::{ReportNotice, ReportNoticeEvent};
pub use send_modal::{ReportSendModal, ReportSendOutcome, ReportSource};

/// The two organization sources of the off switch, as the app start finds
/// them.
///
/// `main` records them on the app state before the first window, so a surface
/// that draws the offer never probes the environment or the data directory.
/// The setting is not part of this: it stays live in `general_settings`.
pub fn startup_report_off_sources() -> datatab_ui_base::ReportOffSources {
    let switch = datatab_ui_windows::reports::current_switch(true);

    datatab_ui_base::ReportOffSources {
        environment_value: switch.env_value.clone(),
        managed_file_found: switch.managed_file_found,
    }
}

/// Installs the answer error toasts ask before drawing "Send report…".
///
/// The decision lives in `datatab_diagnostics` and the surfaces read it
/// through `datatab_ui_windows::reports`; this hands it to `datatab_ui_base`,
/// which cannot read the three sources itself because it keeps the report
/// client out of its dependency tree.
pub fn install_report_offer(cx: &mut App) {
    datatab_ui_windows::reports::install_offer_seam(cx);
}
