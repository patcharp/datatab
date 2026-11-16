use super::super::*;

#[test]
fn sslmode_unknown_profile_is_refused() {
    for value in ["verify-ful", ""] {
        let mut config = DbConfig::default_redshift();
        if let DbConfig::Redshift { ssl_mode, .. } = &mut config {
            *ssl_mode = Some(value.into());
        }
        let profile = ConnectionProfile::new("TLS validation", config);
        let error = RedshiftDriver::new()
            .validate_profile(&profile)
            .expect_err("unknown TLS mode must be refused");
        assert!(matches!(error, DbError::InvalidProfile(_)));
        let message = error.to_string();
        assert!(!message.contains(&format!("'{value}'")), "{message}");
        for accepted in [
            "disable",
            "allow",
            "prefer",
            "require",
            "verify-ca",
            "verify-full",
        ] {
            assert!(message.contains(accepted), "{message}");
        }
    }
}

#[test]
fn sslmode_saved_value_policy_table() {
    let mut failures = Vec::new();
    for (value, expected) in [
        ("disable", RedshiftSslMode::Disable),
        ("allow", RedshiftSslMode::Prefer),
        ("prefer", RedshiftSslMode::Prefer),
        ("require", RedshiftSslMode::Require),
        ("verify-ca", RedshiftSslMode::VerifyCa),
        ("verify-full", RedshiftSslMode::VerifyFull),
    ] {
        let mut config = DbConfig::default_redshift();
        if let DbConfig::Redshift { ssl_mode, .. } = &mut config {
            *ssl_mode = Some(value.into());
        }
        let mut profile = ConnectionProfile::new("TLS validation", config);
        assert!(
            RedshiftDriver::new().validate_profile(&profile).is_ok(),
            "{value}"
        );
        assert_eq!(RedshiftSslMode::parse(value).expect("known mode"), expected);
        let uppercase = value.to_ascii_uppercase();
        if RedshiftSslMode::parse(&uppercase).is_ok() {
            failures.push(format!("{uppercase} was accepted"));
        }
        if let DbConfig::Redshift { ssl_mode, .. } = &mut profile.config {
            *ssl_mode = Some(uppercase.clone());
        }
        if RedshiftDriver::new().validate_profile(&profile).is_ok() {
            failures.push(format!("{uppercase} profile was accepted"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn sslmode_uppercase_uri_preserves_verification() {
    let (config, mode) =
        build_uri_config("redshift://localhost/dev?SSLMODE=verify-full", None).expect("valid URI");
    assert_eq!(mode, RedshiftSslMode::VerifyFull);
    assert_eq!(config.get_ssl_mode(), postgres::config::SslMode::Require);
}

#[test]
fn sslmode_unknown_uri_is_refused() {
    let error = build_uri_config("redshift://localhost/dev?SSLMODE=verify-ful", None)
        .expect_err("unknown mode");
    assert!(!error.to_string().contains("'verify-ful'"));
}

#[test]
fn sslmode_duplicate_uri_keys_are_refused() {
    let mut failures = Vec::new();
    for query in [
        "sslmode=verify-full&sslmode=disable",
        "sslmode=verify-full&SSLMODE=disable",
        "SSLMODE=disable&sslmode=verify-full",
        "sslmode=verify-full&sslmode=verify-full",
        "sslmode=verify-full&SSLMODE",
    ] {
        let uri = format!("postgresql://localhost/app?{query}");
        if redshift_uri_sslmode(&uri).is_ok() || build_uri_config(&uri, None).is_ok() {
            failures.push(query);
        }
        let mut config = DbConfig::default_redshift();
        if let DbConfig::Redshift {
            use_uri,
            uri: profile_uri,
            ..
        } = &mut config
        {
            *use_uri = true;
            *profile_uri = Some(uri);
        }
        if RedshiftDriver::new()
            .validate_profile(&ConnectionProfile::new("TLS validation", config))
            .is_ok()
        {
            failures.push(query);
        }
    }
    assert!(failures.is_empty(), "duplicates accepted: {failures:?}");
}

#[test]
fn sslmode_keyword_policy_matches_wire_mode() {
    for (value, expected, wire) in [
        (
            "disable",
            RedshiftSslMode::Disable,
            postgres::config::SslMode::Disable,
        ),
        (
            "allow",
            RedshiftSslMode::Prefer,
            postgres::config::SslMode::Prefer,
        ),
        (
            "prefer",
            RedshiftSslMode::Prefer,
            postgres::config::SslMode::Prefer,
        ),
        (
            "require",
            RedshiftSslMode::Require,
            postgres::config::SslMode::Require,
        ),
        (
            "verify-ca",
            RedshiftSslMode::VerifyCa,
            postgres::config::SslMode::Require,
        ),
        (
            "verify-full",
            RedshiftSslMode::VerifyFull,
            postgres::config::SslMode::Require,
        ),
    ] {
        for assignment in [format!("sslmode={value}"), format!("SSLMODE = '{value}'")] {
            let input =
                format!("host=localhost dbname=app {assignment} application_name='TLS check'");
            assert_eq!(
                redshift_uri_sslmode(&input).expect("known mode"),
                expected,
                "{assignment}"
            );
            let config = build_uri_config(&input, None).expect("valid keywords").0;
            assert_eq!(config.get_ssl_mode(), wire, "{assignment}");
            assert_eq!(config.get_application_name(), Some("TLS check"));
        }
    }
}

#[test]
fn sslmode_keyword_quoting_and_escapes_follow_libpq() {
    for assignment in [r"sslmode=verify\-full", r"sslmode='verify\-full'"] {
        let input = format!(
            r"host=localhost application_name='it\'s sslmode=disable ?sslmode=disable' {assignment}"
        );
        assert_eq!(
            redshift_uri_sslmode(&input).expect("escaped mode"),
            RedshiftSslMode::VerifyFull
        );
        let config = build_uri_config(&input, None).expect("valid keywords").0;
        assert_eq!(config.get_ssl_mode(), postgres::config::SslMode::Require);
        assert_eq!(
            config.get_application_name(),
            Some("it's sslmode=disable ?sslmode=disable")
        );
    }
}

#[test]
fn sslmode_keyword_invalid_or_duplicate_is_refused() {
    let mut accepted = Vec::new();
    for input in [
        "host=localhost sslmode=VERIFY-FULL",
        "host=localhost sslmode=verify-ful",
        "host=localhost sslmode=''",
        "host=localhost sslmode=verify-full sslmode=disable",
        "host=localhost SSLMODE='disable' sslmode=verify-full",
        "host=localhost sslmode='require' sslmode='require'",
        "host=localhost sslmode='require'sslmode=disable",
        "host=localhost sslmode='verify-full",
        "host=localhost sslmode",
    ] {
        if redshift_uri_sslmode(input).is_ok() {
            accepted.push(input);
            continue;
        }
        assert!(
            build_uri_config(input, None).is_err(),
            "wire accepts invalid keywords: {input}"
        );
        let mut config = DbConfig::default_redshift();
        if let DbConfig::Redshift { use_uri, uri, .. } = &mut config {
            *use_uri = true;
            *uri = Some(input.into());
        }
        assert!(
            RedshiftDriver::new()
                .validate_profile(&ConnectionProfile::new("TLS validation", config))
                .is_err()
        );
    }
    assert!(
        accepted.is_empty(),
        "accepted invalid keywords: {accepted:?}"
    );
}

#[test]
fn sslmode_keyword_trailing_escape_cannot_consume_protocol_mode() {
    let input = "host=localhost sslmode=verify-full application_name=TLS\\";
    assert_eq!(
        redshift_uri_sslmode(input).expect("known mode"),
        RedshiftSslMode::VerifyFull
    );
    let config = build_uri_config(input, None).expect("valid keywords").0;
    assert_eq!(config.get_ssl_mode(), postgres::config::SslMode::Require);
    assert_eq!(config.get_application_name(), Some("TLS"));
}

#[test]
fn sslmode_percent_encoded_query_keys_are_refused() {
    for query in [
        "sslmode=verify-full&ssl%6Dode=disable",
        "ssl%6dode=disable&sslmode=verify-full",
        "%73slmode=verify-full",
        "sslmode=verify-full&%53SLMODE=disable",
        "sslmode=verify-full&application%5fname=TLS",
    ] {
        let input = format!("postgresql://localhost/app?{query}");
        assert!(
            redshift_uri_sslmode(&input).is_err(),
            "accepted encoded query key: {query}"
        );
        assert!(
            build_uri_config(&input, None).is_err(),
            "wire accepts encoded query key: {query}"
        );
    }
}

#[test]
fn sslmode_url_question_mark_before_first_at_is_refused() {
    let input = "postgres://u:p?w@host/db?sslmode=require";
    assert!(
        redshift_uri_sslmode(input).is_err(),
        "ambiguous credentials accepted"
    );
    assert!(
        build_uri_config(input, None).is_err(),
        "ambiguous URL reached wire config"
    );
}

#[test]
fn sslmode_errors_do_not_echo_values() {
    let mut exposed = Vec::new();
    for (name, input) in [
        (
            "empty keyword before password",
            "host=h sslmode= password=hunter2",
        ),
        ("unknown keyword", "host=h sslmode=hunter2"),
        ("quoted empty keyword", "host=h sslmode='' password=hunter2"),
        ("unknown URL", "postgres://localhost/db?sslmode=hunter2"),
        (
            "empty URL",
            "postgres://localhost/db?sslmode=&password=hunter2",
        ),
    ] {
        let error = redshift_uri_sslmode(input).expect_err("invalid mode");
        if error.to_string().contains("hunter2") {
            exposed.push(name);
        }
        let error = build_uri_config(input, None).expect_err("invalid mode before wire config");
        if error.to_string().contains("hunter2") {
            exposed.push(name);
        }
    }
    assert!(exposed.is_empty(), "mode errors expose values: {exposed:?}");
}

#[test]
fn sslmode_percent_encoded_password_delimiters_still_work() {
    for (input, password, expected_mode, wire_mode) in [
        (
            "postgres://u:p%3Fw%40@localhost/db?sslmode=require",
            "p?w@",
            RedshiftSslMode::Require,
            postgres::config::SslMode::Require,
        ),
        (
            "postgres://u:x%3Fsslmode%3Dverify-full%26k%3D@localhost/app?application_name=a",
            "x?sslmode=verify-full&k=",
            RedshiftSslMode::Prefer,
            postgres::config::SslMode::Prefer,
        ),
    ] {
        let (config, mode) = build_uri_config(input, None).expect("valid URI");
        assert_eq!(mode, expected_mode);
        assert_eq!(config.get_ssl_mode(), wire_mode);
        assert_eq!(config.get_password(), Some(password.as_bytes()));
    }
}
