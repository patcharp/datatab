use chrono::TimeZone;

use super::*;
use crate::report::{MAX_TRAIL_EVENTS, ReportKind};
use datatab_core::observability::{EventCategory, EventOutcome, EventSeverity};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 23, 5, 42, 13).unwrap()
}

fn panic_input<'a>(message: PanicMessage<'a>) -> PanicInput<'a> {
    PanicInput {
        message,
        location: None,
        backtrace: backtrace::Backtrace::new_unresolved(),
        trail: Vec::new(),
        now: now(),
    }
}

#[test]
fn fixed_panic_text_is_kept_and_scrubbed() {
    let report = build_panic_report(panic_input(PanicMessage::Static(
        "open /home/alice/x.csv failed at db.example.com",
    )));

    let message = report.panic_message.expect("fixed text is kept");

    assert!(!message.contains("alice"));
    assert!(!message.contains("db.example.com"));
}

#[test]
fn a_panic_message_carrying_a_credential_is_dropped() {
    let report = build_panic_report(panic_input(PanicMessage::Static(
        "connect failed: PGPASSWORD=hunter2 database=app",
    )));

    assert!(
        report.panic_message.is_none(),
        "a credential message never ships"
    );
}

#[test]
fn a_panic_message_carrying_leak_shapes_keeps_none_of_them() {
    let message = "dial postgres:5432 and host=db2.internal user=alice from /home/höz/x.csv and C:\\Program Files\\DataTab\\app.exe with Authorization: Bearer abc";

    let report = build_panic_report(panic_input(PanicMessage::Static(message)));
    let bytes = String::from_utf8(report.to_json().expect("serializes")).expect("utf-8");

    for secret in [
        "postgres",
        "5432",
        "db2.internal",
        "alice",
        "höz",
        "x.csv",
        "Program Files",
        "app.exe",
        "abc",
    ] {
        assert!(!bytes.contains(secret), "{secret} reached the report bytes");
    }
}

#[test]
fn formatted_panic_text_becomes_a_length_marker() {
    let dynamic = format!("row {} had the wrong type", 42);

    let report = build_panic_report(panic_input(PanicMessage::Formatted(&dynamic)));

    assert_eq!(
        report.panic_message.as_deref(),
        Some(format!("<dropped: {} chars>", dynamic.chars().count()).as_str())
    );
}

#[test]
fn a_panic_message_holding_a_query_is_dropped() {
    let report = build_panic_report(panic_input(PanicMessage::Static(
        "SELECT email FROM customers failed",
    )));

    assert!(report.panic_message.is_none());
}

#[test]
fn unknown_payload_carries_no_message() {
    let report = build_panic_report(panic_input(PanicMessage::Unknown));

    assert!(report.panic_message.is_none());
}

#[test]
fn panic_report_has_panic_kind_and_the_hour() {
    let report = build_panic_report(panic_input(PanicMessage::Unknown));

    assert_eq!(report.kind, ReportKind::Panic);
    assert_eq!(report.created_hour, "2026-11-23T05:00:00Z");
    assert!(report.error_kind.is_none());
    assert!(report.severity.is_none());
}

#[test]
fn workspace_paths_keep_only_the_crates_part() {
    assert_eq!(
        reduce_location("/home/alice/datatab/crates/datatab/src/main.rs", 184).as_deref(),
        Some("crates/datatab/src/main.rs:184")
    );
    assert_eq!(
        reduce_location("C:\\build\\datatab\\crates\\datatab_core\\src\\lib.rs", 12).as_deref(),
        Some("crates/datatab_core/src/lib.rs:12")
    );
}

#[test]
fn dependency_paths_keep_the_crate_directory() {
    assert_eq!(
        reduce_location(
            "/home/alice/.cargo/registry/src/index.crates.io-1234/serde-1.0.0/src/de.rs",
            99
        )
        .as_deref(),
        Some("serde-1.0.0/src/de.rs:99")
    );
}

#[test]
fn anything_else_is_external() {
    assert_eq!(
        reduce_location("/opt/nightly-run/panic.rs", 7).as_deref(),
        Some("<external>:7")
    );
}

#[test]
fn driver_ids_are_closed_to_builtin_names_or_rpc() {
    assert_eq!(closed_driver_id("postgres").as_deref(), Some("postgres"));
    assert_eq!(closed_driver_id("duckdb").as_deref(), Some("duckdb"));
    assert_eq!(
        closed_driver_id("rpc:/run/user/1000/datatab/peer.sock").as_deref(),
        Some("rpc")
    );
    assert_eq!(closed_driver_id("rpc").as_deref(), Some("rpc"));
    assert_eq!(closed_driver_id("my-driver"), None);
    assert_eq!(closed_driver_id("reserved:s3"), None);
}

