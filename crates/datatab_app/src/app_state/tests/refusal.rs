use super::*;

pub(super) struct PanicOnReadSecretStore;

impl SecretStore for PanicOnReadSecretStore {
    fn is_available(&self) -> bool {
        true
    }

    fn get(&self, _: &str) -> Result<Option<SecretString>, DbError> {
        panic!("refused profile read the secret store")
    }

    fn set(&self, _: &str, _: &SecretString) -> Result<(), DbError> {
        panic!("refused profile wrote the secret store")
    }

    fn delete(&self, _: &str) -> Result<(), DbError> {
        panic!("refused profile deleted from the secret store")
    }
}

fn legacy_refusing_state(profile: ConnectionProfile) -> AppState {
    assert!(profile.access_kind.is_none());
    let mut state = test_state_with_profiles(
        test_drivers(&profile.driver_id(), false, true),
        vec![profile],
    );
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));
    state
}

fn assert_legacy_refused(state: &AppState, profile_id: Uuid) {
    match state.prepare_connect_profile_with_passphrase(profile_id, None) {
        Err(PrepareConnectError::ProfileRefused { error }) => {
            assert!(error.contains("fixture refusal"), "{error}");
        }
        Err(error) => panic!("expected driver refusal, got {error}"),
        Ok(_) => panic!("refused profile reached PreConnect hooks"),
    }
}

#[test]
fn legacy_app_prepare_refuses_before_proxy_secret_read() {
    let mut profile = ConnectionProfile::new("refused", DbConfig::default_postgres());
    let mut proxy = datatab_core::ProxyProfile::new("fixture", datatab_core::ProxyKind::Socks5);
    proxy.save_secret = true;
    profile.proxy_profile_id = Some(proxy.id);
    let profile_id = profile.id;
    let mut state = legacy_refusing_state(profile);
    state.facade.proxies.items.push(proxy);

    assert_legacy_refused(&state, profile_id);
}

#[test]
fn legacy_app_prepare_refuses_before_ssh_secret_read() {
    let mut config = DbConfig::default_postgres();
    if let DbConfig::Postgres { ssh_tunnel, .. } = &mut config {
        *ssh_tunnel = Some(datatab_core::SshTunnelConfig {
            host: "127.0.0.1".into(),
            port: 22,
            user: "fixture".into(),
            auth_method: datatab_core::SshAuthMethod::Password,
            ssh_config_host: None,
        });
    }
    let profile = ConnectionProfile::new("refused", config);
    let profile_id = profile.id;
    let state = legacy_refusing_state(profile);

    assert_legacy_refused(&state, profile_id);
}

/// A profile whose driver id has no registered driver stays in the profile
/// list. Both the refusal check and the connect preparation must return the
/// generic `DriverNotRegistered` error before any secret read, and must never
/// panic.
#[test]
fn profile_with_unregistered_driver_is_refused_generically() {
    let profile = ConnectionProfile::new_with_driver(
        "saved-with-removed-driver",
        DbKind::Postgres,
        "removed-driver",
        DbConfig::default_postgres(),
    );
    let profile_id = profile.id;
    let mut state = test_state_with_profiles(HashMap::new(), vec![profile]);
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));

    assert!(
        state
            .profiles()
            .iter()
            .any(|profile| profile.id == profile_id),
        "a profile with an unregistered driver must stay in the profile list"
    );

    let expected = PrepareConnectError::DriverNotRegistered {
        driver_id: "removed-driver".to_string(),
    };

    match state.validate_connect_profile(profile_id) {
        Err(error) => assert_eq!(error, expected),
        Ok(()) => panic!("an unregistered driver passed validation"),
    }
    match state.prepare_connect_profile(profile_id) {
        Err(error) => {
            assert_eq!(error, expected);
            assert_eq!(
                error.to_string(),
                "No driver registered for 'removed-driver'"
            );
        }
        Ok(_) => panic!("a profile with an unregistered driver reached connect preparation"),
    }
}

