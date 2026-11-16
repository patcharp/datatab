use super::super::*;

#[test]
fn sslmode_unknown_profile_is_refused() {
    for value in ["verify-ful", ""] {
        let mut config = DbConfig::default_sqlserver();
        if let DbConfig::SqlServer { ssl_mode, .. } = &mut config {
            *ssl_mode = Some(value.into());
        }
        let profile = ConnectionProfile::new("TLS validation", config);
        let error = MssqlDriver::new()
            .validate_profile(&profile)
            .expect_err("unknown TLS mode must be refused");
        assert!(matches!(error, DbError::InvalidProfile(_)));
        let message = error.to_string();
        assert!(message.contains(value), "{message}");
        for accepted in ["off", "disabled", "on", "required"] {
            assert!(message.contains(accepted), "{message}");
        }
    }
}

#[test]
fn sslmode_saved_value_policy_table() {
    let mut failures = Vec::new();
    for (value, expected) in [
        ("disable", EncryptionLevel::On),
        ("allow", EncryptionLevel::On),
        ("prefer", EncryptionLevel::On),
        ("require", EncryptionLevel::On),
        ("verify-ca", EncryptionLevel::Required),
        ("verify-full", EncryptionLevel::Required),
        ("off", EncryptionLevel::Off),
        ("disabled", EncryptionLevel::Off),
        ("on", EncryptionLevel::On),
        ("required", EncryptionLevel::Required),
    ] {
        let mut config = DbConfig::default_sqlserver();
        if let DbConfig::SqlServer { ssl_mode, .. } = &mut config {
            *ssl_mode = Some(value.into());
        }
        if MssqlDriver::new()
            .validate_profile(&ConnectionProfile::new("TLS validation", config))
            .is_err()
        {
            failures.push(format!("{value} profile refused"));
        }
        match encryption_for(value) {
            Ok(actual) if actual == expected => {}
            actual => failures.push(format!("{value}: expected {expected:?}, got {actual:?}")),
        }
        if matches!(value, "verify-ca" | "verify-full") {
            let (_, trusts_any_certificate) =
                super::extract_ssl(super::build_form_values_for_ssl(Some(value)));
            if trusts_any_certificate {
                failures.push(format!("{value} form disables certificate verification"));
            }
            let debug = super::tiberius_debug(value, true, None);
            if !debug.contains("trust: Default") {
                failures.push(format!(
                    "{value} connection disables certificate verification: {debug}"
                ));
            }
        }
    }
    for value in [
        "DISABLE",
        "ALLOW",
        "PREFER",
        "REQUIRE",
        "VERIFY-CA",
        "VERIFY-FULL",
        "OFF",
        "DISABLED",
        "ON",
        "REQUIRED",
    ] {
        if encryption_for(value).is_ok() {
            failures.push(format!("{value} accepted"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn sslmode_unknown_url_encrypt_is_refused() {
    let error = parse_mssql_url("sqlserver://localhost/db?ENCRYPT=requierd")
        .expect_err("unknown encrypt mode must be refused");
    let message = error.to_string();
    assert!(message.contains("requierd"), "{message}");
    assert!(
        message.contains("Accepted values: true, yes, on, 1, false, no, off, 0, strict, required"),
        "{message}"
    );
}

#[test]
fn sslmode_url_encrypt_values_ignore_case() {
    let mut failures = Vec::new();
    for (value, expected, trust) in [
        ("True", EncryptionLevel::On, "TrustAll"),
        ("TRUE", EncryptionLevel::On, "TrustAll"),
        ("YES", EncryptionLevel::On, "TrustAll"),
        ("ON", EncryptionLevel::On, "TrustAll"),
        ("FALSE", EncryptionLevel::Off, "TrustAll"),
        ("NO", EncryptionLevel::Off, "TrustAll"),
        ("OFF", EncryptionLevel::Off, "TrustAll"),
        ("STRICT", EncryptionLevel::Required, "Default"),
        ("REQUIRED", EncryptionLevel::Required, "Default"),
    ] {
        match parse_uri_query_params(&format!("encrypt={value}")).map(|parsed| parsed.encryption) {
            Ok(actual) if actual == expected => {}
            result => failures.push(format!(
                "encrypt={value}: expected {expected:?}, got {result:?}"
            )),
        }
        match parse_mssql_url(&format!("sqlserver://localhost/db?encrypt={value}")) {
            Ok(config) => {
                let debug = format!("{config:?}");
                if !debug.contains(&format!("trust: {trust}")) {
                    failures.push(format!("encrypt={value}: expected {trust}, got {debug}"));
                }
            }
            Err(error) => failures.push(format!("encrypt={value}: {error}")),
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn sslmode_form_values_match_exact_case() {
    let mut values = super::build_form_values_for_ssl(Some("REQUIRED"));
    assert!(MssqlDriver::new().build_config(&values).is_err());
    values.insert("ssl_mode".into(), "required".into());
    let (_, trusts_any_certificate) = super::extract_ssl(values);
    assert!(!trusts_any_certificate);
}

#[test]
fn sslmode_validate_profile_checks_uri_encrypt() {
    let mut failures = Vec::new();
    for scheme in ["sqlserver", "mssql"] {
        for (value, valid) in [
            ("on", true),
            ("required", true),
            ("requierd", false),
            ("", false),
        ] {
            let mut config = DbConfig::default_sqlserver();
            if let DbConfig::SqlServer {
                use_uri,
                uri,
                ssl_mode,
                ..
            } = &mut config
            {
                *use_uri = true;
                *uri = Some(format!("{scheme}://localhost/db?ENCRYPT={value}"));
                *ssl_mode = Some("unused-in-uri-mode".into());
            }
            let profile = ConnectionProfile::new("TLS URI validation", config);
            match MssqlDriver::new().validate_profile(&profile) {
                Ok(()) if valid => {}
                Err(DbError::InvalidProfile(message)) if !valid => {
                    if !message.contains("Accepted values:") || !message.contains(value) {
                        failures.push(format!("{scheme} encrypt={value}: {message}"));
                    }
                }
                result => failures.push(format!(
                    "{scheme} encrypt={value}: expected valid={valid}, got {result:?}"
                )),
            }
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn sslmode_verify_modes_use_root_cert_even_when_trust_is_set() {
    for mode in ["verify-ca", "verify-full"] {
        let debug = super::tiberius_debug(mode, true, Some("/etc/ssl/corp-ca.pem"));
        assert!(debug.contains("encryption: Required"), "{debug}");
        assert!(
            debug.contains("trust: CaCertificateLocation(\"/etc/ssl/corp-ca.pem\")"),
            "{debug}"
        );
    }
}
