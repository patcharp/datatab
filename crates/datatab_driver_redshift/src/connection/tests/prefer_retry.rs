use super::super::{RedshiftSslMode, should_retry_without_tls};
use std::io::{Error, ErrorKind};

#[test]
fn prefer_does_not_retry_an_io_reset_before_the_ssl_reply() {
    let error = Error::new(
        ErrorKind::ConnectionReset,
        "connection reset before the SSL negotiation response",
    );

    assert!(
        !should_retry_without_tls(RedshiftSslMode::Prefer, Some(&error)),
        "an I/O reset before the SSL response must not retry the login in clear text"
    );
}

#[test]
fn prefer_does_not_retry_an_error_without_a_source() {
    assert!(!should_retry_without_tls(RedshiftSslMode::Prefer, None));
}

#[test]
fn only_prefer_retries_an_error_with_a_native_tls_source() {
    let Err(error) = native_tls::Certificate::from_pem(b"invalid certificate") else {
        panic!("invalid PEM must produce a native TLS error");
    };

    assert!(should_retry_without_tls(
        RedshiftSslMode::Prefer,
        Some(&error)
    ));
    for mode in [
        RedshiftSslMode::Disable,
        RedshiftSslMode::Require,
        RedshiftSslMode::VerifyCa,
        RedshiftSslMode::VerifyFull,
    ] {
        assert!(
            !should_retry_without_tls(mode, Some(&error)),
            "{mode:?} must not retry in clear text"
        );
    }
}
