//! The notice the next start shows for a report a panic left.
//!
//! ADR 0008 § 1: "Next start: a non-blocking notice offers the report. About
//! lists pending reports." It draws the product's banner primitive
//! (`BannerBlock`) at the top of the document area, so it never blocks the
//! window and disappears on any answer. Its two answers are the ones the
//! window offers — look at the report, or do not send it — and the close
//! button leaves the question open for the next start.

use std::collections::HashSet;
use std::path::PathBuf;

use datatab_components::controls::Button;
use datatab_components::primitives::{BannerBlock, BannerVariant};
use datatab_components::tokens::Spacing;
use datatab_diagnostics::{PendingReport, mark_decided, readable_pending_reports};
use datatab_ui_base::{AppStateChanged, AppStateEntity};
use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Window, div};

use datatab_ui_windows::reports;

/// What the notice was answered with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportNoticeEvent {
    /// Show the report in the send window.
    Review(PathBuf),
    /// The user will not send it: the answer is recorded and the notice goes.
    Declined,
    /// Closed without an answer; the next start may ask again.
    Dismissed,
}

/// The startup offer of one pending report.
pub struct ReportNotice {
    app_state: Entity<AppStateEntity>,
    pending: Option<PendingReport>,
    /// Reports this session is done with: reviewed, declined or closed with
    /// "Later". Read in addition to the marker on disk, so a refresh — after a
    /// decision, or after the setting changed — never brings the banner back.
    hidden: HashSet<PathBuf>,
    /// The setting the last read saw. An app-state change that leaves it alone
    /// needs no new read of the folder.
    last_setting: Option<bool>,
    _app_state_subscription: gpui::Subscription,
}

impl ReportNotice {
    pub fn new(app_state: Entity<AppStateEntity>, cx: &mut Context<Self>) -> Self {
        let mut notice = Self {
            _app_state_subscription: cx
                .subscribe(&app_state, |this, _, _: &AppStateChanged, cx| {
                    this.refresh_if_setting_changed(cx)
                }),
            app_state,
            pending: None,
            hidden: HashSet::new(),
            last_setting: None,
        };
        notice.refresh(cx);
        notice
    }

    /// The app state the notice reads its folder and the switch from, for a
    /// window that drives it.
    pub fn app_state(&self) -> &Entity<AppStateEntity> {
        &self.app_state
    }

    /// The report this notice offers, if any.
    pub fn pending(&self) -> Option<&PendingReport> {
        self.pending.as_ref()
    }

    pub fn is_visible(&self) -> bool {
        self.pending.is_some()
    }

    /// The app-state change hook: reads the folder again only when the
    /// setting it depends on moved.
    ///
    /// `AppStateChanged` fires for every saved profile, task and error, and
    /// reading the folder each time would put file reads on the thread that
    /// draws. Reports off hides the banner at once, because the offer is
    /// recomputed without touching the folder.
    fn refresh_if_setting_changed(&mut self, cx: &mut Context<Self>) {
        let setting = self
            .app_state
            .read(cx)
            .general_settings()
            .send_error_reports;

        if self.last_setting == Some(setting) {
            return;
        }

        self.refresh(cx);
    }

    /// Re-reads the pending folder and the off switch.
    ///
    /// The oldest report the user never answered is the one offered: the
    /// others wait in Settings > About, so the notice stays one line. A report
    /// this session is done with is never offered again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let state = self.app_state.read(cx);
        let offered = reports::offer(state, cx).is_enabled();
        self.last_setting = Some(state.general_settings().send_error_reports);

        self.pending = if offered {
            state
                .reports_pending_dir()
                .map(readable_pending_reports)
                .unwrap_or_default()
                .into_iter()
                .find(|report| !report.decided && !self.hidden.contains(&report.path))
        } else {
            None
        };
        cx.notify();
    }

    /// "Review and send…": asks the workspace to show the report.
    ///
    /// The report leaves the notice for the rest of the session: the user saw
    /// it, and closing the window without an answer must not bring the banner
    /// back.
    pub fn review(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };

        self.hidden.insert(pending.path.clone());
        cx.emit(ReportNoticeEvent::Review(pending.path));
        cx.notify();
    }

    /// Answers the notice with "Don't send": the file stays and nothing asks
    /// about it again.
    pub fn decline(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };

        if let Err(error) = mark_decided(&pending.path) {
            log::warn!(
                target: "datatab_ui::diagnostics",
                "decision not recorded: {error}"
            );
        }

        self.hidden.insert(pending.path);
        cx.emit(ReportNoticeEvent::Declined);
        cx.notify();
    }

    /// Closes the notice without answering.
    ///
    /// The report keeps its place on disk for the next start; this session
    /// does not offer it again, so every `AppStateChanged` cannot bring the
    /// banner back under the user.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };

        self.hidden.insert(pending.path);
        cx.emit(ReportNoticeEvent::Dismissed);
        cx.notify();
    }
}

impl EventEmitter<ReportNoticeEvent> for ReportNotice {}

impl Render for ReportNotice {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending.is_none() {
            return div().into_any_element();
        }

        let message = datatab_i18n::t!("reports.notice.body");

        let actions = div()
            .flex()
            .items_center()
            .gap(Spacing::XS)
            .child(
                Button::new(
                    "report-notice-review",
                    datatab_i18n::t!("reports.notice.review"),
                )
                .tab_stop(true)
                .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                    this.review(cx);
                })),
            )
            .child(
                Button::new(
                    "report-notice-decline",
                    datatab_i18n::t!("reports.action.dont_send"),
                )
                .tab_stop(true)
                .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                    this.decline(cx);
                })),
            )
            .child(
                Button::new(
                    "report-notice-dismiss",
                    datatab_i18n::t!("reports.notice.dismiss"),
                )
                .tab_stop(true)
                .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                    this.dismiss(cx);
                })),
            );

        div()
            .id("report-notice")
            .px(Spacing::SM)
            .pb(Spacing::XS)
            .child(
                BannerBlock::new(
                    BannerVariant::Info,
                    datatab_i18n::t!("reports.notice.title"),
                )
                .id("report-notice-banner")
                .with_body(message)
                .with_actions(actions),
            )
            .into_any_element()
    }
}
