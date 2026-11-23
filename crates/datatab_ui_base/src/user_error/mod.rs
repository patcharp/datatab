use datatab_core::FormattedError;
use datatab_core::observability::EventSeverity;
use gpui::{App, AsyncApp};
use uuid::Uuid;

pub mod throttle;

/// Coarse-grained taxonomy of user-visible failures.
///
/// Used for styling, audit `action` discrimination, and badge classification.
/// The free-form `summary` carries the actionable message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Storage,
    Network,
    Auth,
    Hook,
    Driver,
    User,
    Config,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Network => "network",
            Self::Auth => "auth",
            Self::Hook => "hook",
            Self::Driver => "driver",
            Self::User => "user",
            Self::Config => "config",
        }
    }
}

/// A user-visible failure with enough context to render a toast and an audit
/// event that share a correlation id.
///
/// Construction is always at the *first catch site*. Propagators above MUST NOT
/// re-report — there is no runtime deduplication.
#[derive(Debug, Clone)]
pub struct UserFacingError {
    pub kind: ErrorKind,
    /// Severity drives toast styling and throttle eligibility.
    /// Default for `UserFacingError::new` is `EventSeverity::Error`.
    pub severity: EventSeverity,
    pub summary: String,
    pub cause: Option<String>,
    pub suggested_action: Option<String>,
    pub correlation_id: Uuid,
    /// The driver the failure came from, as the call site knows it (`postgres`,
    /// `rpc:<socket_id>`). A report maps it to a closed value or drops it, so
    /// this never carries a path out of the app.
    pub driver_id: Option<String>,
    /// The driver or error code, such as a SQLSTATE. A report keeps it only
    /// when it matches the short-token pattern.
    pub error_code: Option<String>,
}

impl UserFacingError {
    /// Constructs a new error with `severity = Error` and a freshly generated
    /// UUID v7 correlation id.
    pub fn new(kind: ErrorKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            severity: EventSeverity::Error,
            summary: summary.into(),
            cause: None,
            suggested_action: None,
            correlation_id: Uuid::now_v7(),
            driver_id: None,
            error_code: None,
        }
    }

    pub fn with_severity(mut self, severity: EventSeverity) -> Self {
        self.severity = severity;
        self
    }

    pub fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }

    pub fn with_suggested_action(mut self, action: impl Into<String>) -> Self {
        self.suggested_action = Some(action.into());
        self
    }

    /// Records the driver the failure came from, for the report's `driver_id`.
    pub fn with_driver_id(mut self, driver_id: impl Into<String>) -> Self {
        self.driver_id = Some(driver_id.into());
        self
    }

    /// Records a short driver or error code, for the report's `error_code`.
    pub fn with_error_code(mut self, code: impl Into<String>) -> Self {
        self.error_code = Some(code.into());
        self
    }

    /// The data the "Send report…" action needs, without the toast text.
    pub fn send_report_request(&self) -> SendReportRequest {
        SendReportRequest::from_error(self)
    }

    /// Test-only seam: override the auto-generated correlation id.
    pub fn with_correlation_id(mut self, id: Uuid) -> Self {
        self.correlation_id = id;
        self
    }

    /// Driver-agnostic constructor: takes the already-formatted output of a
    /// driver's error formatter. The driver-side formatter call lives in the
    /// call site (drivers, not UI).
    pub fn from_formatted(kind: ErrorKind, fe: FormattedError) -> Self {
        let cause = match (&fe.detail, &fe.hint, &fe.code) {
            (None, None, None) => None,
            _ => Some(fe.to_string()),
        };
        let error_code = fe.code.clone();

        Self {
            kind,
            severity: EventSeverity::Error,
            summary: fe.message,
            cause,
            suggested_action: None,
            correlation_id: Uuid::now_v7(),
            driver_id: None,
            error_code,
        }
    }
}

/// What a "Send report…" action carries for one user-facing error.
///
/// It holds no free text: the toast summary and cause never reach a report, so
/// this type only names the closed fields the report builder accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReportRequest {
    pub correlation_id: Uuid,
    pub error_kind: &'static str,
    pub severity: EventSeverity,
    pub driver_id: Option<String>,
    pub error_code: Option<String>,
}

impl SendReportRequest {
    pub fn from_error(error: &UserFacingError) -> Self {
        Self {
            correlation_id: error.correlation_id,
            error_kind: error.kind.as_str(),
            severity: error.severity,
            driver_id: error.driver_id.clone(),
            error_code: error.error_code.clone(),
        }
    }
}

