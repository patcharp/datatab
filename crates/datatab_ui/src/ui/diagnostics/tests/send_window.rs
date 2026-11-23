//! The send window: the bytes on screen, the default action, and the two
//! things the footer can do to the report.

use std::sync::{Arc, Mutex};

use datatab_components::accessibility::{accessible_name, assert_clickables_named};
use datatab_diagnostics::{SendError, SubmitOutcome, SubmittedReport, is_decided, mark_decided};
use datatab_ui_base::keyboard_coverage::FrameCapture;
use datatab_ui_base::user_error::SendReportRequest;
use gpui::{TestAppContext, VisualTestContext};

use super::super::send_modal::ReportSendModal;
use super::{open_send_window, panic_report, write_report};

/// A sender that records what it was handed and answers with a stored report.
fn recording_sender() -> (
    datatab_ui_windows::reports::ReportSender,
    Arc<Mutex<Vec<Vec<u8>>>>,
) {
    let posted: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = posted.clone();

    let sender: datatab_ui_windows::reports::ReportSender =
        Arc::new(move |_enabled: bool, body: &[u8]| {
            seen.lock().expect("posted bodies").push(body.to_vec());

            Ok(SubmitOutcome::Sent(SubmittedReport {
                report_id: uuid::Uuid::new_v4(),
                delete_token: "test-token".to_string(),
            }))
        });

    (sender, posted)
}

/// A sender that answers with a fixed stored report, so a test knows the id
/// and token the service handed back.
fn sender_returning(
    report_id: uuid::Uuid,
    delete_token: &'static str,
) -> datatab_ui_windows::reports::ReportSender {
    Arc::new(move |_enabled: bool, _body: &[u8]| {
        Ok(SubmitOutcome::Sent(SubmittedReport {
            report_id,
            delete_token: delete_token.to_string(),
        }))
    })
}

/// A sender that fails, the way a blocked address does.
fn failing_sender() -> datatab_ui_windows::reports::ReportSender {
    Arc::new(|_enabled: bool, _body: &[u8]| {
        Err(SendError::Transport("connection refused".to_string()))
    })
}

fn request() -> SendReportRequest {
    SendReportRequest {
        correlation_id: uuid::Uuid::now_v7(),
        error_kind: "driver",
        severity: datatab_core::observability::EventSeverity::Error,
        driver_id: None,
        error_code: None,
    }
}

/// The drawer of the modal, refreshed and captured.
fn draw(
    window: &mut VisualTestContext,
    modal: &gpui::Entity<ReportSendModal>,
) -> Arc<FrameCapture> {
    window.update(|window, _| window.refresh());
    window.run_until_parked();

    let _ = modal;
    FrameCapture::observe(window)
}

