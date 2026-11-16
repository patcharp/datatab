use super::refusal::PanicOnReadSecretStore;
use super::*;

fn broken_reference_state(field: &str, kind: &str) -> (AppState, Uuid) {
    let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
    let mut profile = ConnectionProfile::new("broken source", DbConfig::default_postgres());
    profile.save_password = true;
    let reference = match kind {
        "dotenv" => datatab_core::ValueRef::dotenv("unused-source", "PASSWORD"),
        "secret" => datatab_core::ValueRef::secret("unused-provider", "unused-locator", None),
        _ => panic!("unknown fixture kind"),
    };
    profile.value_refs.insert(field.into(), reference);
    crate::config_loader::save_profiles(&runtime, std::slice::from_ref(&profile))
        .expect("save profile");
    let column = if kind == "dotenv" {
        "env_key"
    } else {
        "ref_provider"
    };
    runtime
        .open_datatab_db()
        .expect("database")
        .execute(
            &format!(
                "UPDATE cfg_connection_profile_value_refs SET {column} = NULL WHERE profile_id = ?1"
            ),
            [profile.id.to_string()],
        )
        .expect("break reference");
    let mut state = AppState::new_with_storage_runtime(runtime).expect("load state");
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));
    (state, profile.id)
}

fn refusal_message(state: &AppState, profile_id: Uuid) -> String {
    match state.validate_connect_profile(profile_id) {
        Err(PrepareConnectError::ProfileRefused { error }) => error,
        result => panic!("expected broken reference refusal, got {result:?}"),
    }
}

#[test]
#[cfg(feature = "postgres")]
fn unknown_value_ref_kind_flags_field_and_refuses_before_secret_reads() {
    for field in ["password", "host"] {
        let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
        let mut profile = ConnectionProfile::new("unknown source", DbConfig::default_postgres());
        profile.save_password = true;
        profile
            .value_refs
            .insert(field.into(), datatab_core::ValueRef::env("UNUSED_SOURCE"));
        let other = ConnectionProfile::new("unaffected", DbConfig::default_postgres());
        crate::config_loader::save_profiles(&runtime, &[profile.clone(), other.clone()])
            .expect("save profiles");
        runtime.open_datatab_db().expect("database").execute(
            "UPDATE cfg_connection_profile_value_refs SET ref_kind = 'retired-kind' WHERE profile_id = ?1",
            [profile.id.to_string()],
        ).expect("store unknown kind");
        let mut state = AppState::new_with_storage_runtime(runtime).expect("load state");
        state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));

        assert_eq!(state.profiles().len(), 2);
        assert!(
            state
                .profiles()
                .iter()
                .any(|loaded| loaded.id == profile.id)
        );
        let diagnostics = state.value_ref_diagnostics(profile.id);
        assert_eq!(diagnostics.len(), 1, "unknown source must flag its field");
        assert_eq!(diagnostics[0].field, field);
        let expected = format!("Value ref '{field}' has unknown source kind \"retired-kind\"");
        assert_eq!(diagnostics[0].message(), expected);
        assert_eq!(refusal_message(&state, profile.id), expected);
        for passphrase in [None, Some("explicit fixture passphrase")] {
            match state.prepare_connect_profile_with_passphrase(profile.id, passphrase) {
                Err(PrepareConnectError::ProfileRefused { error }) => assert_eq!(error, expected),
                Err(error) => panic!("wrong refusal: {error}"),
                Ok(_) => panic!("unknown source reached connect preparation"),
            }
        }
        assert!(state.connection_password_prompt(profile.id).is_none());
        assert!(state.value_ref_diagnostics(other.id).is_empty());
        assert_eq!(state.validate_connect_profile(other.id), Ok(()));
        #[cfg(feature = "mcp")]
        {
            let (profiles, errors) =
                datatab_mcp_server::state::load_profiles(state.storage_runtime())
                    .expect("MCP loads other profiles");
            assert_eq!(profiles.len(), 1);
            assert_eq!(profiles[0].id, other.id);
            assert_eq!(errors.get(&profile.id), Some(&expected));
        }
    }
}

