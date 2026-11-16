//! `InspectorPanel` — a tabular live-view document that executes an
//! `InstanceInspectorQuery` against the connection and displays the result
//! as a data grid (e.g. process list, active sessions).
//!
//! Unlike `ChartDocument`, `InspectorPanel` issues queries with no time window.
//! The source context is always `ExecutionSourceContext::InstanceInspectorQuery`.

pub mod pane;

use super::DataGridPanel;
use super::data_grid_panel::DataGridEvent;
use super::handle::DocumentEvent;
use super::task_runner::DocumentTaskRunner;
use super::types::{DocumentId, DocumentState};
use crate::refresh::MIN_REFRESH_FLOOR_SECS;
use datatab_app::keymap::{Command, ContextId};
use datatab_components::controls::Button;
use datatab_components::icons::AppIcon;
use datatab_components::modals::{Modal, ModalFocus};
use datatab_components::primitives::{Icon, SurfaceRole, Text, overlay_bg, surface};
use datatab_components::result_panel::{ResultPanel, ViewHandle};
use datatab_components::tokens::{Radii, Spacing};
use datatab_core::{
    ExecutionContext, ExecutionSourceContext, LogErr, QueryRequest, QueryResult, RefreshPolicy,
    Value,
};
use datatab_ui_base::AppStateEntity;
use datatab_ui_base::AsyncUpdateResultExt;
use datatab_ui_base::user_error::{ErrorKind, UserFacingError, report_error_async};
use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Subscription, Task, Window};
use gpui_component::ActiveTheme;
use std::sync::Arc;
use uuid::Uuid;

/// A pending query result from the background task runner.
struct PendingResult {
    task_id: datatab_core::TaskId,
    result: Result<QueryResult, datatab_core::DbError>,
}

/// State held while the kill-confirmation modal is visible.
struct PendingKillConfirm {
    action_id: String,
    action_label: String,
    row_values: Vec<Value>,
}

/// Live tabular inspector panel tied to a specific instance-metric inspector.
///
/// Executes `InstanceInspectorQuery` against the profile's connection and renders
/// the result as a data grid. No time-window fields are used — inspectors always
/// reflect the live state of the database.
pub struct InspectorPanel {
    id: DocumentId,
    state: DocumentState,

    profile_id: Uuid,
    metric_id: String,

    result: Option<Arc<QueryResult>>,
    last_error: Option<String>,

    refresh_policy: RefreshPolicy,
    _timer: Option<Task<()>>,
    is_active_tab: bool,

    runner: DocumentTaskRunner,
    app_state: Entity<AppStateEntity>,
    pending_result: Option<PendingResult>,

    focus_handle: FocusHandle,

    /// `DataGridPanel` entity that owns the rendered result table.
    ///
    /// Instantiated lazily on the first render after a successful fetch, because
    /// `DataGridPanel::new_for_result` requires a `Window` reference (for its
    /// `InputState` fields) that is only available during `render()`.
    data_grid: Option<Entity<DataGridPanel>>,

    /// Buffered result waiting for the first render to create `data_grid`.
    ///
    /// Written by `apply_result` when no grid exists yet; consumed and cleared
    /// by `render()` which uses it to construct `DataGridPanel::new_for_result`.
    pending_grid_result: Option<Arc<QueryResult>>,

    /// Chrome-row host built lazily alongside `data_grid` on first render.
    pub(super) result_panel: Option<Entity<ResultPanel>>,

    /// Subscription kept alive while `data_grid` exists, routing
    /// `DataGridEvent::RowActionRequested` events to this panel.
    _data_grid_subscription: Option<Subscription>,

    /// State while the kill-confirmation modal is visible.
    pending_kill_confirm: Option<PendingKillConfirm>,

    /// Moves the keyboard into the kill confirmation and gives it back to
    /// the table when it closes.
    kill_confirm_focus: ModalFocus,

    /// Deferred first-fetch flag.
    ///
    /// Set on construction; drained on the first render pass after all parent
    /// subscriptions (e.g. the dashboard semaphore) have been installed. This
    /// guarantees the initial `ExecutionFinished` event is observed and the
    /// semaphore is correctly decremented.
    pending_initial_exec: bool,
}

impl EventEmitter<DocumentEvent> for InspectorPanel {}