/// Foreground-only entry point.
///
/// MUST be called from the foreground (`&mut App`). Calling from a
/// `background_executor().spawn()` closure panics — the GPUI global pulled
/// in for `ToastHost` is foreground-only. Background callers MUST use
/// `report_error_async` instead.
///
/// | Caller context                          | Use                     |
/// | --------------------------------------- | ----------------------- |
/// | `&mut Context<T>`, `&mut App`           | `report_error`          |
/// | Inside `background_executor().spawn()`  | `report_error_async`    |
/// | Inside `cx.spawn(async ...)`            | `report_error_async`    |
/// | Inside a `cx.update(|cx| { ... })`      | `report_error`          |
pub fn report_error(err: UserFacingError, cx: &mut App) {
    use crate::app_state_entity::AppStateGlobal;
    use crate::toast::{Toast, ToastAction, copy_action, now_hms};

    let id_str = err.correlation_id.to_string();
    let summary = &err.summary;

    trace_user_error(&err);

    let mut toast = match err.severity {
        EventSeverity::Warn => Toast::warning(err.summary.clone()),
        EventSeverity::Fatal => Toast::fatal(err.summary.clone()),
        _ => Toast::error(err.summary.clone()),
    };

    if let Some(c) = &err.cause {
        toast = toast.code_block(c.clone());
    }

    if let Some(a) = &err.suggested_action {
        toast = toast.body(a.clone());
    }

    let id_for_action = err.correlation_id;
    let view_in_audit = ToastAction::new(
        "view-in-audit",
        datatab_i18n::t!("errors.action.view_in_audit"),
    )
    // The status-bar badge and the audit row keep the error after its toast
    // closes, so this link does not hold the toast open.
    .lets_toast_close()
    .on_click(move |cx: &mut App| {
        if let Some(g) = cx.try_global::<AppStateGlobal>() {
            let entity = g.entity.clone();
            entity.update(cx, |s, cx| s.request_open_audit(Some(id_for_action), cx));
        }
    });

    toast = toast
        .meta_right(now_hms())
        .details(datatab_i18n::t!("errors.correlation.detail", id = id_str))
        .action(copy_action(datatab_i18n::t!(
            "errors.correlation.clipboard",
            summary = summary,
            id = id_str
        )))
        .action(view_in_audit);

    if crate::report_offer::reports_offered(cx) {
        toast = toast.action(send_report_action(err.send_report_request()));
    }

    toast.push(cx);

    if let Some(app_state_global) = cx.try_global::<AppStateGlobal>() {
        let entity = app_state_global.entity.clone();
        entity.update(cx, |s, cx| {
            s.notifications
                .record_error(crate::notifications::ErrorNotification::from_user_error(
                    &err,
                    datatab_core::chrono::Utc::now(),
                ));
            s.note_user_error(err.correlation_id, err.severity, cx);
        });
    }
}

/// Builds the "Send report…" action of an error toast.
///
/// It opens the send window through the workspace, and it is drawn only when
/// the off switch lets a report be offered: an organization that turned
/// reports off must not see the button at all (ADR 0008, `docs/SETTINGS.md`).
fn send_report_action(request: SendReportRequest) -> crate::toast::ToastAction {
    crate::toast::ToastAction::new("send-report", datatab_i18n::t!("errors.action.send_report"))
        // The window it opens is the error's outcome, not the toast's: the toast
        // may still close on its own timer.
        .lets_toast_close()
        .on_click(move |cx: &mut App| {
            if let Some(global) = cx.try_global::<crate::app_state_entity::AppStateGlobal>() {
                let entity = global.entity.clone();
                let request = crate::app_state_entity::ReportWindowRequest::Error(request.clone());
                entity.update(cx, |state, cx| state.request_report_window(request, cx));
            }
        })
}

