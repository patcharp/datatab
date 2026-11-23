use super::*;

#[test]
fn clickhouse_tls_config_without_driver_row_keeps_legacy_verification() {
    let runtime = StorageRuntime::in_memory().expect("storage");
    let repo = runtime.connection_profiles();
    let id = uuid::Uuid::new_v4();
    let mut profile = ConnectionProfileDto::new(id, "legacy ClickHouse".into());
    profile.kind = Some("ClickHouse".into());
    repo.insert(&profile)
        .expect("save profile without driver config");

    let (profiles, errors) = load_profiles(&runtime).expect("load legacy profile");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, id);
    let datatab_core::DbConfig::ClickHouse { ssl_mode, .. } = &profiles[0].config else {
        panic!("expected ClickHouse config");
    };
    assert_eq!(
        ssl_mode, &None,
        "missing config keeps legacy TLS verification"
    );
    assert!(
        repo.driver_configs()
            .get_for_profile(&id.to_string())
            .expect("read driver config")
            .is_none(),
        "loading must not create a driver-config row"
    );
}

/// One stored profile without a usable driver config must not stop the other
/// profiles from loading (#311 ruling 1 item 3).
#[test]
fn profile_without_valid_driver_config_rejects_only_that_profile() {
    let runtime = StorageRuntime::in_memory().expect("storage");
    let repo = runtime.connection_profiles();

    let broken_id = uuid::Uuid::new_v4();
    let mut broken = ConnectionProfileDto::new(broken_id, "unknown kind".into());
    broken.kind = Some("RetiredKind".into());
    repo.insert(&broken).expect("save broken profile");

    let valid_id = uuid::Uuid::new_v4();
    let mut valid = ConnectionProfileDto::new(valid_id, "unaffected PostgreSQL".into());
    valid.kind = Some("Postgres".into());
    repo.insert(&valid).expect("save valid profile");

    let (profiles, errors) =
        load_profiles(&runtime).expect("MCP profiles load despite a bad driver config");

    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, valid_id);
    assert_eq!(profiles[0].kind(), datatab_core::DbKind::Postgres);
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors.get(&broken_id).map(String::as_str),
        Some(format!("Connection profile '{broken_id}' is missing a valid driver config").as_str())
    );
}

/// Rows the removed AWS drivers left behind (#311) are rejected one by one:
/// with a driver-config row, or with only the profile's `kind` column. The
/// PostgreSQL profile stored beside them is still listed.
#[test]
fn stored_aws_rows_reject_only_those_profiles() {
    use datatab_storage::repositories::connection_driver_configs::ConnectionDriverConfigDto;

    let runtime = StorageRuntime::in_memory().expect("storage");
    let repo = runtime.connection_profiles();

    let valid_id = uuid::Uuid::new_v4();
    let mut valid = ConnectionProfileDto::new(valid_id, "unaffected PostgreSQL".into());
    valid.kind = Some("Postgres".into());
    repo.insert(&valid).expect("save valid profile");

    let mut aws_ids = Vec::new();
    for kind in ["DynamoDB", "CloudWatchLogs", "S3", "AwsSecrets"] {
        for has_driver_config in [true, false] {
            let id = uuid::Uuid::new_v4();
            let mut profile = ConnectionProfileDto::new(id, format!("stored {kind}"));
            profile.kind = Some(kind.to_string());
            repo.insert(&profile).expect("store the AWS profile row");

            if has_driver_config {
                let mut config = ConnectionDriverConfigDto::new(id.to_string(), kind.to_string());
                config.dynamo_region = Some("us-east-1".to_string());
                repo.driver_configs()
                    .insert(&config)
                    .expect("store the AWS driver-config row");
            }
            aws_ids.push(id);
        }
    }

    let (profiles, errors) = load_profiles(&runtime).expect("MCP profiles load beside AWS rows");

    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, valid_id);
    assert_eq!(errors.len(), aws_ids.len());
    for id in aws_ids {
        assert_eq!(
            errors.get(&id).map(String::as_str),
            Some(format!("Connection profile '{id}' is missing a valid driver config").as_str())
        );
    }
}