/// The Parameter value source was removed. A complete stored `param` row
/// flags its profile like any unknown source kind, before any secret read,
/// and the PostgreSQL profile next to it still loads and validates.
#[test]
#[cfg(feature = "postgres")]
fn stored_param_value_ref_flags_profile_and_postgres_still_loads() {
    for field in ["password", "host"] {
        let runtime = datatab_storage::bootstrap::StorageRuntime::in_memory().expect("storage");
        let mut profile = ConnectionProfile::new("stored parameter", DbConfig::default_postgres());
        profile.save_password = true;
        profile.value_refs.insert(
            field.into(),
            datatab_core::ValueRef::secret("aws-ssm", "/db/value", None),
        );
        let other = ConnectionProfile::new("unaffected", DbConfig::default_postgres());
        crate::config_loader::save_profiles(&runtime, &[profile.clone(), other.clone()])
            .expect("save profiles");
        runtime
            .open_datatab_db()
            .expect("database")
            .execute(
                "UPDATE cfg_connection_profile_value_refs \
                 SET ref_kind = 'param', param_name = secret_locator, secret_locator = NULL \
                 WHERE profile_id = ?1",
                [profile.id.to_string()],
            )
            .expect("store a complete param row");
        let mut state = AppState::new_with_storage_runtime(runtime).expect("load state");
        state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));

        assert_eq!(state.profiles().len(), 2);
        let loaded = state
            .profiles()
            .iter()
            .find(|loaded| loaded.id == profile.id)
            .expect("the flagged profile is listed")
            .clone();
        assert!(
            loaded.value_refs.is_empty(),
            "the stored parameter reference must not load"
        );
        let diagnostics = state.value_ref_diagnostics(profile.id);
        assert_eq!(diagnostics.len(), 1, "the param row must flag its field");
        assert_eq!(diagnostics[0].field, field);
        let expected = format!("Value ref '{field}' has unknown source kind \"param\"");
        assert_eq!(refusal_message(&state, profile.id), expected);
        match state.prepare_connect_profile_with_passphrase(profile.id, None) {
            Err(PrepareConnectError::ProfileRefused { error }) => assert_eq!(error, expected),
            Err(error) => panic!("wrong refusal: {error}"),
            Ok(_) => panic!("stored parameter reference reached connect preparation"),
        }
        match state.build_pipeline_input_for_profile(loaded, CancelToken::new()) {
            Err(error) => assert_eq!(error, expected),
            Ok(_) => panic!("stored parameter reference reached pipeline preparation"),
        }
        assert!(state.connection_password_prompt(profile.id).is_none());
        assert!(state.value_ref_diagnostics(other.id).is_empty());
        assert_eq!(state.validate_connect_profile(other.id), Ok(()));
    }
}

#[test]
#[cfg(feature = "postgres")]
fn broken_value_ref_password_is_refused_with_field_and_missing_part() {
    for (kind, expected) in [
        ("dotenv", "Dotenv value ref 'password' is missing its key"),
        ("secret", "Secret value ref 'password' is missing provider"),
    ] {
        let (state, profile_id) = broken_reference_state("password", kind);
        assert_eq!(refusal_message(&state, profile_id), expected);
        assert!(
            state
                .profiles()
                .iter()
                .any(|profile| profile.id == profile_id)
        );
    }
}

