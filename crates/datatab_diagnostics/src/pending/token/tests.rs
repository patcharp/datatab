//! The token file beside a sent report, and the delete that uses it.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::pending::{delete_sent_report, is_decided, mark_decided, pending_reports};
use crate::report::{ErrorReport, ReportKind};
use crate::sender::ReportSender;
use crate::test_http::serve_once;

const REPORT_ID: &str = "6f1c6c2e-6d1e-4c50-9b34-91a1a2b3c4d5";
const TOKEN: &str = "tok-456";

fn report() -> ErrorReport {
    ErrorReport::new(ReportKind::Panic, "2026-11-23T05:00:00Z")
}

fn submitted(report_id: Uuid, delete_token: &str) -> SubmittedReport {
    SubmittedReport {
        report_id,
        delete_token: delete_token.to_string(),
    }
}

/// The token file a report at `path` keeps beside it.
fn token_path(path: &Path) -> std::path::PathBuf {
    sent_token_path(path)
}

/// A sent report: the file, the decision marker and the token beside it.
fn sent_report(dir: &Path) -> (std::path::PathBuf, Uuid) {
    let report_id = Uuid::parse_str(REPORT_ID).expect("fixed id");
    let path = crate::pending::write_pending_report(dir, &report()).expect("written");
    write_sent_token(&path, &submitted(report_id, TOKEN)).expect("token written");
    mark_decided(&path).expect("sent reports are decided");

    (path, report_id)
}

#[test]
fn a_sent_report_keeps_an_owner_only_token_file_with_the_id() {
    let dir = TempDir::new().expect("temp dir");
    let path = crate::pending::write_pending_report(dir.path(), &report()).expect("written");
    let report_id = Uuid::parse_str(REPORT_ID).expect("fixed id");

    let written = write_sent_token(&path, &submitted(report_id, TOKEN)).expect("token written");

    assert_eq!(
        sent_token(&path),
        SentTokenLookup::Found(SentToken {
            report_id,
            delete_token: TOKEN.to_string(),
        }),
        "the token file holds the id and the token the service returned"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&written)
            .expect("token metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the token is owner-only from creation");
    }

    assert_eq!(
        pending_reports(dir.path()).len(),
        1,
        "a token file is never listed as a report"
    );
}

#[test]
fn delete_posts_the_token_and_the_id_and_removes_the_files_on_success() {
    let dir = TempDir::new().expect("temp dir");
    let (path, report_id) = sent_report(dir.path());

    let (addr, server) =
        serve_once("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let endpoint = format!("http://{addr}");

    delete_sent_report(&path, |sent| {
        ReportSender::for_test(&endpoint)
            .expect("test sender")
            .delete(sent.report_id, &sent.delete_token)
    })
    .expect("delete succeeds");

    let request = server.join().expect("server thread");
    assert!(
        request.starts_with(&format!("DELETE /v1/reports/{report_id} HTTP/1.1\r\n")),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {TOKEN}")),
        "the token from the file is the bearer credential: {request}"
    );

    assert!(!path.is_file(), "the report itself is removed");
    assert!(!is_decided(&path), "the decision marker is removed");
    assert_eq!(
        sent_token(&path),
        SentTokenLookup::Missing,
        "the token is removed"
    );
    assert!(pending_reports(dir.path()).is_empty(), "nothing is left");
}

#[test]
fn a_failed_delete_keeps_every_local_file_and_says_why() {
    let dir = TempDir::new().expect("temp dir");
    let (path, _) = sent_report(dir.path());

    let (addr, server) =
        serve_once("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let endpoint = format!("http://{addr}");

    let error = delete_sent_report(&path, |sent| {
        ReportSender::for_test(&endpoint)
            .expect("test sender")
            .delete(sent.report_id, &sent.delete_token)
    })
    .expect_err("the service refused the token");
    let request = server.join().expect("server thread");

    assert!(request.starts_with("DELETE "), "{request}");
    assert!(
        matches!(error, crate::pending::DeleteError::Remote(_)),
        "the refusal is the reason: {error:?}"
    );
    assert!(error.to_string().contains("403"), "{error}");

    assert!(path.is_file(), "the report stays");
    assert!(is_decided(&path), "the marker stays");
    assert!(
        matches!(sent_token(&path), SentTokenLookup::Found(_)),
        "the token stays"
    );
}

#[test]
fn a_report_without_a_token_is_not_deleted() {
    let dir = TempDir::new().expect("temp dir");
    let path = crate::pending::write_pending_report(dir.path(), &report()).expect("written");
    mark_decided(&path).expect("decided");
    let called = AtomicBool::new(false);

    let error = delete_sent_report(&path, |_| {
        called.store(true, Ordering::Relaxed);
        Ok(())
    })
    .expect_err("nothing to delete with");

    assert!(matches!(error, crate::pending::DeleteError::NoToken));
    assert!(
        !called.load(Ordering::Relaxed),
        "a report without a token must not reach the service"
    );
    assert!(path.is_file(), "nothing local is removed");
}

#[test]
fn a_corrupt_token_file_is_treated_as_no_token() {
    let dir = TempDir::new().expect("temp dir");
    let path = crate::pending::write_pending_report(dir.path(), &report()).expect("written");
    std::fs::write(token_path(&path), b"not a token at all").expect("corrupt token file");
    let called = AtomicBool::new(false);

    assert!(
        matches!(sent_token(&path), SentTokenLookup::Unreadable(_)),
        "a damaged file is reported as unreadable"
    );

    let error = delete_sent_report(&path, |_| {
        called.store(true, Ordering::Relaxed);
        Ok(())
    })
    .expect_err("a damaged token is no token");

    assert!(matches!(error, crate::pending::DeleteError::NoToken));
    assert!(
        !called.load(Ordering::Relaxed),
        "a damaged token must never delete anything"
    );
    assert!(path.is_file());
    assert!(
        token_path(&path).is_file(),
        "the damaged file is left for the user to see"
    );
}

#[test]
fn an_empty_token_is_not_a_token() {
    let dir = TempDir::new().expect("temp dir");
    let path = crate::pending::write_pending_report(dir.path(), &report()).expect("written");
    let empty = format!(r#"{{"report_id":"{REPORT_ID}","delete_token":""}}"#);
    std::fs::write(token_path(&path), empty.as_bytes()).expect("empty token file");

    assert!(
        matches!(sent_token(&path), SentTokenLookup::Unreadable(_)),
        "an empty bearer credential is a damaged file, not a token"
    );
}

#[test]
fn a_pruned_report_takes_its_token_with_it() {
    let dir = TempDir::new().expect("temp dir");
    let mut written = Vec::new();

    for _ in 0..(crate::pending::MAX_PENDING_REPORTS + 1) {
        let path = crate::pending::write_pending_report(dir.path(), &report()).expect("written");
        write_sent_token(&path, &submitted(Uuid::new_v4(), TOKEN)).expect("token written");
        written.push(path);
    }

    assert!(
        !written[0].is_file(),
        "the oldest report leaves the store at the cap"
    );
    assert!(
        !token_path(&written[0]).exists(),
        "a token that belongs to a removed report deletes nothing and goes too"
    );
    assert!(token_path(&written[written.len() - 1]).is_file());
}
