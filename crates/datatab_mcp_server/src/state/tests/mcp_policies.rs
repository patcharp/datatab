//! The standalone MCP server's load boundary for #490: a stored policy whose
//! class sets contradict the permission ladder is kept, reported once, and
//! grants nothing.

use datatab_core::{GovernanceSettings, ToolPolicyConfig};
use datatab_mcp::McpGovernanceService;

use super::super::apply_governance_settings;
use super::runtime_for_governance_tests;

/// Captures the lines this thread logs. A logger is global to a test binary, so
/// each binary that asserts on log output installs its own; the buffer itself
/// is thread-local, so tests do not read each other's lines.
struct ThreadLogCapture;

thread_local! {
    static CAPTURED_LOGS: std::cell::RefCell<Option<Vec<String>>> = const {
        std::cell::RefCell::new(None)
    };
}

impl log::Log for ThreadLogCapture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        CAPTURED_LOGS.with(|logs| {
            if let Some(logs) = logs.borrow_mut().as_mut() {
                logs.push(record.args().to_string());
            }
        });
    }

    fn flush(&self) {}
}

fn capture_logs(action: impl FnOnce()) -> Vec<String> {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        log::set_logger(&ThreadLogCapture).expect("test logger");
        log::set_max_level(log::LevelFilter::Trace);
    });
    CAPTURED_LOGS.with(|logs| *logs.borrow_mut() = Some(Vec::new()));
    action();
    CAPTURED_LOGS.with(|logs| logs.borrow_mut().take().expect("captured logs"))
}

/// The shape from #490: the top class allowed while the classes below it are
/// asked about.
fn incoherent_policy() -> ToolPolicyConfig {
    let to_ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect();

    ToolPolicyConfig {
        id: "custom".to_string(),
        allowed_tools: vec!["drop_table".to_string()],
        allowed_classes: to_ids(&[
            "metadata",
            "read",
            "write",
            "destructive",
            "admin_destructive",
        ]),
        approval_classes: to_ids(&["admin_safe", "admin"]),
    }
}

#[test]
fn apply_governance_settings_keeps_an_incoherent_stored_policy_reports_it_and_grants_nothing() {
    let mut runtime = runtime_for_governance_tests("governance-incoherent-policy.sqlite");

    let governance = GovernanceSettings {
        mcp_enabled_by_default: true,
        trusted_clients: vec![],
        roles: vec![],
        policies: vec![incoherent_policy()],
    };

    let logs = capture_logs(|| {
        apply_governance_settings(&mut runtime, &governance)
            .expect("the load keeps going past an incoherent stored policy");
    });

    assert!(
        logs.iter().any(|line| line.contains("grants nothing")
            && line.contains("custom")
            && line.contains("AdminDestructive")),
        "one warning names the policy and the pair: {logs:?}"
    );

    let policies = runtime.list_policies().expect("list policies");
    assert!(
        policies.iter().any(|policy| policy.id == "custom"),
        "the row stays in the runtime so the GUI can show and repair it"
    );
    assert!(
        !runtime
            .policies_for_engine()
            .iter()
            .any(|policy| policy.id == "custom"),
        "the engine must not be built from it"
    );
}