#[gpui::test]
fn a_pending_report_is_shown_byte_for_byte_and_posted_unchanged(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let file_bytes = std::fs::read(&on_disk).expect("the report is readable");

    let (sender, posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();

    let shown = fixture
        .modal
        .read_with(window, |modal, _| modal.text().map(str::to_string))
        .expect("the window shows the report");
    assert_eq!(
        shown.as_bytes(),
        file_bytes,
        "the window must show the exact bytes of the file"
    );

    fixture.modal.update(window, |modal, cx| modal.send(cx));
    window.run_until_parked();

    let posted = posted.lock().expect("posted bodies").clone();
    assert_eq!(posted.len(), 1, "one press sends one report");
    assert_eq!(
        posted[0], file_bytes,
        "the sender must post the bytes the window showed"
    );
    assert_eq!(
        posted[0],
        shown.as_bytes(),
        "the window and the wire carry one string"
    );
}

#[gpui::test]
fn an_error_report_is_shown_as_the_bytes_it_posts(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let (sender, posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_error(&request(), cx));
    window.run_until_parked();

    let shown = fixture
        .modal
        .read_with(window, |modal, _| modal.text().map(str::to_string))
        .expect("the window shows the report");
    let report = datatab_diagnostics::ErrorReport::from_json(shown.as_bytes())
        .expect("what the window shows is a report");

    assert_eq!(
        report.error_kind.as_deref(),
        Some("driver"),
        "the report holds the closed fields of the error"
    );
    assert_eq!(
        shown.as_bytes(),
        report.to_json().expect("serializes"),
        "the window shows the report as it goes on the wire"
    );

    fixture.modal.update(window, |modal, cx| modal.send(cx));
    window.run_until_parked();

    assert_eq!(
        posted.lock().expect("posted bodies").as_slice(),
        [shown.as_bytes().to_vec()],
        "the sender posts exactly what the window showed"
    );
}

#[gpui::test]
fn enter_runs_dont_send_which_keeps_the_file_and_marks_it(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let (sender, posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();

    let capture = draw(window, &fixture.modal);
    let frame = capture.frame(window);
    assert_eq!(
        accessible_name(&frame, "report-send-decline").as_deref(),
        Some("Don't send"),
        "the default action is named for what it does"
    );
    assert_eq!(
        accessible_name(&frame, "report-send-send").as_deref(),
        Some("Send")
    );
    assert_clickables_named(&frame);

    window.simulate_keystrokes("enter");
    window.run_until_parked();

    assert!(
        !fixture
            .modal
            .read_with(window, |modal, _| modal.is_visible()),
        "Enter must close the window"
    );
    assert!(on_disk.is_file(), "declining keeps the file");
    assert!(is_decided(&on_disk), "the decision is recorded");
    assert!(
        posted.lock().expect("posted bodies").is_empty(),
        "declining must not send anything"
    );
    assert_eq!(
        fixture.outcomes.borrow().as_slice(),
        [super::super::send_modal::ReportSendOutcome { settled: true }],
        "the report is settled, so the next start does not offer it"
    );
}

#[gpui::test]
fn escape_closes_without_answering(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let (sender, _posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();

    window.simulate_keystrokes("escape");
    window.run_until_parked();

    assert!(
        !fixture
            .modal
            .read_with(window, |modal, _| modal.is_visible())
    );
    assert!(
        !is_decided(&on_disk),
        "Escape answered nothing, so the next start may ask again"
    );
}

#[gpui::test]
fn a_sent_report_is_kept_and_not_offered_again(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let (sender, posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();
    fixture.modal.update(window, |modal, cx| modal.send(cx));
    window.run_until_parked();

    assert_eq!(posted.lock().expect("posted bodies").len(), 1);
    assert!(on_disk.is_file(), "sending keeps the local file");
    assert!(
        is_decided(&on_disk),
        "a report that left the machine is not offered again"
    );
    let status = fixture
        .modal
        .read_with(window, |modal, _| modal.status_text())
        .expect("the window says what happened");
    assert!(status.contains("sent"), "{status}");
}

/// The token the service returned is kept beside the local file, owner-only,
/// so the report can be deleted from the server later (ADR 0008 § 3).
#[gpui::test]
fn a_sent_report_keeps_its_delete_token_beside_the_file(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let report_id = uuid::Uuid::parse_str("6f1c6c2e-6d1e-4c50-9b34-91a1a2b3c4d5").expect("id");
    let (fixture, window) = open_send_window(cx, dir.path(), sender_returning(report_id, "tok-9"));

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();
    fixture.modal.update(window, |modal, cx| modal.send(cx));
    window.run_until_parked();

    assert_eq!(
        datatab_diagnostics::sent_token(&on_disk),
        datatab_diagnostics::SentTokenLookup::Found(datatab_diagnostics::SentToken {
            report_id,
            delete_token: "tok-9".to_string(),
        }),
        "the id and the token the service returned are kept beside the file"
    );
    assert_eq!(
        datatab_diagnostics::readable_pending_reports(dir.path()).len(),
        1,
        "the token file is never listed as a report"
    );
    assert!(datatab_diagnostics::is_decided(&on_disk));
    assert!(
        !fixture
            .modal
            .read_with(window, |modal, _| modal.status_text())
            .unwrap_or_default()
            .contains("delete token"),
        "a kept token needs no line about a missing one"
    );
}

#[gpui::test]
fn a_failed_send_says_so_and_points_at_saving_the_file(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let (fixture, window) = open_send_window(cx, dir.path(), failing_sender());

    fixture
        .modal
        .update(window, |modal, cx| modal.open_error(&request(), cx));
    window.run_until_parked();
    fixture.modal.update(window, |modal, cx| modal.send(cx));
    window.run_until_parked();

    let status = fixture
        .modal
        .read_with(window, |modal, _| modal.status_text())
        .expect("the window says what happened");
    assert!(status.contains("connection refused"), "{status}");
    assert!(status.contains("email"), "{status}");
}

/// The off switch: no Send button, and the same words the settings doc uses
/// for the source that turned reports off.
#[gpui::test]
fn reports_off_hides_send_and_names_the_source(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let (sender, _posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture.app_state.update(window, |state, _| {
        let mut settings = state.general_settings().clone();
        settings.send_error_reports = false;
        state.update_general_settings(settings);
    });

    fixture
        .modal
        .update(window, |modal, cx| modal.open_error(&request(), cx));
    window.run_until_parked();

    let capture = draw(window, &fixture.modal);
    let frame = capture.frame(window);

    assert!(
        accessible_name(&frame, "report-send-send").is_none(),
        "the off switch removes the send button"
    );
    assert!(
        accessible_name(&frame, "report-send-decline").is_some(),
        "the window still lets the user answer"
    );
    let reason = accessible_name(&frame, "report-send-off").expect("the reason is on screen");
    assert!(
        reason.contains("Offer to send a report is off in Settings"),
        "{reason}"
    );
}

/// An organization that dropped the managed file turns the button off too,
/// and the line names that file.
#[gpui::test]
fn a_managed_file_hides_send_and_names_the_file(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let (sender, _posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture.app_state.update(window, |state, _| {
        state.set_report_off_sources(datatab_ui_base::ReportOffSources {
            environment_value: None,
            managed_file_found: true,
        });
    });

    fixture
        .modal
        .update(window, |modal, cx| modal.open_error(&request(), cx));
    window.run_until_parked();

    let capture = draw(window, &fixture.modal);
    let frame = capture.frame(window);

    assert!(accessible_name(&frame, "report-send-send").is_none());
    let reason = accessible_name(&frame, "report-send-off").expect("the reason is on screen");
    assert!(reason.contains("error-reports-disabled"), "{reason}");
}

/// Declining a report in the window settles it, so a notice built afterwards
/// in the same folder has nothing to offer.
#[gpui::test]
fn declining_leaves_the_next_start_quiet(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let (sender, _posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();
    fixture.modal.update(window, |modal, cx| modal.decline(cx));
    window.run_until_parked();

    assert!(mark_decided(&on_disk).is_ok());
    let (notice, window) = super::open_notice(cx, dir.path());
    assert!(
        !notice.read_with(window, |notice, _| notice.is_visible()),
        "the decided report must not be offered again"
    );
}

/// "Save as file" leaves the report undecided: every button stays, so the user
/// can still send or decline it.
#[gpui::test]
fn saving_a_copy_keeps_the_three_buttons(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let target = dir.path().join("copy.json");
    let picker: datatab_ui_base::SaveTargetProvider = Arc::new({
        let target = target.clone();
        move |_request| {
            let target = target.clone();
            gpui::Task::ready(datatab_ui_base::SaveTargetOutcome::Selected {
                path: target,
                used_fallback: false,
            })
        }
    });

    let app_state = super::app_state_with_picker(cx, dir.path(), picker);
    let (sender, _posted) = recording_sender();
    let (fixture, window) = super::open_send_window_with(cx, app_state, sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();
    fixture
        .modal
        .update(window, |modal, cx| modal.save_as_file(cx));
    window.run_until_parked();

    let capture = draw(window, &fixture.modal);
    let frame = capture.frame(window);

    for id in [
        "report-send-save",
        "report-send-send",
        "report-send-decline",
    ] {
        assert!(
            accessible_name(&frame, id).is_some(),
            "{id} must stay after a save: the report is not decided"
        );
    }
    assert!(target.is_file(), "the copy was written");

    fixture.modal.update(window, |modal, cx| modal.decline(cx));
    window.run_until_parked();

    assert!(
        is_decided(&on_disk),
        "the user can still decline after saving"
    );
    assert_eq!(
        fixture.outcomes.borrow().as_slice(),
        [super::super::send_modal::ReportSendOutcome { settled: true }]
    );
}

/// Two presses before the first send lands post one report, not two.
#[gpui::test]
fn a_second_send_while_one_is_in_flight_posts_once(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let (sender, posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();

    fixture.modal.update(window, |modal, cx| {
        modal.send(cx);
        modal.send(cx);
    });
    window.run_until_parked();

    assert_eq!(
        posted.lock().expect("posted bodies").len(),
        1,
        "the window must never post the same report twice"
    );
}

/// The same for "Save as file": two presses open one dialog.
#[gpui::test]
fn a_second_save_while_one_is_in_flight_opens_one_dialog(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let asked = Arc::new(Mutex::new(0usize));
    let target = dir.path().join("copy.json");
    let picker: datatab_ui_base::SaveTargetProvider = {
        let asked = asked.clone();
        Arc::new(move |_request| {
            *asked.lock().expect("picker calls") += 1;
            gpui::Task::ready(datatab_ui_base::SaveTargetOutcome::Selected {
                path: target.clone(),
                used_fallback: false,
            })
        })
    };

    let app_state = super::app_state_with_picker(cx, dir.path(), picker);
    let (sender, _posted) = recording_sender();
    let (fixture, window) = super::open_send_window_with(cx, app_state, sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();

    fixture.modal.update(window, |modal, cx| {
        modal.save_as_file(cx);
        modal.save_as_file(cx);
    });
    window.run_until_parked();

    assert_eq!(*asked.lock().expect("picker calls"), 1);
}

/// A send that lands after Escape closed the window still records the
/// decision: the report left the machine, so the next start must not offer it
/// again.
#[gpui::test]
fn a_send_that_lands_after_the_window_closed_still_records_the_decision(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let (sender, posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();

    // Send, then Escape before the answer arrives: the window is closed and
    // its draft is gone while the request is still on the wire.
    fixture.modal.update(window, |modal, cx| {
        modal.send(cx);
        modal.close(false, cx);
    });
    assert!(
        !fixture
            .modal
            .read_with(window, |modal, _| modal.is_visible())
    );

    window.run_until_parked();

    assert_eq!(posted.lock().expect("posted bodies").len(), 1);
    assert!(
        is_decided(&on_disk),
        "a report that was sent is not offered again, even when the window closed first"
    );
}

/// Enter runs "Don't send", and "Don't send" is refused while a request is on
/// the wire: a send cannot be stopped, so recording a refusal would be a lie
/// about both.
#[gpui::test]
fn dont_send_during_a_send_decides_nothing(cx: &mut TestAppContext) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let on_disk = write_report(dir.path(), &panic_report());
    let (sender, posted) = recording_sender();
    let (fixture, window) = open_send_window(cx, dir.path(), sender);

    fixture
        .modal
        .update(window, |modal, cx| modal.open_pending(&on_disk, cx));
    window.run_until_parked();

    // The keyboard path Enter takes is `decline`, called while sending.
    fixture.modal.update(window, |modal, cx| {
        modal.send(cx);
        modal.decline(cx);
    });

    assert!(
        fixture
            .modal
            .read_with(window, |modal, _| modal.is_visible()),
        "the answer waits for the send: the window stays open"
    );
    assert!(
        !is_decided(&on_disk),
        "no decision may be written while the report is being sent"
    );
    assert!(
        fixture.outcomes.borrow().is_empty(),
        "a refused answer emits nothing"
    );

    window.run_until_parked();

    assert_eq!(posted.lock().expect("posted bodies").len(), 1);
    assert!(
        is_decided(&on_disk),
        "the send itself settles the report once it lands"
    );
}
