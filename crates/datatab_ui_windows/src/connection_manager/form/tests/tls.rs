use super::*;
use crate::connection_manager::{MainExtraStop, SslCertSlot};

#[::core::prelude::v1::test]
fn tls_saved_redshift_verify_full_survives_edit_and_save() {
    let mut cx = TestAppContext::single();
    init_form_test_runtime(&mut cx);
    let app_state = test_app_state(
        &mut cx,
        SecretStoreFixture::new(PasswordSaveOutcome::Success),
    );
    let mut config = DbConfig::default_redshift();
    if let DbConfig::Redshift { ssl_mode, .. } = &mut config {
        *ssl_mode = Some("verify-full".into());
    }
    let profile = ConnectionProfile::new("verified redshift", config);
    app_state.update(&mut cx, |state, _| {
        state.add_profile_in_folder(profile.clone(), None)
    });
    let window = open_edit_profile_window(app_state.clone(), profile, &mut cx);
    cx.run_until_parked();
    window
        .update(&mut cx, |manager, window, cx| {
            assert_eq!(manager.form.selected_ssl_mode, "verify-full");
            manager.save_profile(window, cx);
        })
        .expect("save runs");
    cx.run_until_parked();
    let saved = cx.update(|cx| app_state.read(cx).profiles().to_vec());
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "edited profile", "the edit was persisted");
    assert!(
        matches!(&saved[0].config, DbConfig::Redshift { ssl_mode: Some(mode), .. } if mode == "verify-full")
    );
    assert!(
        window.root(&mut cx).is_err(),
        "successful save closes the window"
    );
}

#[::core::prelude::v1::test]
fn tls_cassandra_mode_and_certificates_reach_the_config() {
    // Reading a saved Cassandra profile: the shared Transport control shows the
    // saved mode instead of falling back to the driver's first option.
    let mut config = DbConfig::default_cassandra();
    if let DbConfig::Cassandra { ssl_mode, .. } = &mut config {
        *ssl_mode = Some("verify-ca".into());
    }
    assert_eq!(
        crate::connection_manager::ssl_mode_from_config(&config).as_deref(),
        Some("verify-ca")
    );

    // A saved config restores all three pickers, not just the CA.
    let mut saved_config = DbConfig::default_cassandra();
    if let DbConfig::Cassandra {
        ssl_root_cert_path,
        ssl_client_cert_path,
        ssl_client_key_path,
        ..
    } = &mut saved_config
    {
        *ssl_root_cert_path = Some("/etc/ca.pem".into());
        *ssl_client_cert_path = Some("/etc/client.pem".into());
        *ssl_client_key_path = Some("/etc/client.key".into());
    }
    assert_eq!(
        crate::connection_manager::ssl_cert_paths_from_config(&saved_config),
        (
            "/etc/ca.pem".to_string(),
            "/etc/client.pem".to_string(),
            "/etc/client.key".to_string()
        )
    );

    // Saving: the control's mode and the certificate paths reach the config,
    // which is the only place the driver reads TLS from.
    let mut config = DbConfig::default_cassandra();
    apply_ssl_selection(
        &mut config,
        Some("verify-full"),
        Some("/etc/ca.pem".into()),
        Some("/etc/client.pem".into()),
        Some("/etc/client.key".into()),
    );
    assert!(
        matches!(
            &config,
            DbConfig::Cassandra {
                ssl_mode: Some(mode),
                ssl_root_cert_path: Some(ca),
                ssl_client_cert_path: Some(cert),
                ssl_client_key_path: Some(key),
                ..
            } if mode == "verify-full"
                && ca == "/etc/ca.pem"
                && cert == "/etc/client.pem"
                && key == "/etc/client.key"
        ),
        "the mode and the paths are written back: {config:?}"
    );
}

#[::core::prelude::v1::test]
fn tls_clickhouse_mode_and_certificates_reach_the_config() {
    let legacy = DbConfig::ClickHouse {
        url: "https://localhost:8443".into(),
        user: "default".into(),
        database: "default".into(),
        request_timeout_seconds: None,
        allow_insecure_http_password: false,
        ssl_mode: None,
        ssl_root_cert_path: None,
        ssl_client_cert_path: None,
        ssl_client_key_path: None,
    };
    assert_eq!(
        crate::connection_manager::ssl_mode_from_config(&legacy),
        Some(String::new()),
        "editing a legacy profile requires an explicit choice"
    );
    // Reading a saved ClickHouse profile: the shared Transport control shows the
    // saved mode instead of falling back to the driver's first option.
    let mut config = DbConfig::default_clickhouse();
    if let DbConfig::ClickHouse { ssl_mode, .. } = &mut config {
        *ssl_mode = Some("verify-ca".into());
    }
    assert_eq!(
        crate::connection_manager::ssl_mode_from_config(&config).as_deref(),
        Some("verify-ca")
    );

    // A saved config restores all three pickers, not just the CA.
    let mut saved_config = DbConfig::default_clickhouse();
    if let DbConfig::ClickHouse {
        ssl_root_cert_path,
        ssl_client_cert_path,
        ssl_client_key_path,
        ..
    } = &mut saved_config
    {
        *ssl_root_cert_path = Some("/etc/ca.pem".into());
        *ssl_client_cert_path = Some("/etc/client.pem".into());
        *ssl_client_key_path = Some("/etc/client.key".into());
    }
    assert_eq!(
        crate::connection_manager::ssl_cert_paths_from_config(&saved_config),
        (
            "/etc/ca.pem".to_string(),
            "/etc/client.pem".to_string(),
            "/etc/client.key".to_string()
        )
    );

    // Saving: the control's mode and the certificate paths reach the config,
    // which is the only place the driver reads TLS from.
    let mut config = DbConfig::default_clickhouse();
    apply_ssl_selection(
        &mut config,
        Some("verify-full"),
        Some("/etc/ca.pem".into()),
        Some("/etc/client.pem".into()),
        Some("/etc/client.key".into()),
    );
    assert!(
        matches!(
            &config,
            DbConfig::ClickHouse {
                ssl_mode: Some(mode),
                ssl_root_cert_path: Some(ca),
                ssl_client_cert_path: Some(cert),
                ssl_client_key_path: Some(key),
                ..
            } if mode == "verify-full"
                && ca == "/etc/ca.pem"
                && cert == "/etc/client.pem"
                && key == "/etc/client.key"
        ),
        "the mode and the paths are written back: {config:?}"
    );
}

