//! Guards against the reported client identity drifting from the app version.
//!
//! `datatab_core::client_identity()` bakes in `datatab_core`'s compile-time
//! version. Every crate inherits the single `[workspace.package]` version, so
//! the two must always match; this test fails if a crate ever stops
//! inheriting it.

#[test]
fn client_identity_matches_app_version() {
    let expected = format!("datatab/{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(datatab_core::client_identity(), expected);
}

#[test]
fn client_identity_token_matches_app_version() {
    let expected = format!("datatab-{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(datatab_core::client_identity_token(), expected);
}
