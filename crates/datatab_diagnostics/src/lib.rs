//! Error and panic reports the user chooses to send.
//!
//! Implements the client side of ADR 0008 (`docs/adr/0008-error-reports.md`).
//! A report is a typed struct that *is* the allowlist: the serializer cannot
//! emit a field that is not on it. Free text is a rare exception
//! (`panic_message`, `location`) and passes the scrubber in [`scrub`].
//!
//! Nothing here opens a connection on its own. [`sender::ReportClient`] is the
//! only code that talks to the report service and must be called by the user's
//! send action.

pub mod builder;
pub mod pending;
pub mod platform;
pub mod report;
pub mod scrub;
pub mod sender;
pub mod switch;

#[cfg(test)]
mod test_http;

pub use builder::{ErrorInput, PanicInput, PanicMessage, build_error_report, build_panic_report};
pub use pending::token::{
    SENT_TOKEN_SUFFIX, SentToken, SentTokenLookup, sent_token, write_sent_token,
};
pub use pending::{
    DECIDED_MARKER_SUFFIX, DeleteError, MAX_PENDING_REPORTS, PendingError, PendingReport,
    delete_sent_report, is_decided, mark_decided, readable_pending_reports, write_pending_report,
    write_report_file,
};
pub use report::{
    ErrorReport, Frame, MAX_FRAMES, MAX_REPORT_BYTES, MAX_TRAIL_EVENTS, ReportError, ReportKind,
    SCHEMA_VERSION, TrailEvent,
};
pub use scrub::{ScrubOutcome, scrub};
pub use sender::{ReportClient, ReportSender, SendError, SubmitOutcome, SubmittedReport};
pub use switch::{DISABLE_ENV_VAR, DISABLE_FILE_NAME, DisableSource, ReportSwitch, SwitchState};
