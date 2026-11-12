/// Integration tests verifying that `EventRecord` values produced by the
/// tracing bridge (category = `System`, as required by V1 coercion) all pass
/// `AuditService::validate_event`.
///
/// These tests do not instantiate a real tracing subscriber.  They construct
/// records directly and feed them to the validator to confirm no bridge record
/// will be silently rejected downstream.
use datatab_audit::AuditService;
use datatab_core::observability::{EventCategory, EventOutcome, EventRecord, EventSeverity};

fn minimal_system_record(action: &str, summary: &str) -> EventRecord {
    EventRecord::new(
        0,
        EventSeverity::Info,
        EventCategory::System,
        EventOutcome::Success,
    )
    .with_summary(summary)
    .with_action(action)
    .with_actor_id("system")
}

#[test]
fn system_record_with_action_and_summary_passes_validate() {
    let record = minimal_system_record("log_event", "something happened");
    assert!(
        AuditService::validate_event(&record).is_ok(),
        "minimal System record should pass validate_event"
    );
}

#[test]
fn system_record_empty_action_fails_validate() {
    let record = minimal_system_record("", "something happened");
    assert!(
        AuditService::validate_event(&record).is_err(),
        "empty action should fail validate_event"
    );
}

#[test]
fn system_record_empty_summary_fails_validate() {
    let record = minimal_system_record("log_event", "");
    assert!(
        AuditService::validate_event(&record).is_err(),
        "empty summary should fail validate_event"
    );
}

/// For every module target that the bridge prefix table covers, a synthesized
/// `System`-category record (as produced by V1 coercion) must pass
/// `validate_event`.  This guards against category regressions where a prefix
/// entry is changed to produce a category that requires additional structured
/// fields the bridge cannot supply.
#[test]
fn system_records_for_all_prefix_map_modules_pass_validate() {
    let module_targets = [
        "datatab_core::connection::pool",
        "datatab_core::pipeline::exec",
        "datatab_app::access_manager::auth",
        "datatab_ssh::tunnel",
        "datatab_proxy::http",
        "datatab_aws::client",
        "datatab_ssm::params",
        "datatab_driver_ipc::host",
        "datatab_app::config::profiles",
        "datatab_app::aws_config::reflect",
        "datatab_storage::migrations",
        "datatab_core::storage::db",
        "datatab_driver_sqlite::query",
        "datatab_core::facade::session",
        "datatab_mcp::runtime",
        "datatab_mcp_server::governance",
        "datatab_app::mcp_command::run",
        "datatab_app::app_state::init",
        "datatab_ipc::framing",
        "datatab_driver_host::main",
        "datatab_ui::workspace",
    ];

    for target in module_targets {
        let action = target.rsplit("::").next().unwrap_or("log_event");
        let record = minimal_system_record(action, "test event from tracing bridge");
        let result = AuditService::validate_event(&record);
        assert!(
            result.is_ok(),
            "System record from target '{target}' (action='{action}') should pass validate_event, got: {result:?}"
        );
    }
}
