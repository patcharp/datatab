use super::super::*;

#[test]
fn tls_metadata_mssql_on_label_discloses_unverified_server_certificate() {
    let modes = METADATA.ssl_modes.expect("TLS modes");
    let mode = modes.iter().find(|mode| mode.id == "on").expect("mode");
    assert_eq!(
        mode.label,
        "on (encrypted, server certificate not verified)"
    );
}

#[test]
fn tls_metadata_certificate_pickers_follow_selected_mode() {
    let metadata = &*METADATA;
    let fields = metadata.ssl_cert_fields.expect("certificate fields");
    for (mode, root, client) in [
        ("off", false, false),
        ("on", false, false),
        ("required", true, false),
    ] {
        assert_eq!(
            fields.visible_for_mode(mode),
            (root, client),
            "{} mode {mode}",
            metadata.id
        );
    }
}
