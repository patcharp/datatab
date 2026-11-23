//! Tests of the send window and the startup notice.
//!
//! Every test works on a temporary pending folder and an in-memory app state,
//! and the send window's sender is always a function the test supplies: no
//! test opens a connection to the report service, which ADR 0008 forbids.

mod notice;
mod send_window;

use std::cell::RefCell;
use std::rc::Rc;

use datatab_diagnostics::{ErrorReport, ReportKind};
use datatab_ui_base::AppStateEntity;
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use super::ReportNotice;
use super::send_modal::{ReportSendModal, ReportSendOutcome};

/// An app state over an in-memory store, with the pending reports pointed at
/// `dir` — never the folder of the user running the tests.
pub(super) fn app_state(cx: &mut TestAppContext, dir: &std::path::Path) -> Entity<AppStateEntity> {
    cx.update(|cx| {
        cx.new(|_| {
            let runtime =
                datatab_storage::bootstrap::StorageRuntime::in_memory().expect("in-memory storage");
            let mut state =
                AppStateEntity::new_with_storage_runtime(runtime).expect("test app state");
            state.set_reports_pending_dir(Some(dir.to_path_buf()));
            state
        })
    })
}

/// A panic report with only the fields the builder would fill.
pub(super) fn panic_report() -> ErrorReport {
    ErrorReport::new(ReportKind::Panic, "2026-11-23T05:00:00Z")
}

/// Writes `report` into `dir` and returns its path.
pub(super) fn write_report(dir: &std::path::Path, report: &ErrorReport) -> std::path::PathBuf {
    datatab_diagnostics::write_pending_report(dir, report).expect("pending report written")
}

/// An app state whose "Save as file" goes to `picker` instead of a native
/// dialog.
pub(super) fn app_state_with_picker(
    cx: &mut TestAppContext,
    dir: &std::path::Path,
    picker: datatab_ui_base::SaveTargetProvider,
) -> Entity<AppStateEntity> {
    let dir = dir.to_path_buf();
    let state = cx.new(move |_| {
        let runtime =
            datatab_storage::bootstrap::StorageRuntime::in_memory().expect("in-memory storage");
        let mut state = AppStateEntity::new_with_storage_runtime(runtime)
            .expect("test app state")
            .with_save_target_override(picker);
        state.set_reports_pending_dir(Some(dir));
        state
    });

    state
}

/// The send window in a window of its own, with `sender` answering Send.
pub(super) struct SendWindow {
    pub modal: Entity<ReportSendModal>,
    pub app_state: Entity<AppStateEntity>,
    pub outcomes: Rc<RefCell<Vec<ReportSendOutcome>>>,
}

pub(super) fn open_send_window<'a>(
    cx: &'a mut TestAppContext,
    dir: &std::path::Path,
    sender: datatab_ui_windows::reports::ReportSender,
) -> (SendWindow, &'a mut VisualTestContext) {
    let app_state = app_state(cx, dir);
    open_send_window_with(cx, app_state, sender)
}

/// The same window over a caller-built app state, for a test that installs a
/// save picker before the entity exists.
pub(super) fn open_send_window_with<'a>(
    cx: &'a mut TestAppContext,
    app_state: Entity<AppStateEntity>,
    sender: datatab_ui_windows::reports::ReportSender,
) -> (SendWindow, &'a mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    cx.update(datatab_ui_base::keymap::init_keymap);
    cx.update(datatab_components::modals::modal::install_modal_seams);

    let holder: Rc<RefCell<Option<Entity<ReportSendModal>>>> = Rc::new(RefCell::new(None));
    let outcomes = Rc::new(RefCell::new(Vec::new()));

    let (_, window) = cx.add_window_view({
        let holder = holder.clone();
        let state_for_window = app_state.clone();
        move |window, cx| {
            let modal =
                cx.new(|cx| ReportSendModal::new(state_for_window.clone(), cx).with_sender(sender));
            holder.replace(Some(modal.clone()));
            gpui_component::Root::new(modal, window, cx)
        }
    });

    let modal = holder
        .borrow()
        .clone()
        .expect("the send window was created");

    window.update(|_, cx| {
        let outcomes = outcomes.clone();
        cx.subscribe(&modal, move |_, event: &ReportSendOutcome, _| {
            outcomes.borrow_mut().push(*event);
        })
        .detach();
    });

    (
        SendWindow {
            modal,
            app_state,
            outcomes,
        },
        window,
    )
}

/// The startup notice in a window of its own.
pub(super) fn open_notice<'a>(
    cx: &'a mut TestAppContext,
    dir: &std::path::Path,
) -> (Entity<ReportNotice>, &'a mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);

    let app_state = app_state(cx, dir);
    let holder: Rc<RefCell<Option<Entity<ReportNotice>>>> = Rc::new(RefCell::new(None));

    let (_, window) = cx.add_window_view({
        let holder = holder.clone();
        let state_for_window = app_state.clone();
        move |window, cx| {
            let notice = cx.new(|cx| ReportNotice::new(state_for_window.clone(), cx));
            holder.replace(Some(notice.clone()));
            gpui_component::Root::new(notice, window, cx)
        }
    });
    window.run_until_parked();

    let notice = holder.borrow().clone().expect("the notice was created");
    (notice, window)
}
