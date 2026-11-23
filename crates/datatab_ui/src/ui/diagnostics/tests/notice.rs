//! The startup notice: it appears for a pending panic report, it can be
//! answered in two ways, and closing it asks nothing of the file.

use datatab_components::accessibility::{accessible_name, assert_clickables_named};
use datatab_diagnostics::is_decided;
use datatab_ui_base::keyboard_coverage::FrameCapture;
use gpui::TestAppContext;

use super::{open_notice, panic_report, write_report};

#[gpui::test]
fn a_pending_panic_report_is_offered_at_the_next_start(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    write_report(dir.path(), &panic_report());

    let (notice, window) = open_notice(cx, dir.path());
    window.update(|window, _| window.refresh());
    window.run_until_parked();

    assert!(
        notice.read_with(window, |notice, _| notice.is_visible()),
        "a report a panic left must be offered at the next start"
    );

    let capture = FrameCapture::observe(window);
    let frame = capture.frame(window);
    assert_clickables_named(&frame);

    for (id, name) in [
        ("report-notice-review", "Review and send…"),
        ("report-notice-decline", "Don't send"),
        ("report-notice-dismiss", "Later"),
    ] {
        assert_eq!(
            accessible_name(&frame, id).as_deref(),
            Some(name),
            "{id} must carry its label"
        );
    }
}

#[gpui::test]
fn a_folder_without_undecided_reports_has_no_notice(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = write_report(dir.path(), &panic_report());
    datatab_diagnostics::mark_decided(&path).expect("decided");

    let (notice, window) = open_notice(cx, dir.path());

    assert!(
        !notice.read_with(window, |notice, _| notice.is_visible()),
        "a decided report is not offered again"
    );
}

#[gpui::test]
fn dont_send_keeps_the_file_and_stops_the_question(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = write_report(dir.path(), &panic_report());

    let (notice, window) = open_notice(cx, dir.path());
    window.update(|_, cx| notice.update(cx, |notice, cx| notice.decline(cx)));
    window.run_until_parked();

    assert!(path.is_file(), "declining keeps the report");
    assert!(is_decided(&path), "the decision is recorded");
    assert!(!notice.read_with(window, |notice, _| notice.is_visible()));

    // The next start reads the same folder and stays quiet.
    let (next_start, window) = open_notice(cx, dir.path());
    assert!(!next_start.read_with(window, |notice, _| notice.is_visible()));
}

#[gpui::test]
fn later_closes_the_notice_without_answering(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = write_report(dir.path(), &panic_report());

    let (notice, window) = open_notice(cx, dir.path());
    window.update(|_, cx| notice.update(cx, |notice, cx| notice.dismiss(cx)));
    window.run_until_parked();

    assert!(!notice.read_with(window, |notice, _| notice.is_visible()));
    assert!(
        !is_decided(&path),
        "closing the notice is not an answer about the report"
    );
}

#[gpui::test]
fn reviewing_asks_for_the_window_on_that_file(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = write_report(dir.path(), &panic_report());

    let (notice, window) = open_notice(cx, dir.path());
    let asked = std::rc::Rc::new(std::cell::RefCell::new(None));
    window.update(|_, cx| {
        let asked = asked.clone();
        cx.subscribe(
            &notice,
            move |_, event: &super::super::ReportNoticeEvent, _| {
                if let super::super::ReportNoticeEvent::Review(path) = event {
                    asked.replace(Some(path.clone()));
                }
            },
        )
        .detach();
    });

    window.update(|_, cx| notice.update(cx, |notice, cx| notice.review(cx)));
    window.run_until_parked();

    assert_eq!(asked.borrow().as_deref(), Some(path.as_path()));
}

/// Reviewing the report takes the banner away for the session: closing the
/// window with Escape must not put it back, because the notice is not
/// refreshed without a decision.
#[gpui::test]
fn reviewing_keeps_the_banner_away_for_the_session(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    write_report(dir.path(), &panic_report());

    let (notice, window) = open_notice(cx, dir.path());
    window.update(|_, cx| notice.update(cx, |notice, cx| notice.review(cx)));
    window.run_until_parked();

    // What a refresh after a non-settling outcome would do.
    window.update(|_, cx| notice.update(cx, |notice, cx| notice.refresh(cx)));
    window.run_until_parked();

    assert!(
        !notice.read_with(window, |notice, _| notice.is_visible()),
        "a refresh must not bring back a report the user already opened"
    );
}

/// "Later" is remembered for the session, so a passing app-state change
/// cannot bring the banner back under the user.
#[gpui::test]
fn later_survives_an_app_state_change(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = write_report(dir.path(), &panic_report());

    let (notice, window) = open_notice(cx, dir.path());
    let app_state = notice.read_with(window, |notice, _| notice.app_state().clone());
    window.update(|_, cx| notice.update(cx, |notice, cx| notice.dismiss(cx)));
    window.run_until_parked();

    window.update(|_, cx| app_state.update(cx, |_, cx| cx.emit(datatab_ui_base::AppStateChanged)));
    window.run_until_parked();
    // And a refresh at any other moment must not resurrect it either.
    window.update(|_, cx| notice.update(cx, |notice, cx| notice.refresh(cx)));
    window.run_until_parked();

    assert!(
        !notice.read_with(window, |notice, _| notice.is_visible()),
        "Later must hold for the session"
    );
    assert!(
        !datatab_diagnostics::is_decided(&path),
        "Later decides nothing about the file"
    );

    // The next start reads the same folder and offers it again.
    let (next_start, window) = open_notice(cx, dir.path());
    assert!(next_start.read_with(window, |notice, _| notice.is_visible()));
}

/// An app-state change that leaves the setting alone does not read the folder:
/// a report written after the window opened is not picked up until something
/// asks for it.
#[gpui::test]
fn an_unrelated_app_state_change_does_not_re_read_the_folder(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let (notice, window) = open_notice(cx, dir.path());
    assert!(!notice.read_with(window, |notice, _| notice.is_visible()));

    write_report(dir.path(), &panic_report());
    let app_state = notice.read_with(window, |notice, _| notice.app_state().clone());
    window.update(|_, cx| app_state.update(cx, |_, cx| cx.emit(datatab_ui_base::AppStateChanged)));
    window.run_until_parked();

    assert!(
        !notice.read_with(window, |notice, _| notice.is_visible()),
        "the notice must not read the pending folder on every app-state change"
    );
}

/// Turning reports off hides the banner at once, and turning them back on
/// reads the folder again.
#[gpui::test]
fn the_setting_moves_the_banner(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    write_report(dir.path(), &panic_report());

    let (notice, window) = open_notice(cx, dir.path());
    let app_state = notice.read_with(window, |notice, _| notice.app_state().clone());
    assert!(notice.read_with(window, |notice, _| notice.is_visible()));

    window.update(|_, cx| {
        app_state.update(cx, |state, cx| {
            let mut settings = state.general_settings().clone();
            settings.send_error_reports = false;
            state.update_general_settings(settings);
            cx.emit(datatab_ui_base::AppStateChanged);
        });
    });
    window.run_until_parked();

    assert!(
        !notice.read_with(window, |notice, _| notice.is_visible()),
        "reports off hides the banner"
    );
}
