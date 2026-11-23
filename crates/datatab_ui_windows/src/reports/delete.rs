//! "Delete from the server": the one action that removes a report already
//! sent.
//!
//! ADR 0008 § 3 promises the report can be removed before its 90 days, with
//! the per-report token the service handed back. Deleting removes data and
//! sends none, so the off switch does not apply to it: a user who turned
//! reports off can still take back what they sent.

use datatab_diagnostics::{SendError, SentToken};

/// How one delete attempt is made.
///
/// Production removes the stored report through the report client. A test
/// substitutes a function, so no test opens a socket.
pub type ReportDeleter = std::sync::Arc<dyn Fn(&SentToken) -> Result<(), SendError> + Send + Sync>;

/// The production deleter.
pub fn default_deleter() -> ReportDeleter {
    std::sync::Arc::new(delete_blocking)
}

/// The one delete attempt, blocking.
///
/// The caller runs it on the background executor: it opens a connection and
/// waits for the answer, which must never happen on the thread that draws.
pub fn delete_blocking(token: &SentToken) -> Result<(), SendError> {
    datatab_diagnostics::ReportClient::new()?.delete(token.report_id, &token.delete_token)
}
