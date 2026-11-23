//! The send window: every byte of one report, and the three things a user can
//! do with it.
//!
//! ADR 0008 § 2 is the specification: one serialized body is shown, saved and
//! sent, byte for byte; "Don't send" is the default action; declining keeps
//! the local file and never asks again for that report; nothing is sent
//! before the Send button is pressed. The layout follows the modals board
//! (`design/redesign-islands-overlays-and-flows-{dark,light}/03-isl-modals.html`):
//! one modal shell with an icon, Escape and a close button, and the primary
//! action last in the footer.

use std::path::{Path, PathBuf};

use datatab_components::controls::Button;
use datatab_components::modals::modal::{Modal, ModalFocus};
use datatab_components::primitives::{SurfaceRole, Text, surface};
use datatab_components::tokens::{FontSizes, Spacing};
use datatab_core::LogErr;
use datatab_diagnostics::{
    ErrorInput, MAX_TRAIL_EVENTS, SendError, SubmitOutcome, SwitchState, build_error_report,
    mark_decided, write_sent_token,
};
use datatab_ui_base::user_error::SendReportRequest;
use datatab_ui_base::{AppStateEntity, SaveTargetProvider};
use datatab_ui_windows::reports;
use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Role, ScrollHandle, Window, div, px};
use gpui_component::ActiveTheme;
use gpui_component::scroll::ScrollableElement;

/// Tallest the byte block grows before it scrolls, so a 64 KiB report never
/// pushes the buttons off the window.
const BYTES_MAX_HEIGHT: gpui::Pixels = px(340.0);

/// Where the report on screen came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportSource {
    /// A file a panic left. Declining, or sending, marks it so nothing asks
    /// about it again; the file stays either way.
    Pending(PathBuf),
    /// An error reported in this session. It exists only in memory.
    Error,
}

/// What the window is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SendState {
    Idle,
    Sending,
    Sent,
    Saving,
    Failed(String),
    /// A copy was written. The report is not settled: the user still decides.
    Saved(PathBuf),
}

/// Emitted when the window closes, so the next-start notice can re-read what
/// is still waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportSendOutcome {
    /// The report will not be offered again: declined or sent.
    pub settled: bool,
}

struct Draft {
    bytes: Vec<u8>,
    /// The bytes as text. `from_utf8_lossy` only ever changes bytes that are
    /// not UTF-8, and a report is JSON, which is UTF-8 by construction.
    text: String,
    source: ReportSource,
    /// What "Save as file" suggests: the report's kind and id.
    file_name: String,
}

/// The window that shows one report.
pub struct ReportSendModal {
    app_state: Entity<AppStateEntity>,
    draft: Option<Draft>,
    focus: ModalFocus,
    scroll: ScrollHandle,
    state: SendState,
    /// What went with the last action besides its state: a send that landed
    /// but could not be recorded.
    note: Option<String>,
    sender: reports::ReportSender,
}

impl ReportSendModal {
    pub fn new(app_state: Entity<AppStateEntity>, cx: &mut Context<Self>) -> Self {
        Self {
            app_state,
            draft: None,
            focus: ModalFocus::new(cx),
            scroll: ScrollHandle::new(),
            state: SendState::Idle,
            note: None,
            sender: reports::default_sender(),
        }
    }

    /// Test seam: answer Send without a connection.
    #[cfg(test)]
    pub fn with_sender(mut self, sender: reports::ReportSender) -> Self {
        self.sender = sender;
        self
    }

    pub fn is_visible(&self) -> bool {
        self.draft.is_some()
    }

    /// The exact bytes on screen: what "Save as file" writes and "Send"
    /// posts.
    pub fn bytes(&self) -> Option<&[u8]> {
        self.draft.as_ref().map(|draft| draft.bytes.as_slice())
    }

    /// The text the window draws.
    pub fn text(&self) -> Option<&str> {
        self.draft.as_ref().map(|draft| draft.text.as_str())
    }

    /// What the last action did, as the line under the bytes shows it. `None`
    /// when the window has not done anything yet.
    pub fn status_text(&self) -> Option<String> {
        self.status_line()
    }

    fn is_sending(&self) -> bool {
        matches!(self.state, SendState::Sending)
    }

