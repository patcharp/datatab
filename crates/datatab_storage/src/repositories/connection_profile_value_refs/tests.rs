use super::*;

#[test]
fn load_diagnostic_identifies_unknown_kind_without_stored_values() {
    let mut reference = ConnectionProfileValueRefDto::new_literal(
        "profile".into(),
        "password".into(),
        "private stored value".into(),
    );
    reference.ref_kind = "retired-kind".into();
    assert_eq!(
        reference.load_diagnostic(),
        Some(ValueRefLoadDiagnostic {
            field: "password".into(),
            failure: ValueRefLoadFailure::UnknownSourceKind("retired-kind".into()),
        })
    );
}

#[test]
fn load_diagnostic_accepts_complete_known_kinds() {
    let references = [
        ConnectionProfileValueRefDto::new_literal("profile".into(), "field".into(), "value".into()),
        ConnectionProfileValueRefDto::new_env("profile".into(), "field".into(), "KEY".into()),
        ConnectionProfileValueRefDto::new_dotenv(
            "profile".into(),
            "field".into(),
            "unused".into(),
            "KEY".into(),
        ),
        ConnectionProfileValueRefDto::new_file(
            "profile".into(),
            "field".into(),
            "unused".into(),
            None,
        ),
        ConnectionProfileValueRefDto::new_secret(
            "profile".into(),
            "field".into(),
            "provider".into(),
            "locator".into(),
            None,
        ),
        ConnectionProfileValueRefDto::new_auth("profile".into(), "field".into(), "token".into()),
    ];
    for reference in references {
        assert_eq!(reference.load_diagnostic(), None, "{}", reference.ref_kind);
    }
}

#[test]
fn unknown_kind_message_escapes_control_characters() {
    let diagnostic = ValueRefLoadDiagnostic {
        field: "password".into(),
        failure: ValueRefLoadFailure::UnknownSourceKind("retired\n\r\t\0\"\\kind".into()),
    };
    assert_eq!(
        diagnostic.message(),
        r#"Value ref 'password' has unknown source kind "retired\n\r\t\0\"\\kind""#
    );
}

#[test]
fn unknown_kind_message_limits_source_to_64_characters() {
    for length in [63, 64, 65] {
        let diagnostic = ValueRefLoadDiagnostic {
            field: "host".into(),
            failure: ValueRefLoadFailure::UnknownSourceKind("é".repeat(length)),
        };
        assert_eq!(diagnostic.message().matches('é').count(), length.min(64));
    }
}

/// The `param` kind went away with the Parameter value source. A complete
/// stored row of that kind is reported like any other unknown kind.
#[test]
fn load_diagnostic_flags_param_as_unknown_kind() {
    let mut reference = ConnectionProfileValueRefDto::new_secret(
        "profile".into(),
        "password".into(),
        "provider".into(),
        "name".into(),
        None,
    );
    reference.ref_kind = "param".into();
    reference.secret_locator = None;
    reference.param_name = Some("name".into());

    let diagnostic = reference
        .load_diagnostic()
        .expect("param is not a source kind");
    assert_eq!(
        diagnostic,
        ValueRefLoadDiagnostic {
            field: "password".into(),
            failure: ValueRefLoadFailure::UnknownSourceKind("param".into()),
        }
    );
    assert_eq!(
        diagnostic.message(),
        r#"Value ref 'password' has unknown source kind "param""#
    );
}