#[test]
#[cfg(feature = "postgres")]
fn broken_value_ref_prepare_reads_no_password_proxy_or_ssh_secret() {
    for secret_kind in ["password", "proxy", "ssh"] {
        let (mut state, profile_id) = broken_reference_state("password", "dotenv");
        if secret_kind == "proxy" {
            let mut proxy =
                datatab_core::ProxyProfile::new("fixture", datatab_core::ProxyKind::Socks5);
            proxy.save_secret = true;
            state.facade.profiles.profiles[0].proxy_profile_id = Some(proxy.id);
            state.facade.proxies.items.push(proxy);
        }
        if secret_kind == "ssh"
            && let DbConfig::Postgres { ssh_tunnel, .. } =
                &mut state.facade.profiles.profiles[0].config
        {
            *ssh_tunnel = Some(datatab_core::SshTunnelConfig {
                host: "127.0.0.1".into(),
                port: 22,
                user: "fixture".into(),
                auth_method: datatab_core::SshAuthMethod::Password,
                ssh_config_host: None,
            });
        }
        for passphrase in [None, Some("explicit fixture passphrase")] {
            match state.prepare_connect_profile_with_passphrase(profile_id, passphrase) {
                Err(PrepareConnectError::ProfileRefused { error }) => {
                    assert!(error.contains("password"))
                }
                Err(error) => panic!("wrong refusal: {error}"),
                Ok(_) => panic!("broken reference reached connect preparation"),
            }
        }
    }
}

#[test]
#[cfg(feature = "postgres")]
fn broken_value_ref_offers_no_password_prompt() {
    let (mut state, profile_id) = broken_reference_state("password", "dotenv");
    state.facade.profiles.profiles[0].save_password = false;
    assert!(state.connection_password_prompt(profile_id).is_none());
}

#[test]
#[cfg(feature = "postgres")]
fn broken_value_ref_non_password_refuses_literal_fallback_and_pipeline() {
    let (state, profile_id) = broken_reference_state("host", "secret");
    assert_eq!(
        refusal_message(&state, profile_id),
        "Secret value ref 'host' is missing provider"
    );
    let profile = state
        .profiles()
        .iter()
        .find(|profile| profile.id == profile_id)
        .expect("profile");
    match state.build_pipeline_input_for_profile(profile.clone(), CancelToken::new()) {
        Err(error) => assert_eq!(error, refusal_message(&state, profile_id)),
        Ok(_) => panic!("broken reference reached pipeline preparation"),
    }
}

#[test]
#[cfg(all(feature = "postgres", feature = "mcp"))]
fn broken_value_ref_app_and_mcp_use_the_same_message_for_the_same_row() {
    for kind in ["dotenv", "secret"] {
        let (state, profile_id) = broken_reference_state("password", kind);
        let (profiles, mut errors) =
            datatab_mcp_server::state::load_profiles(state.storage_runtime())
                .expect("MCP loads despite malformed reference");
        assert!(profiles.is_empty());
        let mcp_error = errors
            .remove(&profile_id)
            .expect("MCP rejects malformed profile");
        assert_eq!(refusal_message(&state, profile_id), mcp_error);
    }
}

#[test]
#[cfg(feature = "postgres")]
fn broken_value_ref_only_connection_manager_save_clears_diagnostics() {
    let (mut state, profile_id) = broken_reference_state("password", "dotenv");
    let mut profile = state
        .profiles()
        .iter()
        .find(|profile| profile.id == profile_id)
        .expect("profile")
        .clone();
    profile.name = "background edit".into();
    state.update_profile(profile.clone());
    let loaded =
        crate::config_loader::load_config(state.storage_runtime()).expect("load saved edit");
    assert_eq!(loaded.profiles[0].name, "background edit");
    assert!(!state.value_ref_diagnostics(profile_id).is_empty());
    assert!(state.validate_connect_profile(profile_id).is_err());

    profile.value_refs.insert(
        "password".into(),
        datatab_core::ValueRef::env("REPLACEMENT_PASSWORD"),
    );
    state
        .update_profile_from_connection_manager(profile, "")
        .expect("save repaired source");
    assert!(state.value_ref_diagnostics(profile_id).is_empty());
    assert_eq!(state.validate_connect_profile(profile_id), Ok(()));
    let reloaded = AppState::new_with_storage_runtime(state.storage_runtime).expect("reload");
    assert_eq!(reloaded.validate_connect_profile(profile_id), Ok(()));
}