impl InspectorPanel {
    pub fn new(
        profile_id: Uuid,
        metric_id: String,
        app_state: Entity<AppStateEntity>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut runner = DocumentTaskRunner::new(app_state.clone());
        runner.set_profile_id(profile_id);

        Self {
            id: DocumentId::new(),
            state: DocumentState::Clean,
            profile_id,
            metric_id,
            result: None,
            last_error: None,
            refresh_policy: RefreshPolicy::Manual,
            _timer: None,
            is_active_tab: true,
            runner,
            app_state,
            pending_result: None,
            focus_handle: cx.focus_handle(),
            data_grid: None,
            pending_grid_result: None,
            result_panel: None,
            _data_grid_subscription: None,
            pending_kill_confirm: None,
            kill_confirm_focus: ModalFocus::new(cx),
            pending_initial_exec: false,
        }
    }

    /// Sets the deferred-initial-exec flag so the first `request_reexec` fires
    /// from `render()` after all parent subscriptions have been installed.
    pub fn defer_initial_exec(&mut self) {
        self.pending_initial_exec = true;
    }

    pub fn id(&self) -> DocumentId {
        self.id
    }

    pub fn title(&self) -> String {
        self.metric_id.clone()
    }

    pub fn state(&self) -> DocumentState {
        self.state
    }

    pub fn can_close(&self) -> bool {
        true
    }

    pub fn connection_id(&self) -> Option<Uuid> {
        Some(self.profile_id)
    }

    /// The table's key context once the result has arrived, so the table
    /// keys and its context menu (with the row actions) work in the panel.
    pub fn active_context(&self, cx: &App) -> ContextId {
        match &self.data_grid {
            Some(grid) => grid.read(cx).active_context(cx),
            None => ContextId::Global,
        }
    }

    pub fn change_summary(&self, _cx: &App) -> Option<String> {
        None
    }

    pub fn refresh_policy(&self) -> RefreshPolicy {
        self.refresh_policy
    }

    pub fn set_refresh_policy(&mut self, policy: RefreshPolicy, cx: &mut Context<Self>) {
        self.refresh_policy = policy;
        self.update_timer(cx);
    }

    pub fn set_active_tab(&mut self, active: bool) {
        self.is_active_tab = active;
    }

    #[cfg(test)]
    pub(crate) fn is_active_tab(&self) -> bool {
        self.is_active_tab
    }

    fn auto_refresh_tick_allowed(&self, cx: &App) -> bool {
        let settings = self.app_state.read(cx).general_settings();

        if settings.auto_refresh_pause_on_error && self.state == DocumentState::Error {
            return false;
        }

        !settings.auto_refresh_only_if_visible || self.is_active_tab
    }