/// Emits the tracing event the audit bridge turns into the audit row.
///
/// `cause` and `suggested_action` are not typed audit fields, so the bridge
/// stores them in `details_json` next to `kind`. Both are skipped when unset.
fn trace_user_error(err: &UserFacingError) {
    let id_str = err.correlation_id.to_string();
    let kind_str = err.kind.as_str();
    let summary = &err.summary;
    let cause = err.cause.as_deref();
    let suggested_action = err.suggested_action.as_deref();

    match err.severity {
        EventSeverity::Warn => tracing::warn!(
            target: "datatab_ui::user_error",
            correlation_id = %id_str,
            kind            = %kind_str,
            outcome         = "failure",
            action          = "user_error",
            cause,
            suggested_action,
            "{summary}",
        ),
        _ => tracing::error!(
            target: "datatab_ui::user_error",
            correlation_id = %id_str,
            kind            = %kind_str,
            outcome         = "failure",
            action          = "user_error",
            cause,
            suggested_action,
            "{summary}",
        ),
    }
}

/// Background-safe entry point. Marshals to the foreground via `cx.update`.
///
/// Fire-and-forget: if the foreground has been dropped the call is silently
/// ignored.
///
/// | Caller context                          | Use                     |
/// | --------------------------------------- | ----------------------- |
/// | `&mut Context<T>`, `&mut App`           | `report_error`          |
/// | Inside `background_executor().spawn()`  | `report_error_async`    |
/// | Inside `cx.spawn(async ...)`            | `report_error_async`    |
/// | Inside a `cx.update(|cx| { ... })`      | `report_error`          |
pub fn report_error_async(err: UserFacingError, cx: &AsyncApp) {
    let cx = cx.clone();
    cx.update(move |cx| report_error(err, cx));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_kind_name_is_reportable() {
        // No wildcard arm: a new `ErrorKind` variant fails to compile here, so
        // this list, the array below and the report's allowlist are updated
        // together rather than a name being dropped silently.
        fn reportable_arm(kind: ErrorKind) -> &'static str {
            match kind {
                ErrorKind::Storage
                | ErrorKind::Network
                | ErrorKind::Auth
                | ErrorKind::Hook
                | ErrorKind::Driver
                | ErrorKind::User
                | ErrorKind::Config => kind.as_str(),
            }
        }

        let all = [
            ErrorKind::Storage,
            ErrorKind::Network,
            ErrorKind::Auth,
            ErrorKind::Hook,
            ErrorKind::Driver,
            ErrorKind::User,
            ErrorKind::Config,
        ];
        let names: Vec<&'static str> = all.into_iter().map(reportable_arm).collect();

        assert_eq!(
            names.len(),
            datatab_diagnostics::builder::ERROR_KINDS.len(),
            "the report allowlist must name every ErrorKind"
        );

        for name in names {
            assert!(
                datatab_diagnostics::builder::ERROR_KINDS.contains(&name),
                "{name} is missing from the report allowlist"
            );
        }
    }

    #[test]
    fn send_report_request_carries_only_the_closed_fields() {
        let err = UserFacingError::new(ErrorKind::Network, "dial failed")
            .with_driver_id("rpc:/run/user/1000/datatab/peer.sock")
            .with_error_code("08006")
            .with_cause("secret detail")
            .with_suggested_action("retry");

        let request = err.send_report_request();

        assert_eq!(request.correlation_id, err.correlation_id);
        assert_eq!(request.error_kind, "network");
        assert_eq!(request.severity, EventSeverity::Error);
        assert_eq!(
            request.driver_id.as_deref(),
            Some("rpc:/run/user/1000/datatab/peer.sock"),
            "the request keeps the raw id; the report builder closes it"
        );
        assert_eq!(request.error_code.as_deref(), Some("08006"));
    }

    #[test]
    fn from_formatted_takes_the_code_for_the_report() {
        let formatted = FormattedError::new("syntax error")
            .with_detail("near SELECT")
            .with_code("42601");

        let err = UserFacingError::from_formatted(ErrorKind::Driver, formatted);

        assert_eq!(err.error_code.as_deref(), Some("42601"));
        assert!(err.driver_id.is_none());
    }

    #[test]
    fn new_defaults_to_error_severity() {
        let err = UserFacingError::new(ErrorKind::Driver, "something failed");
        assert_eq!(err.severity, EventSeverity::Error);
        assert_eq!(err.kind, ErrorKind::Driver);
        assert_eq!(err.summary, "something failed");
        assert!(err.cause.is_none());
        assert!(err.suggested_action.is_none());
    }

    #[test]
    fn with_correlation_id_overrides_generated_uuid() {
        let known = Uuid::nil();
        let err = UserFacingError::new(ErrorKind::Storage, "io error").with_correlation_id(known);
        assert_eq!(err.correlation_id, known);
    }

    #[test]
    fn with_severity_round_trips() {
        let err = UserFacingError::new(ErrorKind::User, "mild problem")
            .with_severity(EventSeverity::Warn);
        assert_eq!(err.severity, EventSeverity::Warn);
    }

    #[test]
    fn from_formatted_sets_driver_kind_and_populates_cause() {
        let fe = FormattedError::new("connection refused")
            .with_detail("no route to host")
            .with_code("08006");
        let err = UserFacingError::from_formatted(ErrorKind::Driver, fe);
        assert_eq!(err.kind, ErrorKind::Driver);
        assert_eq!(err.summary, "connection refused");
        assert!(
            err.cause.is_some(),
            "cause must be populated from detail/code"
        );
    }

    #[test]
    fn from_formatted_no_extras_has_no_cause() {
        let fe = FormattedError::new("simple error");
        let err = UserFacingError::from_formatted(ErrorKind::Network, fe);
        assert!(
            err.cause.is_none(),
            "cause must be None when no detail/hint/code"
        );
    }

    /// Collects the string fields of every event emitted while it is the
    /// default subscriber.
    #[derive(Default)]
    struct FieldCapture {
        fields: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    }

    impl tracing::field::Visit for &FieldCapture {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if let Ok(mut fields) = self.fields.lock() {
                fields.insert(field.name().to_owned(), value.to_owned());
            }
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if let Ok(mut fields) = self.fields.lock() {
                fields.insert(field.name().to_owned(), format!("{value:?}"));
            }
        }
    }

    impl tracing::Subscriber for FieldCapture {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = self;
            event.record(&mut visitor);
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    fn traced_fields(err: &UserFacingError) -> std::collections::BTreeMap<String, String> {
        let capture = std::sync::Arc::new(FieldCapture::default());

        tracing::subscriber::with_default(capture.clone(), || trace_user_error(err));

        capture
            .fields
            .lock()
            .map(|fields| fields.clone())
            .unwrap_or_default()
    }

    #[test]
    fn traced_event_carries_the_cause_and_suggested_action() {
        let err = UserFacingError::new(ErrorKind::Network, "Failed to load the database list")
            .with_cause("Connection failed: error sending request")
            .with_suggested_action("Check that the server is reachable");

        let fields = traced_fields(&err);

        assert_eq!(
            fields.get("cause").map(String::as_str),
            Some("Connection failed: error sending request")
        );
        assert_eq!(
            fields.get("suggested_action").map(String::as_str),
            Some("Check that the server is reachable")
        );
        assert_eq!(fields.get("kind").map(String::as_str), Some("network"));
    }

    #[test]
    fn traced_event_omits_an_unset_cause() {
        let err = UserFacingError::new(ErrorKind::Storage, "Export failed");

        let fields = traced_fields(&err);

        assert!(!fields.contains_key("cause"));
        assert!(!fields.contains_key("suggested_action"));
    }

    #[test]
    fn error_kind_as_str_returns_lowercase() {
        assert_eq!(ErrorKind::Storage.as_str(), "storage");
        assert_eq!(ErrorKind::Network.as_str(), "network");
        assert_eq!(ErrorKind::Auth.as_str(), "auth");
        assert_eq!(ErrorKind::Hook.as_str(), "hook");
        assert_eq!(ErrorKind::Driver.as_str(), "driver");
        assert_eq!(ErrorKind::User.as_str(), "user");
        assert_eq!(ErrorKind::Config.as_str(), "config");
    }
}

