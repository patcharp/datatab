use std::path::Path;

use tempfile::TempDir;

use super::*;
use crate::report::{ErrorReport, ReportKind};

fn report() -> ErrorReport {
    ErrorReport::new(ReportKind::Panic, "2026-11-23T05:00:00Z")
}

fn report_with(report_id: uuid::Uuid) -> ErrorReport {
    let mut report = report();
    report.report_id = report_id;
    report
}

/// A stable version-four UUID, so a test can control which id sorts first.
fn numbered_uuid(step: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0000 + step)
}

#[test]
fn one_file_holds_the_exact_bytes_of_the_report() {
    let dir = TempDir::new().expect("temp dir");
    let report = report();

    let path = write_pending_report(dir.path(), &report).expect("written");

    let written = std::fs::read(&path).expect("read back");
    assert_eq!(written, report.to_json().expect("serializes"));

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("name");
    assert!(
        name.ends_with(&format!("-{}.json", report.report_id)),
        "{name}"
    );
}

#[test]
fn the_cap_keeps_five_reports_and_drops_the_oldest() {
    let dir = TempDir::new().expect("temp dir");
    let mut written = Vec::new();

    for index in 0..(MAX_PENDING_REPORTS + 2) {
        let path =
            write_pending_report_at(dir.path(), &report(), 1_000 + index as u64).expect("written");
        written.push(path);
    }

    let remaining = pending_reports(dir.path());

    assert_eq!(remaining.len(), 5, "ADR 0008 keeps at most five files");
    assert!(!remaining.contains(&written[0]), "oldest must go");
    assert!(!remaining.contains(&written[1]), "second oldest must go");
    assert!(remaining.contains(&written[written.len() - 1]));
}

#[test]
fn the_cap_breaks_ties_by_age_not_by_id() {
    let dir = TempDir::new().expect("temp dir");
    let same_millisecond = 7_000;

    // Reports written in the same millisecond, newest id first, so ordering by
    // the id would drop the wrong two: only the write order may decide.
    let mut written = Vec::new();
    for step in (1u128..=(MAX_PENDING_REPORTS as u128 + 2)).rev() {
        let id = numbered_uuid(step);
        let path = write_pending_report_at(dir.path(), &report_with(id), same_millisecond)
            .expect("written");
        written.push(path);
    }

    let remaining = pending_reports(dir.path());

    assert_eq!(remaining.len(), MAX_PENDING_REPORTS);
    assert!(
        !remaining.contains(&written[0]),
        "the first report written is the oldest and must go"
    );
    assert!(!remaining.contains(&written[1]), "the second must go too");
    assert!(
        remaining.contains(&written[written.len() - 1]),
        "the newest stays"
    );
}

#[test]
fn the_oldest_is_decided_by_the_recorded_instant_even_if_the_file_time_lies() {
    let dir = TempDir::new().expect("temp dir");

    let newest = write_pending_report_at(dir.path(), &report(), 9_000).expect("written");
    let oldest = write_pending_report_at(dir.path(), &report(), 1_000).expect("written");
    for _ in 0..(MAX_PENDING_REPORTS - 2) {
        write_pending_report_at(dir.path(), &report(), 5_000).expect("written");
    }

    // Make the filesystem timestamps contradict the recorded instants: the
    // filesystem calls the newest report the oldest one.
    set_modified(&newest, 1);
    set_modified(&oldest, 9_999);

    write_pending_report_at(dir.path(), &report(), 11_000).expect("written");

    let remaining = pending_reports(dir.path());
    assert!(
        !remaining.contains(&oldest),
        "the recorded-oldest report goes, whatever the file time says"
    );
    assert!(
        remaining.contains(&newest),
        "the recorded-newest report stays"
    );
}