    /// Moves the keyboard to the table, or to the panel while the result has
    /// not arrived.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.data_grid {
            Some(grid) => grid.update(cx, |grid, cx| grid.focus_table(window, cx)),
            None => self.focus_handle.focus(window, cx),
        }
    }

    /// Runs a keymap command: Enter and Escape answer an open row-action
    /// confirmation, F5 fetches a fresh snapshot and every other command
    /// goes to the table, whose context menu lists the row actions (`m`).
    pub fn dispatch_command(
        &mut self,
        cmd: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.pending_kill_confirm.is_some() {
            match cmd {
                Command::Execute => self.confirm_kill_action(cx),
                Command::Cancel => self.cancel_kill_action(cx),
                _ => return false,
            }
            return true;
        }

        if cmd == Command::RefreshSchema {
            self.request_reexec(cx);
            return true;
        }

        match &self.data_grid {
            Some(grid) => grid.update(cx, |grid, cx| grid.dispatch_command(cmd, window, cx)),
            None => false,
        }
    }

    /// The panel's actions for the pane-actions menu: a fresh snapshot. The
    /// row actions are in the table's own context menu (`m`).
    pub(crate) fn pane_actions(&self) -> Vec<crate::pane::PaneAction> {
        vec![
            crate::pane::PaneAction::command(
                "inspector-refresh",
                datatab_i18n::t!("document.chart.pane_actions.refresh"),
                Command::RefreshSchema,
                ContextId::Results,
            )
            .icon(AppIcon::RefreshCcw),
        ]
    }

    pub fn metric_id(&self) -> &str {
        &self.metric_id
    }

    /// Shows `result` as if a fetch had returned it; the table is built on
    /// the next render.
    #[cfg(test)]
    pub(crate) fn show_result_for_test(&mut self, result: QueryResult, cx: &mut Context<Self>) {
        let result = Arc::new(result);
        self.pending_grid_result = Some(result.clone());
        self.result = Some(result);
        cx.notify();
    }

    /// Returns `true` if the panel has received at least one successful result.
    #[cfg(test)]
    pub(crate) fn has_result(&self) -> bool {
        self.result.is_some()
    }

    /// Schedule a fresh inspector snapshot fetch.
    ///
    /// Builds an `InstanceInspectorQuery` execution context (no time window)
    /// and dispatches through the connection. The query text is empty — the
    /// driver dispatches based solely on the source context variant.
    pub fn request_reexec(&mut self, cx: &mut Context<Self>) {
        let profile_id = self.profile_id;

        let conn = {
            let state = self.app_state.read(cx);
            let Some(connected) = state.connections().get(&profile_id) else {
                self.last_error = Some(datatab_i18n::t!(
                    "document.instance_inspector.connection_not_found"
                ));
                self.state = DocumentState::Error;
                cx.notify();
                return;
            };
            match connected.resolve_connection_for_execution(None) {
                Ok(c) => c,
                Err(e) => {
                    self.last_error = Some(datatab_i18n::t!(
                        "document.instance_inspector.connection_error",
                        detail = format!("{e:?}")
                    ));
                    self.state = DocumentState::Error;
                    cx.notify();
                    return;
                }
            }
        };

        let request = build_inspector_request(&self.metric_id);
        let (task_id, cancel_token) = self.runner.start_primary(
            datatab_core::TaskKind::Query,
            datatab_i18n::t!(
                "document.instance_inspector.task_label",
                metric_id = self.metric_id
            ),
            cx,
        );

        self.state = DocumentState::Loading;
        cx.notify();

        let conn_cleanup = conn.clone();
        let task = cx
            .background_executor()
            .spawn(async move { conn.execute(&request) });

        cx.spawn(async move |this, cx| {
            let result = task.await;

            cx.update(|cx| {
                if cancel_token.is_cancelled() {
                    if let Err(e) = conn_cleanup.cleanup_after_cancel() {
                        log::warn!("[inspector] cleanup after cancel failed: {}", e);
                    }
                    return;
                }

                let Some(entity) = this.upgrade() else { return };
                entity.update(cx, |panel, cx| {
                    panel.pending_result = Some(PendingResult { task_id, result });
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn apply_result(&mut self, pending: PendingResult, cx: &mut Context<Self>) {
        self.runner.complete_primary(pending.task_id, cx);

        match pending.result {
            Ok(result) => {
                self.state = DocumentState::Clean;
                self.last_error = None;

                let arc = Arc::new(result);

                if let Some(grid) = &self.data_grid {
                    grid.update(cx, |g, cx| g.set_result((*arc).clone(), cx));
                } else {
                    self.pending_grid_result = Some(arc.clone());
                }

                self.result = Some(arc);
            }
            Err(err) => {
                self.state = DocumentState::Error;
                self.last_error = Some(err.to_string());
            }
        }

        cx.emit(DocumentEvent::ExecutionFinished);
        cx.notify();
    }

    fn update_timer(&mut self, cx: &mut Context<Self>) {
        self._timer = None;

        let duration = match self.refresh_policy {
            RefreshPolicy::Manual => return,
            RefreshPolicy::Interval { every_secs } => {
                let clamped = every_secs.max(MIN_REFRESH_FLOOR_SECS as u32);
                std::time::Duration::from_secs(clamped as u64)
            }
        };

        self._timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(duration).await;

                cx.update(|cx| {
                    let Some(entity) = this.upgrade() else {
                        return;
                    };
                    entity.update(cx, |panel, cx| {
                        if !panel
                            .app_state
                            .read(cx)
                            .connections()
                            .contains_key(&panel.profile_id)
                        {
                            return;
                        }
                        if !panel.auto_refresh_tick_allowed(cx) {
                            return;
                        }
                        panel.request_reexec(cx);
                    });
                });
            }
        }));
    }
}

impl InspectorPanel {
    /// Resolve the row-action provider from the current live connection and
    /// install it on `grid`. Called each time a grid is created or updated so
    /// the provider always reflects the active connection rather than a
    /// connection captured at an earlier point in time.
    ///
    /// `instance_catalog()` probes privileges on the client connection, so the
    /// lookup runs on the background executor. The actions are installed only
    /// while the profile still holds the connection and session generation
    /// captured before it; a reconnect in between drops them.
    fn refresh_row_action_provider(&self, grid: &Entity<DataGridPanel>, cx: &mut Context<Self>) {
        let profile_id = self.profile_id;
        let metric_id = self.metric_id.clone();
        let app_state = self.app_state.clone();

        // Fail-closed: no lookup, and so no actions, while the profile is gone.
        let Some(expected_session) = app_state.read(cx).capture_connection_session(profile_id)
        else {
            return;
        };
        let connection = expected_session.connection.clone();
        let lookup_metric_id = metric_id.clone();
        let lookup = cx.background_executor().spawn(async move {
            connection
                .instance_catalog()
                .map(|catalog| catalog.row_actions(&lookup_metric_id))
        });

        let grid = grid.clone();
        cx.spawn(async move |_this, cx| {
            let actions = lookup.await;
            cx.update(|cx| {
                if !app_state
                    .read(cx)
                    .is_same_connection_session(profile_id, &expected_session)
                {
                    return;
                }
                let Some(actions) = actions.filter(|actions| !actions.is_empty()) else {
                    return;
                };
                grid.update(cx, |panel, _cx| {
                    panel.set_row_action_provider(Arc::new(move |mid| {
                        if mid == metric_id {
                            actions.clone()
                        } else {
                            Vec::new()
                        }
                    }));
                });
            });
        })
        .detach();
    }

    fn cancel_kill_action(&mut self, cx: &mut Context<Self>) {
        self.pending_kill_confirm = None;
        self.kill_confirm_focus.restore(cx);
        cx.notify();
    }

    fn confirm_kill_action(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.pending_kill_confirm.take() else {
            return;
        };
        self.kill_confirm_focus.restore(cx);

        let profile_id = self.profile_id;
        let metric_id = self.metric_id.clone();
        let action_id = confirm.action_id.clone();
        let row_values = confirm.row_values.clone();
        let action_label = confirm.action_label.clone();
        let audit_service = self.app_state.read(cx).audit_service().clone();
        let app_state = self.app_state.clone();
        // Capture on the foreground, before the lookup, so a reconnect in
        // between only drops the panel refresh.
        let expected_session = app_state.read(cx).capture_connection_session(profile_id);

        cx.notify();

        cx.spawn(async move |this, cx| {
            // The connection was captured when the user confirmed, not at
            // click time. The audit row and the error describe what happened
            // on that connection, so they always go out; only the panel
            // refresh is fenced, since it would land on the new connection.
            let Some(expected_session) = expected_session else {
                let uf = UserFacingError::new(
                    ErrorKind::Driver,
                    datatab_i18n::t!("document.instance_inspector.action_unavailable"),
                );
                report_error_async(uf, cx);
                return;
            };
            let connection = expected_session.connection.clone();
            let execution_metric_id = metric_id.clone();
            let execution_action_id = action_id.clone();
            let execution_row_values = row_values.clone();

            // `instance_catalog()` probes privileges on the client connection;
            // keep the lookup and the action off the foreground thread.
            let execution = cx.background_executor().spawn(async move {
                match connection.instance_catalog() {
                    Some(catalog) => {
                        catalog
                            .execute_row_action(
                                &execution_metric_id,
                                &execution_action_id,
                                &execution_row_values,
                            )
                            .await
                    }
                    None => Err(datatab_core::DbError::NotSupported(
                        "driver does not support instance catalog".to_string(),
                    )),
                }
            });
            let result = execution.await;

            // Written before the fence: the kill already ran on the captured
            // connection, so its governance record must not depend on what
            // the inspector now shows.
            emit_kill_audit(
                &audit_service,
                &metric_id,
                &action_id,
                &action_label,
                profile_id,
                &result,
            );

            if let Err(ref e) = result {
                let uf = kill_error_to_user_facing(&action_id, &action_label, &metric_id, e);
                report_error_async(uf, cx);
                return;
            }

            // Only the inspector re-run stays behind the fence, so a stale
            // completion never reshapes the panel of a new connection.
            let still_current = cx.update(|cx| {
                app_state
                    .read(cx)
                    .is_same_connection_session(profile_id, &expected_session)
            });
            if !still_current {
                return;
            }

            cx.update(|cx| {
                if let Some(entity) = this.upgrade() {
                    entity.update(cx, |panel, cx| panel.request_reexec(cx));
                }
            });
        })
        .detach();
    }

    fn render_kill_confirm_modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::div;

        let action_label = self
            .pending_kill_confirm
            .as_ref()
            .map(|c| c.action_label.clone())
            .unwrap_or_else(|| datatab_i18n::t!("document.instance_inspector.kill_default_label"));

        let title = format!("{}?", action_label);
        let description = datatab_i18n::t!("document.instance_inspector.kill_confirm_body");

        let footer = div()
            .flex()
            .gap(Spacing::SM)
            .child(
                Button::new(
                    "kill-cancel-btn",
                    datatab_i18n::t!("document.instance_inspector.cancel"),
                )
                .icon(AppIcon::X)
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.cancel_kill_action(cx);
                })),
            )
            .child(
                Button::new(
                    "kill-confirm-btn",
                    datatab_i18n::t!("document.instance_inspector.confirm"),
                )
                .danger()
                .icon(AppIcon::Delete)
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.confirm_kill_action(cx);
                })),
            );

        // Enter confirms and Escape cancels, like the footer buttons.
        Modal::new(title)
            .id("kill-confirm-overlay")
            .danger()
            .icon(AppIcon::TriangleAlert)
            .width(gpui::px(400.0))
            .body(Text::body(description))
            .footer(footer)
            .focus_handle(self.kill_confirm_focus.handle())
            .on_close({
                let weak_self = cx.weak_entity();
                move |_window, cx| {
                    weak_self
                        .update(cx, |this, cx| this.cancel_kill_action(cx))
                        .log_err();
                }
            })
            .on_confirm({
                let weak_self = cx.weak_entity();
                move |_window, cx| {
                    weak_self
                        .update(cx, |this, cx| this.confirm_kill_action(cx))
                        .log_err();
                }
            })
    }
}

