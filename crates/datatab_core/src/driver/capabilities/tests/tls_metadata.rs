use super::super::SslCertFields;

#[test]
fn tls_metadata_visibility_uses_driver_modes_and_supported_fields() {
    let fields = SslCertFields {
        root_cert: true,
        client_cert: true,
        root_cert_modes: Some(&["strict"]),
        client_cert_modes: Some(&["strict", "encrypted"]),
    };
    for (mode, expected) in [
        ("off", (false, false)),
        ("on", (false, false)),
        ("strict", (true, true)),
        ("encrypted", (false, true)),
        ("unknown", (false, false)),
    ] {
        assert_eq!(fields.visible_for_mode(mode), expected, "mode {mode}");
    }
    let unsupported = SslCertFields {
        root_cert: false,
        client_cert: false,
        ..fields
    };
    assert_eq!(unsupported.visible_for_mode("strict"), (false, false));
}

#[test]
fn tls_metadata_unspecified_modes_keep_legacy_visibility() {
    let fields = SslCertFields {
        root_cert: true,
        client_cert: true,
        ..Default::default()
    };
    for mode in [
        "",
        "off",
        "disable",
        "DISABLED",
        "on",
        "prefer",
        "PREFERRED",
        "require",
        "required",
        "verify",
        "verify-ca",
        "VERIFY_CA",
    ] {
        assert_eq!(
            fields.visible_for_mode(mode),
            (
                crate::ssl_mode_id_requires_root_cert(mode),
                crate::ssl_mode_id_is_cert_active(mode),
            )
        );
    }
}
