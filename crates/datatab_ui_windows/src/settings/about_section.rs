use std::collections::HashSet;
use std::path::PathBuf;

use super::layout;
use super::{SettingsSection, SettingsSectionId};
use crate::reports;
use datatab_app::keymap::Modifiers;
use datatab_components::controls::Button;
use datatab_components::primitives::{SurfaceRole, Text, surface};
use datatab_components::tokens::{FontSizes, Spacing};
use datatab_core::LogErr;
use datatab_diagnostics::{
    DeleteError, ErrorReport, PendingReport, SentToken, SentTokenLookup, delete_sent_report,
    mark_decided, readable_pending_reports, sent_token,
};
use datatab_ui_base::AppStateEntity;
use datatab_ui_base::SaveTargetProvider;
use datatab_ui_base::keymap::key_chord_from_gpui;
use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::scroll::ScrollableElement;

const CONTACT_ADDRESS: &str = "me@pcplab.me";
const CONTACT_URL: &str = "mailto:me@pcplab.me";
const SOURCE_URL: &str = "https://github.com/patcharp/datatab";

/// Tallest the preview of one report's bytes grows before it scrolls.
const REPORT_PREVIEW_MAX_HEIGHT: Pixels = px(220.0);

/// One pending report as About lists it.
struct PendingRow {
    path: PathBuf,
    /// The exact bytes of the file: what the window shows and "Send" posts.
    bytes: Vec<u8>,
    /// The text of those bytes, so the preview is the file itself.
    text: String,
    /// `panic` or `error`, or the file name when the file does not parse.
    kind: String,
    /// UTC hour the report was created, or the file name as a fallback.
    created_hour: String,
    /// The user already answered this report, so it is shown read-only: no
    /// bytes to send, no second Send.
    decided: bool,
    /// The delete token kept beside the file by a send that landed. Only a
    /// report with one can be removed from the server from here.
    token: Option<SentToken>,
    /// The token file exists and cannot be read. It is treated as no token,
    /// never as "delete anyway", and the page says so once.
    token_unreadable: Option<String>,
}

impl PendingRow {
    fn from_report(report: PendingReport) -> Self {
        let parsed = ErrorReport::from_json(&report.bytes).ok();
        let fallback = report
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();

        let (kind, created_hour) = match parsed {
            Some(report) => (
                report.kind.as_str().to_string(),
                report.created_hour.clone(),
            ),
            None => (fallback.clone(), fallback),
        };

        let (token, token_unreadable) = match sent_token(&report.path) {
            SentTokenLookup::Found(token) => (Some(token), None),
            SentTokenLookup::Missing => (None, None),
            SentTokenLookup::Unreadable(reason) => (None, Some(reason)),
        };

        Self {
            text: String::from_utf8_lossy(&report.bytes).into_owned(),
            path: report.path,
            bytes: report.bytes,
            kind,
            created_hour,
            decided: report.decided,
            token,
            token_unreadable,
        }
    }
}

/// One thing the keyboard can reach on the page, in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AboutTarget {
    Contact,
    Source,
    Report { row: usize, action: ReportAction },
}

/// What a report row offers: the same three actions, in the order the send
/// window's footer draws them, with "Don't send" last. A report that was
/// already sent offers one action instead: delete it from the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReportAction {
    Save,
    Send,
    Decline,
    Delete,
}

impl ReportAction {
    /// The three actions of a report that was not answered yet, in reading
    /// order.
    const ALL: [Self; 3] = [Self::Save, Self::Send, Self::Decline];
}

pub(super) struct AboutSection {
    app_state: Entity<AppStateEntity>,
    content_focused: bool,
    /// Index into [`Self::targets`]. Reading order: the two links, then the
    /// three actions of each pending report.
    selected: usize,
    rows: Vec<PendingRow>,
    /// Whether reports may be offered, read when the section is built and
    /// after every action.
    offer: datatab_diagnostics::SwitchState,
    /// One line about the last action, under the list.
    status: Option<String>,
    /// Test seam for "Save as file": production leaves it unset and gets the
    /// native dialog.
    save_provider: Option<SaveTargetProvider>,
    /// A "Save as file" is writing. Its button is disabled until it lands, so
    /// two quick presses cannot open two dialogs or write the file twice.
    saving: bool,
    /// How one "Delete from the server" attempt is made: production removes
    /// the stored report, a test substitutes a function.
    deleter: reports::ReportDeleter,
    /// A delete is in flight. Its button is disabled until the answer lands,
    /// so two quick presses cannot send the same token twice.
    deleting: bool,
    /// Reports whose token file could not be read, so the page says it once
    /// instead of on every visit.
    reported_token_problems: HashSet<PathBuf>,
}

impl AboutSection {
    pub(super) fn new(app_state: Entity<AppStateEntity>, cx: &mut Context<Self>) -> Self {
        let mut section = Self {
            app_state,
            content_focused: false,
            selected: 0,
            rows: Vec::new(),
            offer: datatab_diagnostics::SwitchState::Enabled,
            status: None,
            save_provider: None,
            saving: false,
            deleter: reports::default_deleter(),
            deleting: false,
            reported_token_problems: HashSet::new(),
        };
        section.reload(cx);

        section
    }

    /// Test seam: answer "Delete from the server" without a connection.
    #[cfg(test)]
    pub(super) fn with_deleter(mut self, deleter: reports::ReportDeleter) -> Self {
        self.deleter = deleter;
        self
    }

    /// Test seam: answer "Save as file" without a native dialog.
    #[cfg(test)]
    pub(super) fn with_save_provider(mut self, provider: SaveTargetProvider) -> Self {
        self.save_provider = Some(provider);
        self
    }

    /// Test seam: list the reports of `dir` instead of the app's folder.
    #[cfg(test)]
    pub(super) fn with_pending_dir(
        mut self,
        dir: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> Self {
        self.app_state.update(cx, |state, _| {
            state.set_reports_pending_dir(Some(dir.to_path_buf()));
        });
        self.reload(cx);
        self
    }

    /// Re-reads the pending reports and the switch. Called when the section is
    /// built and after every action, never during a render.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let pending = self
            .app_state
            .read(cx)
            .reports_pending_dir()
            .map(readable_pending_reports)
            .unwrap_or_default();

        self.rows = pending.into_iter().map(PendingRow::from_report).collect();

        self.report_unreadable_tokens();
        self.offer = reports::offer(self.app_state.read(cx), cx);
        self.selected = self.selected.min(self.targets().len().saturating_sub(1));
        cx.notify();
    }