#[test]
fn error_codes_must_match_the_pattern() {
    assert_eq!(closed_error_code("08006").as_deref(), Some("08006"));
    assert_eq!(
        closed_error_code("auth.failed-1").as_deref(),
        Some("auth.failed-1")
    );
    assert_eq!(closed_error_code("has space"), None);
    assert_eq!(closed_error_code("query;DROP"), None);
    assert_eq!(closed_error_code(""), None);
    assert_eq!(closed_error_code(&"a".repeat(17)), None);
}

#[test]
fn error_report_carries_the_closed_kind_severity_and_code() {
    let report = build_error_report(ErrorInput {
        error_kind: "driver".to_string(),
        severity: EventSeverity::Warn,
        driver_id: Some("mysql:3306/internal".to_string()),
        error_code: Some("1045".to_string()),
        trail: Vec::new(),
        now: now(),
    });

    assert_eq!(report.kind, ReportKind::Error);
    assert_eq!(report.error_kind.as_deref(), Some("driver"));
    assert_eq!(report.severity.as_deref(), Some("warn"));
    assert_eq!(report.driver_id, None, "not a closed driver id");
    assert_eq!(report.error_code.as_deref(), Some("1045"));
}

#[test]
fn an_error_kind_outside_the_list_is_dropped() {
    let report = build_error_report(ErrorInput {
        error_kind: "mystery".to_string(),
        severity: EventSeverity::Error,
        driver_id: None,
        error_code: None,
        trail: Vec::new(),
        now: now(),
    });

    assert!(report.error_kind.is_none());
}

fn recent(seconds_ago: i64, action: &str, driver: Option<&str>) -> datatab_audit::RecentEvent {
    let now_ms = now().timestamp_millis();

    datatab_audit::RecentEvent {
        ts_ms: now_ms - seconds_ago * 1000,
        category: EventCategory::Query,
        action: action.to_string(),
        outcome: EventOutcome::Failure,
        driver_id: driver.map(str::to_string),
    }
}

#[test]
fn trail_keeps_the_newest_ten_events_oldest_first() {
    let events: Vec<_> = (0..15)
        .map(|index| recent(15 - index, "query_execute", Some("postgres")))
        .collect();

    let report = build_error_report(ErrorInput {
        error_kind: "network".to_string(),
        severity: EventSeverity::Error,
        driver_id: Some("postgres".to_string()),
        error_code: None,
        trail: events,
        now: now(),
    });

    assert_eq!(report.trail.len(), MAX_TRAIL_EVENTS);
    assert_eq!(report.trail.first().unwrap().seconds_before, 10);
    assert_eq!(report.trail.last().unwrap().seconds_before, 1);
}

#[test]
fn trail_actions_are_closed_and_driver_ids_collapsed() {
    let report = build_error_report(ErrorInput {
        error_kind: "driver".to_string(),
        severity: EventSeverity::Error,
        driver_id: Some("rpc:/tmp/peer.sock".to_string()),
        error_code: None,
        trail: vec![recent(5, "user_error", Some("rpc:/tmp/peer.sock"))],
        now: now(),
    });

    assert_eq!(report.trail[0].action, "other");
    assert_eq!(report.trail[0].driver_id.as_deref(), Some("rpc"));
    assert_eq!(report.driver_id.as_deref(), Some("rpc"));
    assert_eq!(report.trail[0].category, "query");
    assert_eq!(report.trail[0].outcome, "failure");
}

#[test]
fn a_captured_backtrace_yields_bounded_frames_without_directories() {
    let report = build_panic_report(PanicInput {
        message: PanicMessage::Unknown,
        location: None,
        backtrace: backtrace::Backtrace::new(),
        trail: Vec::new(),
        now: now(),
    });

    assert!(!report.frames.is_empty(), "a captured backtrace has frames");
    assert!(report.frames.len() <= crate::report::MAX_FRAMES);

    for frame in &report.frames {
        if let Some(module) = &frame.module {
            assert!(
                !module.contains('/') && !module.contains('\\'),
                "module kept a directory: {module}"
            );
        }
    }
}

#[test]
fn created_hour_never_carries_minutes() {
    let early = Utc.with_ymd_and_hms(2026, 1, 2, 3, 0, 0).unwrap();

    assert_eq!(created_hour(early), "2026-01-02T03:00:00Z");
}