#[::core::prelude::v1::test]
fn tls_postgres_certificate_keyboard_stops_follow_mode() {
    let mut cx = TestAppContext::single();
    let window = open_postgres_form(&mut cx);
    for (mode, expected) in [
        ("disable", vec![]),
        (
            "prefer",
            vec![SslCertSlot::ClientCert, SslCertSlot::ClientKey],
        ),
        (
            "verify-full",
            vec![
                SslCertSlot::CaCert,
                SslCertSlot::ClientCert,
                SslCertSlot::ClientKey,
            ],
        ),
    ] {
        let slots = window
            .update(&mut cx, |manager, _, _| {
                manager.form.selected_ssl_mode = mode.into();
                manager
                    .main_extra_stops()
                    .into_iter()
                    .filter_map(|stop| match stop {
                        MainExtraStop::SslCert(slot) => Some(slot),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .expect("form remains open");
        assert_eq!(slots, expected, "mode {mode}");
    }
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close window");
}

#[::core::prelude::v1::test]
fn tls_postgres_rendered_certificate_pickers_follow_mode() {
    let mut cx = TestAppContext::single();
    let window = open_postgres_form(&mut cx);
    let capture = Arc::new(FrameCapture::default());
    for (mode, expected) in [
        ("disable", vec![]),
        (
            "prefer",
            vec!["ssl-cert-picker-client-cert", "ssl-cert-picker-client-key"],
        ),
        (
            "verify-full",
            vec![
                "ssl-cert-picker-ca",
                "ssl-cert-picker-client-cert",
                "ssl-cert-picker-client-key",
            ],
        ),
    ] {
        window
            .update(&mut cx, |manager, window, cx| {
                manager.form.selected_ssl_mode = mode.into();
                window.observe_frames(&capture);
                window.refresh();
                cx.notify();
            })
            .expect("refresh form");
        cx.run_until_parked();
        let frame = capture
            .0
            .lock()
            .expect("frame lock")
            .clone()
            .expect("frame rendered");
        let mut pickers: Vec<_> = frame
            .nodes()
            .map(|(_, node)| node.id())
            .filter(|id| id.starts_with("ssl-cert-picker-"))
            .collect();
        pickers.sort_unstable();
        assert_eq!(pickers, expected, "mode {mode}");
    }
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .expect("close window");
}

#[::core::prelude::v1::test]
fn tls_clickhouse_form_requires_mode_and_omits_hidden_paths() {
    let mut cx = TestAppContext::single();
    let _host = init_form_test_runtime(&mut cx);
    let app_state = test_app_state(
        &mut cx,
        SecretStoreFixture::new(PasswordSaveOutcome::Success),
    );
    let window = open_new_profile_window(app_state, &mut cx);
    window
        .update(&mut cx, |manager, window, cx| {
            manager.select_driver("clickhouse", window, cx);
            assert_eq!(
                manager.form.selected_ssl_mode, "",
                "new profiles require an explicit TLS mode"
            );
            assert!(
                !manager.validate_form(false, cx),
                "a blank mode cannot be saved"
            );
            for input in [
                &manager.form.ssl_ca_cert_input,
                &manager.form.ssl_client_cert_input,
                &manager.form.ssl_client_key_input,
            ] {
                input.update(cx, |input, cx| {
                    input.set_value("/test-certificate.pem", window, cx)
                });
            }
            for mode in ["disable", "require", "verify-ca", "verify-full"] {
                manager.form.selected_ssl_mode = mode.into();
                assert!(
                    manager.validate_form(false, cx),
                    "{mode}: {:?}",
                    manager.validation_errors
                );
                let config = manager.build_config(cx).expect("explicit mode config");
                let DbConfig::ClickHouse {
                    ssl_mode,
                    ssl_root_cert_path,
                    ssl_client_cert_path,
                    ssl_client_key_path,
                    ..
                } = config
                else {
                    panic!("ClickHouse config expected");
                };
                assert_eq!(ssl_mode.as_deref(), Some(mode));
                assert_eq!(
                    ssl_root_cert_path.is_some(),
                    matches!(mode, "verify-ca" | "verify-full")
                );
                assert_eq!(ssl_client_cert_path.is_some(), mode != "disable");
                assert_eq!(ssl_client_key_path.is_some(), mode != "disable");
            }
        })
        .expect("check shared transport form");
}