/// Rows the removed AWS drivers left behind (#311) are dropped one by one at
/// load: with a driver-config row, or with only the profile's `kind` column.
/// The PostgreSQL profile stored beside them still loads, and nothing reads a
/// secret.
#[test]
fn stored_aws_rows_are_dropped_and_postgres_profile_loads() {
    use datatab_storage::repositories::connection_driver_configs::ConnectionDriverConfigDto;
    use datatab_storage::repositories::connection_profiles::ConnectionProfileDto;

    let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
    let postgres = ConnectionProfile::new("unaffected", DbConfig::default_postgres());
    crate::config_loader::save_profiles(&runtime, std::slice::from_ref(&postgres))
        .expect("save the PostgreSQL profile");

    let repo = runtime.connection_profiles();
    for (kind, driver_id) in [
        ("DynamoDB", "dynamodb"),
        ("CloudWatchLogs", "cloudwatch"),
        ("S3", "s3"),
        ("AwsSecrets", "aws_secrets"),
    ] {
        for has_driver_config in [true, false] {
            let id = Uuid::new_v4();
            let mut profile = ConnectionProfileDto::new(id, format!("stored {kind}"));
            profile.kind = Some(kind.to_string());
            profile.driver_id = Some(driver_id.to_string());
            repo.insert(&profile).expect("store the AWS profile row");

            if has_driver_config {
                let mut config = ConnectionDriverConfigDto::new(id.to_string(), kind.to_string());
                config.dynamo_region = Some("us-east-1".to_string());
                repo.driver_configs()
                    .insert(&config)
                    .expect("store the AWS driver-config row");
            }
        }
    }
    assert_eq!(repo.all().expect("stored rows").len(), 9);

    let mut state = AppState::new_with_storage_runtime(runtime).expect("load state");
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));

    let loaded: Vec<Uuid> = state.profiles().iter().map(|profile| profile.id).collect();
    assert_eq!(
        loaded,
        vec![postgres.id],
        "only the PostgreSQL profile may load beside stored AWS rows"
    );
    assert_eq!(state.profiles()[0].kind(), DbKind::Postgres);
}

/// A profile whose kind is a reserved driver-wire slot, as an RPC driver that
/// declares one would build it.
fn reserved_kind_profile() -> ConnectionProfile {
    ConnectionProfile::new_with_driver(
        "declared by an RPC driver",
        DbKind::ReservedDynamoDb,
        "rpc:reserved.sock",
        DbConfig::External {
            kind: DbKind::ReservedDynamoDb,
            values: datatab_core::FormValues::new(),
        },
    )
}

/// A reserved kind has no stored name, so a saved profile with one would be
/// gone at the next start (#364). The save refuses it with the reason and
/// writes no row for it, while the PostgreSQL profile beside it is saved.
#[test]
fn saving_a_profile_with_a_reserved_kind_is_refused_and_writes_no_row() {
    let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
    let postgres = ConnectionProfile::new("unaffected", DbConfig::default_postgres());
    let reserved = reserved_kind_profile();
    // The config alone can carry the reserved kind, under a kept profile kind.
    let mut reserved_config = reserved_kind_profile();
    reserved_config.kind = Some(DbKind::Postgres);

    let error = crate::config_loader::save_profiles(
        &runtime,
        &[reserved.clone(), postgres.clone(), reserved_config.clone()],
    )
    .expect_err("a reserved kind must be refused")
    .to_string();
    assert!(error.contains("reserved database kind"), "{error}");
    assert!(error.contains(&reserved.id.to_string()), "{error}");
    assert!(error.contains(&reserved_config.id.to_string()), "{error}");

    let repo = runtime.connection_profiles();
    let stored: Vec<String> = repo
        .all()
        .expect("stored rows")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(stored, vec![postgres.id.to_string()]);
    for refused in [&reserved, &reserved_config] {
        assert!(
            repo.driver_configs()
                .get_for_profile(&refused.id.to_string())
                .expect("read driver config")
                .is_none(),
            "no driver-config row may be written for a refused profile"
        );
    }
}

/// The connection manager's save refuses a reserved kind with the reason
/// before the profile enters the profile list.
#[test]
fn connection_manager_save_refuses_a_reserved_kind() {
    let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
    let mut state = AppState::new_with_storage_runtime(runtime).expect("load state");
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));

    match state.update_profile_from_connection_manager(reserved_kind_profile(), "") {
        Err(DbError::InvalidProfile(message)) => {
            assert!(message.contains("reserved database kind"), "{message}");
        }
        Err(error) => panic!("wrong refusal: {error}"),
        Ok(()) => panic!("a reserved kind passed the connection manager save"),
    }
    assert!(state.profiles().is_empty());
}

#[test]
fn pipeline_prepare_refuses_before_pre_connect_hooks() {
    let profile = ConnectionProfile::new("refused", DbConfig::default_postgres());
    let state = test_state_with_profiles(
        test_drivers(&profile.driver_id(), false, true),
        vec![profile.clone()],
    );
    // Sidebar preparation and the connection-manager Test path share this gate.
    for result in [
        state.build_pipeline_input_for_profile(profile.clone(), CancelToken::new()),
        state
            .prepare_pipeline_input(profile.id, CancelToken::new())
            .map(|(input, _, _)| input),
    ] {
        match result {
            Err(error) => assert!(error.contains("fixture refusal"), "{error}"),
            Ok(_) => panic!("refused profile reached PreConnect hooks"),
        }
    }
}
