//! The off switch, read the same way by every surface that offers a report.
//!
//! ADR 0008 names three sources and `docs/SETTINGS.md` documents them: the
//! setting "Offer to send a report", `DATATAB_DISABLE_ERROR_REPORTS`, and a
//! managed file named `error-reports-disabled`. `datatab_diagnostics` holds
//! the decision; this module reads the sources the decision needs and turns
//! the answer into the words a surface shows.

use datatab_diagnostics::switch::managed_file_paths;
use datatab_diagnostics::{DisableSource, ReportSwitch, SwitchState};
use datatab_ui_base::{AppStateEntity, AppStateGlobal, ReportOfferSeam};
use gpui::App;

/// The switch with all three sources read now.
///
/// It is read again at every send and every time a surface opens, so turning
/// reports off applies without a restart. A data directory that cannot be
/// found drops its managed file from the list; the other two sources still
/// decide.
pub fn current_switch(setting_enabled: bool) -> ReportSwitch {
    let paths = datatab_storage::paths::data_dir()
        .map(|dir| managed_file_paths(&dir))
        .unwrap_or_default();

    ReportSwitch::from_process(setting_enabled, &paths)
}

/// Whether reports may be offered, from the app's persisted setting and the
/// two organization sources the app start read.
///
/// The setting is read at every call, so turning reports off applies without a
/// restart; the environment variable and the managed file arrive as data, so a
/// draw never touches the environment or the data directory.
pub fn offer(app_state: &AppStateEntity, _cx: &App) -> SwitchState {
    let sources = app_state.report_off_sources();

    ReportSwitch::new(
        app_state.general_settings().send_error_reports,
        sources.environment_value.clone(),
        sources.managed_file_found,
    )
    .state()
}

/// Why reports are not offered, in the words `docs/SETTINGS.md` uses, or
/// `None` when they are offered.
///
/// A surface that hides its send button shows this line instead, so a user
/// who expected the button learns which of the three sources turned it off.
pub fn off_reason(state: SwitchState) -> Option<String> {
    match state {
        SwitchState::Enabled => None,
        SwitchState::Disabled(source) => Some(match source {
            DisableSource::Setting => datatab_i18n::t!("reports.off.setting"),
            DisableSource::Environment => datatab_i18n::t!("reports.off.environment"),
            DisableSource::ManagedFile => datatab_i18n::t!("reports.off.managed_file"),
        }),
    }
}

/// Installs the answer error toasts ask for.
///
/// `report_error` lives in `datatab_ui_base`, which does not link the report
/// client, so the workspace installs this once at startup. The function reads
/// the setting from the global app state each time it runs: a user who turns
/// reports off stops seeing "Send report…" on the next error.
pub fn install_offer_seam(cx: &mut App) {
    cx.set_global(ReportOfferSeam(offered_now));
}

/// The seam itself: the setting from the global app state, and the two
/// organization sources from this machine.
///
/// A non-capturing function, so the seam holds no state of its own. Without a
/// global app state (a window drawn before startup finished) nothing is
/// offered, which fails closed.
fn offered_now(cx: &App) -> bool {
    let Some(global) = cx.try_global::<AppStateGlobal>() else {
        return false;
    };

    offer(global.entity.read(cx), cx).is_enabled()
}
