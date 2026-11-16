use super::super::*;

#[test]
fn tls_metadata_redshift_allow_label_discloses_unverified_server_certificate() {
    let modes = METADATA.ssl_modes.expect("TLS modes");
    let mode = modes.iter().find(|mode| mode.id == "allow").expect("mode");
    assert_eq!(mode.label, "allow (unverified)");
}

#[test]
fn tls_metadata_redshift_prefer_label_discloses_unverified_server_certificate() {
    let modes = METADATA.ssl_modes.expect("TLS modes");
    let mode = modes.iter().find(|mode| mode.id == "prefer").expect("mode");
    assert_eq!(mode.label, "prefer (unverified)");
}

#[test]
fn tls_metadata_redshift_require_label_discloses_unverified_server_certificate() {
    let modes = METADATA.ssl_modes.expect("TLS modes");
    let mode = modes
        .iter()
        .find(|mode| mode.id == "require")
        .expect("mode");
    assert_eq!(mode.label, "require (unverified)");
}

#[test]
fn tls_metadata_certificate_pickers_follow_selected_mode() {
    let metadata = &*METADATA;
    let fields = metadata.ssl_cert_fields.expect("certificate fields");
    for (mode, root, client) in [
        ("disable", false, false),
        ("allow", false, true),
        ("prefer", false, true),
        ("require", false, true),
        ("verify-ca", true, true),
        ("verify-full", true, true),
    ] {
        assert_eq!(
            fields.visible_for_mode(mode),
            (root, client),
            "{} mode {mode}",
            metadata.id
        );
    }
}