#[cfg(test)]
mod i18n_tests {
    const USER_ERROR_KEYS: &[&str] = &[
        "errors.action.view_in_audit",
        "errors.correlation.detail",
        "errors.correlation.clipboard",
    ];

    #[test]
    fn user_error_catalog_keys_resolve() {
        for key in USER_ERROR_KEYS {
            let english = datatab_i18n::t!(key);

            assert!(!english.is_empty(), "empty English translation for {key}");
            assert_ne!(english, *key, "missing English translation for {key}");
        }
    }

    #[test]
    fn correlation_clipboard_ends_with_correlation_detail() {
        let clipboard =
            datatab_i18n::t!("errors.correlation.clipboard", summary = "boom", id = "ID");
        let detail = datatab_i18n::t!("errors.correlation.detail", id = "ID");

        assert!(clipboard.starts_with("boom\n"));
        assert!(clipboard.ends_with(&detail));
    }
}

#[cfg(test)]
mod lifetime_tests {
    use std::time::Duration;

    use datatab_core::observability::EventSeverity;
    use datatab_storage::bootstrap::StorageRuntime;
    use gpui::{AppContext as _, Entity, TestAppContext};

    use super::{ErrorKind, UserFacingError, report_error};
    use crate::app_state_entity::{AppStateEntity, AppStateGlobal};
    use crate::toast::{ToastGlobal, ToastHost};