impl Render for InspectorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::div;

        // Drain the finished result first so its ExecutionFinished event
        // decrements inflight before any new dispatch increments it.
        if let Some(pending) = self.pending_result.take() {
            self.apply_result(pending, cx);
        }

        if self.pending_initial_exec {
            self.pending_initial_exec = false;
            self.request_reexec(cx);
        }

        // Lazily construct the DataGridPanel + ResultPanel when the first
        // successful result arrives. Both require a Window reference (for
        // InputState / TimeRangePanel internals), so they cannot be created
        // outside render().
        if self.data_grid.is_none() && self.pending_grid_result.is_some() {
            #[expect(
                clippy::unwrap_used,
                reason = "take() returns Some because pending_grid_result.is_some() was checked two lines above"
            )]
            let result = self.pending_grid_result.take().unwrap();
            let profile_id = self.profile_id;
            let app_state = self.app_state.clone();
            let metric_id = self.metric_id.clone();

            let grid = cx.new(|cx| {
                DataGridPanel::new_for_result(
                    result,
                    metric_id.clone(),
                    Some(profile_id),
                    app_state,
                    window,
                    cx,
                )
            });

            // Resolve the row-action provider from the live connection. This
            // runs at render time (not capture time), so it reflects the
            // current connection rather than a stale Arc.
            self.refresh_row_action_provider(&grid, cx);

            let subscription = cx.subscribe(&grid, |this, _grid, event: &DataGridEvent, cx| {
                if let DataGridEvent::RowActionRequested {
                    action_id,
                    action_label,
                    row_values,
                    ..
                } = event
                {
                    this.pending_kill_confirm = Some(PendingKillConfirm {
                        action_id: action_id.clone(),
                        action_label: action_label.clone(),
                        row_values: row_values.clone(),
                    });
                    this.kill_confirm_focus.focus_on_next_render();
                    cx.notify();
                }
            });

            let view_handle = DataGridPanel::into_view_handle(grid.clone(), cx);
            let panel = cx.new(|cx| ResultPanel::new(view_handle, cx));

            self.data_grid = Some(grid);
            self.result_panel = Some(panel);
            self._data_grid_subscription = Some(subscription);
        }

        let focus_handle = self.focus_handle.clone();

        if let Some(result_panel) = self.result_panel.as_ref().cloned() {
            let has_kill_confirm = self.pending_kill_confirm.is_some();

            if has_kill_confirm {
                self.kill_confirm_focus.apply_pending(window, cx);
            }

            return div()
                .size_full()
                .relative()
                .track_focus(&focus_handle)
                .child(result_panel)
                .when(has_kill_confirm, |d| {
                    d.child(self.render_kill_confirm_modal(cx))
                })
                .into_any();
        }

        // No result yet — show a loading or error placeholder.
        let msg = if let Some(err) = &self.last_error {
            datatab_i18n::t!("document.instance_inspector.error_prefix", error = err)
        } else if self.state == DocumentState::Loading {
            datatab_i18n::t!("document.instance_inspector.loading")
        } else {
            datatab_i18n::t!("document.instance_inspector.empty")
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .text_sm()
            .track_focus(&focus_handle)
            .child(msg)
            .into_any()
    }
}

