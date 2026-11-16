use super::super::*;

#[test]
fn tls_metadata_mongodb_metadata_on_label_discloses_unverified_server_certificate() {
    let modes = MONGODB_METADATA.ssl_modes.expect("TLS modes");
    let mode = modes.iter().find(|mode| mode.id == "on").expect("mode");
    assert_eq!(
        mode.label,
        "on (encrypted, server certificate not verified)"
    );
}

#[test]
fn tls_metadata_certificate_pickers_follow_selected_mode() {
    let metadata = &*MONGODB_METADATA;
    let fields = metadata.ssl_cert_fields.expect("certificate fields");
    for (mode, root, client) in [
        ("off", false, false),
        ("on", false, false),
        ("verify", true, true),
    ] {
        assert_eq!(
            fields.visible_for_mode(mode),
            (root, client),
            "{} mode {mode}",
            metadata.id
        );
    }
}

#[test]
fn tls_metadata_unused_client_files_are_not_read() {
    let missing = std::env::temp_dir().join(format!("datatab-missing-{}", Uuid::new_v4()));
    let missing = missing.to_string_lossy().into_owned();
    for (mode, expected) in [
        ("on", "&tls=true&tlsAllowInvalidCertificates=true"),
        ("off", ""),
    ] {
        let mut config = extract_mongodb_config(&DbConfig::default_mongodb()).expect("config");
        config.ssl_mode = Some(mode.into());
        config.ssl_client_cert_path = Some(missing.clone());
        config.ssl_client_key_path = Some(missing.clone());
        let (params, guard) = prepare_mongo_tls(&config).expect("unused files must not be read");
        assert_eq!(params, expected, "mode {mode}");
        assert!(
            guard.is_none(),
            "mode {mode} must not create a temporary PEM"
        );
    }
    let mut config = extract_mongodb_config(&DbConfig::default_mongodb()).expect("config");
    config.ssl_mode = Some("verify".into());
    config.ssl_client_cert_path = Some(missing.clone());
    config.ssl_client_key_path = Some(missing);
    assert!(
        matches!(prepare_mongo_tls(&config), Err(DbError::InvalidProfile(_))),
        "verify still requires readable client files"
    );
    config.use_uri = true;
    let (params, guard) = prepare_mongo_tls(&config).expect("URI mode owns its TLS options");
    assert!(params.is_empty());
    assert!(guard.is_none());
}