    fn setup(cx: &mut TestAppContext) -> (Entity<AppStateEntity>, Entity<ToastHost>) {
        let app_state = cx.update(|cx| {
            cx.new(|_| {
                AppStateEntity::new_with_storage_runtime(
                    StorageRuntime::in_memory().expect("isolated storage runtime"),
                )
                .expect("test storage setup")
            })
        });
        let host = cx.update(|cx| {
            let host = cx.new(|_| ToastHost::new());
            cx.set_global(ToastGlobal { host: host.clone() });
            cx.set_global(AppStateGlobal {
                entity: app_state.clone(),
            });
            host
        });
        (app_state, host)
    }

    fn toasts_shown(host: &Entity<ToastHost>, cx: &TestAppContext) -> usize {
        host.read_with(cx, |host, _| host.toast_count())
    }

    fn tick(cx: &TestAppContext, duration: Duration) {
        cx.executor().advance_clock(duration);
        cx.run_until_parked();
    }

    /// The toast carries Copy and "View in Audit", and still closes after
    /// twice the base delay. The error stays reachable: the status bar badge
    /// still counts it and the notification center still holds its row.
    #[gpui::test]
    fn a_reported_error_toast_closes_and_the_error_stays_reachable(cx: &mut TestAppContext) {
        let (app_state, host) = setup(cx);
        let error = UserFacingError::new(ErrorKind::Storage, "disk full");
        let correlation_id = error.correlation_id;

        cx.update(|cx| report_error(error, cx));
        assert_eq!(toasts_shown(&host, cx), 1);

        tick(cx, Duration::from_secs(15));
        assert_eq!(toasts_shown(&host, cx), 1, "shown before twice the delay");

        tick(cx, Duration::from_secs(2));
        assert_eq!(toasts_shown(&host, cx), 0, "closed on its own");

        cx.update(|cx| {
            let state = app_state.read(cx);
            assert_eq!(state.unread_error_count, 1, "the badge keeps counting it");
            assert!(state.notifications.error(correlation_id).is_some());
        });
    }

    #[gpui::test]
    fn a_fatal_report_stays_until_dismissed(cx: &mut TestAppContext) {
        let (_app_state, host) = setup(cx);
        let error = UserFacingError::new(ErrorKind::Storage, "database is gone")
            .with_severity(EventSeverity::Fatal);

        cx.update(|cx| report_error(error, cx));

        tick(cx, Duration::from_secs(600));
        assert_eq!(toasts_shown(&host, cx), 1);
    }

    /// Each occurrence still counts on the badge and records its own error
    /// with its own correlation id; only the toast merges.
    #[gpui::test]
    fn a_repeated_report_merges_the_toast_but_records_every_occurrence(cx: &mut TestAppContext) {
        let (app_state, host) = setup(cx);
        let first = UserFacingError::new(ErrorKind::Network, "no route");
        let second = UserFacingError::new(ErrorKind::Network, "no route");
        let (first_id, second_id) = (first.correlation_id, second.correlation_id);

        cx.update(|cx| {
            report_error(first, cx);
            report_error(second, cx);
        });

        assert_eq!(toasts_shown(&host, cx), 1);
        assert_eq!(
            host.read_with(cx, |host, _| host.last_toast_occurrences()),
            Some(2)
        );
        cx.update(|cx| {
            let state = app_state.read(cx);
            assert_eq!(state.unread_error_count, 2);
            assert!(state.notifications.error(first_id).is_some());
            assert!(state.notifications.error(second_id).is_some());
        });
    }
}

/// The "Send report…" action: drawn when reports may be offered, gone when
/// the off switch says no, and carrying the closed fields of the error.
#[cfg(test)]
mod report_action_tests {
    use datatab_core::observability::EventSeverity;
    use datatab_storage::bootstrap::StorageRuntime;
    use gpui::{AppContext as _, Entity, TestAppContext};

