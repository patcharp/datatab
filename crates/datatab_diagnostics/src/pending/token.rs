//! The delete token kept beside a report that was sent.
//!
//! ADR 0008 § 3: the service answers a send with one per-report delete token,
//! and the app keeps that token beside the local report so the report can be
//! removed before the 90 days are up. The file sits next to the report, is
//! owner-only from the instant it exists, and is never listed by the store:
//! `<name>.json` and `<name>.json.sent` differ in extension, so only the
//! report is a report.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{PendingError, write_hook_note, write_private_file};
use crate::sender::SubmittedReport;

/// Added to a report's file name for the delete token of a sent report.
pub const SENT_TOKEN_SUFFIX: &str = ".sent";

/// What one successful send returned, kept so the report can be deleted.
///
/// It lives in its own file and never in the database: it belongs to one
/// report, and the local file it sits beside is the whole of its lifetime.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentToken {
    pub report_id: Uuid,
    pub delete_token: String,
}

/// The token is a credential: it never reaches a log line or a test failure.
impl std::fmt::Debug for SentToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SentToken")
            .field("report_id", &self.report_id)
            .field("delete_token", &"<redacted>")
            .finish()
    }
}

/// What reading the token beside a report found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SentTokenLookup {
    /// A readable token: this report can be deleted from the server.
    Found(SentToken),
    /// No token file. The report was sent before DataTab kept tokens, or its
    /// token was removed. Nothing can be deleted from here.
    Missing,
    /// A token file that exists and cannot be read. It is treated as no token,
    /// never as "delete anyway", and the reason is reported once.
    Unreadable(String),
}

/// Keeps the token `submitted` returned beside the report at `report`.
///
/// The file is replaced when one is already there: the newest send owns the
/// stored copy, so its token is the one that deletes it. The write goes
/// through a temporary file and a rename, so a crash never leaves half a
/// token file in place of a whole one.
pub fn write_sent_token(
    report: &Path,
    submitted: &SubmittedReport,
) -> Result<PathBuf, PendingError> {
    let token = SentToken {
        report_id: submitted.report_id,
        delete_token: submitted.delete_token.clone(),
    };
    let bytes = serde_json::to_vec(&token).map_err(|error| PendingError::Io {
        path: sent_token_path(report),
        source: std::io::Error::other(error),
    })?;

    let final_path = sent_token_path(report);
    let mut temp_name = final_path.as_os_str().to_owned();
    temp_name.push(".tmp");
    let temp_path = PathBuf::from(temp_name);

    // A temporary file a crash left behind would make `create_new` fail for
    // every later send, so a stale one is removed first.
    remove_quietly(&temp_path);
    write_private_file(&temp_path, &bytes)?;
    std::fs::rename(&temp_path, &final_path).map_err(|source| PendingError::Io {
        path: final_path.clone(),
        source,
    })?;

    Ok(final_path)
}

/// The token kept beside the report at `report`.
///
/// Never an error: a missing or damaged token file is one of the answers, so
/// every caller decides what to say instead of handling a failure.
pub fn sent_token(report: &Path) -> SentTokenLookup {
    let path = sent_token_path(report);

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SentTokenLookup::Missing;
        }
        Err(error) => return SentTokenLookup::Unreadable(format!("{path:?}: {error}")),
    };

    match serde_json::from_slice::<SentToken>(&bytes) {
        // An empty token or a nil id would be posted as a bearer credential
        // that can never match: treat it as a damaged file, not as a token.
        Ok(token) if token.delete_token.is_empty() || token.report_id.is_nil() => {
            SentTokenLookup::Unreadable(format!("{path:?}: empty token"))
        }
        Ok(token) => SentTokenLookup::Found(token),
        Err(error) => SentTokenLookup::Unreadable(format!("{path:?}: {error}")),
    }
}

/// Removes the token beside the report at `report`, if it has one.
///
/// Called when a report leaves the store and after a delete reached the
/// server: a token for a report nobody can read any more deletes nothing.
pub(super) fn remove_sent_token(report: &Path) {
    let path = sent_token_path(report);

    match std::fs::remove_file(&path) {
        Ok(()) => {}
        // No token is the normal case: most reports were never sent.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => write_hook_note(&format!(
            "delete token not removed at {}: {error}",
            path.display()
        )),
    }
}

/// The token file that belongs to the report at `path`.
///
/// The report's own name is kept whole, as the decision marker keeps it, so
/// `<name>.json.sent` keeps its place beside the file it belongs to and is
/// never listed as a report.
pub(super) fn sent_token_path(report: &Path) -> PathBuf {
    let mut name = report.as_os_str().to_owned();
    name.push(SENT_TOKEN_SUFFIX);
    PathBuf::from(name)
}

/// Removes a file that should not be there, ignoring the normal absence.
fn remove_quietly(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => write_hook_note(&format!(
            "stale file not removed at {}: {error}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests;
