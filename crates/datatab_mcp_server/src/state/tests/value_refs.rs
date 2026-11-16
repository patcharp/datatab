use super::*;

#[test]
fn unknown_value_ref_kind_rejects_only_affected_profile() {
    use datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto;

    let runtime = StorageRuntime::in_memory().expect("storage");
    let repo = runtime.connection_profiles();
    let profile_id = uuid::Uuid::new_v4();
    let mut profile = ConnectionProfileDto::new(profile_id, "unknown source".into());
    profile.kind = Some("Postgres".into());
    profile.save_password = true;
    repo.insert(&profile).expect("save profile");
    let mut reference = ConnectionProfileValueRefDto::new_literal(
        profile_id.to_string(),
        "password".into(),
        "stored value must not enter diagnostics".into(),
    );
    reference.ref_kind = "retired-kind".into();
    repo.value_refs()
        .insert(&reference)
        .expect("save unknown source");

    let valid_id = uuid::Uuid::new_v4();
    let mut valid = ConnectionProfileDto::new(valid_id, "unaffected PostgreSQL".into());
    valid.kind = Some("Postgres".into());
    repo.insert(&valid).expect("save valid profile");

    let expected = "Value ref 'password' has unknown source kind \"retired-kind\"";
    let (profiles, errors) = load_profiles(&runtime).expect("MCP profiles load despite bad source");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, valid_id);
    assert_eq!(profiles[0].kind(), datatab_core::DbKind::Postgres);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors.get(&profile_id).map(String::as_str), Some(expected));
    assert_eq!(
        load_profile_value_refs(&repo, &profile_id.to_string())
            .expect_err("reject unknown source field"),
        expected
    );
}

#[test]
fn malformed_known_value_refs_reject_only_affected_profile() {
    use datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto;

    for (kind, expected) in [
        ("dotenv", "Dotenv value ref 'password' is missing its key"),
        ("secret", "Secret value ref 'password' is missing provider"),
    ] {
        let runtime = StorageRuntime::in_memory().expect("storage");
        let repo = runtime.connection_profiles();
        let broken_id = uuid::Uuid::new_v4();
        let valid_id = uuid::Uuid::new_v4();
        for id in [broken_id, valid_id] {
            let mut profile = ConnectionProfileDto::new(id, id.to_string());
            profile.kind = Some("Postgres".into());
            repo.insert(&profile).expect("save profile");
        }
        let mut reference = ConnectionProfileValueRefDto::new_literal(
            broken_id.to_string(),
            "password".into(),
            "unused value".into(),
        );
        reference.ref_kind = kind.into();
        repo.value_refs()
            .insert(&reference)
            .expect("save malformed source");
        let (profiles, errors) = load_profiles(&runtime).expect("load other profiles");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, valid_id);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors.get(&broken_id).map(String::as_str), Some(expected));
    }
}

/// A complete stored row of the removed `param` kind, as the app wrote it
/// while the Parameter value source existed.
fn stored_param_reference(
    profile_id: uuid::Uuid,
) -> datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto {
    use datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto;

    let mut reference = ConnectionProfileValueRefDto::new_secret(
        profile_id.to_string(),
        "password".into(),
        "aws-ssm".into(),
        "/db/password".into(),
        None,
    );
    reference.ref_kind = "param".into();
    reference.secret_locator = None;
    reference.param_name = Some("/db/password".into());
    reference
}

fn retired_kind_reference(
    profile_id: uuid::Uuid,
) -> datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto {
    use datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto;

    let mut reference = ConnectionProfileValueRefDto::new_literal(
        profile_id.to_string(),
        "password".into(),
        "private stored value".into(),
    );
    reference.ref_kind = "retired-kind".into();
    reference
}

#[test]
fn stored_param_value_ref_rejects_only_affected_profile() {
    let runtime = StorageRuntime::in_memory().expect("storage");
    let repo = runtime.connection_profiles();
    let profile_id = uuid::Uuid::new_v4();
    let valid_id = uuid::Uuid::new_v4();
    for (id, name) in [(profile_id, "stored parameter"), (valid_id, "unaffected")] {
        let mut profile = ConnectionProfileDto::new(id, name.into());
        profile.kind = Some("Postgres".into());
        repo.insert(&profile).expect("save profile");
    }
    repo.value_refs()
        .insert(&stored_param_reference(profile_id))
        .expect("save param row");

    let expected = "Value ref 'password' has unknown source kind \"param\"";
    let (profiles, errors) = load_profiles(&runtime).expect("MCP profiles load despite param row");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, valid_id);
    assert_eq!(profiles[0].kind(), datatab_core::DbKind::Postgres);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors.get(&profile_id).map(String::as_str), Some(expected));
    assert_eq!(
        load_profile_value_refs(&repo, &profile_id.to_string())
            .expect_err("reject the stored param row"),
        expected
    );
}