    /// Says once, per report, that its delete token could not be read.
    ///
    /// The row already treats a damaged token file as no token; this is the
    /// line that tells the user why the button is gone. A report whose token
    /// reads again leaves the set, so a later corruption is reported again.
    fn report_unreadable_tokens(&mut self) {
        let unreadable: HashSet<PathBuf> = self
            .rows
            .iter()
            .filter(|row| row.token_unreadable.is_some())
            .map(|row| row.path.clone())
            .collect();

        for row in &self.rows {
            let Some(reason) = row.token_unreadable.as_ref() else {
                continue;
            };

            if self.reported_token_problems.insert(row.path.clone()) {
                log::warn!(
                    target: "datatab_ui_windows::settings",
                    "delete token not readable at {}: {reason}",
                    row.path.display()
                );
            }
        }

        self.reported_token_problems
            .retain(|path| unreadable.contains(path));
    }

    /// The actions one report row offers, in reading order.
    ///
    /// A report that was not answered offers the send window's three actions,
    /// minus Send while reports are off. A report that left the machine has
    /// nothing left to send; its one action removes it from the server, and
    /// only while its token is still beside the file. The off switch does not
    /// reach that action: deleting removes data and sends none.
    fn actions_of(&self, pending: &PendingRow) -> Vec<ReportAction> {
        if pending.decided {
            return if pending.token.is_some() {
                vec![ReportAction::Delete]
            } else {
                Vec::new()
            };
        }

        ReportAction::ALL
            .into_iter()
            .filter(|action| *action != ReportAction::Send || self.offer.is_enabled())
            .collect()
    }

    /// Every target of the page, in reading order.
    ///
    /// A report the user already answered contributes only its delete action,
    /// when it has a token: the ring cannot reach a Send that would upload a
    /// report the user already settled, and Enter cannot run one.
    fn targets(&self) -> Vec<AboutTarget> {
        let mut targets = vec![AboutTarget::Contact, AboutTarget::Source];

        for (row, pending) in self.rows.iter().enumerate() {
            for action in self.actions_of(pending) {
                targets.push(AboutTarget::Report { row, action });
            }
        }

        targets
    }

    fn selected_target(&self) -> Option<AboutTarget> {
        self.targets().get(self.selected).copied()
    }

    /// Moves the selection by `step`, wrapping at both ends.
    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.targets().len() as isize;

        if count == 0 {
            return;
        }