#[test]
#[cfg(feature = "postgres")]
fn broken_value_ref_unrelated_save_preserves_the_broken_row_after_reload() {
    let (mut state, profile_id) = broken_reference_state("password", "dotenv");
    state.add_profile_in_folder(
        ConnectionProfile::new("unrelated", DbConfig::default_postgres()),
        None,
    );
    let rows = state
        .storage_runtime()
        .connection_profiles()
        .value_refs()
        .get_for_profile(&profile_id.to_string())
        .expect("rows");
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].load_diagnostic().is_some(),
        "unrelated save retains the broken row"
    );
    let reloaded = AppState::new_with_storage_runtime(state.storage_runtime).expect("reload");
    assert_eq!(
        refusal_message(&reloaded, profile_id),
        "Dotenv value ref 'password' is missing its key"
    );
}

#[test]
#[cfg(feature = "postgres")]
fn broken_value_ref_refusal_names_every_broken_field() {
    use datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto;
    let (state, profile_id) = broken_reference_state("password", "dotenv");
    let mut row = ConnectionProfileValueRefDto::new_secret(
        profile_id.to_string(),
        "host".into(),
        "unused-provider".into(),
        "unused-locator".into(),
        None,
    );
    row.ref_provider = None;
    state
        .storage_runtime()
        .connection_profiles()
        .value_refs()
        .insert(&row)
        .expect("add broken host");
    let reloaded = AppState::new_with_storage_runtime(state.storage_runtime).expect("reload");
    assert_eq!(
        refusal_message(&reloaded, profile_id),
        "Secret value ref 'host' is missing provider\nDotenv value ref 'password' is missing its key"
    );
}

#[test]
#[cfg(feature = "postgres")]
fn broken_value_ref_remembered_password_is_not_offered() {
    let (mut state, profile_id) = broken_reference_state("password", "dotenv");
    state.facade.profiles.profiles[0].save_password = false;
    state.remember_connection_password(profile_id, SecretString::from("remembered fixture"));
    assert!(state.remembered_connection_password(profile_id).is_none());
}

#[test]
#[cfg(all(feature = "postgres", feature = "mcp"))]
fn broken_value_ref_app_and_mcp_message_parity_for_multiple_rows() {
    use datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto;
    let (state, profile_id) = broken_reference_state("password", "dotenv");
    let mut host = ConnectionProfileValueRefDto::new_secret(
        profile_id.to_string(),
        "host".into(),
        "unused".into(),
        "unused".into(),
        None,
    );
    host.ref_provider = None;
    state
        .storage_runtime()
        .connection_profiles()
        .value_refs()
        .insert(&host)
        .expect("insert");
    let (profiles, mut errors) =
        datatab_mcp_server::state::load_profiles(state.storage_runtime()).expect("MCP loads");
    assert!(profiles.is_empty());
    let error = errors
        .remove(&profile_id)
        .expect("MCP rejects broken profile");
    let reloaded = AppState::new_with_storage_runtime(state.storage_runtime).expect("reload");
    assert_eq!(error, refusal_message(&reloaded, profile_id));
    assert!(error.contains("host") && error.contains("password"));
}

#[test]
#[cfg(feature = "postgres")]
fn broken_value_ref_manager_refuses_unresolved_password_without_writes() {
    for source in [None, Some(datatab_core::ValueRef::literal(""))] {
        let (mut state, profile_id) = broken_reference_state("password", "dotenv");
        let mut profile = state.profiles()[0].clone();
        profile.name = "refused edit".into();
        if let Some(source) = source {
            profile.value_refs.insert("password".into(), source);
        }
        assert!(
            state
                .update_profile_from_connection_manager(profile, "")
                .is_err()
        );
        assert_eq!(state.profiles()[0].name, "broken source");
        assert!(!state.value_ref_diagnostics(profile_id).is_empty());
        let rows = state
            .storage_runtime()
            .connection_profiles()
            .value_refs()
            .get_for_profile(&profile_id.to_string())
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].load_diagnostic().is_some());
        let reloaded = AppState::new_with_storage_runtime(state.storage_runtime).expect("reload");
        assert_eq!(reloaded.profiles()[0].name, "broken source");
        assert!(!reloaded.value_ref_diagnostics(profile_id).is_empty());
    }
}