    /// Shows a report a panic left.
    ///
    /// The bytes are read from the file, not rebuilt: the file holds exactly
    /// what the sender posts.
    pub fn open_pending(&mut self, path: &Path, cx: &mut Context<Self>) {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::warn!(
                    target: "datatab_ui::diagnostics",
                    "pending report not readable: {error}"
                );
                return;
            }
        };

        self.show(bytes, ReportSource::Pending(path.to_path_buf()), cx);
    }

    /// Shows a report built now from a reported error.
    ///
    /// The report holds the closed fields of the request and the audit trail,
    /// never the toast's text (ADR 0008 § 1).
    pub fn open_error(&mut self, request: &SendReportRequest, cx: &mut Context<Self>) {
        let (trail, now) = {
            let state = self.app_state.read(cx);
            (
                state.audit_service().try_recent_events(MAX_TRAIL_EVENTS),
                datatab_core::chrono::Utc::now(),
            )
        };

        let report = build_error_report(ErrorInput {
            error_kind: request.error_kind.to_string(),
            severity: request.severity,
            driver_id: request.driver_id.clone(),
            error_code: request.error_code.clone(),
            trail,
            now,
        });

        match report.to_json() {
            Ok(bytes) => self.show(bytes, ReportSource::Error, cx),
            Err(error) => {
                log::warn!(
                    target: "datatab_ui::diagnostics",
                    "error report not built: {error}"
                );
            }
        }
    }

    fn show(&mut self, bytes: Vec<u8>, source: ReportSource, cx: &mut Context<Self>) {
        let file_name = reports::suggested_file_name_for(&bytes);

        self.state = SendState::Idle;
        self.note = None;
        self.draft = Some(Draft {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            bytes,
            source,
            file_name,
        });
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        self.focus.focus_on_next_render();
        cx.notify();
    }

    pub fn close(&mut self, settled: bool, cx: &mut Context<Self>) {
        if self.draft.take().is_none() {
            return;
        }

        self.state = SendState::Idle;
        self.focus.restore(cx);
        cx.emit(ReportSendOutcome { settled });
        cx.notify();
    }

    /// "Don't send": keeps the file, marks it so nothing asks again, closes.
    ///
    /// Refused while an action is in flight. Enter runs this action, and a
    /// request already on the wire cannot be stopped: recording "Don't send"
    /// beside a report that is being sent would be a lie about both.
    pub fn decline(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }

        let Some(draft) = self.draft.as_ref() else {
            return;
        };

        let settled = match &draft.source {
            ReportSource::Pending(path) => match mark_decided(path) {
                Ok(()) => true,
                Err(error) => {
                    // The answer stands even if the marker could not be
                    // written: the session stops offering it, and the next
                    // start asks once more rather than losing a report.
                    log::warn!(
                        target: "datatab_ui::diagnostics",
                        "decision not recorded: {error}"
                    );
                    false
                }
            },
            // An error report exists only here: closing the window is the
            // whole of declining it.
            ReportSource::Error => true,
        };

        self.close(settled, cx);
    }

    /// "Save as file": the same bytes, in a file the user can mail.
    pub fn save_as_file(&mut self, cx: &mut Context<Self>) {
        // One action at a time: two presses before the first lands would open
        // two dialogs and write the file twice.
        if self.is_busy() {
            return;
        }

        let Some(draft) = self.draft.as_ref() else {
            return;
        };

        let bytes = draft.bytes.clone();
        let name = draft.file_name.clone();
        let provider: Option<SaveTargetProvider> = self.app_state.read(cx).save_target_override();

        self.state = SendState::Saving;

        cx.spawn(async move |this, cx| {
            let outcome =
                datatab_ui_windows::reports::save_report_copy(provider, &name, &bytes).await;

            this.update(cx, |window, cx| {
                window.state = match outcome {
                    Ok(Some(path)) => SendState::Saved(path),
                    Ok(None) => SendState::Idle,
                    Err(error) => {
                        SendState::Failed(datatab_i18n::t!("reports.save.failed", error = error))
                    }
                };
                cx.notify();
            })
            .log_err();
        })
        .detach();

        cx.notify();
    }

    /// "Send": one report, now, because the user pressed the button.
    pub fn send(&mut self, cx: &mut Context<Self>) {
        // One action at a time: two presses before the first lands would post
        // the same report twice.
        if self.is_busy() {
            return;
        }

        let Some(draft) = self.draft.as_ref() else {
            return;
        };

        let bytes = draft.bytes.clone();
        // The decision is recorded from this copy, not from `self.draft`: a
        // send can land after the window closed, and closing takes the draft.
        let source = draft.source.clone();
        let setting_enabled = self
            .app_state
            .read(cx)
            .general_settings()
            .send_error_reports;

        self.state = SendState::Sending;
        cx.notify();

        let sender = self.sender.clone();
        let send = cx
            .background_executor()
            .spawn(async move { sender(setting_enabled, &bytes) });

        cx.spawn(async move |this, cx| {
            let outcome = send.await;

            this.update(cx, |window, cx| {
                window.settle_send(outcome, &source, cx);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    /// Applies the answer of one send attempt.
    ///
    /// `source` is the report the attempt was made for, taken before the send
    /// started: a send that lands after the window closed still records the
    /// decision, so the next start does not offer a report that was sent.
    fn settle_send(
        &mut self,
        outcome: Result<SubmitOutcome, SendError>,
        source: &ReportSource,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok(SubmitOutcome::Sent(submitted)) => {
                // The report left the machine, so nothing is left to ask: the
                // file stays and the decision is recorded, so the next start
                // does not offer it again.
                self.state = SendState::Sent;

                if let ReportSource::Pending(path) = source {
                    // The delete token goes beside the file first: the marker
                    // is what makes the row read-only in Settings > About, and
                    // its delete action needs the token by the time it is
                    // drawn. An error report exists only in this window, so it
                    // has no file to keep a token beside.
                    let mut notes = Vec::new();

                    if let Err(error) = write_sent_token(path, &submitted) {
                        notes.push(datatab_i18n::t!(
                            "reports.send.token_not_kept",
                            error = error.to_string()
                        ));
                    }

                    match mark_decided(path) {
                        Ok(()) => {}
                        // The send stands; the file and the question stay, and
                        // the line says so rather than hiding it.
                        Err(error) => notes.push(datatab_i18n::t!(
                            "reports.send.not_marked",
                            error = error.to_string()
                        )),
                    }

                    if !notes.is_empty() {
                        self.note = Some(notes.join(" "));
                    }
                }
            }
            Ok(SubmitOutcome::Disabled(source)) => {
                let state = SwitchState::Disabled(source);
                self.state = SendState::Failed(
                    datatab_ui_windows::reports::off_reason(state)
                        .unwrap_or_else(|| datatab_i18n::t!("reports.off.setting")),
                );
                cx.notify();
            }
            Err(error) => {
                self.state = SendState::Failed(datatab_i18n::t!(
                    "reports.send.failed",
                    error = error.to_string()
                ));
            }
        }
    }
}

impl EventEmitter<ReportSendOutcome> for ReportSendModal {}

impl Render for ReportSendModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(text) = self.text().map(str::to_string) else {
            return div().into_any_element();
        };

        self.focus.apply_pending(window, cx);

        let theme = cx.theme();
        // Only a send settles the report: after "Save as file" the user
        // still has to decide, so every button stays.
        let settled = matches!(self.state, SendState::Sent);
        let offer = datatab_ui_windows::reports::offer(self.app_state.read(cx), cx);

        let body = div()
            .flex()
            .flex_col()
            .gap(Spacing::MD)
            .child(Text::body(datatab_i18n::t!("reports.send.lead")))
            .child(
                surface(SurfaceRole::Raised, cx)
                    .id("report-send-bytes")
                    .w_full()
                    .max_h(BYTES_MAX_HEIGHT)
                    .overflow_y_scrollbar()
                    .p_2()
                    .child(
                        div()
                            .text_size(FontSizes::SM)
                            .font_family(datatab_components::fonts::editor_family(cx))
                            .text_color(theme.foreground)
                            .child(text),
                    ),
            )
            .child(
                Text::body(datatab_i18n::t!("reports.send.decline_note"))
                    .color(theme.muted_foreground),
            )
            // Reports are off: the Send button is gone, and this line says
            // which source turned it off, in the words of the settings doc.
            .when_some(reports::off_reason(offer), |el, reason| {
                el.child(
                    div()
                        .id("report-send-off")
                        .role(Role::Note)
                        .aria_label(reason.clone())
                        .child(Text::body(reason).color(theme.muted_foreground)),
                )
            })
            .when_some(self.status_line(), |el, line| {
                el.child(
                    div()
                        .id("report-send-status")
                        .child(Text::body(line).color(theme.muted_foreground)),
                )
            });

        let footer = self.render_footer(offer, settled, cx);

        Modal::new(datatab_i18n::t!("reports.send.title"))
            .id("report-send-window")
            .body(body)
            .footer(footer)
            .icon(datatab_components::icons::AppIcon::Info)
            .width(px(720.0))
            .focus_handle(self.focus.handle())
            .scroll_with_keys(&self.scroll)
            // Escape and the close button leave the report where it is: the
            // user answered nothing, so the next start may ask again.
            .on_close({
                let entity = cx.entity().downgrade();
                move |_, cx| {
                    entity
                        .update(cx, |this, cx| this.close(false, cx))
                        .log_err();
                }
            })
            // Enter runs the default action, and the default action is not to
            // send.
            .on_confirm({
                let entity = cx.entity().downgrade();
                move |_, cx| {
                    entity.update(cx, |this, cx| this.decline(cx)).log_err();
                }
            })
            .into_any_element()
    }
}

impl ReportSendModal {
    /// One line about the send or save that ran, or the last failure, with
    /// the note of a send that landed but could not be recorded.
    fn status_line(&self) -> Option<String> {
        let line = self.state_line();

        match (&line, &self.note) {
            (Some(line), Some(note)) => Some(format!("{line} {note}")),
            (Some(line), None) => Some(line.clone()),
            (None, Some(note)) => Some(note.clone()),
            (None, None) => None,
        }
    }

    fn state_line(&self) -> Option<String> {
        match &self.state {
            SendState::Idle => None,
            SendState::Sending => Some(datatab_i18n::t!("reports.send.in_progress")),
            SendState::Saving => Some(datatab_i18n::t!("reports.save.in_progress")),
            SendState::Sent => Some(datatab_i18n::t!("reports.send.done")),
            SendState::Failed(message) => Some(message.clone()),
            SendState::Saved(path) => Some(datatab_i18n::t!(
                "reports.save.done",
                path = path.display().to_string()
            )),
        }
    }

    /// The footer: the safe action last, as the primary action of the modal.
    fn render_footer(
        &self,
        offer: SwitchState,
        settled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // One action at a time: a second press while the first is in flight
        // would post the same report twice, or open two save dialogs.
        let busy = self.is_busy();

        let save_click = cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
            this.save_as_file(cx);
        });
        let send_click = cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
            this.send(cx);
        });
        let decline_click = cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
            this.decline(cx);
        });
        let close_click = cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
            this.close(!this.settled_by_state(), cx);
        });

        let mut footer = div().flex().items_center().gap(Spacing::SM);

        if settled {
            // The buttons did their work; the window has nothing left to do
            // but close.
            return footer.child(
                Button::new(
                    "report-send-close",
                    datatab_i18n::t!("reports.action.close"),
                )
                .primary()
                .tab_stop(true)
                .on_click(close_click),
            );
        }

        footer = footer.child(
            Button::new("report-send-save", datatab_i18n::t!("reports.action.save"))
                .tab_stop(true)
                .disabled(busy)
                .on_click(save_click),
        );

        // The off switch hides the send button, not the window: "Save as file"
        // writes a local file and needs no consent for anything leaving the
        // machine.
        if offer.is_enabled() {
            footer = footer.child(
                Button::new("report-send-send", datatab_i18n::t!("reports.action.send"))
                    .tab_stop(true)
                    .disabled(busy)
                    .on_click(send_click),
            );
        }

        footer.child(
            Button::new(
                "report-send-decline",
                datatab_i18n::t!("reports.action.dont_send"),
            )
            .primary()
            .tab_stop(true)
            .disabled(busy)
            .on_click(decline_click),
        )
    }

    /// Whether the current state already settled the report.
    fn settled_by_state(&self) -> bool {
        matches!(self.state, SendState::Sent)
    }

    /// Whether "Save as file" is writing.
    fn is_saving(&self) -> bool {
        matches!(self.state, SendState::Saving)
    }

    /// Whether an action is in flight, so no second one may start.
    fn is_busy(&self) -> bool {
        self.is_sending() || self.is_saving()
    }
}