        self.selected = (self.selected as isize + step).rem_euclid(count) as usize;
        cx.notify();
    }

    fn activate_selected(&mut self, cx: &mut Context<Self>) {
        match self.selected_target() {
            Some(AboutTarget::Contact) => cx.open_url(CONTACT_URL),
            Some(AboutTarget::Source) => cx.open_url(SOURCE_URL),
            Some(AboutTarget::Report { row, action }) => self.run_action(row, action, cx),
            None => {}
        }
    }

    fn run_action(&mut self, row: usize, action: ReportAction, cx: &mut Context<Self>) {
        let Some(pending) = self.rows.get(row) else {
            return;
        };

        match action {
            ReportAction::Decline => {
                if let Err(error) = mark_decided(&pending.path) {
                    self.status = Some(datatab_i18n::t!(
                        "reports.about.decide_failed",
                        error = error.to_string()
                    ));
                } else {
                    self.status = Some(datatab_i18n::t!("reports.about.decided_now"));
                }
                self.reload(cx);
            }
            ReportAction::Save => self.save_row(row, cx),
            ReportAction::Send => self.review_row(row, cx),
            ReportAction::Delete => self.delete_row(row, cx),
        }
    }

    /// "Delete from the server": removes the stored copy, then the local
    /// report, its decision marker and its token.
    ///
    /// The off switch is not read here: deleting removes data, it sends none.
    /// Only a row with a token draws this action, so the token check happens
    /// once, inside [`delete_sent_report`], which refuses a report without one.
    fn delete_row(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.deleting {
            return;
        }

        let Some(pending) = self.rows.get(row) else {
            return;
        };

        let path = pending.path.clone();
        let deleter = self.deleter.clone();

        self.deleting = true;
        self.status = Some(datatab_i18n::t!("reports.about.deleting"));

        let delete = cx
            .background_executor()
            .spawn(async move { delete_sent_report(&path, |sent| deleter(sent)) });

        cx.spawn(async move |this, cx| {
            let outcome = delete.await;

            this.update(cx, |section, cx| {
                section.deleting = false;
                section.status = Some(match outcome {
                    Ok(()) => datatab_i18n::t!("reports.about.deleted"),
                    // The server deleted it and a local file stayed: saying
                    // "not deleted" and "everything stays" here would be two
                    // lies about one half-success.
                    Err(DeleteError::Local(error)) => {
                        datatab_i18n::t!("reports.about.deleted_but_local", error = error)
                    }
                    Err(error) => {
                        datatab_i18n::t!("reports.about.delete_failed", error = error.to_string())
                    }
                });
                section.reload(cx);
            })
            .log_err();
        })
        .detach();

        cx.notify();
    }

    /// "Send": opens the send window on this report, exactly as the startup
    /// notice does.
    ///
    /// The window shows every byte and needs a second press to upload, so one
    /// keystroke here can never send anything. This crate cannot build the
    /// window — it lives in the workspace — so the request goes through the
    /// app state, the same path an error toast's "Send report…" takes.
    fn review_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(pending) = self.rows.get(row) else {
            return;
        };

        let path = pending.path.clone();
        self.status = Some(datatab_i18n::t!("reports.about.review_in_progress"));

        self.app_state.update(cx, |state, cx| {
            state.request_report_window(datatab_ui_base::ReportWindowRequest::Pending(path), cx);
        });
        cx.notify();
    }

    fn save_row(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }

        let Some(pending) = self.rows.get(row) else {
            return;
        };

        let name = reports::suggested_file_name(&pending.kind, &report_slug(&pending.path));
        let bytes = pending.bytes.clone();
        let provider = self.save_provider.clone();

        self.saving = true;
        self.status = Some(datatab_i18n::t!("reports.save.in_progress"));

        cx.spawn(async move |this, cx| {
            let outcome = reports::save_report_copy(provider, &name, &bytes).await;

            this.update(cx, |section, cx| {
                section.saving = false;
                section.status = Some(match outcome {
                    Ok(Some(path)) => {
                        datatab_i18n::t!("reports.save.done", path = path.display().to_string())
                    }
                    Ok(None) => datatab_i18n::t!("reports.save.cancelled"),
                    Err(error) => datatab_i18n::t!("reports.save.failed", error = error),
                });
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }
}

impl SettingsSection for AboutSection {
    fn section_id(&self) -> SettingsSectionId {
        SettingsSectionId::About
    }

    fn handle_key_event(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.content_focused {
            return;
        }

        let chord = key_chord_from_gpui(&event.keystroke);

        match (chord.key.as_str(), chord.modifiers) {
            ("j" | "down" | "tab", modifiers) if modifiers == Modifiers::none() => {
                self.move_selection(1, cx);
            }
            ("k" | "up", modifiers) if modifiers == Modifiers::none() => {
                self.move_selection(-1, cx);
            }
            ("tab", modifiers) if modifiers == Modifiers::shift() => {
                self.move_selection(-1, cx);
            }
            ("enter" | "space", modifiers) if modifiers == Modifiers::none() => {
                self.activate_selected(cx);
            }
            _ => {}
        }
    }

    fn focus_in(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.content_focused = true;
        // The line about the last action belongs to the visit that made it.
        self.status = None;
        // The window the Send action opens decides in the workspace, so the
        // page reads the folder again when the user comes back to it.
        self.reload(cx);
    }

    fn focus_out(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.content_focused = false;
        cx.notify();
    }
}

/// The `report_id` at the end of a pending file's name (`<ms>-<n>-<uuid>.json`).
fn report_slug(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    name.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(name)
        .rsplit_once('-')
        .map(|(_, slug)| slug.to_string())
        .unwrap_or_else(|| name.to_string())
}

impl Render for AboutSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const VERSION: &str = env!("CARGO_PKG_VERSION");
        const AUTHORS: &str = env!("CARGO_PKG_AUTHORS");
        const LICENSE: &str = env!("CARGO_PKG_LICENSE");

        #[cfg(debug_assertions)]
        const PROFILE: &str = "debug";
        #[cfg(not(debug_assertions))]
        const PROFILE: &str = "release";

        // Rendered through `img` (full color) rather than the monochrome icon
        // path so the channel-specific mark — including nightly — shows in color.
        let mark_path = match datatab_core::ReleaseChannel::current() {
            datatab_core::ReleaseChannel::Nightly => "branding/nightly/mark-256.png",
            _ => "branding/stable/mark-256.png",
        };

        let targets = self.targets();
        let selected = targets.get(self.selected).copied();
        let contact_focused = self.content_focused && selected == Some(AboutTarget::Contact);
        let source_focused = self.content_focused && selected == Some(AboutTarget::Source);
        let author_name = author_display_name(AUTHORS);
        let license_display = LICENSE.replace(" OR ", " and ");
        let copyright_line = crate::labels::about_copyright(author_name);
        let license_line = crate::labels::about_license(&license_display);
        let derived_notice = datatab_i18n::t!("settings.about.derived_notice");

        // Built before the theme borrow: each row registers its own click
        // handlers, which need the context mutably.
        let mut report_rows: Vec<AnyElement> = Vec::with_capacity(self.rows.len());
        for row in 0..self.rows.len() {
            let focused = self.content_focused
                && matches!(selected, Some(AboutTarget::Report { row: r, .. }) if r == row);
            report_rows.push(self.render_report_row(row, focused, selected, cx));
        }

        let theme = cx.theme();

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(datatab_components::composites::page_header(
                datatab_i18n::t!("settings.about.title"),
                datatab_i18n::t!("settings.about.subtitle"),
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px(crate::tokens::SettingsMetrics::BODY_PADDING_X)
                    .py(datatab_components::tokens::Spacing::LG)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(img(mark_path).size(px(65.0)))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(Text::heading("DataTab"))
                                            .child(
                                                Text::code(format!("{} ({})", VERSION, PROFILE))
                                                    .muted_foreground(),
                                            ),
                                    ),
                            )
                            .child(
                                // items_baseline + Body-wrapped fillers keeps
                                // the link and the address beside it on the
                                // same baseline; bare &str children sit on a
                                // different metric and look pulled up.
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_1()
                                    .child(layout::cursor_ring(
                                        contact_focused,
                                        div()
                                            .id("about-link-contact")
                                            .cursor_pointer()
                                            .hover(|d| d.underline())
                                            .on_click(|_, _, cx| {
                                                cx.open_url(CONTACT_URL);
                                            })
                                            .child(
                                                Text::body(datatab_i18n::t!(
                                                    "settings.about.report_bug"
                                                ))
                                                .color(theme.link),
                                            ),
                                        cx,
                                    ))
                                    // The platform gives no answer when no mail
                                    // client opens, so the address stays on the
                                    // page for the user to read and type.
                                    .child(
                                        Text::body(format!("({CONTACT_ADDRESS})"))
                                            .color(theme.muted_foreground),
                                    ),
                            )
                            .child(layout::cursor_ring(
                                source_focused,
                                div()
                                    .id("about-link-source")
                                    .cursor_pointer()
                                    .hover(|d| d.underline())
                                    .on_click(|_, _, cx| cx.open_url(SOURCE_URL))
                                    .child(
                                        Text::body(datatab_i18n::t!("settings.about.view_source"))
                                            .color(theme.link),
                                    ),
                                cx,
                            ))
                            .child(Text::body(copyright_line))
                            .child(Text::body(license_line))
                            .child(
                                div()
                                    .id("about-derived-notice")
                                    .role(Role::Note)
                                    .aria_label(derived_notice.clone())
                                    .child(Text::body(derived_notice)),
                            )
                            .child(self.render_pending_reports(report_rows, cx))
                            .child(
                                div()
                                    .mt_4()
                                    .pt_4()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(Text::body(datatab_i18n::t!(
                                        "settings.about.third_party_licenses"
                                    )))
                                    .child(
                                        Text::body(datatab_i18n::t!("settings.about.lucide"))
                                            .color(theme.muted_foreground),
                                    )
                                    .child(
                                        Text::body(datatab_i18n::t!("settings.about.simple_icons"))
                                            .color(theme.muted_foreground),
                                    ),
                            ),
                    ),
            )
    }
}