    use super::{ErrorKind, UserFacingError, report_error};
    use crate::app_state_entity::{AppStateEntity, AppStateGlobal};
    use crate::report_offer::ReportOfferSeam;
    use crate::toast::{ToastGlobal, ToastHost};

    fn setup(cx: &mut TestAppContext) -> (Entity<AppStateEntity>, Entity<ToastHost>) {
        let app_state = cx.update(|cx| {
            cx.new(|_| {
                AppStateEntity::new_with_storage_runtime(
                    StorageRuntime::in_memory().expect("isolated storage runtime"),
                )
                .expect("test storage setup")
            })
        });
        let host = cx.update(|cx| {
            let host = cx.new(|_| ToastHost::new());
            cx.set_global(ToastGlobal { host: host.clone() });
            cx.set_global(AppStateGlobal {
                entity: app_state.clone(),
            });
            host
        });
        (app_state, host)
    }

    /// The ids of the buttons the newest toast offers.
    fn toast_action_ids(host: &Entity<ToastHost>, cx: &TestAppContext) -> Vec<String> {
        host.read_with(cx, |host, _cx| {
            let id = host.latest_toast_id().expect("a toast is on screen");
            host.toast_controls(id)
                .expect("controls of the newest toast")
                .actions
                .into_iter()
                .map(|action| action.id.to_string())
                .collect()
        })
    }

    #[gpui::test]
    fn an_error_toast_offers_send_report_when_reports_may_be_offered(cx: &mut TestAppContext) {
        let (_app_state, host) = setup(cx);
        cx.update(|cx| cx.set_global(ReportOfferSeam(|_| true)));

        cx.update(|cx| {
            report_error(
                UserFacingError::new(ErrorKind::Driver, "query failed").with_driver_id("postgres"),
                cx,
            )
        });

        let ids = toast_action_ids(&host, cx);
        assert!(ids.contains(&"send-report".to_string()), "{ids:?}");
    }

    #[gpui::test]
    fn the_off_switch_takes_send_report_off_the_toast(cx: &mut TestAppContext) {
        let (_app_state, host) = setup(cx);
        cx.update(|cx| cx.set_global(ReportOfferSeam(|_| false)));

        cx.update(|cx| report_error(UserFacingError::new(ErrorKind::Storage, "disk full"), cx));

        let ids = toast_action_ids(&host, cx);
        assert!(!ids.contains(&"send-report".to_string()), "{ids:?}");
    }

    #[gpui::test]
    fn without_the_seam_no_toast_offers_a_report(cx: &mut TestAppContext) {
        let (_app_state, host) = setup(cx);

        cx.update(|cx| report_error(UserFacingError::new(ErrorKind::Storage, "disk full"), cx));

        let ids = toast_action_ids(&host, cx);
        assert!(!ids.contains(&"send-report".to_string()), "{ids:?}");
    }

    /// Pressing the action hands the workspace the report request, and only
    /// the closed fields of the error travel with it.
    #[gpui::test]
    fn pressing_send_report_hands_over_the_closed_fields(cx: &mut TestAppContext) {
        let (app_state, host) = setup(cx);
        cx.update(|cx| cx.set_global(ReportOfferSeam(|_| true)));

        let error = UserFacingError::new(ErrorKind::Driver, "query failed")
            .with_severity(EventSeverity::Warn)
            .with_driver_id("postgres")
            .with_error_code("42P01");
        let expected = error.send_report_request();
        cx.update(|cx| report_error(error, cx));

        let (toast_id, index) = host.read_with(cx, |host, _cx| {
            let id = host.latest_toast_id().expect("a toast is on screen");
            let index = host
                .toast_controls(id)
                .expect("controls")
                .actions
                .iter()
                .position(|action| action.id == "send-report")
                .expect("the toast offers Send report");
            (id, index)
        });

        cx.update(|cx| ToastHost::run_action(&host, toast_id, index, cx));

        let request =
            cx.update(|cx| app_state.update(cx, |state, _| state.take_pending_report_window()));
        assert_eq!(
            request,
            Some(crate::app_state_entity::ReportWindowRequest::Error(
                expected
            )),
            "the request carries the closed fields and no free text"
        );
        assert_eq!(
            cx.update(|cx| app_state.update(cx, |state, _| state.take_pending_report_window())),
            None,
            "one press hands over one request"
        );
    }
}