/// Runs the test `test_name` again in a child process with its own data
/// directories. Only that child drives `check`.
fn run_in_isolated_child(test_name: &str, check: impl std::future::Future<Output = ()>) {
    const CHILD: &str = "DATATAB_TEST_PROFILE_ISOLATION";
    if std::env::var_os(CHILD).is_none() {
        let directory = tempfile::tempdir().expect("isolated server directory");
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD, "1")
            .env("XDG_DATA_HOME", directory.path().join("data"))
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env("XDG_STATE_HOME", directory.path().join("state"))
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .output()
            .expect("run isolated server test");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
            "isolated child did not run the expected test: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(check);
}

#[test]
fn server_starts_and_lists_postgres_despite_unknown_value_ref_kind() {
    run_in_isolated_child(
        "state::tests::value_refs::server_starts_and_lists_postgres_despite_unknown_value_ref_kind",
        assert_server_profile_isolation(
            retired_kind_reference,
            "Value ref 'password' has unknown source kind \"retired-kind\"",
        ),
    );
}

#[test]
fn server_starts_and_lists_postgres_despite_stored_param_value_ref() {
    run_in_isolated_child(
        "state::tests::value_refs::server_starts_and_lists_postgres_despite_stored_param_value_ref",
        assert_server_profile_isolation(
            stored_param_reference,
            "Value ref 'password' has unknown source kind \"param\"",
        ),
    );
}

async fn assert_server_profile_isolation(
    broken_reference: fn(
        uuid::Uuid,
    )
        -> datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto,
    expected: &'static str,
) {
    use datatab_storage::repositories::governance_settings::{
        GovernanceSettingsDto, TrustedClientDto,
    };
    let runtime = open_storage_runtime().expect("isolated storage");
    let repo = runtime.connection_profiles();
    let broken_id = uuid::Uuid::new_v4();
    let valid_id = uuid::Uuid::new_v4();
    for (id, name) in [(broken_id, "broken"), (valid_id, "valid PostgreSQL")] {
        let mut profile = ConnectionProfileDto::new(id, name.into());
        profile.kind = Some("Postgres".into());
        repo.insert(&profile).expect("save profile");
    }
    repo.value_refs()
        .insert(&broken_reference(broken_id))
        .expect("save unreadable source");
    #[allow(clippy::arc_with_non_send_sync)]
    let governance =
        GovernanceSettingsRepository::new(Arc::new(runtime.open_datatab_db().expect("database")));
    governance
        .upsert(&GovernanceSettingsDto {
            id: 1,
            mcp_enabled_by_default: 1,
            updated_at: String::new(),
        })
        .expect("settings");
    governance
        .replace_trusted_clients(&[TrustedClientDto {
            id: uuid::Uuid::new_v4().to_string(),
            governance_id: 1,
            client_id: "test-client".into(),
            name: "test client".into(),
            issuer: None,
            active: 1,
        }])
        .expect("trusted client");
    let state =
        ServerState::new("test-client".into(), None).expect("MCP starts with rejected profile");
    assert_eq!(
        state
            .profile_manager
            .read()
            .await
            .profile_load_error(broken_id),
        Some(expected)
    );
    assert_eq!(
        crate::server::DataTabServer::connect_impl(state.clone(), &broken_id.to_string())
            .await
            .expect_err("reject connection with unreadable value reference"),
        expected
    );
    state
        .runtime
        .write()
        .await
        .save_connection_policy_assignment_mut(datatab_mcp::ConnectionPolicyAssignmentDto {
            connection_id: valid_id.to_string(),
            assignments: vec![datatab_policy::ConnectionPolicyAssignment {
                actor_id: "test-client".into(),
                scope: datatab_policy::PolicyBindingScope {
                    connection_id: valid_id.to_string(),
                },
                role_ids: vec!["builtin/read-only".into()],
                policy_ids: vec![],
            }],
        })
        .expect("assign valid profile");
    assert_mcp_lists_postgres(state, valid_id).await;
}

async fn assert_mcp_lists_postgres(state: ServerState, valid_id: uuid::Uuid) {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::async_rw::AsyncRwTransport};
    let (server_io, client_io) = tokio::io::duplex(8192);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server, client) = tokio::try_join!(
        async {
            crate::server::DataTabServer::new(state)
                .serve(AsyncRwTransport::new_server(server_read, server_write))
                .await
                .map_err(|error| error.to_string())
        },
        async {
            rmcp::serve_client((), AsyncRwTransport::new_client(client_read, client_write))
                .await
                .map_err(|error| error.to_string())
        },
    )
    .expect("MCP handshake");
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("list_connections"))
        .await
        .expect("list connections");
    let content = result
        .content
        .first()
        .and_then(|block| block.as_text())
        .expect("JSON content");
    let listed: serde_json::Value = serde_json::from_str(&content.text).expect("connections JSON");
    assert_eq!(
        listed["connections"].as_array().expect("connections").len(),
        1
    );
    assert_eq!(listed["connections"][0]["id"], valid_id.to_string());
    assert_eq!(listed["connections"][0]["kind"], "Postgres");
    client.cancel().await.expect("stop client");
    server.cancel().await.expect("stop server");
}
