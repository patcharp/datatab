use super::super::*;

#[test]
fn tls_metadata_mysql_metadata_preferred_label_discloses_unverified_server_certificate() {
    let modes = MYSQL_METADATA.ssl_modes.expect("TLS modes");
    let mode = modes
        .iter()
        .find(|mode| mode.id == "PREFERRED")
        .expect("mode");
    assert_eq!(mode.label, "preferred (unverified)");
}

#[test]
fn tls_metadata_mysql_metadata_required_label_discloses_unverified_server_certificate() {
    let modes = MYSQL_METADATA.ssl_modes.expect("TLS modes");
    let mode = modes
        .iter()
        .find(|mode| mode.id == "REQUIRED")
        .expect("mode");
    assert_eq!(mode.label, "required (unverified)");
}

#[test]
fn tls_metadata_mariadb_metadata_preferred_label_discloses_unverified_server_certificate() {
    let modes = MARIADB_METADATA.ssl_modes.expect("TLS modes");
    let mode = modes
        .iter()
        .find(|mode| mode.id == "PREFERRED")
        .expect("mode");
    assert_eq!(mode.label, "preferred (unverified)");
}

#[test]
fn tls_metadata_mariadb_metadata_required_label_discloses_unverified_server_certificate() {
    let modes = MARIADB_METADATA.ssl_modes.expect("TLS modes");
    let mode = modes
        .iter()
        .find(|mode| mode.id == "REQUIRED")
        .expect("mode");
    assert_eq!(mode.label, "required (unverified)");
}

#[test]
fn tls_metadata_certificate_pickers_follow_selected_mode() {
    for metadata in [&*MYSQL_METADATA, &*MARIADB_METADATA] {
        let fields = metadata.ssl_cert_fields.expect("certificate fields");
        for (mode, root, client) in [
            ("DISABLED", false, false),
            ("PREFERRED", false, true),
            ("REQUIRED", false, true),
            ("VERIFY_CA", true, true),
            ("VERIFY_IDENTITY", true, true),
        ] {
            assert_eq!(
                fields.visible_for_mode(mode),
                (root, client),
                "{} mode {mode}",
                metadata.id
            );
        }
    }
}
