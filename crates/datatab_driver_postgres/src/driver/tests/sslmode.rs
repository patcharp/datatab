use super::super::*;

#[test]
fn sslmode_unknown_profile_is_refused() {
    for value in ["verify-ful", ""] {
        let mut config = DbConfig::default_postgres();
        if let DbConfig::Postgres { ssl_mode, .. } = &mut config {
            *ssl_mode = Some(value.into());
        }
        let profile = ConnectionProfile::new("TLS validation", config);
        let error = PostgresDriver::new()
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
        ("disable", PgSslMode::Disable),
        ("allow", PgSslMode::Prefer),
        ("prefer", PgSslMode::Prefer),
        ("require", PgSslMode::Require),
        ("verify-ca", PgSslMode::VerifyCa),
        ("verify-full", PgSslMode::VerifyFull),
    ] {
        let mut config = DbConfig::default_postgres();
        if let DbConfig::Postgres { ssl_mode, .. } = &mut config {
            *ssl_mode = Some(value.into());
        }
        let mut profile = ConnectionProfile::new("TLS validation", config);
        assert!(
            PostgresDriver::new().validate_profile(&profile).is_ok(),
            "{value}"
        );
        assert_eq!(parse_pg_sslmode(value).expect("known mode"), expected);
        let uppercase = value.to_ascii_uppercase();
        if parse_pg_sslmode(&uppercase).is_ok() {
            failures.push(format!("{uppercase} was accepted"));
        }
        if let DbConfig::Postgres { ssl_mode, .. } = &mut profile.config {
            *ssl_mode = Some(uppercase.clone());
        }
        if PostgresDriver::new().validate_profile(&profile).is_ok() {
            failures.push(format!("{uppercase} profile was accepted"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn sslmode_uppercase_uri_preserves_verification() {
    let uri = "postgresql://localhost/app?SSLMODE=verify-full";
    assert_eq!(
        parse_pg_uri_sslmode(uri).expect("known mode"),
        PgSslMode::VerifyFull
    );
    let config: postgres::Config = with_protocol_sslmode(uri)
        .expect("known mode")
        .parse()
        .expect("valid URI");
    assert_eq!(config.get_ssl_mode(), postgres::config::SslMode::Require);
}

#[test]
fn sslmode_unknown_uri_is_refused() {
    let mut config = DbConfig::default_postgres();
    if let DbConfig::Postgres { use_uri, uri, .. } = &mut config {
        *use_uri = true;
        *uri = Some("postgresql://localhost/app?SSLMODE=verify-ful".into());
    }
    let error = PostgresDriver::new()
        .validate_profile(&ConnectionProfile::new("TLS validation", config))
        .expect_err("unknown URI mode");
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
        if parse_pg_uri_sslmode(&uri).is_ok() || with_protocol_sslmode(&uri).is_ok() {
            failures.push(query);
        }
        let mut config = DbConfig::default_postgres();
        if let DbConfig::Postgres {
            use_uri,
            uri: profile_uri,
            ..
        } = &mut config
        {
            *use_uri = true;
            *profile_uri = Some(uri);
        }
        if PostgresDriver::new()
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
            PgSslMode::Disable,
            postgres::config::SslMode::Disable,
        ),
        (
            "allow",
            PgSslMode::Prefer,
            postgres::config::SslMode::Prefer,
        ),
        (
            "prefer",
            PgSslMode::Prefer,
            postgres::config::SslMode::Prefer,
        ),
        (
            "require",
            PgSslMode::Require,
            postgres::config::SslMode::Require,
        ),
        (
            "verify-ca",
            PgSslMode::VerifyCa,
            postgres::config::SslMode::Require,
        ),
        (
            "verify-full",
            PgSslMode::VerifyFull,
            postgres::config::SslMode::Require,
        ),
    ] {
        for assignment in [format!("sslmode={value}"), format!("SSLMODE = '{value}'")] {
            let input =
                format!("host=localhost dbname=app {assignment} application_name='TLS check'");
            assert_eq!(
                parse_pg_uri_sslmode(&input).expect("known mode"),
                expected,
                "{assignment}"
            );
            let config = with_protocol_sslmode(&input)
                .expect("valid keywords")
                .parse::<postgres::Config>()
                .expect("wire config");
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
            parse_pg_uri_sslmode(&input).expect("escaped mode"),
            PgSslMode::VerifyFull
        );
        let config = with_protocol_sslmode(&input)
            .expect("valid keywords")
            .parse::<postgres::Config>()
            .expect("wire config");
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
        if parse_pg_uri_sslmode(input).is_ok() {
            accepted.push(input);
            continue;
        }
        assert!(
            with_protocol_sslmode(input).is_err(),
            "wire accepts invalid keywords: {input}"
        );
        let mut config = DbConfig::default_postgres();
        if let DbConfig::Postgres { use_uri, uri, .. } = &mut config {
            *use_uri = true;
            *uri = Some(input.into());
        }
        assert!(
            PostgresDriver::new()
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
        parse_pg_uri_sslmode(input).expect("known mode"),
        PgSslMode::VerifyFull
    );
    let config = with_protocol_sslmode(input)
        .expect("valid keywords")
        .parse::<postgres::Config>()
        .expect("wire config");
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
            parse_pg_uri_sslmode(&input).is_err(),
            "accepted encoded query key: {query}"
        );
        assert!(
            with_protocol_sslmode(&input).is_err(),
            "wire accepts encoded query key: {query}"
        );
    }
}

#[test]
fn sslmode_url_question_mark_before_first_at_is_refused() {
    let input = "postgres://u:x?sslmode=verify-full&k=@db.example/app?application_name=a";
    assert!(
        parse_pg_uri_sslmode(input).is_err(),
        "ambiguous credentials accepted"
    );
    assert!(
        with_protocol_sslmode(input).is_err(),
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
        let error = parse_pg_uri_sslmode(input).expect_err("invalid mode");
        if error.to_string().contains("hunter2") {
            exposed.push(name);
        }
        let error = with_protocol_sslmode(input).expect_err("invalid mode before wire config");
        if error.to_string().contains("hunter2") {
            exposed.push(name);
        }
    }
    assert!(exposed.is_empty(), "mode errors expose values: {exposed:?}");
}

#[test]
fn connection_url_gate_distinguishes_keywords() {
    for input in ["postgres://localhost/db", "postgresql://localhost/db"] {
        assert!(is_connection_url(input));
    }
    for input in [
        "host=localhost application_name='postgres://localhost/db'",
        "application_name='postgresql://localhost/db' sslmode=require",
        "redshift://localhost/db",
        "POSTGRES://localhost/db",
        "postgres:/localhost/db",
        "",
    ] {
        assert!(!is_connection_url(input));
    }
}

#[test]
fn sslmode_percent_encoded_password_delimiters_still_work() {
    for (input, password, expected_mode, wire_mode) in [
        (
            "postgres://u:p%3Fw%40@localhost/db?sslmode=require",
            "p?w@",
            PgSslMode::Require,
            postgres::config::SslMode::Require,
        ),
        (
            "postgres://u:x%3Fsslmode%3Dverify-full%26k%3D@localhost/app?application_name=a",
            "x?sslmode=verify-full&k=",
            PgSslMode::Prefer,
            postgres::config::SslMode::Prefer,
        ),
    ] {
        let mode = parse_pg_uri_sslmode(input).expect("valid policy");
        let rewritten = with_protocol_sslmode(input).expect("valid URI");
        let config = parse_pg_config(&rewritten, mode).expect("valid wire config");
        assert_eq!(mode, expected_mode);
        assert_eq!(config.get_ssl_mode(), wire_mode);
        assert_eq!(config.get_password(), Some(password.as_bytes()));
    }
}

#[test]
fn sslmode_parsed_config_enforces_policy_over_string_mode() {
    for (mode, wire_mode) in [
        (PgSslMode::Disable, postgres::config::SslMode::Disable),
        (PgSslMode::Prefer, postgres::config::SslMode::Prefer),
        (PgSslMode::Require, postgres::config::SslMode::Require),
        (PgSslMode::VerifyCa, postgres::config::SslMode::Require),
        (PgSslMode::VerifyFull, postgres::config::SslMode::Require),
    ] {
        for input in [
            "host=localhost sslmode=disable",
            "postgres://localhost/db?sslmode=disable",
        ] {
            let config = parse_pg_config(input, mode).expect("valid config");
            assert_eq!(config.get_ssl_mode(), wire_mode);
        }
    }
}
