use super::super::*;

#[test]
fn sslmode_unknown_profile_is_refused() {
    for value in ["verify-ful", ""] {
        let mut config = DbConfig::default_mysql();
        if let DbConfig::MySQL { ssl_mode, .. } = &mut config {
            *ssl_mode = Some(value.into());
        }
        let profile = ConnectionProfile::new("TLS validation", config);
        let error = MysqlDriver::new(DbKind::MySQL)
            .validate_profile(&profile)
            .expect_err("unknown TLS mode must be refused");
        assert!(matches!(error, DbError::InvalidProfile(_)));
        let message = error.to_string();
        assert!(message.contains(value), "{message}");
        for accepted in [
            "DISABLED",
            "PREFERRED",
            "REQUIRED",
            "VERIFY_CA",
            "VERIFY_IDENTITY",
        ] {
            assert!(message.contains(accepted), "{message}");
        }
    }
}

#[test]
fn sslmode_saved_value_policy_table() {
    let paths = MysqlSslPaths {
        root_cert: None,
        client_cert: None,
        client_key: None,
    };
    let mut failures = Vec::new();
    for (value, native) in [
        ("disable", "REQUIRED"),
        ("allow", "REQUIRED"),
        ("prefer", "REQUIRED"),
        ("require", "REQUIRED"),
        ("verify-ca", "VERIFY_CA"),
        ("verify-full", "VERIFY_IDENTITY"),
        ("DISABLED", "DISABLED"),
        ("PREFERRED", "PREFERRED"),
        ("REQUIRED", "REQUIRED"),
        ("VERIFY_CA", "VERIFY_CA"),
        ("VERIFY_IDENTITY", "VERIFY_IDENTITY"),
    ] {
        for kind in [DbKind::MySQL, DbKind::MariaDB] {
            let mut config = DbConfig::default_mysql();
            if let DbConfig::MySQL { ssl_mode, .. } = &mut config {
                *ssl_mode = Some(value.into());
            }
            if MysqlDriver::new(kind)
                .validate_profile(&ConnectionProfile::new("TLS validation", config))
                .is_err()
            {
                failures.push(format!("{kind:?}: {value} refused"));
            }
        }
        let expected = build_mysql_opts("localhost", 3306, "user", None, None, native, &paths)
            .expect("native mode");
        match build_mysql_opts("localhost", 3306, "user", None, None, value, &paths) {
            Ok(actual) if actual.get_ssl_opts() == expected.get_ssl_opts() => {}
            actual => failures.push(format!(
                "{value}: expected {native} TLS options, got {actual:?}"
            )),
        }
    }
    for value in [
        "DISABLE",
        "ALLOW",
        "PREFER",
        "REQUIRE",
        "VERIFY-CA",
        "VERIFY-FULL",
        "disabled",
        "preferred",
        "required",
        "verify_ca",
        "verify_identity",
    ] {
        if MysqlSslMode::parse(value).is_ok() {
            failures.push(format!("{value} accepted"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn sslmode_absent_mysql_profile_survives_real_store_reload() {
    use datatab_storage::{
        StorageRuntime,
        repositories::{
            connection_driver_configs::ConnectionDriverConfigDto,
            connection_profiles::ConnectionProfileDto,
        },
    };

    let runtime = StorageRuntime::in_memory().expect("temporary SQLite store");
    let mut config = DbConfig::default_mysql();
    if let DbConfig::MySQL { ssl_mode, .. } = &mut config {
        *ssl_mode = None;
    }
    let mut profile = ConnectionProfile::new("Imported MySQL", config);
    let mut dto = ConnectionProfileDto::new(profile.id, profile.name.clone());
    dto.driver_id = Some("builtin:mysql".into());
    dto.kind = Some("MySQL".into());
    runtime
        .connection_profiles()
        .insert(&dto)
        .expect("save profile");
    runtime
        .connection_profiles()
        .driver_configs()
        .insert(&ConnectionDriverConfigDto::from_db_config(
            dto.id.clone(),
            &profile.config,
        ))
        .expect("save absent SSL mode");

    let reloaded_store = StorageRuntime::for_path(runtime.datatab_db_path().to_path_buf())
        .expect("reopen SQLite store");
    let saved = reloaded_store
        .connection_profiles()
        .driver_configs()
        .get_for_profile(&dto.id)
        .expect("read saved config")
        .expect("config exists");
    profile.config = saved.to_db_config().expect("reload config");
    let mode = extract_mysql_config(&profile.config)
        .expect("MySQL config")
        .ssl_mode;
    assert_eq!(mode, "prefer", "store writes its legacy default");
    MysqlDriver::new(DbKind::MySQL)
        .validate_profile(&profile)
        .expect("imported profile remains valid after reload");
    let paths = MysqlSslPaths {
        root_cert: None,
        client_cert: None,
        client_key: None,
    };
    let actual = build_mysql_opts("localhost", 3306, "user", None, None, &mode, &paths)
        .expect("reloaded mode");
    let old = build_mysql_opts("localhost", 3306, "user", None, None, "REQUIRED", &paths)
        .expect("unverified TLS without fallback");
    assert_eq!(actual.get_ssl_opts(), old.get_ssl_opts());
}
