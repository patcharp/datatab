use super::*;
use crate::ClickHouseDriver;
use datatab_core::{DbDriver, ValueRef};

fn profile(
    mode: Option<&str>,
    root: Option<&str>,
    certificate: Option<&str>,
    key: Option<&str>,
) -> ConnectionProfile {
    ConnectionProfile::new(
        "TLS test",
        DbConfig::ClickHouse {
            url: if mode == Some("disable") {
                "http://localhost:8123"
            } else {
                "https://localhost:8443"
            }
            .into(),
            user: "default".into(),
            database: "default".into(),
            request_timeout_seconds: Some(30),
            allow_insecure_http_password: false,
            ssl_mode: mode.map(str::to_string),
            ssl_root_cert_path: root.map(str::to_string),
            ssl_client_cert_path: certificate.map(str::to_string),
            ssl_client_key_path: key.map(str::to_string),
        },
    )
}

#[test]
fn tls_verification_flags_are_pinned_per_mode() {
    for (mode, expected_mode, expected_flags) in [
        (None, TlsMode::LegacySystem, (true, false, false)),
        (Some("disable"), TlsMode::Disable, (false, false, false)),
        (Some("require"), TlsMode::Require, (false, true, false)),
        (Some("verify-ca"), TlsMode::VerifyCa, (false, false, true)),
        (
            Some("verify-full"),
            TlsMode::VerifyFull,
            (false, false, false),
        ),
    ] {
        let root =
            matches!(mode, Some("verify-ca" | "verify-full")).then_some("/nonexistent-test-ca.pem");
        let profile = profile(mode, root, None, None);
        let options = TlsOptions::from_profile(&profile).expect("pure TLS options");
        assert_eq!(options.mode, expected_mode);
        assert_eq!(
            options.verification_flags(),
            expected_flags,
            "{expected_mode:?}"
        );
    }
}

#[test]
fn tls_profile_validation_and_private_ca_client() {
    let driver = ClickHouseDriver::new();
    for mode in ["", "prefer", "VERIFY_CA", "Require", "unknown", " require"] {
        let error = driver
            .validate_profile(&profile(Some(mode), None, None, None))
            .expect_err("unknown or ambiguous mode must be refused");
        assert!(
            error.to_string().contains("Unknown or ambiguous"),
            "{error}"
        );
    }
    for mode in ["verify-ca", "verify-full"] {
        let error = driver
            .validate_profile(&profile(Some(mode), None, None, None))
            .expect_err("verification needs a root CA path");
        assert!(error.to_string().contains("root CA path"), "{error}");
    }
    assert!(
        driver
            .validate_profile(&profile(None, None, None, None))
            .is_ok()
    );
    for mode in ["require", "verify-ca", "verify-full"] {
        let root = (mode != "require").then_some("/nonexistent-test-ca.pem");
        let valid = profile(
            Some(mode),
            root,
            Some("/test-client.pem"),
            Some("/test-client-key.pem"),
        );
        driver
            .validate_profile(&valid)
            .expect("validation is pure: no certificate reads");
        let rebuilt = driver
            .build_config(&driver.extract_values(&valid.config))
            .expect("TLS form values");
        assert_eq!(
            serde_json::to_value(rebuilt).unwrap(),
            serde_json::to_value(&valid.config).unwrap()
        );
        assert!(
            driver
                .validate_profile(&profile(Some(mode), root, Some("/test-client.pem"), None))
                .is_err()
        );
        assert!(
            driver
                .validate_profile(&profile(
                    Some(mode),
                    root,
                    None,
                    Some("/test-client-key.pem")
                ))
                .is_err()
        );
        let mut insecure = valid.clone();
        if let DbConfig::ClickHouse { url, .. } = &mut insecure.config {
            *url = "http://localhost:8123".into();
        }
        assert!(driver.validate_profile(&insecure).is_err());
    }
    for mode in ["disable", "require"] {
        assert!(
            driver
                .validate_profile(&profile(Some(mode), Some("/unused.pem"), None, None))
                .is_err()
        );
    }
    for field in [
        "ssl_mode",
        "ssl_root_cert_path",
        "ssl_client_cert_path",
        "ssl_client_key_path",
    ] {
        let mut invalid = profile(Some("require"), None, None, None);
        invalid
            .value_refs
            .insert(field.into(), ValueRef::secret("fake", "test", None));
        assert!(driver.validate_profile(&invalid).is_err());
    }
    let rebuilt_legacy = driver
        .build_config(&driver.extract_values(&profile(None, None, None, None).config))
        .expect("form values");
    assert!(
        driver
            .validate_profile(&ConnectionProfile::new("edited", rebuilt_legacy))
            .is_err(),
        "saving an edit cannot retain the legacy-only missing mode"
    );
    private_ca_and_identity_build();
}

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove generated TLS fixtures");
    }
}

fn private_ca_and_identity_build() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let fixture =
        Fixture(std::env::temp_dir().join(format!("datatab-262-{}-{nonce}", std::process::id())));
    std::fs::create_dir(&fixture.0).expect("fixture directory");
    // Generated solely for this test. No real certificate or credential is read.
    let output = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=DataTab fake TLS test",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-keyout",
            "client-key.pem",
            "-out",
            "ca.pem",
        ])
        .current_dir(&fixture.0)
        .output()
        .expect("generate disposable PEM");
    assert!(output.status.success(), "openssl fixture generation failed");
    let root = fixture.0.join("ca.pem");
    let key = fixture.0.join("client-key.pem");
    let root = root.to_str().expect("test path");
    let key = key.to_str().expect("test path");
    for mode in ["verify-ca", "verify-full", "require"] {
        let profile = profile(
            Some(mode),
            (mode != "require").then_some(root),
            Some(root),
            Some(key),
        );
        let options = TlsOptions::from_profile(&profile).expect("valid TLS settings");
        let builder = options
            .configure(reqwest::blocking::Client::builder().use_rustls_tls())
            .expect("private CA and identity");
        builder
            .build()
            .expect("custom-only TLS client builds without network");
    }
    std::fs::write(
        fixture.0.join("invalid.pem"),
        "clearly fake, not a certificate",
    )
    .expect("fake PEM");
    let invalid = fixture.0.join("invalid.pem");
    let invalid_profile = profile(Some("verify-full"), invalid.to_str(), None, None);
    assert!(
        TlsOptions::from_profile(&invalid_profile)
            .expect("pure validation")
            .configure(reqwest::blocking::Client::builder())
            .is_err()
    );
    let missing = profile(
        Some("verify-full"),
        Some("/nonexistent-datatab-test-ca.pem"),
        None,
        None,
    );
    assert!(
        TlsOptions::from_profile(&missing)
            .expect("pure validation")
            .configure(reqwest::blocking::Client::builder())
            .is_err()
    );
}
