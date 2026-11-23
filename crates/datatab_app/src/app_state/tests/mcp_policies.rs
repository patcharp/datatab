//! Policies whose class sets contradict the permission ladder (#490), at the
//! save boundary and at the load boundary of the app.

use super::*;

/// A policy that allows the top class while it asks about the classes below
/// it: the shape #490 refuses.
fn incoherent_policy() -> ToolPolicyDto {
    let to_ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect();

    ToolPolicyDto {
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

fn test_state() -> AppState {
    let runtime =
        datatab_storage::bootstrap::StorageRuntime::in_memory().expect("in-memory storage runtime");

    AppState::new_with_storage_runtime(runtime).expect("build app state")
}

fn stored_ids(state: &AppState) -> Vec<String> {
    state
        .list_mcp_policies()
        .expect("list policies")
        .into_iter()
        .map(|policy| policy.id)
        .collect()
}

/// The save boundary: `upsert_mcp_policy` itself refuses the shape, names the
/// pair, and stores nothing.
#[test]
fn upsert_mcp_policy_refuses_an_incoherent_policy_with_the_pair() {
    let mut state = test_state();

    let error = state
        .upsert_mcp_policy(incoherent_policy())
        .expect_err("an incoherent policy must be refused");

    for expected in ["custom", "AdminDestructive", "AdminSafe"] {
        assert!(error.contains(expected), "{error} must name {expected}");
    }

    assert!(!stored_ids(&state).contains(&"custom".to_string()));
}

/// A coherent policy still saves on the same path.
#[test]
fn upsert_mcp_policy_still_saves_a_coherent_policy() {
    let mut state = test_state();
    let mut policy = incoherent_policy();
    policy.allowed_classes = vec![
        "metadata".to_string(),
        "read".to_string(),
        "write".to_string(),
    ];
    policy.approval_classes = vec!["destructive".to_string()];

    state
        .upsert_mcp_policy(policy)
        .expect("a coherent policy must save");

    assert!(stored_ids(&state).contains(&"custom".to_string()));
}

/// The app's load boundary: a stored policy of that shape is kept, so Settings
/// lists it and a person can repair it, is reported once in the log with the
/// pair, and grants nothing because the engine is never built from it.
#[test]
fn bootstrap_keeps_an_incoherent_stored_policy_reports_it_and_grants_nothing() {
    let mut state = test_state();
    let stored = incoherent_policy();

    state
        .storage_runtime
        .governance_settings()
        .replace_tool_policies(&[
            datatab_storage::repositories::governance_settings::ToolPolicyDto {
                id: Uuid::new_v4().to_string(),
                governance_id: 1,
                policy_id: stored.id.clone(),
                allowed_tools: stored.allowed_tools.clone(),
                allowed_classes: stored.allowed_classes.clone(),
                approval_classes: stored.approval_classes.clone(),
            },
        ])
        .expect("seed the stored policy");

    let logs = super::external_secrets::capture_logs(|| {
        state
            .bootstrap_mcp_runtime_from_persistence()
            .expect("bootstrap keeps going past an incoherent stored policy");
    });

    assert!(
        logs.iter().any(|line| {
            line.contains("grants nothing")
                && line.contains("custom")
                && line.contains("AdminDestructive")
        }),
        "one warning names the policy and the pair: {logs:?}"
    );

    assert!(
        stored_ids(&state).contains(&"custom".to_string()),
        "the row stays visible so a person can repair it"
    );
    assert!(
        !state
            .mcp_runtime
            .policies_for_engine()
            .iter()
            .any(|policy| policy.id == "custom"),
        "the engine must not be built from it"
    );
}