impl AboutSection {
    /// The pending-report list, its off-switch line and the last action's
    /// result.
    fn render_pending_reports(
        &self,
        rows: Vec<AnyElement>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let off_reason = reports::off_reason(self.offer);

        div()
            .id("about-pending-reports")
            .mt_4()
            .pt_4()
            .border_t_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap_2()
            .child(Text::body(datatab_i18n::t!("reports.about.title")))
            .when_some(off_reason, |el, reason| {
                // The same words the settings doc uses for the source that
                // turned reports off, so a missing button explains itself.
                el.child(
                    div()
                        .id("about-reports-off")
                        .role(Role::Note)
                        .aria_label(reason.clone())
                        .child(Text::body(reason).color(theme.muted_foreground)),
                )
            })
            .when(self.rows.is_empty(), |el| {
                el.child(
                    Text::body(datatab_i18n::t!("reports.about.empty"))
                        .color(theme.muted_foreground),
                )
            })
            .children(rows)
            .when_some(self.status.clone(), |el, status| {
                el.child(
                    div()
                        .id("about-reports-status")
                        .child(Text::body(status).color(theme.muted_foreground)),
                )
            })
    }

    /// One pending report: what it is, its exact bytes, and its three actions.
    ///
    /// A report the user already answered shows only what it is and the marker
    /// text: there is nothing left to send, so no action and no bytes.
    fn render_report_row(
        &self,
        row: usize,
        focused: bool,
        selected: Option<AboutTarget>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(pending) = self.rows.get(row) else {
            return div().into_any_element();
        };

        let identity = format!(
            "{} · {}",
            datatab_i18n::t!("reports.about.row", kind = pending.kind.clone()),
            pending.created_hour
        );

        if pending.decided {
            return self.render_answered_row(row, pending, &identity, focused, selected, cx);
        }

        // The buttons are built before the theme borrow: each registers its
        // own click handler, which needs the context mutably.
        let mut actions = div().flex().items_center().gap(Spacing::SM);

        for action in self.actions_of(pending) {
            actions = actions.child(self.render_action_button(row, action, focused, selected, cx));
        }

        let theme = cx.theme();

        div()
            .id(SharedString::from(format!("about-report-{row}")))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(theme.secondary)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(Text::body(identity).color(theme.foreground)),
            )
            .child(
                surface(SurfaceRole::Raised, cx)
                    .id(SharedString::from(format!("about-report-{row}-bytes")))
                    .w_full()
                    .max_h(REPORT_PREVIEW_MAX_HEIGHT)
                    .overflow_y_scrollbar()
                    .p_2()
                    .child(
                        div()
                            .text_size(FontSizes::SM)
                            .font_family(datatab_components::fonts::editor_family(cx))
                            .text_color(theme.foreground)
                            .child(pending.text.clone()),
                    ),
            )
            .child(actions)
            .into_any_element()
    }

    /// One report that was already answered: what it is, the marker, and its
    /// one remaining action when its delete token is still beside the file.
    ///
    /// A report with no token shows why instead of a button that could not
    /// work: it was sent before DataTab kept tokens, or its token file was
    /// removed.
    fn render_answered_row(
        &self,
        row: usize,
        pending: &PendingRow,
        identity: &str,
        focused: bool,
        selected: Option<AboutTarget>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let marker = datatab_i18n::t!("reports.about.decided");
        // Built before the theme borrow, for the reason the row above gives.
        let mut trailing = div().flex().items_center().gap(Spacing::SM);

        for action in self.actions_of(pending) {
            trailing =
                trailing.child(self.render_action_button(row, action, focused, selected, cx));
        }

        if pending.token.is_none() {
            let note = datatab_i18n::t!("reports.about.no_token", address = CONTACT_ADDRESS);

            trailing = trailing.child(
                div()
                    .id(SharedString::from(format!("about-report-{row}-no-token")))
                    .role(Role::Note)
                    .aria_label(note.clone())
                    .child(Text::body(note).muted_foreground()),
            );
        }

        trailing = trailing.child(
            div()
                .id(SharedString::from(format!("about-report-{row}-decided")))
                .role(Role::Note)
                .aria_label(marker.clone())
                .child(Text::body(marker).muted_foreground()),
        );

        let theme = cx.theme();

        div()
            .id(SharedString::from(format!("about-report-{row}")))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(theme.secondary)
            .child(Text::body(identity.to_string()).color(theme.foreground))
            .child(trailing)
            .into_any_element()
    }

    /// One action button of a report row.
    ///
    /// Every press runs through [`Self::run_action`], the same path Enter
    /// takes on the ring, so the pointer and the keyboard do one thing.
    fn render_action_button(
        &self,
        row: usize,
        action: ReportAction,
        focused: bool,
        selected: Option<AboutTarget>,
        cx: &mut Context<Self>,
    ) -> Button {
        let is_selected = selected == Some(AboutTarget::Report { row, action });
        let label = match action {
            ReportAction::Decline => datatab_i18n::t!("reports.action.dont_send"),
            ReportAction::Save => datatab_i18n::t!("reports.action.save"),
            ReportAction::Send => datatab_i18n::t!("reports.action.send"),
            ReportAction::Delete => datatab_i18n::t!("reports.about.delete"),
        };
        let id = format!("about-report-{}-{}", row, report_action_slug(action));
        let button = if action == ReportAction::Decline {
            Button::new(id, label).primary()
        } else {
            Button::new(id, label)
        };

        button
            .focused(focused && is_selected)
            .disabled(self.saving || self.deleting)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.run_action(row, action, cx);
            }))
    }
}

/// The id fragment of one action, so tests and automation find the button.
fn report_action_slug(action: ReportAction) -> &'static str {
    match action {
        ReportAction::Decline => "decline",
        ReportAction::Save => "save",
        ReportAction::Send => "send",
        ReportAction::Delete => "delete",
    }
}

