use super::super::*;

#[test]
fn tls_metadata_certificate_pickers_follow_selected_mode() {
    let metadata = &*REDIS_METADATA;
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
