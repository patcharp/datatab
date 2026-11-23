//! The off switch and the send/save helpers, without a window.

use datatab_diagnostics::{DisableSource, SubmitOutcome, SwitchState};

use super::offer::off_reason;
use super::{send_blocking, suggested_file_name, suggested_file_name_for};

/// The line a surface shows when the setting turned reports off, in the words
/// of `docs/SETTINGS.md`.
#[test]
fn the_setting_reason_names_the_setting() {
    let reason = off_reason(SwitchState::Disabled(DisableSource::Setting)).expect("a reason");

    assert!(
        reason.contains("Offer to send a report is off in Settings"),
        "{reason}"
    );
    assert!(reason.contains("stay on disk"), "{reason}");
}

#[test]
fn the_environment_reason_names_the_variable() {
    let reason = off_reason(SwitchState::Disabled(DisableSource::Environment)).expect("a reason");

    assert!(reason.contains("DATATAB_DISABLE_ERROR_REPORTS"), "{reason}");
}

#[test]
fn the_managed_file_reason_names_the_file() {
    let reason = off_reason(SwitchState::Disabled(DisableSource::ManagedFile)).expect("a reason");

    assert!(reason.contains("error-reports-disabled"), "{reason}");
}

#[test]
fn an_enabled_switch_has_no_reason() {
    assert!(off_reason(SwitchState::Enabled).is_none());
}

/// The setting is one of the three sources, so turning it off is enough even
/// when the environment variable says "off" and no file was found.
#[test]
fn the_setting_alone_turns_sends_off() {
    let switch = datatab_diagnostics::ReportSwitch::new(false, Some("off".to_string()), false);

    assert_eq!(
        switch.state(),
        SwitchState::Disabled(DisableSource::Setting)
    );
}

/// Nothing is posted when the switch is off: no connection is opened at all.
#[test]
fn a_disabled_switch_posts_nothing() {
    let outcome = send_blocking(false, b"{}").expect("no transport error");

    assert!(
        matches!(outcome, SubmitOutcome::Disabled(_)),
        "a switch that is off must not reach the network: {outcome:?}"
    );
}

#[test]
fn the_suggested_name_carries_the_kind_and_the_id() {
    let name = suggested_file_name("panic", "6f1c6c2e-6d1e-4c50-9b34-91a1a2b3c4d5");

    assert_eq!(
        name,
        "datatab-report-panic-6f1c6c2e-6d1e-4c50-9b34-91a1a2b3c4d5.json"
    );
}

#[test]
fn the_suggested_name_is_read_from_the_report_itself() {
    let report = datatab_diagnostics::ErrorReport::new(
        datatab_diagnostics::ReportKind::Error,
        "2026-11-23T05:00:00Z",
    );
    let bytes = report.to_json().expect("serializes");

    assert_eq!(
        suggested_file_name_for(&bytes),
        format!("datatab-report-error-{}.json", report.report_id)
    );
    assert_eq!(
        suggested_file_name_for(b"not a report"),
        "datatab-report.json"
    );
}

/// The two organization sources reach the surfaces as data, so an
/// organization's managed file turns reports off without the surface reading
/// the data directory.
mod sources {
    use datatab_storage::bootstrap::StorageRuntime;
    use datatab_ui_base::{AppStateEntity, ReportOffSources};
    use gpui::{AppContext as _, Entity, TestAppContext};

    use super::super::offer::offer;
    use datatab_diagnostics::{DisableSource, SwitchState};

    fn app_state(cx: &mut TestAppContext, sources: ReportOffSources) -> Entity<AppStateEntity> {
        let state = cx.new(|_| {
            AppStateEntity::new_with_storage_runtime(
                StorageRuntime::in_memory().expect("in-memory storage"),
            )
            .expect("test app state")
        });
        state.update(cx, |state, _| state.set_report_off_sources(sources));
        state
    }

    #[gpui::test]
    fn an_app_state_with_no_organization_source_offers_reports(cx: &mut TestAppContext) {
        let state = app_state(cx, ReportOffSources::default());
        let values = cx.update(|cx| offer(state.read(cx), cx));

        assert_eq!(values, SwitchState::Enabled);
    }

    /// `docs/SETTINGS.md`: the managed file wins over the environment
    /// variable, which wins over the setting.
    #[gpui::test]
    fn the_managed_file_is_named_as_the_source(cx: &mut TestAppContext) {
        let state = app_state(
            cx,
            ReportOffSources {
                environment_value: Some("1".to_string()),
                managed_file_found: true,
            },
        );
        let values = cx.update(|cx| offer(state.read(cx), cx));

        assert_eq!(
            values,
            SwitchState::Disabled(DisableSource::ManagedFile),
            "the managed file outranks the environment variable"
        );
    }

    #[gpui::test]
    fn the_environment_variable_is_named_as_the_source(cx: &mut TestAppContext) {
        let state = app_state(
            cx,
            ReportOffSources {
                environment_value: Some("yes".to_string()),
                managed_file_found: false,
            },
        );
        let values = cx.update(|cx| offer(state.read(cx), cx));

        assert_eq!(values, SwitchState::Disabled(DisableSource::Environment));
    }
}
