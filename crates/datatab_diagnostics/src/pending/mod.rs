//! Pending report files: what a panic leaves for the next start.
//!
//! Writing one is best effort and bounded: never more than
//! [`MAX_PENDING_REPORTS`] files, never more than [`MAX_REPORT_BYTES`] each,
//! no network and no blocking work beyond the write itself. The file name
//! starts with the instant it was written and the order it was written in, so
//! the cap drops the oldest by age and never by the random report id.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::report::{ErrorReport, MAX_REPORT_BYTES, ReportError};
use crate::sender::SendError;
use token::{SentToken, SentTokenLookup, remove_sent_token, sent_token, sent_token_path};

pub mod token;

#[cfg(test)]
mod tests;

/// Pending reports kept on disk. The oldest goes when a sixth arrives.
pub const MAX_PENDING_REPORTS: usize = 5;

/// The report file extension the store recognizes.
pub const REPORT_FILE_EXTENSION: &str = "json";

/// Added to a report's file name to mark that the user decided about it. The
/// marker is an empty file beside the report: the report itself stays where it
/// was.
pub const DECIDED_MARKER_SUFFIX: &str = ".decided";

/// Write counter, so two reports written in the same millisecond still have an
/// order. It is part of the file name; it resets with the process, and the
/// millisecond prefix always dominates it.
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Why a pending report could not be written.
#[derive(Debug, thiserror::Error)]
pub enum PendingError {
    #[error("report not written: {0}")]
    Report(#[from] ReportError),
    #[error("report not written to {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Writes `report` into `dir` and drops the oldest file when the cap is passed.
///
/// The file holds the exact bytes the send window will show. A report over
/// [`MAX_REPORT_BYTES`] is refused, never truncated, and the file is created
/// owner-only, so there is no window where another user can read it.
pub fn write_pending_report(dir: &Path, report: &ErrorReport) -> Result<PathBuf, PendingError> {
    write_pending_report_at(dir, report, now_epoch_millis())
}

fn write_pending_report_at(
    dir: &Path,
    report: &ErrorReport,
    written_ms: u64,
) -> Result<PathBuf, PendingError> {
    let bytes = report.to_json()?;
    debug_assert!(bytes.len() <= MAX_REPORT_BYTES);

    create_private_dir(dir)?;

    let file_name = format!(
        "{written_ms}-{}-{}",
        WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        report.report_id
    );
    let final_path = dir.join(format!("{file_name}.{REPORT_FILE_EXTENSION}"));
    let temp_path = dir.join(format!("{file_name}.{REPORT_FILE_EXTENSION}.tmp"));

    write_private_file(&temp_path, &bytes)?;
    std::fs::rename(&temp_path, &final_path).map_err(|source| PendingError::Io {
        path: final_path.clone(),
        source,
    })?;

    prune(dir);

    Ok(final_path)
}

/// The pending reports in `dir`, oldest first. Unreadable entries are skipped.
pub fn pending_reports(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut files: Vec<((u64, u64), PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == REPORT_FILE_EXTENSION)
        })
        .filter_map(|path| Some((ordering_key(&path)?, path)))
        .collect();

    // By write millisecond, then by write order. Both are recorded in the name,
    // so neither a coarse filesystem timestamp nor the random report id can
    // decide which report is the oldest.
    files.sort();
    files.into_iter().map(|(_, path)| path).collect()
}

/// The write millisecond and write order a pending file's name records.
fn ordering_key(path: &Path) -> Option<(u64, u64)> {
    let name = path.file_name()?.to_str()?;
    let mut parts = name.splitn(3, '-');

    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// Drops the oldest files until at most [`MAX_PENDING_REPORTS`] remain.
fn prune(dir: &Path) {
    let files = pending_reports(dir);
    let excess = files.len().saturating_sub(MAX_PENDING_REPORTS);

    for path in files.iter().take(excess) {
        if let Err(error) = std::fs::remove_file(path) {
            write_hook_note(&format!(
                "pending report not removed at {}: {error}",
                path.display()
            ));
        }

        // The decision marker and the delete token belong to their report;
        // leaving them behind would keep a note about a file nobody can read
        // any more, and a token that deletes nothing.
        remove_decided_marker(path);
        remove_sent_token(path);
    }
}

/// Why a sent report could not be removed from the service, or from this
/// machine. Every variant is safe to show the user.
#[derive(Debug, thiserror::Error)]
pub enum DeleteError {
    #[error(
        "no delete token is kept for this report, so it cannot be deleted from the server here"
    )]
    NoToken,
    #[error("{0}")]
    Remote(#[from] SendError),
    #[error("the report is gone from the server, but {0}")]
    Local(String),
}

