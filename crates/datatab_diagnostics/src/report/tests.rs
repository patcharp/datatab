use std::collections::BTreeSet;

use serde_json::Value;

use super::*;

/// The schema file is the allowlist the server pins; the struct must match it.
fn schema() -> Value {
    serde_json::from_str(include_str!("../../schema/error-report-v1.schema.json"))
        .expect("schema parses")
}

fn schema_keys() -> BTreeSet<String> {
    schema()["properties"]
        .as_object()
        .expect("schema has properties")
        .keys()
        .cloned()
        .collect()
}

fn required_keys() -> BTreeSet<String> {
    schema()["required"]
        .as_array()
        .expect("schema has required")
        .iter()
        .map(|value| value.as_str().expect("required entry").to_string())
        .collect()
}

fn keys_of(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("report is an object")
        .keys()
        .cloned()
        .collect()
}

fn full_report() -> ErrorReport {
    let mut report = ErrorReport::new(ReportKind::Error, "2026-11-23T05:00:00Z");
    report.error_kind = Some("driver".to_string());
    report.severity = Some("error".to_string());
    report.driver_id = Some("postgres".to_string());
    report.error_code = Some("08006".to_string());
    report.panic_message = Some("assertion failed".to_string());
    report.location = Some("crates/datatab/src/main.rs:184".to_string());
    report.frames = vec![Frame {
        function: Some("datatab::main".to_string()),
        module: Some("main.rs".to_string()),
        offset: Some(12),
    }];
    report.trail = vec![TrailEvent {
        category: "query".to_string(),
        action: "query_execute".to_string(),
        outcome: "success".to_string(),
        driver_id: Some("postgres".to_string()),
        seconds_before: 4,
    }];
    report
}

#[test]
fn full_report_keys_equal_the_schema() {
    let json = full_report().to_json().expect("serializes");
    let value: Value = serde_json::from_slice(&json).expect("valid json");

    assert_eq!(keys_of(&value), schema_keys());
}

#[test]
fn a_new_field_fails_the_allowlist_check() {
    let json = full_report().to_json().expect("serializes");
    let mut value: Value = serde_json::from_slice(&json).expect("valid json");

    value["query_text"] = Value::String("SELECT 1".to_string());

    assert_ne!(
        keys_of(&value),
        schema_keys(),
        "an extra key must break the allowlist comparison"
    );
}

#[test]
fn minimal_report_carries_only_required_keys() {
    let report = ErrorReport::new(ReportKind::Panic, "2026-11-23T05:00:00Z");
    let json = report.to_json().expect("serializes");
    let value: Value = serde_json::from_slice(&json).expect("valid json");

    let keys = keys_of(&value);

    assert!(required_keys().is_subset(&keys));
    assert!(keys.is_subset(&schema_keys()));
    // The only optional field a fresh report can carry is the OS release name,
    // which exists exactly when the platform tells us one.
    assert_eq!(
        keys.len(),
        required_keys().len() + usize::from(keys.contains("os_version"))
    );
}

#[test]
fn report_id_is_a_fresh_version_four_uuid() {
    let first = ErrorReport::new(ReportKind::Panic, "2026-11-23T05:00:00Z");
    let second = ErrorReport::new(ReportKind::Panic, "2026-11-23T05:00:00Z");

    assert_ne!(first.report_id, second.report_id);
    assert_eq!(first.report_id.get_version_num(), 4);
}

#[test]
fn report_over_the_byte_cap_is_refused() {
    let mut report = ErrorReport::new(ReportKind::Panic, "2026-11-23T05:00:00Z");
    report.frames = (0..MAX_FRAMES)
        .map(|index| Frame {
            function: Some(format!("datatab::{index}::{}", "x".repeat(1024))),
            module: Some("main.rs".to_string()),
            offset: Some(index as u64),
        })
        .collect();

    let error = report.to_json().expect_err("over the cap");

    assert!(matches!(error, ReportError::TooLarge { .. }));
}

#[test]
fn json_round_trips_through_the_pending_file_format() {
    let report = full_report();
    let json = report.to_json().expect("serializes");
    let read_back = ErrorReport::from_json(&json).expect("parses");

    assert_eq!(read_back, report);
}

#[test]
fn channel_and_version_come_from_the_running_build() {
    let report = ErrorReport::new(ReportKind::Error, "2026-11-23T05:00:00Z");

    assert_eq!(report.schema, SCHEMA_VERSION);
    assert_eq!(report.app_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        report.channel,
        datatab_core::ReleaseChannel::current().as_str()
    );
    assert_eq!(report.os, std::env::consts::OS);
    assert_eq!(report.arch, std::env::consts::ARCH);
}