/// The name before the address in the first entry of `CARGO_PKG_AUTHORS`.
fn author_display_name(authors: &str) -> &str {
    authors.split('<').next().unwrap_or(authors).trim()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use datatab_components::accessibility::{accessible_name, assert_clickables_named};
    use datatab_diagnostics::{ErrorReport, ReportKind, is_decided};
    use datatab_storage::bootstrap::StorageRuntime;
    use datatab_ui_base::keyboard_coverage::FrameCapture;
    use datatab_ui_base::{
        AppStateEntity, ReportWindowRequest, SaveTargetOutcome, SaveTargetProvider,
    };
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

    use super::{AboutSection, CONTACT_ADDRESS, CONTACT_URL, author_display_name};
    use crate::labels::{about_copyright, about_license};

    const ABOUT_CATALOG_KEYS: &[&str] = &[
        "settings.about.title",
        "settings.about.subtitle",
        "settings.about.report_bug",
        "settings.about.view_source",
        "settings.about.copyright",
        "settings.about.license",
        "settings.about.derived_notice",
        "settings.about.third_party_licenses",
        "settings.about.lucide",
        "settings.about.simple_icons",
        "reports.about.title",
        "reports.about.empty",
        "reports.about.row",
        "reports.about.decided",
        "reports.about.decided_now",
        "reports.about.decide_failed",
        "reports.about.review_in_progress",
        "reports.about.delete",
        "reports.about.deleting",
        "reports.about.deleted",
        "reports.about.deleted_but_local",
        "reports.about.delete_failed",
        "reports.about.no_token",
        "reports.action.dont_send",
        "reports.action.save",
        "reports.action.send",
        "reports.off.setting",
        "reports.off.environment",
        "reports.off.managed_file",
        "reports.save.dialog_title",
        "reports.save.dialog_filter",
        "reports.save.in_progress",
        "reports.save.done",
        "reports.save.cancelled",
        "reports.save.failed",
        "reports.send.in_progress",
        "reports.send.done",
        "reports.send.failed",
        "reports.send.token_not_kept",
    ];

    #[test]
    fn settings_about_keys_resolve() {
        for key in ABOUT_CATALOG_KEYS {
            let value = datatab_i18n::t!(key, locale = "en");

            assert!(!value.is_empty(), "key {key} resolved empty");
            assert_ne!(value, *key, "key {key} did not resolve");
            assert_ne!(
                value,
                format!("en.{key}"),
                "key {key} fell back to the raw en-qualified form"
            );
        }
    }

    #[test]
    fn settings_about_title_english_text() {
        let english = datatab_i18n::t!("settings.about.title", locale = "en");

        assert_eq!(english, "About");
    }

    #[test]
    fn about_copyright_embeds_author_name() {
        let en = about_copyright("Jane Doe");

        assert!(en.contains("Jane Doe"));
    }

    #[test]
    fn about_license_embeds_license_identifier() {
        let en = about_license("MIT and Apache-2.0");

        assert!(en.contains("MIT and Apache-2.0"));
    }

    #[test]
    fn the_copyright_line_names_pcplab() {
        let author = author_display_name(env!("CARGO_PKG_AUTHORS"));

        assert_eq!(author, "PCPLab");

        let line = datatab_i18n::translate_in("en", "settings.about.copyright")
            .replace("%{author}", author);

        assert!(line.contains("PCPLab"), "{line}");
        assert!(!line.contains("%{"), "{line}");
    }

    #[test]
    fn the_contact_link_is_a_mail_to_the_shown_address() {
        assert_eq!(CONTACT_URL, format!("mailto:{CONTACT_ADDRESS}"));
    }

    // -- The pending reports list ------------------------------------------

    fn app_state(cx: &mut TestAppContext) -> Entity<AppStateEntity> {
        cx.update(|cx| {
            cx.new(|_| {
                AppStateEntity::new_with_storage_runtime(
                    StorageRuntime::in_memory().expect("in-memory storage"),
                )
                .expect("test app state")
            })
        })
    }

    fn panic_report() -> ErrorReport {
        ErrorReport::new(ReportKind::Panic, "2026-11-23T05:00:00Z")
    }

    /// The section in a window of its own, over `dir`.
    fn open_section<'a>(
        cx: &'a mut TestAppContext,
        dir: &std::path::Path,
        build: impl FnOnce(AboutSection) -> AboutSection + 'static,
    ) -> (Entity<AboutSection>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);

        let app_state = app_state(cx);
        let holder: std::rc::Rc<std::cell::RefCell<Option<Entity<AboutSection>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let dir = dir.to_path_buf();

        let (_, window) = cx.add_window_view({
            let holder = holder.clone();
            move |window, cx| {
                let state = app_state.clone();
                let section = cx.new(move |cx| {
                    let build = build;
                    let section = AboutSection::new(state, cx).with_pending_dir(&dir, cx);
                    build(section)
                });
                holder.replace(Some(section.clone()));
                gpui_component::Root::new(section, window, cx)
            }
        });
        window.run_until_parked();

        let section = holder.borrow().clone().expect("the section was created");
        (section, window)
    }

    fn draw(window: &mut VisualTestContext, section: &Entity<AboutSection>) {
        let _ = section;
        window.update(|window, _| window.refresh());
        window.run_until_parked();
    }

    /// A pending report is listed with its exact bytes and its three actions,
    /// and the off switch line is absent while reports are offered.
    #[gpui::test]
    fn a_pending_report_is_listed_with_its_bytes_and_three_actions(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let report = panic_report();
        let path = datatab_diagnostics::write_pending_report(dir.path(), &report).expect("written");

        let (section, window) = open_section(cx, dir.path(), |section| section);
        draw(window, &section);

        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);

        for id in [
            "about-report-0-decline",
            "about-report-0-save",
            "about-report-0-send",
        ] {
            assert!(
                accessible_name(&frame, id).is_some(),
                "{id} must be on the page"
            );
        }
        assert_clickables_named(&frame);

        let shown = section.read_with(window, |section, _| {
            section
                .rows
                .first()
                .map(|row| row.text.clone())
                .expect("the row is listed")
        });
        assert_eq!(
            shown.as_bytes(),
            std::fs::read(&path).expect("the report is readable"),
            "the page shows the exact bytes of the file"
        );
        assert!(
            accessible_name(&frame, "about-reports-off").is_none(),
            "reports are offered, so no reason is shown"
        );
    }

    #[gpui::test]
    fn dont_send_marks_the_report_and_the_list_says_so(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = datatab_diagnostics::write_pending_report(dir.path(), &panic_report())
            .expect("written");

        let (section, window) = open_section(cx, dir.path(), |section| section);
        draw(window, &section);

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.run_action(0, super::ReportAction::Decline, cx);
            });
        });
        window.run_until_parked();

        assert!(path.is_file(), "declining keeps the file");
        assert!(is_decided(&path), "the decision is recorded");
        draw(window, &section);

        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);
        assert!(
            accessible_name(&frame, "about-report-0-decided").is_some(),
            "the row stays listed after it is answered, with the marker text"
        );
        let status = section
            .read_with(window, |section, _| section.status.clone())
            .expect("the page says what happened");
        assert!(
            status.contains("will not ask about this report again"),
            "{status}"
        );
    }

    #[gpui::test]
    fn save_writes_the_exact_bytes(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let report = panic_report();
        let path = datatab_diagnostics::write_pending_report(dir.path(), &report).expect("written");
        let bytes = std::fs::read(&path).expect("readable");

        let target = dir.path().join("copy.json");
        let picker: SaveTargetProvider = Arc::new({
            let target = target.clone();
            move |_request| {
                let target = target.clone();
                gpui::Task::ready(SaveTargetOutcome::Selected {
                    path: target,
                    used_fallback: false,
                })
            }
        });

        let (section, window) = open_section(cx, dir.path(), move |section| {
            section.with_save_provider(picker)
        });
        draw(window, &section);

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.run_action(0, super::ReportAction::Save, cx);
            });
        });
        window.run_until_parked();
        window.run_until_parked();

        assert_eq!(
            std::fs::read(&target).expect("the copy was written"),
            bytes,
            "the copy holds the bytes the page showed"
        );
    }

    /// The line about the last action belongs to the visit that made it:
    /// coming back to the page starts clean.
    #[gpui::test]
    fn opening_the_page_again_clears_the_last_line(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = datatab_diagnostics::write_pending_report(dir.path(), &panic_report())
            .expect("written");

        let (section, window) = open_section(cx, dir.path(), |section| section);
        draw(window, &section);
        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.run_action(0, super::ReportAction::Decline, cx);
            });
        });
        window.run_until_parked();
        assert!(
            section
                .read_with(window, |section, _| section.status.clone())
                .is_some(),
            "the last action leaves a line"
        );
        let _ = path;

        window.update(|window, cx| {
            section.update(cx, |section, cx| {
                crate::settings::SettingsSection::focus_in(section, window, cx);
            });
        });
        window.run_until_parked();

        assert!(
            section
                .read_with(window, |section, _| section.status.clone())
                .is_none(),
            "opening the page again clears the line"
        );
    }

    /// Two presses on "Save as file" before the first lands open one dialog:
    /// the row is disabled while a save is in flight.
    #[gpui::test]
    fn a_second_save_while_one_is_in_flight_opens_one_dialog(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        datatab_diagnostics::write_pending_report(dir.path(), &panic_report()).expect("written");

        let asked = Arc::new(std::sync::Mutex::new(0usize));
        let target = dir.path().join("copy.json");
        let picker: SaveTargetProvider = {
            let asked = asked.clone();
            Arc::new(move |_request| {
                *asked.lock().expect("picker calls") += 1;
                gpui::Task::ready(SaveTargetOutcome::Selected {
                    path: target.clone(),
                    used_fallback: false,
                })
            })
        };

        let (section, window) = open_section(cx, dir.path(), move |section| {
            section.with_save_provider(picker)
        });
        draw(window, &section);

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.run_action(0, super::ReportAction::Save, cx);
                section.run_action(0, super::ReportAction::Save, cx);
            });
        });
        window.run_until_parked();
        window.run_until_parked();

        assert_eq!(*asked.lock().expect("picker calls"), 1);
    }

    /// "Send" opens the send window on that report and posts nothing: the
    /// window shows every byte and needs its own press (ADR 0008 § 2).
    #[gpui::test]
    fn send_asks_for_the_window_and_posts_nothing(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = datatab_diagnostics::write_pending_report(dir.path(), &panic_report())
            .expect("written");

        let (section, window) = open_section(cx, dir.path(), |section| section);
        draw(window, &section);

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.run_action(0, super::ReportAction::Send, cx);
            });
        });
        window.run_until_parked();

        let app_state = section.read_with(window, |section, _| section.app_state.clone());
        let request = window
            .update(|_, cx| app_state.update(cx, |state, _| state.take_pending_report_window()));
        assert_eq!(
            request,
            Some(ReportWindowRequest::Pending(path.clone())),
            "Send must ask the workspace for the window, not post the report"
        );
        assert!(
            !is_decided(&path),
            "nothing was decided or sent from this page"
        );
        let status = section
            .read_with(window, |section, _| section.status.clone())
            .expect("the page says what it is doing");
        assert!(status.contains("Opening the report"), "{status}");
    }

    /// A declined report is read-only: no Send target, no Send button.
    #[gpui::test]
    fn a_declined_report_offers_no_send(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = datatab_diagnostics::write_pending_report(dir.path(), &panic_report())
            .expect("written");
        datatab_diagnostics::mark_decided(&path).expect("decided elsewhere");

        let (section, window) = open_section(cx, dir.path(), |section| section);
        draw(window, &section);

        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);

        for id in [
            "about-report-0-send",
            "about-report-0-save",
            "about-report-0-decline",
        ] {
            assert!(
                accessible_name(&frame, id).is_none(),
                "{id} must be gone once the report is answered"
            );
        }
        assert!(accessible_name(&frame, "about-report-0-decided").is_some());
        let targets = section.read_with(window, |section, _| section.targets());
        assert!(
            !targets
                .iter()
                .any(|target| matches!(target, super::AboutTarget::Report { .. })),
            "the ring must not reach an answered report: {targets:?}"
        );
    }

    /// The same after a send: the window marks it, and the row stops offering
    /// to upload it again.
    #[gpui::test]
    fn a_sent_report_offers_no_send(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = datatab_diagnostics::write_pending_report(dir.path(), &panic_report())
            .expect("written");
        datatab_diagnostics::mark_decided(&path).expect("the window recorded the send");

        let (section, window) = open_section(cx, dir.path(), |section| section);
        draw(window, &section);

        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);

        assert!(accessible_name(&frame, "about-report-0-send").is_none());
        assert!(accessible_name(&frame, "about-report-0-decided").is_some());
    }

    /// The off switch: the Send button is gone from every row, and the page
    /// says which source turned reports off.
    #[gpui::test]
    fn reports_off_removes_send_and_says_why(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        datatab_diagnostics::write_pending_report(dir.path(), &panic_report()).expect("written");

        let (section, window) = open_section(cx, dir.path(), |section| section);
        section.update(window, |section, cx| {
            let mut settings = section.app_state.read(cx).general_settings().clone();
            settings.send_error_reports = false;
            section.app_state.update(cx, |state, _| {
                state.update_general_settings(settings);
            });
            section.reload(cx);
        });
        draw(window, &section);

        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);

        assert!(
            accessible_name(&frame, "about-report-0-send").is_none(),
            "the off switch removes every send button"
        );
        assert!(
            accessible_name(&frame, "about-report-0-save").is_some(),
            "saving a local copy stays available"
        );
        let reason = accessible_name(&frame, "about-reports-off").expect("the reason is shown");
        assert!(
            reason.contains("Offer to send a report is off in Settings"),
            "{reason}"
        );
    }

    // -- Deleting a report that was sent ------------------------------------

    /// A report that was sent: the file, its decision marker, and the token
    /// the service handed back.
    fn sent_report(
        dir: &std::path::Path,
        delete_token: &str,
    ) -> (std::path::PathBuf, datatab_diagnostics::SubmittedReport) {
        let report = panic_report();
        let path = datatab_diagnostics::write_pending_report(dir, &report).expect("written");
        let submitted = datatab_diagnostics::SubmittedReport {
            report_id: report.report_id,
            delete_token: delete_token.to_string(),
        };
        datatab_diagnostics::write_sent_token(&path, &submitted).expect("token written");
        datatab_diagnostics::mark_decided(&path).expect("a sent report is decided");

        (path, submitted)
    }

    /// A deleter that records what it was handed and answers `reply` once.
    fn recording_deleter(
        reply: datatab_diagnostics::SendError,
    ) -> (
        crate::reports::ReportDeleter,
        Arc<std::sync::Mutex<Vec<datatab_diagnostics::SentToken>>>,
    ) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        let reply = Arc::new(std::sync::Mutex::new(Some(Err(reply))));
        let deleter: crate::reports::ReportDeleter = Arc::new(move |token| {
            record.lock().expect("deletes").push(token.clone());
            reply
                .lock()
                .expect("reply")
                .take()
                .expect("one delete per test")
        });

        (deleter, seen)
    }

    /// A deleter that records what it was handed and succeeds.
    fn succeeding_deleter() -> (
        crate::reports::ReportDeleter,
        Arc<std::sync::Mutex<Vec<datatab_diagnostics::SentToken>>>,
    ) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        let deleter: crate::reports::ReportDeleter = Arc::new(move |token| {
            record.lock().expect("deletes").push(token.clone());
            Ok(())
        });

        (deleter, seen)
    }

    /// "Delete from the server" reaches the service with the token from the
    /// file, and removes the report, its marker and its token on success.
    #[gpui::test]
    fn a_sent_report_is_deleted_with_its_token_and_leaves_nothing(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let (path, submitted) = sent_report(dir.path(), "tok-456");

        let (deleter, seen) = succeeding_deleter();
        let (section, window) =
            open_section(cx, dir.path(), move |section| section.with_deleter(deleter));
        draw(window, &section);

        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);
        let label = accessible_name(&frame, "about-report-0-delete").expect("the action is drawn");
        assert_eq!(label, "Delete from the server");
        assert_clickables_named(&frame);

        // The action is in the settings window's keyboard registry, so a user
        // without a pointer reaches it: the frame is checked against it.
        datatab_ui_base::keyboard_coverage::Coverage::new(crate::keyboard_coverage::SETTINGS)
            .assert_covered(&frame);

        let targets = section.read_with(window, |section, _| section.targets());
        assert!(
            targets.contains(&super::AboutTarget::Report {
                row: 0,
                action: super::ReportAction::Delete,
            }),
            "the keyboard ring reaches the delete: {targets:?}"
        );

        // Enter on the ring runs it, the same path a click takes.
        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.content_focused = true;
                section.selected = targets
                    .iter()
                    .position(|target| {
                        *target
                            == super::AboutTarget::Report {
                                row: 0,
                                action: super::ReportAction::Delete,
                            }
                    })
                    .expect("the target index");
                section.activate_selected(cx);
            });
        });
        window.run_until_parked();
        window.run_until_parked();

        assert_eq!(
            seen.lock().expect("deletes").clone(),
            vec![datatab_diagnostics::SentToken {
                report_id: submitted.report_id,
                delete_token: "tok-456".to_string(),
            }],
            "the service is asked with the id and the token from the file"
        );
        assert!(!path.is_file(), "the report is removed");
        assert!(!is_decided(&path), "the marker is removed");
        assert_eq!(
            datatab_diagnostics::sent_token(&path),
            datatab_diagnostics::SentTokenLookup::Missing,
            "the token is removed"
        );

        draw(window, &section);
        let frame = FrameCapture::observe(window);
        let frame = frame.frame(window);
        assert!(
            accessible_name(&frame, "about-report-0-delete").is_none(),
            "nothing is left to delete"
        );
        let status = section
            .read_with(window, |section, _| section.status.clone())
            .expect("the page says what happened");
        assert!(status.contains("deleted from the server"), "{status}");
    }

    /// A delete the service refused keeps every local file and says why.
    #[gpui::test]
    fn a_refused_delete_keeps_the_files_and_says_why(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let (path, _) = sent_report(dir.path(), "tok-456");

        let (deleter, seen) =
            recording_deleter(datatab_diagnostics::SendError::Status { status: 503 });
        let (section, window) =
            open_section(cx, dir.path(), move |section| section.with_deleter(deleter));
        draw(window, &section);

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.run_action(0, super::ReportAction::Delete, cx);
            });
        });
        window.run_until_parked();
        window.run_until_parked();

        assert_eq!(seen.lock().expect("deletes").len(), 1);
        assert!(path.is_file(), "the report stays");
        assert!(is_decided(&path), "the marker stays");
        assert!(
            matches!(
                datatab_diagnostics::sent_token(&path),
                datatab_diagnostics::SentTokenLookup::Found(_)
            ),
            "the token stays, so the delete can be tried again"
        );

        let status = section
            .read_with(window, |section, _| section.status.clone())
            .expect("the page says what happened");
        assert!(status.contains("503"), "{status}");
        assert!(status.contains("stays on this machine"), "{status}");

        draw(window, &section);
        let frame = FrameCapture::observe(window);
        let frame = frame.frame(window);
        assert!(
            accessible_name(&frame, "about-report-0-delete").is_some(),
            "the action stays for the next try"
        );
    }

    /// The service accepted the delete and a local file stayed: the line says
    /// that, instead of claiming the report was not deleted and that
    /// everything stays.
    #[gpui::test]
    fn a_delete_the_server_accepted_says_which_local_file_stayed(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let (path, _) = sent_report(dir.path(), "tok-456");

        let (deleter, seen) = succeeding_deleter();
        let (section, window) =
            open_section(cx, dir.path(), move |section| section.with_deleter(deleter));
        draw(window, &section);

        // The report path becomes a non-empty directory, which `remove_file`
        // refuses: the delete reaches the service and then fails here only.
        std::fs::remove_file(&path).expect("the report file is replaced");
        std::fs::create_dir(&path).expect("a directory at the report path");
        std::fs::write(path.join("kept.txt"), b"kept").expect("the directory is not empty");

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.run_action(0, super::ReportAction::Delete, cx);
            });
        });
        window.run_until_parked();
        window.run_until_parked();

        assert_eq!(seen.lock().expect("deletes").len(), 1);
        assert!(
            path.is_dir(),
            "the local file that stayed is still where it was"
        );
        assert_eq!(
            datatab_diagnostics::sent_token(&path),
            datatab_diagnostics::SentTokenLookup::Missing,
            "the token went with the delete the service accepted"
        );

        let status = section
            .read_with(window, |section, _| section.status.clone())
            .expect("the page says what happened");
        assert!(
            status.contains("deleted from the server, but its file stayed"),
            "{status}"
        );
        assert!(!status.contains("was not deleted"), "{status}");
        assert!(!status.contains("Everything stays"), "{status}");
    }

    /// Two presses before the first answer deletes once: the button is off
    /// while a delete is in flight.
    #[gpui::test]
    fn a_second_delete_while_one_is_in_flight_deletes_once(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let (_path, _) = sent_report(dir.path(), "tok-456");

        let (deleter, seen) = succeeding_deleter();
        let (section, window) =
            open_section(cx, dir.path(), move |section| section.with_deleter(deleter));
        draw(window, &section);

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.run_action(0, super::ReportAction::Delete, cx);
                section.run_action(0, super::ReportAction::Delete, cx);
            });
        });
        window.run_until_parked();
        window.run_until_parked();

        assert_eq!(
            seen.lock().expect("deletes").len(),
            1,
            "one press pair sends the token once"
        );
        // A second attempt would run after the first removed the token, so it
        // could only end in "no delete token is kept": the line must be the
        // success one, which is what proves the guard stopped it.
        let status = section
            .read_with(window, |section, _| section.status.clone())
            .expect("the page says what happened");
        assert!(status.contains("deleted from the server"), "{status}");
        assert!(!status.contains("was not deleted"), "{status}");
    }

    /// A report with no token shows why, not a button that could not work.
    #[gpui::test]
    fn a_report_without_a_token_shows_no_delete_action(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = datatab_diagnostics::write_pending_report(dir.path(), &panic_report())
            .expect("written");
        datatab_diagnostics::mark_decided(&path).expect("decided, sent before tokens were kept");

        let (section, window) = open_section(cx, dir.path(), |section| section);
        draw(window, &section);

        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);
        assert!(
            accessible_name(&frame, "about-report-0-delete").is_none(),
            "a report without a token must not offer a dead button"
        );
        let note = accessible_name(&frame, "about-report-0-no-token")
            .expect("the page says why: it was sent before tokens were kept");
        assert!(note.contains("No delete token is kept"), "{note}");
        assert!(note.contains(CONTACT_ADDRESS), "{note}");

        let targets = section.read_with(window, |section, _| section.targets());
        assert!(
            !targets.iter().any(|target| matches!(
                target,
                super::AboutTarget::Report {
                    action: super::ReportAction::Delete,
                    ..
                }
            )),
            "the ring must not reach a delete that cannot work: {targets:?}"
        );
    }

    /// A token file that cannot be read is no token, and the page says so
    /// once for that report.
    #[gpui::test]
    fn a_corrupt_token_file_is_no_token_and_is_said_once(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = datatab_diagnostics::write_pending_report(dir.path(), &panic_report())
            .expect("written");
        let mut token_path = path.as_os_str().to_owned();
        token_path.push(datatab_diagnostics::SENT_TOKEN_SUFFIX);
        std::fs::write(&token_path, b"not a token").expect("corrupt token file");
        datatab_diagnostics::mark_decided(&path).expect("decided");

        let (section, window) = open_section(cx, dir.path(), |section| section);
        draw(window, &section);

        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);
        assert!(
            accessible_name(&frame, "about-report-0-delete").is_none(),
            "a damaged token never offers a delete"
        );
        assert!(accessible_name(&frame, "about-report-0-no-token").is_some());

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.reload(cx);
                section.reload(cx);
            });
        });
        window.run_until_parked();

        assert_eq!(
            section.read_with(window, |section, _| section.reported_token_problems.len()),
            1,
            "one report, one line about its damaged token"
        );
        assert!(path.is_file(), "a damaged token deletes nothing");
        assert!(std::path::Path::new(&token_path).is_file());
    }

    /// Deleting removes data and sends none, so the off switch does not take
    /// the action away.
    #[gpui::test]
    fn the_off_switch_does_not_hide_delete(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let (_path, _) = sent_report(dir.path(), "tok-456");

        let (deleter, seen) = succeeding_deleter();
        let (section, window) =
            open_section(cx, dir.path(), move |section| section.with_deleter(deleter));
        section.update(window, |section, cx| {
            let mut settings = section.app_state.read(cx).general_settings().clone();
            settings.send_error_reports = false;
            section.app_state.update(cx, |state, _| {
                state.update_general_settings(settings);
            });
            section.reload(cx);
        });
        draw(window, &section);

        let capture = FrameCapture::observe(window);
        let frame = capture.frame(window);
        assert!(
            accessible_name(&frame, "about-reports-off").is_some(),
            "the page says reports are off"
        );
        assert!(
            accessible_name(&frame, "about-report-0-delete").is_some(),
            "deleting stays available while reports are off"
        );

        window.update(|_, cx| {
            section.update(cx, |section, cx| {
                section.run_action(0, super::ReportAction::Delete, cx);
            });
        });
        window.run_until_parked();
        window.run_until_parked();

        assert_eq!(seen.lock().expect("deletes").len(), 1);
    }
}
