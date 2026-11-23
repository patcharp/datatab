//! Whether the app may offer to send a report right now.
//!
//! The answer comes from the off switch in `datatab_diagnostics`: the user's
//! setting, `DATATAB_DISABLE_ERROR_REPORTS`, and a managed file an
//! organization drops on the machine (ADR 0008, `docs/SETTINGS.md`). This
//! crate does not link that client — its manifest keeps the HTTP library out
//! of the base layer — so the UI layer that owns it installs the answer here
//! as one function. Every surface asks the same question, and an uninstalled
//! seam answers "no", so nothing is offered before startup finished.

use gpui::App;

/// What the app start found of the two organization sources of the off switch.
///
/// The setting can change while the app runs, so every surface reads it live;
/// these two are read once at startup, the way `ReportSwitch` decides them,
/// and handed to the UI as data. A surface therefore never touches the
/// environment or the data directory while it draws, and a test runs with both
/// unset unless it says otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportOffSources {
    /// The value of `DATATAB_DISABLE_ERROR_REPORTS`, when the variable is set.
    pub environment_value: Option<String>,
    /// Whether an `error-reports-disabled` file was found on this machine.
    pub managed_file_found: bool,
}

/// The function that answers "may a report be offered now?".
///
/// It reads the three sources each time it runs, so turning reports off in
/// Settings hides the next button without a restart.
pub struct ReportOfferSeam(pub fn(&App) -> bool);

impl gpui::Global for ReportOfferSeam {}

/// Whether an error may offer "Send report…", and whether the send surfaces
/// may appear at all.
///
/// `false` before the seam is installed and whenever any source turns reports
/// off. Failing closed keeps a report from being offered in a build whose UI
/// layer never told this crate how to read the switch.
pub fn reports_offered(cx: &App) -> bool {
    cx.try_global::<ReportOfferSeam>()
        .is_some_and(|seam| (seam.0)(cx))
}

#[cfg(test)]
mod tests {
    use super::{ReportOfferSeam, reports_offered};
    use gpui::TestAppContext;

    #[gpui::test]
    fn an_uninstalled_seam_offers_nothing(cx: &mut TestAppContext) {
        cx.update(|cx| assert!(!reports_offered(cx)));
    }

    #[gpui::test]
    fn the_installed_answer_is_the_one_that_counts(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(ReportOfferSeam(|_| true)));
        cx.update(|cx| assert!(reports_offered(cx)));

        cx.update(|cx| cx.set_global(ReportOfferSeam(|_| false)));
        cx.update(|cx| assert!(!reports_offered(cx)));
    }
}
