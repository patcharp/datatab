use super::*;

const REMOVED_PROVIDER: &str = "aws-ssm";

fn stored_managed_and_postgres() -> (AppState, ConnectionProfile, ConnectionProfile) {
    let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
    let mut managed = ConnectionProfile::new("managed", DbConfig::default_postgres());
    managed.access_kind = Some(AccessKind::Managed {
        provider: REMOVED_PROVIDER.into(),
        params: HashMap::from([
            ("instance_id".to_string(), "i-fixture".to_string()),
            ("region".to_string(), "eu-west-1".to_string()),
            ("remote_port".to_string(), "5432".to_string()),
        ]),
    });
    let postgres = ConnectionProfile::new("unaffected", DbConfig::default_postgres());
    crate::config_loader::save_profiles(&runtime, &[managed.clone(), postgres.clone()])
        .expect("save profiles");
    let state = AppState::new_with_storage_runtime(runtime).expect("load state");
    (state, managed, postgres)
}

fn loaded(state: &AppState, id: Uuid) -> ConnectionProfile {
    state
        .profiles()
        .iter()
        .find(|profile| profile.id == id)
        .expect("profile is listed")
        .clone()
}

/// A stored Managed profile of the removed SSM provider is ordinary data: it
/// loads next to a PostgreSQL profile, and connecting it fails closed.
#[tokio::test]
#[cfg(feature = "postgres")]
async fn managed_aws_ssm_profile_loads_beside_postgres_and_fails_closed() {
    let (mut state, managed, postgres) = stored_managed_and_postgres();

    assert_eq!(state.profiles().len(), 2);
    let mut profile = loaded(&state, managed.id);
    let access_kind = profile.access_kind.clone().expect("stored access kind");
    match &access_kind {
        AccessKind::Managed { provider, params } => {
            assert_eq!(provider, REMOVED_PROVIDER);
            assert_eq!(params.len(), 3);
            assert_eq!(params["instance_id"], "i-fixture");
        }
        other => panic!("expected Managed, got {other:?}"),
    }
    assert!(loaded(&state, postgres.id).access_kind.is_none());
    for id in [managed.id, postgres.id] {
        assert!(state.value_ref_diagnostics(id).is_empty());
        assert_eq!(state.validate_connect_profile(id), Ok(()));
    }
    #[cfg(feature = "mcp")]
    {
        let (profiles, errors) = datatab_mcp_server::state::load_profiles(state.storage_runtime())
            .expect("MCP loads both profiles");
        assert_eq!(profiles.len(), 2);
        assert!(errors.is_empty(), "{errors:?}");
    }

    match state.build_pipeline_input_for_profile(profile.clone(), CancelToken::new()) {
        Err(error) => assert_eq!(
            error,
            "Managed access requires an auth profile. Select one in Access > Auth Profile."
        ),
        Ok(_) => panic!("managed access without an auth profile reached the pipeline"),
    }

    let auth_profile = AuthProfile::new("Fixture", "fixture-auth", HashMap::new());
    profile.auth_profile_id = Some(auth_profile.id);
    state.facade.auth_profiles.items.push(auth_profile);
    state
        .auth_provider_registry
        .register(Arc::new(TestAuthProvider::new("fixture-auth")));
    let input = state
        .build_pipeline_input_for_profile(profile, CancelToken::new())
        .expect("pipeline preparation");
    let error = match input
        .access_manager
        .open(&access_kind, "127.0.0.1", 5432)
        .await
    {
        Err(DbError::ConnectionFailed(error)) => error,
        _ => panic!("the removed provider must not open an access path"),
    };
    assert_eq!(
        error.message,
        "Unknown managed access provider: 'aws-ssm'. No handler registered."
    );
}