/// Build the `ExecutionSourceContext` for an instance inspector snapshot.
///
/// Always constructs `InstanceInspectorQuery` — no time-window fields are
/// present on this variant by design, ensuring callers cannot accidentally
/// inject time bounds.
pub(crate) fn build_inspector_context(metric_id: &str) -> ExecutionSourceContext {
    ExecutionSourceContext::InstanceInspectorQuery {
        metric_id: metric_id.to_string(),
    }
}

/// Build a `QueryRequest` that dispatches an `InstanceInspectorQuery`.
///
/// The `sql` field is empty — the driver routes exclusively on the execution
/// source context variant.
fn build_inspector_request(metric_id: &str) -> QueryRequest {
    QueryRequest {
        execution_context: Some(ExecutionContext {
            source: Some(build_inspector_context(metric_id)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Converts a row-action failure to a `UserFacingError` suitable for display.
///
/// Constructs a `Driver`-kind error whose summary is the driver error message.
/// The `action_label` and `metric_id` are embedded in the cause so the user
/// knows which operation failed.
pub(crate) fn kill_error_to_user_facing(
    _action_id: &str,
    action_label: &str,
    metric_id: &str,
    err: &datatab_core::DbError,
) -> UserFacingError {
    UserFacingError::new(ErrorKind::Driver, err.to_string()).with_cause(datatab_i18n::t!(
        "document.instance_inspector.kill_failed_cause",
        action_label = action_label,
        metric_id = metric_id
    ))
}

fn emit_kill_audit(
    audit_service: &datatab_audit::AuditService,
    metric_id: &str,
    action_id: &str,
    action_label: &str,
    profile_id: Uuid,
    result: &Result<(), datatab_core::DbError>,
) {
    use datatab_core::chrono::Utc;
    use datatab_core::observability::{
        EventCategory, EventOutcome, EventRecord, EventSeverity, EventSink,
    };

    let (severity, outcome, action_key) = match result {
        Ok(()) => (
            EventSeverity::Info,
            EventOutcome::Success,
            "inspector_row_action",
        ),
        Err(_) => (
            EventSeverity::Error,
            EventOutcome::Failure,
            "inspector_row_action_failed",
        ),
    };

    let summary = match result {
        Ok(()) => format!(
            "Inspector row action '{}' ({}) executed on inspector '{}' (profile {})",
            action_label, action_id, metric_id, profile_id
        ),
        Err(e) => format!(
            "Inspector row action '{}' ({}) failed on '{}' (profile {}): {}",
            action_label, action_id, metric_id, profile_id, e
        ),
    };

    let event = EventRecord::new(
        Utc::now().timestamp_millis(),
        severity,
        EventCategory::Governance,
        outcome,
    )
    .with_action(action_key.to_string())
    .with_summary(summary)
    .with_actor_id("ui:user");

    if let Err(e) = audit_service.record(event) {
        log::warn!("[inspector kill] failed to record audit event: {}", e);
    }
}

#[cfg(test)]
mod keyboard_tests;

#[cfg(test)]
mod tests;