/// Removes one sent report from the service, then from this machine.
///
/// `delete` is the one request: the caller passes the sender's
/// [`crate::sender::ReportSender::delete`], so the network code stays where it
/// was and this function owns only what it can see — the token file and the
/// order the local files go in. A report is deleted only when it has a token
/// beside it; a missing or unreadable token is [`DeleteError::NoToken`], never
/// "delete anyway". Nothing is removed locally unless the service confirmed
/// the delete, and a local removal that fails after a successful delete says
/// so instead of hiding it.
pub fn delete_sent_report(
    report: &Path,
    delete: impl FnOnce(&SentToken) -> Result<(), SendError>,
) -> Result<(), DeleteError> {
    let token = match sent_token(report) {
        SentTokenLookup::Found(token) => token,
        SentTokenLookup::Missing | SentTokenLookup::Unreadable(_) => {
            return Err(DeleteError::NoToken);
        }
    };

    delete(&token)?;

    remove_local_report_files(report).map_err(DeleteError::Local)
}

/// Removes the token, the decision marker and the report itself.
///
/// A file that is already gone counts as removed. Everything that is left is
/// named in the answer, so the user learns which file stayed behind.
fn remove_local_report_files(report: &Path) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();

    for path in [
        sent_token_path(report),
        decided_marker_path(report),
        report.to_path_buf(),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => problems.push(format!("{}: {error}", path.display())),
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// One pending report as the send window and About read it: the file, the
/// exact bytes it holds, and whether the user already decided about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReport {
    pub path: PathBuf,
    /// The bytes of the file, unchanged: the same bytes the sender posts.
    pub bytes: Vec<u8>,
    /// The user answered this report: they declined it, or it was sent. The
    /// app stops asking about a decided report.
    pub decided: bool,
}

/// The readable pending reports in `dir`, oldest first.
///
/// A file that cannot be read is skipped, the same way [`pending_reports`]
/// skips an entry whose name carries no order.
pub fn readable_pending_reports(dir: &Path) -> Vec<PendingReport> {
    pending_reports(dir)
        .into_iter()
        .filter_map(|path| {
            let bytes = std::fs::read(&path).ok()?;

            Some(PendingReport {
                decided: is_decided(&path),
                path,
                bytes,
            })
        })
        .collect()
}

/// Marks that the user decided about the report at `path`.
///
/// Both answers settle a report — "Don't send", and a send that reached the
/// service — because neither leaves anything to ask: the file stays, and the
/// app stops offering it. The marker is an empty file beside the report whose
/// name adds [`DECIDED_MARKER_SUFFIX`], so the code never reads as if a send
/// were a refusal. It is not a report itself: the store lists only
/// [`REPORT_FILE_EXTENSION`] files, so a marker never becomes one.
pub fn mark_decided(path: &Path) -> Result<(), PendingError> {
    // A report that is already gone leaves nothing to decide, and writing the
    // marker anyway would leave a note about a file nobody can read.
    if !path.is_file() {
        return Ok(());
    }

    let marker = decided_marker_path(path);

    // Already decided: deciding again is the same answer, not an error.
    if marker.is_file() {
        return Ok(());
    }

    write_private_file(&marker, b"")
}

/// Whether the user already decided about the report at `path`.
pub fn is_decided(path: &Path) -> bool {
    decided_marker_path(path).is_file()
}

/// Removes the decision marker of `path`, if it has one.
fn remove_decided_marker(path: &Path) {
    let marker = decided_marker_path(path);

    match std::fs::remove_file(&marker) {
        Ok(()) => {}
        // No marker is the normal case: most reports were never answered.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => write_hook_note(&format!(
            "decision marker not removed at {}: {error}",
            marker.display()
        )),
    }
}

/// The marker file that records a decision beside the report at `path`.
///
/// The report's own name is kept whole, so the marker keeps its place in the
/// store's order and `<name>.json.decided` is never listed as a report.
fn decided_marker_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(DECIDED_MARKER_SUFFIX);
    PathBuf::from(name)
}

/// Writes one report copy to `path` for the user to send by email.
///
/// The file is created owner-only, and an existing file is replaced: the save
/// dialog already asked whether to overwrite it.
pub fn write_report_file(path: &Path, bytes: &[u8]) -> Result<(), PendingError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(|source| PendingError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    file.write_all(bytes).map_err(|source| PendingError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Creates `dir` owner-only on Unix.
///
/// New directories are created with mode `0o700`; an existing one is narrowed
/// as well, in case it was made by an older build at the process umask.
fn create_private_dir(dir: &Path) -> Result<(), PendingError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }

    builder.create(dir).map_err(|source| PendingError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(dir).map_err(|source| PendingError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(dir, permissions).map_err(|source| PendingError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    }

    Ok(())
}

/// Writes a file that is owner-only from the instant it exists.
///
/// On Unix the mode is set at creation, so the file never exists at the process
/// umask even briefly.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), PendingError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(|source| PendingError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    file.write_all(bytes).map_err(|source| PendingError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes one note to stderr without ever panicking.
///
/// This runs inside the panic hook, where a panic — `eprintln!` panics when the
/// write fails — would abort inside an unwinding thread. `write_all` returns the
/// error instead, and a failed stderr write is where the note ends.
fn write_hook_note(note: &str) {
    let line = format!("[datatab_diagnostics] {note}\n");

    // The same shape the panic hook in `crates/datatab/src/main.rs` uses: a
    // failed stderr write has nowhere left to be reported from inside the hook,
    // and `eprintln!` would panic there instead.
    let _ = std::io::stderr().write_all(line.as_bytes());
}

fn now_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
