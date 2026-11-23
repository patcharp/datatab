use super::*;

/// The server has no RPC driver form, so it cannot tell a secret value of an
/// external profile from any other: an old row's values, typed or a literal
/// reference, are not exposed (#411 ruling 2 item 3).
#[test]
fn external_profile_values_are_withheld() {
    use datatab_storage::repositories::connection_driver_configs::ConnectionDriverConfigDto;
    use datatab_storage::repositories::connection_profile_value_refs::ConnectionProfileValueRefDto;

    const SENTINEL: &str = "sentinel-api-token-7f3a91";
    let runtime = StorageRuntime::in_memory().expect("storage");
    let repo = runtime.connection_profiles();
    let profile_id = uuid::Uuid::new_v4();
    let mut profile = ConnectionProfileDto::new(profile_id, "rpc".into());
    profile.kind = Some("Postgres".into());
    profile.driver_id = Some("rpc:fake".into());
    repo.insert(&profile).expect("save profile");
    let config = ConnectionDriverConfigDto::from_db_config(
        profile_id.to_string(),
        &datatab_core::DbConfig::External {
            kind: datatab_core::DbKind::Postgres,
            values: HashMap::from([
                ("host".to_string(), "rpc.example".to_string()),
                ("api_token".to_string(), SENTINEL.to_string()),
            ]),
        },
    );
    repo.driver_configs().insert(&config).expect("save config");
    repo.value_refs()
        .insert(&ConnectionProfileValueRefDto::new_literal(
            profile_id.to_string(),
            "secret_key".into(),
            SENTINEL.into(),
        ))
        .expect("save literal reference");

    let (profiles, errors) = load_profiles(&runtime).expect("load profiles");

    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(profiles.len(), 1);
    assert!(
        matches!(&profiles[0].config, datatab_core::DbConfig::External { values, .. } if values.is_empty())
    );
    assert!(profiles[0].value_refs.is_empty());
    assert!(!format!("{:?}", profiles[0]).contains(SENTINEL));
}
