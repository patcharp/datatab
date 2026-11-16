use super::super::{RedshiftSslMode, RedshiftTlsCerts, build_tls_connector};
use super::{TEST_CA_PEM, write_temp_file};
use datatab_core::DbError;
use uuid::Uuid;

#[test]
fn require_ignores_a_missing_root_certificate() {
    let missing = std::env::temp_dir().join(format!("datatab_missing_ca_{}", Uuid::new_v4()));
    let certs = RedshiftTlsCerts {
        root_cert_path: Some(missing.to_string_lossy().into_owned()),
        ..Default::default()
    };

    for mode in [RedshiftSslMode::Require, RedshiftSslMode::Prefer] {
        let result = build_tls_connector(mode.tls_verification(), &certs);

        assert!(
            result.is_ok(),
            "{mode:?} must ignore a missing root CA: {result:?}"
        );
    }
}

#[test]
fn verify_modes_report_a_missing_root_certificate() {
    let missing = std::env::temp_dir().join(format!("datatab_missing_ca_{}", Uuid::new_v4()));
    let certs = RedshiftTlsCerts {
        root_cert_path: Some(missing.to_string_lossy().into_owned()),
        ..Default::default()
    };

    for mode in [RedshiftSslMode::VerifyCa, RedshiftSslMode::VerifyFull] {
        let result = build_tls_connector(mode.tls_verification(), &certs);
        let Err(DbError::ConnectionFailed(error)) = result else {
            panic!("{mode:?} must report a missing root CA");
        };
        let message = error.to_string();
        assert!(message.contains("Failed to read TLS root certificate"));
        assert!(message.contains(missing.to_string_lossy().as_ref()));
    }
}

#[test]
fn verify_modes_load_a_valid_root_certificate() {
    let path = write_temp_file("connector_ca", TEST_CA_PEM.as_bytes());
    let certs = RedshiftTlsCerts {
        root_cert_path: Some(path.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let results = [RedshiftSslMode::VerifyCa, RedshiftSslMode::VerifyFull]
        .map(|mode| build_tls_connector(mode.tls_verification(), &certs));
    std::fs::remove_file(&path).expect("remove root CA fixture");

    for result in results {
        assert!(result.is_ok(), "valid root CA must load: {result:?}");
    }
}