#[test]
fn a_new_file_is_owner_only_from_creation() {
    let dir = TempDir::new().expect("temp dir");

    let path = write_pending_report(dir.path(), &report()).expect("written");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[test]
fn a_missing_directory_is_created() {
    let dir = TempDir::new().expect("temp dir");
    let nested = dir.path().join("reports").join("pending");

    let path = write_pending_report(&nested, &report()).expect("written to a new directory");

    assert!(path.exists());
}

#[test]
fn pending_reports_on_a_missing_directory_is_empty() {
    let dir = TempDir::new().expect("temp dir");

    assert!(pending_reports(&dir.path().join("absent")).is_empty());
}

#[test]
fn the_write_instant_and_order_are_read_back_from_the_name() {
    let dir = TempDir::new().expect("temp dir");
    let first = write_pending_report_at(dir.path(), &report(), 1_234_567).expect("written");
    let second = write_pending_report_at(dir.path(), &report(), 1_234_567).expect("written");

    let (first_ms, first_order) = ordering_key(&first).expect("key");
    let (second_ms, second_order) = ordering_key(&second).expect("key");

    assert_eq!(first_ms, 1_234_567);
    assert_eq!(second_ms, 1_234_567);
    assert!(first_order < second_order, "write order must be recorded");
    assert_eq!(ordering_key(Path::new("not-a-report.json")), None);
}

fn set_modified(path: &Path, seconds: u64) {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open for mtime");
    file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
        .expect("set mtime");
}

#[test]
fn deciding_keeps_the_report_and_marks_it() {
    let dir = TempDir::new().expect("temp dir");
    let report = report();
    let path = write_pending_report(dir.path(), &report).expect("written");

    mark_decided(&path).expect("decided");

    assert!(path.is_file(), "deciding must not delete the report");
    assert_eq!(
        std::fs::read(&path).expect("read back"),
        report.to_json().expect("serializes"),
        "the decided report keeps its exact bytes"
    );
    assert!(is_decided(&path));
}

#[test]
fn a_decided_report_is_still_readable_and_not_a_second_report() {
    let dir = TempDir::new().expect("temp dir");
    let report = report();
    let path = write_pending_report(dir.path(), &report).expect("written");
    mark_decided(&path).expect("decided");

    let listed = readable_pending_reports(dir.path());

    assert_eq!(listed.len(), 1, "the marker is not a report of its own");
    assert_eq!(listed[0].path, path);
    assert_eq!(listed[0].bytes, report.to_json().expect("serializes"));
    assert!(listed[0].decided);
}

#[test]
fn deciding_twice_is_the_same_answer() {
    let dir = TempDir::new().expect("temp dir");
    let path = write_pending_report(dir.path(), &report()).expect("written");

    mark_decided(&path).expect("first decision");
    mark_decided(&path).expect("second decision");

    assert!(is_decided(&path));
    assert_eq!(pending_reports(dir.path()).len(), 1);
}

#[test]
fn an_undecided_report_reads_back_as_offered() {
    let dir = TempDir::new().expect("temp dir");
    write_pending_report(dir.path(), &report()).expect("written");

    let listed = readable_pending_reports(dir.path());

    assert_eq!(listed.len(), 1);
    assert!(!listed[0].decided);
    assert!(!is_decided(&listed[0].path));
}

#[test]
fn pruning_a_report_removes_its_decision_marker() {
    let dir = TempDir::new().expect("temp dir");
    let first = write_pending_report_at(dir.path(), &report(), 1_000).expect("written");
    mark_decided(&first).expect("decided");

    for index in 0..MAX_PENDING_REPORTS {
        write_pending_report_at(dir.path(), &report(), 2_000 + index as u64).expect("written");
    }

    assert!(!first.exists(), "the oldest report goes");
    assert!(
        !is_decided(&first),
        "the marker of a removed report goes with it"
    );
}

#[test]
fn a_saved_copy_holds_the_exact_bytes() {
    let dir = TempDir::new().expect("temp dir");
    let report = report();
    let bytes = report.to_json().expect("serializes");
    let target = dir.path().join("report.json");

    write_report_file(&target, &bytes).expect("saved");

    assert_eq!(std::fs::read(&target).expect("read back"), bytes);
}

#[test]
fn a_saved_copy_replaces_a_file_the_dialog_agreed_to_overwrite() {
    let dir = TempDir::new().expect("temp dir");
    let target = dir.path().join("report.json");
    std::fs::write(&target, b"old contents").expect("seed");

    write_report_file(&target, b"new").expect("saved");

    assert_eq!(std::fs::read(&target).expect("read back"), b"new");
}

#[test]
fn a_decision_on_a_report_that_is_gone_leaves_no_orphan_marker() {
    let dir = TempDir::new().expect("temp dir");
    let path = write_pending_report(dir.path(), &report()).expect("written");
    std::fs::remove_file(&path).expect("the report was pruned");

    mark_decided(&path).expect("a decision on a gone report is not an error");

    assert!(
        !is_decided(&path),
        "a marker for a report nobody can read must not be written"
    );
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
